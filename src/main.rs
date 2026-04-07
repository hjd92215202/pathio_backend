use axum::{
    extract::{State, Path},
    http::StatusCode,
    routing::{get, put, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool, postgres::PgPoolOptions};
use std::net::SocketAddr;
use tower_http::cors::CorsLayer;
use uuid::Uuid;
use bcrypt::{hash, verify, DEFAULT_COST};
use jsonwebtoken::{encode, Header, EncodingKey};

// ==========================================
// 1. 数据模型定义
// ==========================================

#[derive(Serialize, Deserialize)]
pub struct Claims {
    pub sub: Uuid,      // 用户 ID
    pub exp: usize,     // 过期时间
}

#[derive(Deserialize)]
pub struct AuthReq {
    pub username: String,       // 对应数据库中的 nickname
    pub email: Option<String>,  // 注册时必填，登录时可选
    pub password: String,
}

#[derive(Serialize, Deserialize, FromRow)]
pub struct Node {
    pub id: Uuid,
    // 暂时用 Option，因为前端刚刚创建的节点可能还没归属路线图
    pub roadmap_id: Option<Uuid>, 
    pub title: String,
    pub status: Option<String>,
    pub pos_x: f64,
    pub pos_y: f64,
}

// 接收前端创建节点的请求体
#[derive(Deserialize)]
pub struct CreateNodeReq {
    pub title: String,
    pub pos_x: f64,
    pub pos_y: f64,
}

// 接收前端更新节点位置的请求体
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

// 增加分享相关的模型
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

// ==========================================
// 2. API 处理函数 (Handlers)
// ==========================================

// 注册接口：创建用户 + 自动创建默认组织
async fn register(
    State(pool): State<PgPool>,
    Json(payload): Json<AuthReq>,
) -> Result<StatusCode, (StatusCode, String)> {
    let hashed = hash(payload.password, DEFAULT_COST).unwrap();
    let email = payload.email.ok_or((StatusCode::BAD_REQUEST, "邮箱必填".to_string()))?;
    
    let mut tx = pool.begin().await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // 插入数据：nickname = username, email = email
    let user_id: Uuid = sqlx::query_scalar(
        "INSERT INTO users (nickname, email, password_hash) VALUES ($1, $2, $3) RETURNING id"
    )
    .bind(payload.username)
    .bind(email)
    .bind(hashed)
    .fetch_one(&mut *tx)
    .await
    .map_err(|_| (StatusCode::BAD_REQUEST, "用户名或邮箱已被占用".to_string()))?;

    // 自动创建默认组织
    sqlx::query("INSERT INTO organizations (name, owner_id) VALUES ($1, $2)")
        .bind("我的空间")
        .bind(user_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    tx.commit().await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(StatusCode::CREATED)
}

// 登录接口：验证密码 + 返回 JWT
async fn login(
    State(pool): State<PgPool>,
    Json(payload): Json<AuthReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // 通过 nickname (用户名) 查找用户
    let (id, hash_val): (Uuid, String) = sqlx::query_as("SELECT id, password_hash FROM users WHERE nickname = $1")
        .bind(payload.username)
        .fetch_optional(&pool)
        .await
        .unwrap()
        .ok_or((StatusCode::UNAUTHORIZED, "用户不存在".to_string()))?;

    if verify(payload.password, &hash_val).unwrap() {
        let claims = Claims { sub: id, exp: 10000000000 }; 
        let token = encode(&Header::default(), &claims, &EncodingKey::from_secret("secret".as_ref())).unwrap();
        Ok(Json(serde_json::json!({ "token": token })))
    } else {
        Err((StatusCode::UNAUTHORIZED, "密码错误".to_string()))
    }
}

// 根据 share_token 获取整个路线图的数据 (只读)
async fn get_shared_roadmap(
    Path(token): Path<String>,
    State(pool): State<PgPool>,
) -> Result<Json<ShareData>, (StatusCode, String)> {
    // 1. 先查 roadmap
    let roadmap = sqlx::query_as::<_, Roadmap>("SELECT id, title, share_token FROM roadmaps WHERE share_token = $1")
        .bind(&token)
        .fetch_optional(&pool)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or((StatusCode::NOT_FOUND, "分享链接已失效".to_string()))?;

    // 2. 查该 roadmap 下的所有节点和连线
    let nodes = sqlx::query_as::<_, Node>("SELECT id, roadmap_id, title, status, pos_x, pos_y FROM nodes WHERE roadmap_id = $1")
        .bind(roadmap.id)
        .fetch_all(&pool)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let edges = sqlx::query_as::<_, Edge>("SELECT id, roadmap_id, source_node_id, target_node_id FROM edges WHERE roadmap_id = $1")
        .bind(roadmap.id)
        .fetch_all(&pool)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(ShareData {
        roadmap_title: roadmap.title,
        nodes,
        edges,
    }))
}

// 获取所有节点 (暂时不分路线图，获取所有供测试)
async fn get_all_nodes(State(pool): State<PgPool>) -> Result<Json<Vec<Node>>, (StatusCode, String)> {
    let query_str = "SELECT id, roadmap_id, title, status, pos_x, pos_y FROM nodes";
    
    match sqlx::query_as::<_, Node>(query_str).fetch_all(&pool).await {
        Ok(nodes) => Ok(Json(nodes)),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
    }
}

// 创建新节点
async fn create_node(
    State(pool): State<PgPool>,
    Json(payload): Json<CreateNodeReq>,
) -> Result<(StatusCode, Json<Node>), (StatusCode, String)> {
    let query_str = "
        INSERT INTO nodes (title, pos_x, pos_y) 
        VALUES ($1, $2, $3) 
        RETURNING id, roadmap_id, title, status, pos_x, pos_y
    ";
    
    match sqlx::query_as::<_, Node>(query_str)
        .bind(payload.title)
        .bind(payload.pos_x)
        .bind(payload.pos_y)
        .fetch_one(&pool)
        .await
    {
        Ok(node) => Ok((StatusCode::CREATED, Json(node))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
    }
}

// 更新节点位置 (拖拽节点时触发)
async fn update_node_position(
    Path(id): Path<Uuid>,
    State(pool): State<PgPool>,
    Json(payload): Json<UpdateNodePosReq>,
) -> Result<StatusCode, (StatusCode, String)> {
    let query_str = "UPDATE nodes SET pos_x = $1, pos_y = $2 WHERE id = $3";
    
    match sqlx::query(query_str)
        .bind(payload.pos_x)
        .bind(payload.pos_y)
        .bind(id)
        .execute(&pool)
        .await
    {
        Ok(_) => Ok(StatusCode::OK),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
    }
}

// 健康检查
async fn health_check() -> &'static str {
    "Pathio API is running!"
}

// 获取所有连线
async fn get_all_edges(State(pool): State<PgPool>) -> Result<Json<Vec<Edge>>, (StatusCode, String)> {
    let query = "SELECT id, roadmap_id, source_node_id, target_node_id FROM edges";
    match sqlx::query_as::<_, Edge>(query).fetch_all(&pool).await {
        Ok(edges) => Ok(Json(edges)),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
    }
}

// 保存新连线
async fn create_edge(
    State(pool): State<PgPool>,
    Json(payload): Json<CreateEdgeReq>,
) -> Result<StatusCode, (StatusCode, String)> {
    let query = "INSERT INTO edges (source_node_id, target_node_id) VALUES ($1, $2) ON CONFLICT DO NOTHING";
    match sqlx::query(query)
        .bind(payload.source)
        .bind(payload.target)
        .execute(&pool)
        .await 
    {
        Ok(_) => Ok(StatusCode::CREATED),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
    }
}

// 获取或初始化笔记
async fn get_node_note(
    Path(id): Path<Uuid>,
    State(pool): State<PgPool>,
) -> Result<Json<Note>, (StatusCode, String)> {
    // 如果没有笔记，初始化一个空的 JSON 对象 {"text": ""}
    let default_content = serde_json::json!({"text": ""});
    
    let query = "
        INSERT INTO notes (node_id, content) VALUES ($1, $2)
        ON CONFLICT (node_id) DO UPDATE SET node_id = EXCLUDED.node_id
        RETURNING node_id, content
    ";
    
    match sqlx::query_as::<_, Note>(query)
        .bind(id)
        .bind(default_content)
        .fetch_one(&pool)
        .await 
    {
        Ok(note) => Ok(Json(note)),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
    }
}

// 保存笔记
async fn update_node_note(
    Path(id): Path<Uuid>,
    State(pool): State<PgPool>,
    Json(payload): Json<UpdateNoteReq>,
) -> Result<StatusCode, (StatusCode, String)> {
    let query = "UPDATE notes SET content = $1, updated_at = CURRENT_TIMESTAMP WHERE node_id = $2";
    match sqlx::query(query).bind(payload.content).bind(id).execute(&pool).await {
        Ok(_) => Ok(StatusCode::OK),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
    }
}

// ==========================================
// 3. 主程序入口
// ==========================================

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    let db_url = std::env::var("DATABASE_URL").expect("未设置 DATABASE_URL");
    let port: u16 = std::env::var("PORT").unwrap_or_else(|_| "3000".to_string()).parse().unwrap();

    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("无法连接数据库");

    println!("✅ 成功连接到 Pathio 数据库!");

    // 配置路由
    let app = Router::new()
        .route("/api/health", get(health_check))
        // 1. 注册认证相关路由
        .route("/api/auth/register", post(register))
        .route("/api/auth/login", post(login))
        // 2. 节点与连线路由
        .route("/api/nodes", get(get_all_nodes).post(create_node))
        .route("/api/nodes/:id/position", put(update_node_position))
        .route("/api/edges", get(get_all_edges).post(create_edge))
        .route("/api/nodes/:id/note", get(get_node_note).put(update_node_note))
        .route("/api/share/:token", get(get_shared_roadmap))
        // 中间件与状态
        .layer(CorsLayer::permissive())
        .with_state(pool);

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    println!("🚀 后端服务启动于: http://{}", addr);
    
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}