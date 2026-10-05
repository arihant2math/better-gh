//! LFS file locking
//! (https://github.com/git-lfs/git-lfs/blob/main/docs/api/locking.md).
//!
//! Listing needs read access; creating, verifying and unlocking need write
//! access and a user (deploy keys cannot lock). Unlocking someone else's
//! lock requires `force` and admin permission.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use bgh_core::prelude::*;
use serde::{Deserialize, Serialize};

use super::{LfsAccess, LfsResult, authorize, lfs_json};

const DEFAULT_LIMIT: i64 = 100;
const MAX_LIMIT: i64 = 1000;

#[derive(Debug, sqlx::FromRow)]
struct LockRow {
    id: i64,
    path: String,
    owner_id: i64,
    owner_login: String,
    created_at: chrono::DateTime<chrono::Utc>,
}

const SELECT: &str = "SELECT l.id, l.path, l.owner_id, u.login AS owner_login, l.created_at
                        FROM lfs_locks l JOIN users u ON u.id = l.owner_id";

#[derive(Debug, Serialize)]
pub struct LockOwner {
    pub name: String,
}

#[derive(Debug, Serialize)]
pub struct Lock {
    pub id: String,
    pub path: String,
    pub locked_at: String,
    pub owner: LockOwner,
}

impl From<&LockRow> for Lock {
    fn from(r: &LockRow) -> Self {
        Self {
            id: r.id.to_string(),
            path: r.path.clone(),
            locked_at: r
                .created_at
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            owner: LockOwner {
                name: r.owner_login.clone(),
            },
        }
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct RefSpec {
    pub name: Option<String>,
}

fn json<T: for<'de> Deserialize<'de>>(body: &Bytes) -> Result<T, ApiError> {
    if body.is_empty() {
        return serde_json::from_slice(b"{}")
            .map_err(|_| ApiError::bad_request("Problems parsing JSON"));
    }
    serde_json::from_slice(body).map_err(|_| ApiError::bad_request("Problems parsing JSON"))
}

fn require_user(lfs: &LfsAccess) -> Result<&AuthContext, ApiError> {
    lfs.user
        .as_ref()
        .ok_or_else(|| ApiError::forbidden("Locking requires a user account."))
}

fn normalize(path: &str) -> String {
    path.trim_matches('/').to_string()
}

#[derive(Debug, Deserialize)]
pub struct CreateRequest {
    pub path: String,
    #[serde(rename = "ref", default)]
    pub refspec: Option<RefSpec>,
}

/// `POST /{owner}/{repo}/info/lfs/locks`
pub async fn create(
    State(state): State<AppState>,
    Path((owner, repo)): Path<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> LfsResult<Response> {
    let req: CreateRequest = json(&body)?;
    let lfs = authorize(&state, &headers, &owner, &repo, true).await?;
    let user = require_user(&lfs)?;
    let path = normalize(&req.path);
    if path.is_empty() {
        return Err(ApiError::unprocessable("Path is required").into());
    }
    let inserted: Option<i64> = sqlx::query_scalar(
        "INSERT INTO lfs_locks (repo_id, path, ref_name, owner_id) VALUES ($1, $2, $3, $4)
         ON CONFLICT (repo_id, path) DO NOTHING RETURNING id",
    )
    .bind(lfs.access.repo.id)
    .bind(&path)
    .bind(req.refspec.and_then(|r| r.name))
    .bind(user.user.id)
    .fetch_optional(&state.db)
    .await?;
    let row: LockRow = sqlx::query_as(&format!("{SELECT} WHERE l.repo_id = $1 AND l.path = $2"))
        .bind(lfs.access.repo.id)
        .bind(&path)
        .fetch_one(&state.db)
        .await?;
    let lock = Lock::from(&row);
    if inserted.is_none() {
        return Ok(lfs_json(
            StatusCode::CONFLICT,
            &serde_json::json!({"lock": lock, "message": "already created lock"}),
        ));
    }
    Ok(lfs_json(
        StatusCode::CREATED,
        &serde_json::json!({ "lock": lock }),
    ))
}

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    pub path: Option<String>,
    pub id: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<i64>,
    pub refspec: Option<String>,
}

fn cursor(c: Option<&str>) -> Result<i64, ApiError> {
    match c.filter(|c| !c.is_empty()) {
        None => Ok(0),
        Some(c) => c
            .parse()
            .map_err(|_| ApiError::unprocessable("Invalid cursor")),
    }
}

async fn page(
    state: &AppState,
    repo_id: i64,
    path: Option<&str>,
    id: Option<i64>,
    after: i64,
    limit: i64,
) -> ApiResult<(Vec<LockRow>, Option<String>)> {
    let mut rows: Vec<LockRow> = sqlx::query_as(&format!(
        "{SELECT} WHERE l.repo_id = $1 AND l.id > $2
            AND ($3::text IS NULL OR l.path = $3)
            AND ($4::bigint IS NULL OR l.id = $4)
          ORDER BY l.id LIMIT $5"
    ))
    .bind(repo_id)
    .bind(after)
    .bind(path)
    .bind(id)
    .bind(limit + 1)
    .fetch_all(&state.db)
    .await?;
    let next = if rows.len() as i64 > limit {
        rows.truncate(limit as usize);
        rows.last().map(|r| r.id.to_string())
    } else {
        None
    };
    Ok((rows, next))
}

/// `GET /{owner}/{repo}/info/lfs/locks`
pub async fn list(
    State(state): State<AppState>,
    Path((owner, repo)): Path<(String, String)>,
    Query(q): Query<ListQuery>,
    headers: HeaderMap,
) -> LfsResult<Response> {
    let lfs = authorize(&state, &headers, &owner, &repo, false).await?;
    let limit = q.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let id = match q.id.as_deref().filter(|s| !s.is_empty()) {
        Some(s) => Some(s.parse::<i64>().map_err(|_| ApiError::NotFound)?),
        None => None,
    };
    let path = q.path.as_deref().map(normalize);
    let (rows, next) = page(
        &state,
        lfs.access.repo.id,
        path.as_deref(),
        id,
        cursor(q.cursor.as_deref())?,
        limit,
    )
    .await?;
    let _ = q.refspec;
    let locks: Vec<Lock> = rows.iter().map(Lock::from).collect();
    let mut body = serde_json::json!({ "locks": locks });
    if let Some(n) = next {
        body["next_cursor"] = n.into();
    }
    Ok(lfs_json(StatusCode::OK, &body))
}

#[derive(Debug, Default, Deserialize)]
pub struct VerifyRequest {
    pub cursor: Option<String>,
    pub limit: Option<i64>,
    #[serde(rename = "ref", default)]
    pub refspec: Option<RefSpec>,
}

/// `POST /{owner}/{repo}/info/lfs/locks/verify`: the caller's locks
/// (`ours`) and everyone else's (`theirs`).
pub async fn verify(
    State(state): State<AppState>,
    Path((owner, repo)): Path<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> LfsResult<Response> {
    let req: VerifyRequest = json(&body)?;
    let lfs = authorize(&state, &headers, &owner, &repo, true).await?;
    let user = require_user(&lfs)?;
    let limit = req.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let (rows, next) = page(
        &state,
        lfs.access.repo.id,
        None,
        None,
        cursor(req.cursor.as_deref())?,
        limit,
    )
    .await?;
    let (ours, theirs): (Vec<&LockRow>, Vec<&LockRow>) =
        rows.iter().partition(|r| r.owner_id == user.user.id);
    let mut body = serde_json::json!({
        "ours": ours.into_iter().map(Lock::from).collect::<Vec<_>>(),
        "theirs": theirs.into_iter().map(Lock::from).collect::<Vec<_>>(),
    });
    if let Some(n) = next {
        body["next_cursor"] = n.into();
    }
    Ok(lfs_json(StatusCode::OK, &body))
}

#[derive(Debug, Default, Deserialize)]
pub struct UnlockRequest {
    #[serde(default)]
    pub force: bool,
}

/// `POST /{owner}/{repo}/info/lfs/locks/{id}/unlock`
pub async fn unlock(
    State(state): State<AppState>,
    Path((owner, repo, id)): Path<(String, String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> LfsResult<Response> {
    let req: UnlockRequest = json(&body)?;
    let lfs = authorize(&state, &headers, &owner, &repo, true).await?;
    let user = require_user(&lfs)?;
    let id: i64 = id.parse().map_err(|_| ApiError::NotFound)?;
    let row: LockRow = sqlx::query_as(&format!("{SELECT} WHERE l.repo_id = $1 AND l.id = $2"))
        .bind(lfs.access.repo.id)
        .bind(id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::NotFound)?;
    if row.owner_id != user.user.id {
        if !req.force {
            return Err(ApiError::forbidden(format!(
                "Lock is owned by {}; use --force to unlock it.",
                row.owner_login
            ))
            .into());
        }
        lfs.access.require(Permission::Admin)?;
    }
    sqlx::query("DELETE FROM lfs_locks WHERE id = $1")
        .bind(id)
        .execute(&state.db)
        .await?;
    Ok(lfs_json(
        StatusCode::OK,
        &serde_json::json!({ "lock": Lock::from(&row) }),
    ))
}
