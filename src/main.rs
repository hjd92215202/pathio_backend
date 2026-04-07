use axum::{
    extract::{State, Path, FromRequestParts, Query},
    http::StatusCode,
    routing::{get, put, post},
    Json, Router,
};
use axum::http::request::Parts;
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool, postgres::PgPoolOptions};
use std::net::SocketAddr;
use tower_http::cors::CorsLayer;
use uuid::Uuid;
use bcrypt::{hash, verify, DEFAULT_COST};
use jsonwebtoken::{encode, Header, EncodingKey, decode, DecodingKey, Validation};

// ==========================================
// 1. 数据模型定义
// ==========================================

// 更新节点信息的请求体
#[derive(Deserialize)]
pub struct UpdateNodeReq {
    pub title: Option<String>,
    pub status: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    pub sub: Uuid,
    pub exp: usize,
}

#[derive(Deserialize)]
pub struct AuthReq {
    pub username: String,
    pub email: Option<String>,
    pub password: String,
}

#[derive(Serialize, Deserialize, FromRow)]
pub struct Node {
    pub id: Uuid,
    pub roadmap_id: Option<Uuid>, 
    pub title: String,
    pub status: Option<String>,
    pub pos_x: f64,
    pub pos_y: f64,
}

// 接收前端过滤参数
#[derive(Deserialize)]
pub struct RoadmapQuery {
    pub roadmap_id: Uuid,
}

#[derive(Deserialize)]
pub struct CreateNodeReq {
    pub roadmap_id: Uuid, // 显式要求归属
    pub title: String,
    pub pos_x: f64,
    pub pos_y: f64,
}

#[derive(Deserialize)]
pub struct UpdateNodePosReq {
    pub pos_x: f64,
    pub pos_y: f64,
}

#[derive(Serialize, Deserialize, FromRow)]
pub struct Edge {
    pub id: Uuid,
    pub roadmap_id: Option<Uuid>,
    pub source_node_id: Uuid,
    pub target_node_id: Uuid,
}

#[derive(Deserialize)]
pub struct CreateEdgeReq {
    pub roadmap_id: Uuid, // 显式要求归属
    pub source: Uuid,
    pub target: Uuid,
}

#[derive(Serialize, Deserialize, FromRow)]
pub struct Note {
    pub node_id: Uuid,
    pub content: serde_json::Value,
}

#[derive(Deserialize)]
pub struct UpdateNoteReq {
    pub content: serde_json::Value,
}

#[derive(Serialize, Deserialize, FromRow)]
pub struct Roadmap {
    pub id: Uuid,
    pub title: String,
    pub share_token: Option<String>,
}

#[derive(Serialize)]
pub struct ShareData {
    pub roadmap_title: String,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

// JWT 提取器
#[axum::async_trait]
impl<S> FromRequestParts<S> for Claims
where S: Send + Sync,
{
    type Rejection = (StatusCode, String);
    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let auth_header = parts.headers.get("Authorization").and_then(|h| h.to_str().ok()).ok_or((StatusCode::UNAUTHORIZED, "未登录".to_string()))?;
        if !auth_header.starts_with("Bearer ") { return Err((StatusCode::UNAUTHORIZED, "Token格式错误".to_string())); }
        let token = &auth_header[7..];
        let token_data = decode::<Claims>(token, &DecodingKey::from_secret("secret".as_ref()), &Validation::default())
            .map_err(|_| (StatusCode::UNAUTHORIZED, "会话过期".to_string()))?;
        Ok(token_data.claims)
    }
}

// ==========================================
// 2. 认证逻辑
// ==========================================

