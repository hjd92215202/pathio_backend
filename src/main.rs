use axum::{
    extract::{State, Path, FromRequestParts, Query},
    http::StatusCode,
    routing::{get, put, post, delete}, // 澧炲姞浜?delete 璺敱
    Json, Router,
};
use axum::http::request::Parts;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{FromRow, PgPool, postgres::PgPoolOptions};
use tower_http::cors::CorsLayer;
use uuid::Uuid;
use bcrypt::{hash, verify, DEFAULT_COST};
use jsonwebtoken::{encode, Header, EncodingKey, decode, DecodingKey, Validation};

const FREE_MAX_ROADMAPS: i64 = 3;
const FREE_MAX_NODES_PER_ORG: i64 = 50;

// ==========================================
// 1. 鏁版嵁妯″瀷瀹氫箟
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

#[derive(Serialize)]
pub struct ShareNoteResponse {
    pub content: serde_json::Value,
    pub references: Vec<NodeReference>,
}

#[derive(Deserialize)]
pub struct UpdateNoteReq {
    pub content: serde_json::Value,
}

// 馃挕 鑺傜偣鍙傝€冨紩鐢ㄦā鍨?
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

fn normalize_note_content_for_storage(content: Value) -> Value {
    match content {
        Value::String(markdown) => json!({ "markdown": markdown, "doc_json": null }),
        Value::Object(mut map) => {
            if !map.contains_key("markdown") {
                let markdown = map
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                map.insert("markdown".to_string(), Value::String(markdown));
            }

            if !map.contains_key("doc_json") {
                map.insert("doc_json".to_string(), Value::Null);
            }

            Value::Object(map)
        }
        Value::Null => json!({ "markdown": "", "doc_json": null }),
        other => json!({ "markdown": other.to_string(), "doc_json": null }),
    }
}

fn normalize_note_content_for_response(content: Value) -> Value {
    let normalized = normalize_note_content_for_storage(content);

    if let Value::Object(mut map) = normalized {
        if !matches!(map.get("markdown"), Some(Value::String(_))) {
            map.insert("markdown".to_string(), Value::String(String::new()));
        }

        if !map.contains_key("doc_json") {
            map.insert("doc_json".to_string(), Value::Null);
        }

        return Value::Object(map);
    }

    normalized
}

// JWT 鎻愬彇鍣?
#[axum::async_trait]
impl<S> FromRequestParts<S> for Claims
where S: Send + Sync,
{
    type Rejection = (StatusCode, String);
    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let auth_header = parts.headers.get("Authorization").and_then(|h| h.to_str().ok()).ok_or((StatusCode::UNAUTHORIZED, "Unauthorized".to_string()))?;
        if !auth_header.starts_with("Bearer ") { return Err((StatusCode::UNAUTHORIZED, "Invalid token format".to_string())); }
        let token = &auth_header[7..];
        let token_data = decode::<Claims>(token, &DecodingKey::from_secret("secret".as_ref()), &Validation::default())
            .map_err(|_| (StatusCode::UNAUTHORIZED, "Session expired".to_string()))?;
        Ok(token_data.claims)
    }
}

// ==========================================
// 2. 涓氬姟閫昏緫 (Handlers)
// ==========================================

// --- 璺嚎鍥剧鐞?---

async fn update_roadmap(
    claims: Claims,
    Path(id): Path<Uuid>,
    State(pool): State<PgPool>,
    Json(payload): Json<serde_json::Value>,
) -> Result<StatusCode, (StatusCode, String)> {
    let title = payload["title"].as_str().ok_or((StatusCode::BAD_REQUEST, "Title is required".to_string()))?;
    // 鍙湁缁勭粐绠＄悊鍛樻垨缂栬緫鑰呭彲浠ヤ慨鏀硅矾绾垮浘鍚嶇О
    let query = "
        UPDATE roadmaps SET title = $1 
        WHERE id = $2 AND org_id IN (
            SELECT org_id FROM org_members WHERE user_id = $3 AND role IN ('admin', 'editor')
        )
    ";
    let res = sqlx::query(query).bind(title).bind(id).bind(claims.sub).execute(&pool).await.unwrap();
    if res.rows_affected() > 0 { Ok(StatusCode::OK) } else { Err((StatusCode::FORBIDDEN, "Forbidden or not found".to_string())) }
}

// --- 鑺傜偣鍙傝€冨紩鐢ㄧ鐞?---

