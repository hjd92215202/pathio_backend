use axum::{
    extract::{State, Path, FromRequestParts, Query},
    http::StatusCode,
    routing::{get, put, post, delete}, // 增加了 delete 路由
    Json, Router,
};
use axum::http::request::Parts;
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool, postgres::PgPoolOptions};
use tower_http::cors::CorsLayer;
use uuid::Uuid;
use bcrypt::{hash, verify, DEFAULT_COST};
use jsonwebtoken::{encode, Header, EncodingKey, decode, DecodingKey, Validation};

// ==========================================
// 1. 数据模型定义
// ==========================================

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
    pub invite_code: Option<String>,
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

#[derive(Deserialize)]
pub struct RoadmapQuery {
    pub roadmap_id: Uuid,
}

#[derive(Deserialize)]
pub struct CreateNodeReq {
    pub roadmap_id: Uuid,
    pub title: String,
    pub pos_x: f64,
    pub pos_y: f64,
}

#[derive(Deserialize)]
pub struct UpdateNodeReq {
    pub title: Option<String>,
    pub status: Option<String>,
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
    pub roadmap_id: Uuid,
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

// 💡 节点参考引用模型
#[derive(Serialize, Deserialize, FromRow)]
pub struct NodeReference {
    pub id: Uuid,
    pub node_id: Uuid,
    pub title: String,
    pub url: String,
}

#[derive(Deserialize)]
pub struct CreateReferenceReq {
    pub title: String,
    pub url: String,
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

#[derive(Serialize)]
pub struct OrgDetails {
    pub name: String,
    pub plan_type: String,
    pub members: Vec<OrgMemberInfo>,
}

#[derive(Serialize, FromRow)]
pub struct OrgMemberInfo {
    pub id: Uuid,
    pub nickname: String,
    pub email: String,
    pub role: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
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
// 2. 业务逻辑 (Handlers)
// ==========================================

// --- 路线图管理 ---

async fn update_roadmap(
    claims: Claims,
    Path(id): Path<Uuid>,
    State(pool): State<PgPool>,
    Json(payload): Json<serde_json::Value>,
) -> Result<StatusCode, (StatusCode, String)> {
    let title = payload["title"].as_str().ok_or((StatusCode::BAD_REQUEST, "标题不能为空".to_string()))?;
    // 只有组织管理员或编辑者可以修改路线图名称
    let query = "
        UPDATE roadmaps SET title = $1 
        WHERE id = $2 AND org_id IN (
            SELECT org_id FROM org_members WHERE user_id = $3 AND role IN ('admin', 'editor')
        )
    ";
    let res = sqlx::query(query).bind(title).bind(id).bind(claims.sub).execute(&pool).await.unwrap();
    if res.rows_affected() > 0 { Ok(StatusCode::OK) } else { Err((StatusCode::FORBIDDEN, "无权修改或不存在".to_string())) }
}

// --- 节点参考引用管理 ---

async fn get_node_references(Path(id): Path<Uuid>, State(pool): State<PgPool>) -> Result<Json<Vec<NodeReference>>, (StatusCode, String)> {
    let res = sqlx::query_as::<_, NodeReference>("SELECT * FROM node_references WHERE node_id = $1 ORDER BY created_at DESC")
        .bind(id).fetch_all(&pool).await.unwrap();
    Ok(Json(res))
}

async fn add_node_reference(claims: Claims, Path(id): Path<Uuid>, State(pool): State<PgPool>, Json(payload): Json<CreateReferenceReq>) -> Result<Json<NodeReference>, (StatusCode, String)> {
    // 简单校验节点是否存在
    let res = sqlx::query_as::<_, NodeReference>("INSERT INTO node_references (node_id, title, url) VALUES ($1, $2, $3) RETURNING *")
        .bind(id).bind(payload.title).bind(payload.url).fetch_one(&pool).await.unwrap();
    Ok(Json(res))
}

async fn delete_node_reference(claims: Claims, Path(id): Path<Uuid>, State(pool): State<PgPool>) -> Result<StatusCode, (StatusCode, String)> {
    // id 为 reference 的 uuid
    sqlx::query("DELETE FROM node_references WHERE id = $1").bind(id).execute(&pool).await.unwrap();
    Ok(StatusCode::OK)
}

// --- 基础 Auth 与 业务逻辑保持不变 (已适配营销卡点) ---

async fn register(State(pool): State<PgPool>, Json(payload): Json<AuthReq>) -> Result<StatusCode, (StatusCode, String)> {
    let email = payload.email.ok_or((StatusCode::BAD_REQUEST, "邮箱必填".to_string()))?;
    let hashed = hash(payload.password, DEFAULT_COST).unwrap();
    let mut tx = pool.begin().await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let user_id: Uuid = sqlx::query_scalar("INSERT INTO users (nickname, email, password_hash) VALUES ($1, $2, $3) RETURNING id")
        .bind(payload.username).bind(email).bind(hashed).fetch_one(&mut *tx).await
        .map_err(|_| (StatusCode::BAD_REQUEST, "用户名或邮箱占用".to_string()))?;
    if let Some(code) = payload.invite_code {
        let org_info: Option<(Uuid, String)> = sqlx::query_as("SELECT o.id, o.plan_type FROM organizations o JOIN invitations i ON o.id = i.org_id WHERE i.code = $1 AND i.is_used = FALSE").bind(&code).fetch_optional(&mut *tx).await.unwrap();
        if let Some((oid, plan)) = org_info {
            if plan == "free" {
                let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM org_members WHERE org_id = $1").bind(oid).fetch_one(&mut *tx).await.unwrap();
                if count >= 2 { return Err((StatusCode::PAYMENT_REQUIRED, "协作席位已满".to_string())); }
            }
            sqlx::query("INSERT INTO org_members (org_id, user_id, role) VALUES ($1, $2, 'member')").bind(oid).bind(user_id).execute(&mut *tx).await.unwrap();
        } else { return Err((StatusCode::BAD_REQUEST, "邀请码无效".to_string())); }
    } else {
        let org_id: Uuid = sqlx::query_scalar("INSERT INTO organizations (name, owner_id, plan_type) VALUES ($1, $2, 'free') RETURNING id").bind("我的默认空间").bind(user_id).fetch_one(&mut *tx).await.unwrap();
        sqlx::query("INSERT INTO org_members (org_id, user_id, role) VALUES ($1, $2, 'admin')").bind(org_id).bind(user_id).execute(&mut *tx).await.unwrap();
        sqlx::query("INSERT INTO roadmaps (org_id, title, share_token) VALUES ($1, $2, $3)").bind(org_id).bind("我的首个研究路径").bind(Uuid::new_v4().to_string()[..8].to_string()).execute(&mut *tx).await.unwrap();
    }
    tx.commit().await.unwrap();
    Ok(StatusCode::CREATED)
}

async fn login(State(pool): State<PgPool>, Json(payload): Json<AuthReq>) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let (id, hash_val): (Uuid, String) = sqlx::query_as("SELECT id, password_hash FROM users WHERE nickname = $1").bind(payload.username).fetch_optional(&pool).await.unwrap().ok_or((StatusCode::UNAUTHORIZED, "用户不存在".to_string()))?;
    if verify(payload.password, &hash_val).unwrap() {
        let claims = Claims { sub: id, exp: 10000000000 }; 
        let token = encode(&Header::default(), &claims, &EncodingKey::from_secret("secret".as_ref())).unwrap();
        Ok(Json(serde_json::json!({ "token": token })))
    } else { Err((StatusCode::UNAUTHORIZED, "密码错误".to_string())) }
}

async fn get_org_details(claims: Claims, State(pool): State<PgPool>) -> Result<Json<OrgDetails>, (StatusCode, String)> {
    let org: (String, String, Uuid) = sqlx::query_as("SELECT o.name, o.plan_type, o.id FROM organizations o JOIN org_members om ON o.id = om.org_id WHERE om.user_id = $1 LIMIT 1").bind(claims.sub).fetch_one(&pool).await.map_err(|_| (StatusCode::NOT_FOUND, "找不到组织".to_string()))?;
    let members = sqlx::query_as::<_, OrgMemberInfo>("SELECT u.id, u.nickname, u.email, om.role, u.created_at FROM users u JOIN org_members om ON u.id = om.user_id WHERE om.org_id = $1").bind(org.2).fetch_all(&pool).await.unwrap();
    Ok(Json(OrgDetails { name: org.0, plan_type: org.1, members }))
}

// 💡 补全：更新组织/空间名称
async fn update_org_details(
    claims: Claims,
    State(pool): State<PgPool>,
    Json(payload): Json<serde_json::Value>,
) -> Result<StatusCode, (StatusCode, String)> {
    let new_name = payload["name"].as_str()
        .ok_or((StatusCode::BAD_REQUEST, "名称不能为空".to_string()))?;

    // 只有管理员 (admin) 有权修改空间名称
    let query = "
        UPDATE organizations SET name = $1 
        WHERE id = (
            SELECT org_id FROM org_members 
            WHERE user_id = $2 AND role = 'admin' 
            LIMIT 1
        )
    ";
    
    let res = sqlx::query(query)
        .bind(new_name)
        .bind(claims.sub)
        .execute(&pool)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    if res.rows_affected() > 0 {
        Ok(StatusCode::OK)
    } else {
        Err((StatusCode::FORBIDDEN, "您没有权限修改该空间名称".to_string()))
    }
}

async fn create_org_invite(claims: Claims, State(pool): State<PgPool>) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let org: (Uuid, String) = sqlx::query_as("SELECT org_id, o.plan_type FROM org_members om JOIN organizations o ON om.org_id = o.id WHERE om.user_id = $1 AND om.role = 'admin'").bind(claims.sub).fetch_one(&pool).await.map_err(|_| (StatusCode::FORBIDDEN, "权限不足".to_string()))?;
    if org.1 == "free" {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM org_members WHERE org_id = $1").bind(org.0).fetch_one(&pool).await.unwrap();
        if count >= 2 { return Err((StatusCode::PAYMENT_REQUIRED, "协作席位已满".to_string())); }
    }
    let code = Uuid::new_v4().to_string()[..6].to_uppercase();
    sqlx::query("INSERT INTO invitations (org_id, inviter_id, code) VALUES ($1, $2, $3)").bind(org.0).bind(claims.sub).bind(&code).execute(&pool).await.unwrap();
    Ok(Json(serde_json::json!({ "code": code })))
}

async fn create_roadmap(claims: Claims, State(pool): State<PgPool>, Json(payload): Json<serde_json::Value>) -> Result<Json<Roadmap>, (StatusCode, String)> {
    let org: (Uuid, String) = sqlx::query_as("SELECT o.id, o.plan_type FROM organizations o JOIN org_members om ON o.id = om.org_id WHERE om.user_id = $1 LIMIT 1").bind(claims.sub).fetch_one(&pool).await.unwrap();
    if org.1 == "free" {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM roadmaps WHERE org_id = $1").bind(org.0).fetch_one(&pool).await.unwrap();
        if count >= 1 { return Err((StatusCode::PAYMENT_REQUIRED, "免费版限1个空间".to_string())); }
    }
    let title = payload["title"].as_str().unwrap_or("未命名路线图");
    let res = sqlx::query_as::<_, Roadmap>("INSERT INTO roadmaps (org_id, title, share_token) VALUES ($1, $2, $3) RETURNING id, title, share_token").bind(org.0).bind(title).bind(Uuid::new_v4().to_string()[..8].to_string()).fetch_one(&pool).await.unwrap();
    Ok(Json(res))
}

async fn get_roadmaps(claims: Claims, State(pool): State<PgPool>) -> Result<Json<Vec<Roadmap>>, (StatusCode, String)> {
    let res = sqlx::query_as::<_, Roadmap>("SELECT r.id, r.title, r.share_token FROM roadmaps r JOIN org_members om ON r.org_id = om.org_id WHERE om.user_id = $1 ORDER BY r.created_at DESC").bind(claims.sub).fetch_all(&pool).await.unwrap();
    Ok(Json(res))
}

async fn get_all_nodes(claims: Claims, Query(q): Query<RoadmapQuery>, State(pool): State<PgPool>) -> Result<Json<Vec<Node>>, (StatusCode, String)> {
    let res = sqlx::query_as::<_, Node>("SELECT n.* FROM nodes n JOIN roadmaps r ON n.roadmap_id = r.id JOIN org_members om ON r.org_id = om.org_id WHERE om.user_id = $1 AND r.id = $2").bind(claims.sub).bind(q.roadmap_id).fetch_all(&pool).await.unwrap();
    Ok(Json(res))
}

async fn create_node(claims: Claims, State(pool): State<PgPool>, Json(payload): Json<CreateNodeReq>) -> Result<(StatusCode, Json<Node>), (StatusCode, String)> {
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM roadmaps r JOIN org_members om ON r.org_id = om.org_id WHERE r.id = $1 AND om.user_id = $2)").bind(payload.roadmap_id).bind(claims.sub).fetch_one(&pool).await.unwrap();
    if !exists { return Err((StatusCode::FORBIDDEN, "无权访问".to_string())); }
    let res = sqlx::query_as::<_, Node>("INSERT INTO nodes (roadmap_id, title, pos_x, pos_y) VALUES ($1, $2, $3, $4) RETURNING *").bind(payload.roadmap_id).bind(payload.title).bind(payload.pos_x).bind(payload.pos_y).fetch_one(&pool).await.unwrap();
    Ok((StatusCode::CREATED, Json(res)))
}