async fn register(State(pool): State<PgPool>, Json(payload): Json<AuthReq>) -> Result<StatusCode, (StatusCode, String)> {
    let email = payload.email.ok_or((StatusCode::BAD_REQUEST, "邮箱必填".to_string()))?;
    let hashed = hash(payload.password, DEFAULT_COST).unwrap();
    let mut tx = pool.begin().await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let user_id: Uuid = sqlx::query_scalar("INSERT INTO users (nickname, email, password_hash) VALUES ($1, $2, $3) RETURNING id")
        .bind(payload.username).bind(email).bind(hashed).fetch_one(&mut *tx).await
        .map_err(|_| (StatusCode::BAD_REQUEST, "用户名或邮箱占用".to_string()))?;

    let org_id: Uuid = sqlx::query_scalar("INSERT INTO organizations (name, owner_id) VALUES ($1, $2) RETURNING id")
        .bind("默认空间").bind(user_id).fetch_one(&mut *tx).await.unwrap();

    sqlx::query("INSERT INTO roadmaps (org_id, title, share_token) VALUES ($1, $2, $3)")
        .bind(org_id).bind("我的首个研究路径").bind(Uuid::new_v4().to_string()[..8].to_string()).execute(&mut *tx).await.unwrap();

    tx.commit().await.unwrap();
    Ok(StatusCode::CREATED)
}

async fn login(State(pool): State<PgPool>, Json(payload): Json<AuthReq>) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let (id, hash_val): (Uuid, String) = sqlx::query_as("SELECT id, password_hash FROM users WHERE nickname = $1").bind(payload.username).fetch_optional(&pool).await.unwrap()
        .ok_or((StatusCode::UNAUTHORIZED, "用户不存在".to_string()))?;
    if verify(payload.password, &hash_val).unwrap() {
        let claims = Claims { sub: id, exp: 10000000000 }; 
        let token = encode(&Header::default(), &claims, &EncodingKey::from_secret("secret".as_ref())).unwrap();
        Ok(Json(serde_json::json!({ "token": token })))
    } else { Err((StatusCode::UNAUTHORIZED, "密码错误".to_string())) }
}

// ==========================================
// 3. 业务逻辑 (已适配 Roadmap 隔离)
// ==========================================

// 更新节点标题或状态
async fn update_node(
    claims: Claims,
    Path(id): Path<Uuid>,
    State(pool): State<PgPool>,
    Json(payload): Json<UpdateNodeReq>,
) -> Result<StatusCode, (StatusCode, String)> {
    let query = "
        UPDATE nodes SET 
            title = COALESCE($1, title),
            status = COALESCE($2, status)
        WHERE id = $3 AND roadmap_id IN (
            SELECT r.id FROM roadmaps r JOIN organizations o ON r.org_id = o.id WHERE o.owner_id = $4
        )
    ";
    sqlx::query(query).bind(payload.title).bind(payload.status).bind(id).bind(claims.sub).execute(&pool).await.unwrap();
    Ok(StatusCode::OK)
}

// 删除节点
async fn delete_node(
    claims: Claims,
    Path(id): Path<Uuid>,
    State(pool): State<PgPool>,
) -> Result<StatusCode, (StatusCode, String)> {
    let query = "
        DELETE FROM nodes WHERE id = $1 AND roadmap_id IN (
            SELECT r.id FROM roadmaps r JOIN organizations o ON r.org_id = o.id WHERE o.owner_id = $2
        )
    ";
    sqlx::query(query).bind(id).bind(claims.sub).execute(&pool).await.unwrap();
    Ok(StatusCode::OK)
}

// 获取分享路线图中的具体节点笔记 (无需登录)
async fn get_shared_note(
    Path((token, node_id)): Path<(String, Uuid)>,
    State(pool): State<PgPool>,
) -> Result<Json<Note>, (StatusCode, String)> {
    // 校验：该 node_id 必须属于这个 share_token 对应的路线图
    let query = "
        SELECT n.node_id, n.content FROM notes n
        JOIN nodes nd ON n.node_id = nd.id
        JOIN roadmaps r ON nd.roadmap_id = r.id
        WHERE r.share_token = $1 AND n.node_id = $2
    ";
    match sqlx::query_as::<_, Note>(query).bind(token).bind(node_id).fetch_one(&pool).await {
        Ok(note) => Ok(Json(note)),
        Err(_) => Err((StatusCode::FORBIDDEN, "无权访问或内容不存在".to_string())),
    }
}

async fn get_roadmaps(claims: Claims, State(pool): State<PgPool>) -> Result<Json<Vec<Roadmap>>, (StatusCode, String)> {
    let query = "SELECT r.id, r.title, r.share_token FROM roadmaps r JOIN organizations o ON r.org_id = o.id WHERE o.owner_id = $1 ORDER BY r.created_at DESC";
    let res = sqlx::query_as::<_, Roadmap>(query).bind(claims.sub).fetch_all(&pool).await.unwrap();
    Ok(Json(res))
}