async fn get_node_references(claims: Claims, Path(id): Path<Uuid>, State(pool): State<PgPool>) -> Result<Json<Vec<NodeReference>>, (StatusCode, String)> {
    let res = sqlx::query_as::<_, NodeReference>(
        r#"SELECT nr.id, nr.node_id, nr.title, nr.url
           FROM node_references nr
           JOIN nodes n ON nr.node_id = n.id
           JOIN roadmaps r ON n.roadmap_id = r.id
           JOIN org_members om ON r.org_id = om.org_id
           WHERE nr.node_id = $1 AND om.user_id = $2
           ORDER BY nr.created_at DESC"#,
    )
        .bind(id)
        .bind(claims.sub)
        .fetch_all(&pool)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(res))
}

async fn get_shared_node_references(Path((token, node_id)): Path<(String, Uuid)>, State(pool): State<PgPool>) -> Result<Json<Vec<NodeReference>>, (StatusCode, String)> {
    let references = sqlx::query_as::<_, NodeReference>(
        r#"SELECT nr.id, nr.node_id, nr.title, nr.url
           FROM node_references nr
           JOIN nodes nd ON nr.node_id = nd.id
           JOIN roadmaps r ON nd.roadmap_id = r.id
           WHERE r.share_token = $1 AND nr.node_id = $2
           ORDER BY nr.created_at DESC"#,
    )
        .bind(token)
        .bind(node_id)
        .fetch_all(&pool)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(references))
}

async fn add_node_reference(claims: Claims, Path(id): Path<Uuid>, State(pool): State<PgPool>, Json(payload): Json<CreateReferenceReq>) -> Result<Json<NodeReference>, (StatusCode, String)> {
    let res = sqlx::query_as::<_, NodeReference>(
        r#"INSERT INTO node_references (node_id, title, url)
           SELECT $1, $2, $3
           WHERE EXISTS (
              SELECT 1
              FROM nodes n
              JOIN roadmaps r ON n.roadmap_id = r.id
              JOIN org_members om ON r.org_id = om.org_id
              WHERE n.id = $1 AND om.user_id = $4
           )
           RETURNING id, node_id, title, url"#,
    )
        .bind(id)
        .bind(payload.title)
        .bind(payload.url)
        .bind(claims.sub)
        .fetch_optional(&pool)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or((StatusCode::FORBIDDEN, "无权访问".to_string()))?;
    Ok(Json(res))
}

async fn delete_node_reference(claims: Claims, Path(id): Path<Uuid>, State(pool): State<PgPool>) -> Result<StatusCode, (StatusCode, String)> {
    let res = sqlx::query(
        r#"DELETE FROM node_references
           WHERE id = $1 AND node_id IN (
              SELECT n.id
              FROM nodes n
              JOIN roadmaps r ON n.roadmap_id = r.id
              JOIN org_members om ON r.org_id = om.org_id
              WHERE om.user_id = $2
           )"#,
    )
        .bind(id)
        .bind(claims.sub)
        .execute(&pool)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    if res.rows_affected() > 0 {
        Ok(StatusCode::OK)
    } else {
        Err((StatusCode::FORBIDDEN, "无权访问".to_string()))
    }
}

async fn register(State(pool): State<PgPool>, Json(payload): Json<AuthReq>) -> Result<StatusCode, (StatusCode, String)> {
    let email = payload.email.ok_or((StatusCode::BAD_REQUEST, "Email is required".to_string()))?;
    let hashed = hash(payload.password, DEFAULT_COST).unwrap();
    let mut tx = pool.begin().await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let user_id: Uuid = sqlx::query_scalar("INSERT INTO users (nickname, email, password_hash) VALUES ($1, $2, $3) RETURNING id")
        .bind(payload.username).bind(email).bind(hashed).fetch_one(&mut *tx).await
        .map_err(|_| (StatusCode::BAD_REQUEST, "Username or email already exists".to_string()))?;
    if let Some(code) = payload.invite_code {
        let org_info: Option<(Uuid, String)> = sqlx::query_as("SELECT o.id, o.plan_type FROM organizations o JOIN invitations i ON o.id = i.org_id WHERE i.code = $1 AND i.is_used = FALSE").bind(&code).fetch_optional(&mut *tx).await.unwrap();
        if let Some((oid, plan)) = org_info {
            if plan == "free" {
                let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM org_members WHERE org_id = $1").bind(oid).fetch_one(&mut *tx).await.unwrap();
                if count >= 2 { return Err((StatusCode::PAYMENT_REQUIRED, "Workspace member limit reached".to_string())); }
            }
            sqlx::query("INSERT INTO org_members (org_id, user_id, role) VALUES ($1, $2, 'member')").bind(oid).bind(user_id).execute(&mut *tx).await.unwrap();
        } else { return Err((StatusCode::BAD_REQUEST, "Invalid invite code".to_string())); }
    } else {
    let org_id: Uuid = sqlx::query_scalar("INSERT INTO organizations (name, owner_id, plan_type) VALUES ($1, $2, 'free') RETURNING id").bind("My Workspace").bind(user_id).fetch_one(&mut *tx).await.unwrap();
        sqlx::query("INSERT INTO org_members (org_id, user_id, role) VALUES ($1, $2, 'admin')").bind(org_id).bind(user_id).execute(&mut *tx).await.unwrap();
        sqlx::query("INSERT INTO roadmaps (org_id, title, share_token) VALUES ($1, $2, $3)").bind(org_id).bind("My First Roadmap").bind(Uuid::new_v4().to_string()[..8].to_string()).execute(&mut *tx).await.unwrap();
    }
    tx.commit().await.unwrap();
    Ok(StatusCode::CREATED)
}