async fn update_node(claims: Claims, Path(id): Path<Uuid>, State(pool): State<PgPool>, Json(payload): Json<UpdateNodeReq>) -> Result<StatusCode, (StatusCode, String)> {
    sqlx::query("UPDATE nodes SET title = COALESCE($1, title), status = COALESCE($2, status) WHERE id = $3 AND roadmap_id IN (SELECT r.id FROM roadmaps r JOIN org_members om ON r.org_id = om.org_id WHERE om.user_id = $4)").bind(payload.title).bind(payload.status).bind(id).bind(claims.sub).execute(&pool).await.unwrap();
    Ok(StatusCode::OK)
}

async fn update_node_position(claims: Claims, Path(id): Path<Uuid>, State(pool): State<PgPool>, Json(payload): Json<UpdateNodePosReq>) -> Result<StatusCode, (StatusCode, String)> {
    let res = sqlx::query("UPDATE nodes SET pos_x = $1, pos_y = $2 WHERE id = $3 AND roadmap_id IN (SELECT r.id FROM roadmaps r JOIN org_members om ON r.org_id = om.org_id WHERE om.user_id = $4)").bind(payload.pos_x).bind(payload.pos_y).bind(id).bind(claims.sub).execute(&pool).await.unwrap();
    if res.rows_affected() > 0 { Ok(StatusCode::OK) } else { Err((StatusCode::FORBIDDEN, "操作失败".to_string())) }
}