async fn create_roadmap(claims: Claims, State(pool): State<PgPool>, Json(payload): Json<serde_json::Value>) -> Result<Json<Roadmap>, (StatusCode, String)> {
    let title = payload["title"].as_str().unwrap_or("未命名路线图");
    let query = "INSERT INTO roadmaps (org_id, title, share_token) VALUES ((SELECT id FROM organizations WHERE owner_id = $1 LIMIT 1), $2, $3) RETURNING id, title, share_token";
    let res = sqlx::query_as::<_, Roadmap>(query).bind(claims.sub).bind(title).bind(Uuid::new_v4().to_string()[..8].to_string()).fetch_one(&pool).await.unwrap();
    Ok(Json(res))
}

async fn get_all_nodes(claims: Claims, Query(q): Query<RoadmapQuery>, State(pool): State<PgPool>) -> Result<Json<Vec<Node>>, (StatusCode, String)> {
    let query = "SELECT n.* FROM nodes n JOIN roadmaps r ON n.roadmap_id = r.id JOIN organizations o ON r.org_id = o.id WHERE o.owner_id = $1 AND r.id = $2";
    let res = sqlx::query_as::<_, Node>(query).bind(claims.sub).bind(q.roadmap_id).fetch_all(&pool).await.unwrap();
    Ok(Json(res))
}

async fn create_node(claims: Claims, State(pool): State<PgPool>, Json(payload): Json<CreateNodeReq>) -> Result<(StatusCode, Json<Node>), (StatusCode, String)> {
    // 校验该路线图是否属于该用户
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM roadmaps r JOIN organizations o ON r.org_id = o.id WHERE r.id = $1 AND o.owner_id = $2)")
        .bind(payload.roadmap_id).bind(claims.sub).fetch_one(&pool).await.unwrap();
    if !exists { return Err((StatusCode::FORBIDDEN, "无权访问该路线图".to_string())); }

    let res = sqlx::query_as::<_, Node>("INSERT INTO nodes (roadmap_id, title, pos_x, pos_y) VALUES ($1, $2, $3, $4) RETURNING *")
        .bind(payload.roadmap_id).bind(payload.title).bind(payload.pos_x).bind(payload.pos_y).fetch_one(&pool).await.unwrap();
    Ok((StatusCode::CREATED, Json(res)))
}

async fn update_node_position(claims: Claims, Path(id): Path<Uuid>, State(pool): State<PgPool>, Json(payload): Json<UpdateNodePosReq>) -> Result<StatusCode, (StatusCode, String)> {
    let query = "UPDATE nodes SET pos_x = $1, pos_y = $2 WHERE id = $3 AND roadmap_id IN (SELECT r.id FROM roadmaps r JOIN organizations o ON r.org_id = o.id WHERE o.owner_id = $4)";
    let res = sqlx::query(query).bind(payload.pos_x).bind(payload.pos_y).bind(id).bind(claims.sub).execute(&pool).await.unwrap();
    if res.rows_affected() > 0 { Ok(StatusCode::OK) } else { Err((StatusCode::FORBIDDEN, "操作失败".to_string())) }
}

async fn get_all_edges(claims: Claims, Query(q): Query<RoadmapQuery>, State(pool): State<PgPool>) -> Result<Json<Vec<Edge>>, (StatusCode, String)> {
    let query = "SELECT e.* FROM edges e JOIN roadmaps r ON e.roadmap_id = r.id JOIN organizations o ON r.org_id = o.id WHERE o.owner_id = $1 AND r.id = $2";
    let res = sqlx::query_as::<_, Edge>(query).bind(claims.sub).bind(q.roadmap_id).fetch_all(&pool).await.unwrap();
    Ok(Json(res))
}