async fn login(State(pool): State<PgPool>, Json(payload): Json<AuthReq>) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let (id, hash_val): (Uuid, String) = sqlx::query_as("SELECT id, password_hash FROM users WHERE nickname = $1").bind(payload.username).fetch_optional(&pool).await.unwrap().ok_or((StatusCode::UNAUTHORIZED, "User not found".to_string()))?;
    if verify(payload.password, &hash_val).unwrap() {
        let claims = Claims { sub: id, exp: 10000000000 }; 
        let token = encode(&Header::default(), &claims, &EncodingKey::from_secret("secret".as_ref())).unwrap();
        Ok(Json(serde_json::json!({ "token": token })))
    } else { Err((StatusCode::UNAUTHORIZED, "Incorrect password".to_string())) }
}

async fn get_org_details(claims: Claims, State(pool): State<PgPool>) -> Result<Json<OrgDetails>, (StatusCode, String)> {
    let org: (String, String, Uuid) = sqlx::query_as("SELECT o.name, o.plan_type, o.id FROM organizations o JOIN org_members om ON o.id = om.org_id WHERE om.user_id = $1 LIMIT 1").bind(claims.sub).fetch_one(&pool).await.map_err(|_| (StatusCode::NOT_FOUND, "Organization not found".to_string()))?;
    let members = sqlx::query_as::<_, OrgMemberInfo>("SELECT u.id, u.nickname, u.email, om.role, u.created_at FROM users u JOIN org_members om ON u.id = om.user_id WHERE om.org_id = $1").bind(org.2).fetch_all(&pool).await.unwrap();
    Ok(Json(OrgDetails { name: org.0, plan_type: org.1, members }))
}

// 馃挕 琛ュ叏锛氭洿鏂扮粍缁?绌洪棿鍚嶇О
async fn update_org_details(
    claims: Claims,
    State(pool): State<PgPool>,
    Json(payload): Json<serde_json::Value>,
) -> Result<StatusCode, (StatusCode, String)> {
    let new_name = payload["name"].as_str()
        .ok_or((StatusCode::BAD_REQUEST, "Name is required".to_string()))?;

    // 鍙湁绠＄悊鍛?(admin) 鏈夋潈淇敼绌洪棿鍚嶇О
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
        Err((StatusCode::FORBIDDEN, "You do not have permission to rename this workspace".to_string()))
    }
}

async fn create_org_invite(claims: Claims, State(pool): State<PgPool>) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let org: (Uuid, String) = sqlx::query_as("SELECT org_id, o.plan_type FROM org_members om JOIN organizations o ON om.org_id = o.id WHERE om.user_id = $1 AND om.role = 'admin'").bind(claims.sub).fetch_one(&pool).await.map_err(|_| (StatusCode::FORBIDDEN, "Permission denied".to_string()))?;
    if org.1 == "free" {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM org_members WHERE org_id = $1").bind(org.0).fetch_one(&pool).await.unwrap();
        if count >= 2 { return Err((StatusCode::PAYMENT_REQUIRED, "Workspace member limit reached".to_string())); }
    }
    let code = Uuid::new_v4().to_string()[..6].to_uppercase();
    sqlx::query("INSERT INTO invitations (org_id, inviter_id, code) VALUES ($1, $2, $3)").bind(org.0).bind(claims.sub).bind(&code).execute(&pool).await.unwrap();
    Ok(Json(serde_json::json!({ "code": code })))
}