async fn delete_node(claims: Claims, Path(id): Path<Uuid>, State(pool): State<PgPool>) -> Result<StatusCode, (StatusCode, String)> {
    sqlx::query("DELETE FROM nodes WHERE id = $1 AND roadmap_id IN (SELECT r.id FROM roadmaps r JOIN org_members om ON r.org_id = om.org_id WHERE om.user_id = $2)").bind(id).bind(claims.sub).execute(&pool).await.unwrap();
    Ok(StatusCode::OK)
}

async fn get_all_edges(claims: Claims, Query(q): Query<RoadmapQuery>, State(pool): State<PgPool>) -> Result<Json<Vec<Edge>>, (StatusCode, String)> {
    let res = sqlx::query_as::<_, Edge>("SELECT e.* FROM edges e JOIN roadmaps r ON e.roadmap_id = r.id JOIN org_members om ON r.org_id = om.org_id WHERE om.user_id = $1 AND r.id = $2").bind(claims.sub).bind(q.roadmap_id).fetch_all(&pool).await.unwrap();
    Ok(Json(res))
}

async fn create_edge(claims: Claims, State(pool): State<PgPool>, Json(payload): Json<CreateEdgeReq>) -> Result<StatusCode, (StatusCode, String)> {
    let query = "INSERT INTO edges (roadmap_id, source_node_id, target_node_id) SELECT $1, $2, $3 WHERE EXISTS (SELECT 1 FROM roadmaps r JOIN org_members om ON r.org_id = om.org_id WHERE r.id = $1 AND om.user_id = $4) ON CONFLICT DO NOTHING";
    sqlx::query(query).bind(payload.roadmap_id).bind(payload.source).bind(payload.target).bind(claims.sub).execute(&pool).await.unwrap();
    Ok(StatusCode::CREATED)
}