async fn create_edge(claims: Claims, State(pool): State<PgPool>, Json(payload): Json<CreateEdgeReq>) -> Result<StatusCode, (StatusCode, String)> {
    let query = "INSERT INTO edges (roadmap_id, source_node_id, target_node_id) SELECT $1, $2, $3 WHERE EXISTS (SELECT 1 FROM roadmaps r JOIN organizations o ON r.org_id = o.id WHERE r.id = $1 AND o.owner_id = $4) ON CONFLICT DO NOTHING";
    sqlx::query(query).bind(payload.roadmap_id).bind(payload.source).bind(payload.target).bind(claims.sub).execute(&pool).await.unwrap();
    Ok(StatusCode::CREATED)
}

async fn get_node_note(claims: Claims, Path(id): Path<Uuid>, State(pool): State<PgPool>) -> Result<Json<Note>, (StatusCode, String)> {
    let query = "INSERT INTO notes (node_id, content) SELECT $1, '{\"blocks\":[]}' WHERE EXISTS (SELECT 1 FROM nodes n JOIN roadmaps r ON n.roadmap_id = r.id JOIN organizations o ON r.org_id = o.id WHERE n.id = $1 AND o.owner_id = $2) ON CONFLICT (node_id) DO UPDATE SET node_id = EXCLUDED.node_id RETURNING node_id, content";
    let res = sqlx::query_as::<_, Note>(query).bind(id).bind(claims.sub).fetch_one(&pool).await.map_err(|_| (StatusCode::FORBIDDEN, "无权访问".to_string()))?;
    Ok(Json(res))
}

async fn update_node_note(claims: Claims, Path(id): Path<Uuid>, State(pool): State<PgPool>, Json(payload): Json<UpdateNoteReq>) -> Result<StatusCode, (StatusCode, String)> {
    let query = "UPDATE notes SET content = $1, updated_at = CURRENT_TIMESTAMP WHERE node_id = $2 AND node_id IN (SELECT n.id FROM nodes n JOIN roadmaps r ON n.roadmap_id = r.id JOIN organizations o ON r.org_id = o.id WHERE o.owner_id = $3)";
    sqlx::query(query).bind(payload.content).bind(id).bind(claims.sub).execute(&pool).await.unwrap();
    Ok(StatusCode::OK)
}

async fn get_shared_roadmap(Path(token): Path<String>, State(pool): State<PgPool>) -> Result<Json<ShareData>, (StatusCode, String)> {
    let roadmap = sqlx::query_as::<_, Roadmap>("SELECT id, title, share_token FROM roadmaps WHERE share_token = $1").bind(&token).fetch_optional(&pool).await.unwrap().ok_or((StatusCode::NOT_FOUND, "无效".to_string()))?;
    let nodes = sqlx::query_as::<_, Node>("SELECT * FROM nodes WHERE roadmap_id = $1").bind(roadmap.id).fetch_all(&pool).await.unwrap();
    let edges = sqlx::query_as::<_, Edge>("SELECT * FROM edges WHERE roadmap_id = $1").bind(roadmap.id).fetch_all(&pool).await.unwrap();
    Ok(Json(ShareData { roadmap_title: roadmap.title, nodes, edges }))
}

async fn health_check() -> &'static str { "Pathio API Running 🚀" }

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    let pool = PgPoolOptions::new().max_connections(5).connect(&std::env::var("DATABASE_URL").unwrap()).await.unwrap();
    let app = Router::new()
        .route("/api/health", get(health_check))
        .route("/api/auth/register", post(register))
        .route("/api/auth/login", post(login))
        .route("/api/nodes", get(get_all_nodes).post(create_node))
        .route("/api/nodes/:id/position", put(update_node_position))
        .route("/api/edges", get(get_all_edges).post(create_edge))
        .route("/api/nodes/:id/note", get(get_node_note).put(update_node_note))
        .route("/api/share/:token", get(get_shared_roadmap))
        .route("/api/roadmaps", get(get_roadmaps).post(create_roadmap))
        .route("/api/share/:token/notes/:node_id", get(get_shared_note))
        .route("/api/nodes/:id", put(update_node).delete(delete_node))
        .layer(CorsLayer::permissive()).with_state(pool);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000").await.unwrap();
    println!("🚀 Pathio Backend Running at http://127.0.0.1:3000");
    axum::serve(listener, app).await.unwrap();
}