async fn create_roadmap(claims: Claims, State(pool): State<PgPool>, Json(payload): Json<serde_json::Value>) -> Result<Json<Roadmap>, (StatusCode, String)> {
    let title = payload["title"].as_str().unwrap_or("Untitled roadmap");
    let mut tx = pool.begin().await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let org: (Uuid, String) = sqlx::query_as("SELECT o.id, o.plan_type FROM organizations o JOIN org_members om ON o.id = om.org_id WHERE om.user_id = $1 LIMIT 1")
        .bind(claims.sub)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or((StatusCode::FORBIDDEN, "Permission denied".to_string()))?;

    // Serialize quota checks inside a txn to avoid concurrent limit bypass.
    sqlx::query_scalar::<_, Uuid>("SELECT id FROM organizations WHERE id = $1 FOR UPDATE")
        .bind(org.0)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    if org.1 == "free" {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM roadmaps WHERE org_id = $1")
            .bind(org.0)
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        if count >= FREE_MAX_ROADMAPS {
            return Err((StatusCode::PAYMENT_REQUIRED, format!("Free plan is limited to {} roadmaps", FREE_MAX_ROADMAPS)));
        }
    }

    let res = sqlx::query_as::<_, Roadmap>("INSERT INTO roadmaps (org_id, title, share_token) VALUES ($1, $2, $3) RETURNING id, title, share_token")
        .bind(org.0)
        .bind(title)
        .bind(Uuid::new_v4().to_string()[..8].to_string())
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    tx.commit().await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
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
    let mut tx = pool.begin().await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let org_and_plan: (Uuid, String) = sqlx::query_as("SELECT r.org_id, o.plan_type FROM roadmaps r JOIN organizations o ON r.org_id = o.id JOIN org_members om ON r.org_id = om.org_id WHERE r.id = $1 AND om.user_id = $2 LIMIT 1")
        .bind(payload.roadmap_id)
        .bind(claims.sub)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or((StatusCode::FORBIDDEN, "Forbidden".to_string()))?;

    // Serialize quota checks inside a txn to avoid concurrent limit bypass.
    sqlx::query_scalar::<_, Uuid>("SELECT id FROM organizations WHERE id = $1 FOR UPDATE")
        .bind(org_and_plan.0)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    if org_and_plan.1 == "free" {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM nodes n JOIN roadmaps r ON n.roadmap_id = r.id WHERE r.org_id = $1")
            .bind(org_and_plan.0)
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

        if count >= FREE_MAX_NODES_PER_ORG {
            return Err((StatusCode::PAYMENT_REQUIRED, format!("Free plan is limited to {} total nodes per workspace", FREE_MAX_NODES_PER_ORG)));
        }
    }

    let res = sqlx::query_as::<_, Node>("INSERT INTO nodes (roadmap_id, title, pos_x, pos_y) VALUES ($1, $2, $3, $4) RETURNING *")
        .bind(payload.roadmap_id)
        .bind(payload.title)
        .bind(payload.pos_x)
        .bind(payload.pos_y)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    tx.commit().await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok((StatusCode::CREATED, Json(res)))
}

async fn update_node(claims: Claims, Path(id): Path<Uuid>, State(pool): State<PgPool>, Json(payload): Json<UpdateNodeReq>) -> Result<StatusCode, (StatusCode, String)> {
    sqlx::query("UPDATE nodes SET title = COALESCE($1, title), status = COALESCE($2, status) WHERE id = $3 AND roadmap_id IN (SELECT r.id FROM roadmaps r JOIN org_members om ON r.org_id = om.org_id WHERE om.user_id = $4)").bind(payload.title).bind(payload.status).bind(id).bind(claims.sub).execute(&pool).await.unwrap();
    Ok(StatusCode::OK)
}

async fn update_node_position(claims: Claims, Path(id): Path<Uuid>, State(pool): State<PgPool>, Json(payload): Json<UpdateNodePosReq>) -> Result<StatusCode, (StatusCode, String)> {
    let res = sqlx::query("UPDATE nodes SET pos_x = $1, pos_y = $2 WHERE id = $3 AND roadmap_id IN (SELECT r.id FROM roadmaps r JOIN org_members om ON r.org_id = om.org_id WHERE om.user_id = $4)").bind(payload.pos_x).bind(payload.pos_y).bind(id).bind(claims.sub).execute(&pool).await.unwrap();
    if res.rows_affected() > 0 { Ok(StatusCode::OK) } else { Err((StatusCode::FORBIDDEN, "Operation failed".to_string())) }
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
    let mut res = sqlx::query_as::<_, Note>("INSERT INTO notes (node_id, content) SELECT $1, '{\"markdown\":\"\",\"doc_json\":null}' WHERE EXISTS (SELECT 1 FROM nodes n JOIN roadmaps r ON n.roadmap_id = r.id JOIN org_members om ON r.org_id = om.org_id WHERE n.id = $1 AND om.user_id = $2) ON CONFLICT (node_id) DO UPDATE SET node_id = EXCLUDED.node_id RETURNING node_id, content").bind(id).bind(claims.sub).fetch_one(&pool).await.map_err(|_| (StatusCode::FORBIDDEN, "Forbidden".to_string()))?;
    res.content = normalize_note_content_for_response(res.content);
    Ok(Json(res))
}