async fn get_node_note(claims: Claims, Path(id): Path<Uuid>, State(pool): State<PgPool>) -> Result<Json<Note>, (StatusCode, String)> {
    let res = sqlx::query_as::<_, Note>("INSERT INTO notes (node_id, content) SELECT $1, '{\"content\":[]}' WHERE EXISTS (SELECT 1 FROM nodes n JOIN roadmaps r ON n.roadmap_id = r.id JOIN org_members om ON r.org_id = om.org_id WHERE n.id = $1 AND om.user_id = $2) ON CONFLICT (node_id) DO UPDATE SET node_id = EXCLUDED.node_id RETURNING node_id, content").bind(id).bind(claims.sub).fetch_one(&pool).await.map_err(|_| (StatusCode::FORBIDDEN, "无权访问".to_string()))?;
    Ok(Json(res))
}

async fn update_node_note(claims: Claims, Path(id): Path<Uuid>, State(pool): State<PgPool>, Json(payload): Json<UpdateNoteReq>) -> Result<StatusCode, (StatusCode, String)> {
    let query = "UPDATE notes SET content = $1, updated_at = CURRENT_TIMESTAMP WHERE node_id = $2 AND node_id IN (SELECT n.id FROM nodes n JOIN roadmaps r ON n.roadmap_id = r.id JOIN org_members om ON r.org_id = om.org_id WHERE om.user_id = $3)";
    sqlx::query(query).bind(payload.content).bind(id).bind(claims.sub).execute(&pool).await.unwrap();
    Ok(StatusCode::OK)
}

async fn get_shared_note(Path((token, node_id)): Path<(String, Uuid)>, State(pool): State<PgPool>) -> Result<Json<Note>, (StatusCode, String)> {
    match sqlx::query_as::<_, Note>("SELECT n.node_id, n.content FROM notes n JOIN nodes nd ON n.node_id = nd.id JOIN roadmaps r ON nd.roadmap_id = r.id WHERE r.share_token = $1 AND n.node_id = $2").bind(token).bind(node_id).fetch_one(&pool).await {
        Ok(note) => Ok(Json(note)),
        Err(_) => Err((StatusCode::FORBIDDEN, "无权访问".to_string())),
    }
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
        .route("/api/roadmaps/:id", put(update_roadmap)) // 💡 新增路线图更名
        .route("/api/nodes/:id/references", get(get_node_references).post(add_node_reference)) // 💡 新增参考引用管理
        .route("/api/references/:id", delete(delete_node_reference)) // 💡 新增引用删除
        .route("/api/share/:token/notes/:node_id", get(get_shared_note))
        .route("/api/nodes/:id", put(update_node).delete(delete_node))
        .route("/api/org/details", get(get_org_details).put(update_org_details))
        .route("/api/org/invite", post(create_org_invite))
        .layer(CorsLayer::permissive()).with_state(pool);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000").await.unwrap();
    println!("🚀 Pathio Backend Pro started at http://127.0.0.1:3000");
    axum::serve(listener, app).await.unwrap();
}