async fn update_node_note(claims: Claims, Path(id): Path<Uuid>, State(pool): State<PgPool>, Json(payload): Json<UpdateNoteReq>) -> Result<StatusCode, (StatusCode, String)> {
    let query = "UPDATE notes SET content = $1, updated_at = CURRENT_TIMESTAMP WHERE node_id = $2 AND node_id IN (SELECT n.id FROM nodes n JOIN roadmaps r ON n.roadmap_id = r.id JOIN org_members om ON r.org_id = om.org_id WHERE om.user_id = $3)";
    let normalized_content = normalize_note_content_for_storage(payload.content);
    sqlx::query(query).bind(normalized_content).bind(id).bind(claims.sub).execute(&pool).await.unwrap();
    Ok(StatusCode::OK)
}

async fn get_shared_note(Path((token, node_id)): Path<(String, Uuid)>, State(pool): State<PgPool>) -> Result<Json<ShareNoteResponse>, (StatusCode, String)> {
    let node_exists: bool = sqlx::query_scalar(
        r#"SELECT EXISTS(
               SELECT 1
               FROM nodes nd
               JOIN roadmaps r ON nd.roadmap_id = r.id
               WHERE r.share_token = $1 AND nd.id = $2
           )"#,
    )
        .bind(&token)
        .bind(node_id)
        .fetch_one(&pool)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    if !node_exists {
        return Err((StatusCode::FORBIDDEN, "无权访问".to_string()));
    }

    let content = sqlx::query_scalar::<_, serde_json::Value>("SELECT content FROM notes WHERE node_id = $1")
        .bind(node_id)
        .fetch_optional(&pool)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .unwrap_or_else(|| json!({ "markdown": "", "doc_json": null }));

    let normalized_content = normalize_note_content_for_response(content);

    let references = sqlx::query_as::<_, NodeReference>(
        r#"SELECT nr.id, nr.node_id, nr.title, nr.url
           FROM node_references nr
           JOIN nodes nd ON nr.node_id = nd.id
           JOIN roadmaps r ON nd.roadmap_id = r.id
           WHERE r.share_token = $1 AND nr.node_id = $2
           ORDER BY nr.created_at DESC"#,
    )
        .bind(&token)
        .bind(node_id)
        .fetch_all(&pool)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(ShareNoteResponse { content: normalized_content, references }))
}

async fn get_shared_roadmap(Path(token): Path<String>, State(pool): State<PgPool>) -> Result<Json<ShareData>, (StatusCode, String)> {
    let roadmap = sqlx::query_as::<_, Roadmap>("SELECT id, title, share_token FROM roadmaps WHERE share_token = $1").bind(&token).fetch_optional(&pool).await.unwrap().ok_or((StatusCode::NOT_FOUND, "Invalid share token".to_string()))?;
    let nodes = sqlx::query_as::<_, Node>("SELECT * FROM nodes WHERE roadmap_id = $1").bind(roadmap.id).fetch_all(&pool).await.unwrap();
    let edges = sqlx::query_as::<_, Edge>("SELECT * FROM edges WHERE roadmap_id = $1").bind(roadmap.id).fetch_all(&pool).await.unwrap();
    Ok(Json(ShareData { roadmap_title: roadmap.title, nodes, edges }))
}

async fn health_check() -> &'static str { "Pathio API Running" }

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
        .route("/api/roadmaps/:id", put(update_roadmap)) // 馃挕 鏂板璺嚎鍥炬洿鍚?
        .route("/api/nodes/:id/references", get(get_node_references).post(add_node_reference)) // 馃挕 鏂板鍙傝€冨紩鐢ㄧ鐞?
        .route("/api/references/:id", delete(delete_node_reference)) // 馃挕 鏂板寮曠敤鍒犻櫎
        .route("/api/share/:token/notes/:node_id", get(get_shared_note))
        .route("/api/share/:token/notes/:node_id/references", get(get_shared_node_references))
        .route("/api/nodes/:id", put(update_node).delete(delete_node))
        .route("/api/org/details", get(get_org_details).put(update_org_details))
        .route("/api/org/invite", post(create_org_invite))
        .layer(CorsLayer::permissive()).with_state(pool);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000").await.unwrap();
    println!("Pathio Backend Pro started at http://127.0.0.1:3000");
    axum::serve(listener, app).await.unwrap();
}

