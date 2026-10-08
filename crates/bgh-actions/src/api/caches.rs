//! Cache management REST API (`gh cache list/delete`):
//!
//! * `GET /repos/{o}/{r}/actions/caches` (`key` prefix, `ref`, `sort`,
//!   `direction`), `DELETE ?key=&ref=`, `DELETE …/caches/{cache_id}`;
//! * `GET /repos/{o}/{r}/actions/cache/usage`;
//! * `GET|PATCH /repos/{o}/{r}/actions/cache/usage-policy` (GHES);
//! * `GET /orgs/{org}/actions/cache/usage` and `…/usage-by-repository`.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use bgh_core::pagination::Pagination;
use bgh_core::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{load_org, wrapped};
use crate::cache::CacheRow;

/// GitHub's `actions-cache-list` item.
#[derive(Debug, Serialize)]
pub struct CacheItem {
    pub id: i64,
    #[serde(rename = "ref")]
    pub git_ref: String,
    pub key: String,
    pub version: String,
    pub last_accessed_at: Timestamp,
    pub created_at: Timestamp,
    pub size_in_bytes: i64,
}

impl From<&CacheRow> for CacheItem {
    fn from(c: &CacheRow) -> Self {
        Self {
            id: c.id,
            git_ref: c.git_ref.clone(),
            key: c.key.clone(),
            version: c.version.clone(),
            last_accessed_at: Timestamp(c.last_accessed_at),
            created_at: Timestamp(c.created_at),
            size_in_bytes: c.size_in_bytes,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct ListQuery {
    pub key: Option<String>,
    #[serde(rename = "ref")]
    pub git_ref: Option<String>,
    pub sort: Option<String>,
    pub direction: Option<String>,
}

/// `refs/heads/x` for a bare branch name (GitHub accepts both forms).
fn full_ref(r: &str) -> String {
    if r.starts_with("refs/") {
        r.to_string()
    } else {
        format!("refs/heads/{r}")
    }
}

fn like_prefix(prefix: &str) -> String {
    let mut out = String::new();
    for c in prefix.chars() {
        if matches!(c, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('%');
    out
}

/// `GET /repos/{owner}/{repo}/actions/caches`
pub async fn list(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo)): Path<(String, String)>,
    Query(q): Query<ListQuery>,
) -> ApiResult<Response> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    if auth.0.is_none() {
        return Err(ApiError::requires_auth());
    }
    let sort = match q.sort.as_deref().unwrap_or("last_accessed_at") {
        s @ ("created_at" | "last_accessed_at" | "size_in_bytes") => s,
        _ => {
            return Err(ApiError::invalid_field(FieldError::invalid(
                "ActionsCache",
                "sort",
            )));
        }
    };
    let dir = match q.direction.as_deref().unwrap_or("desc") {
        "asc" => "ASC",
        "desc" => "DESC",
        _ => {
            return Err(ApiError::invalid_field(FieldError::invalid(
                "ActionsCache",
                "direction",
            )));
        }
    };
    let key = q.key.as_deref().filter(|k| !k.is_empty()).map(like_prefix);
    let git_ref = q.git_ref.as_deref().filter(|r| !r.is_empty()).map(full_ref);
    const WHERE: &str = "repo_id = $1 AND committed AND ($2::text IS NULL OR key LIKE $2)
                         AND ($3::text IS NULL OR ref = $3)";
    let total: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM actions_caches WHERE {WHERE}"
    ))
    .bind(access.repo.id)
    .bind(&key)
    .bind(&git_ref)
    .fetch_one(&state.db)
    .await?;
    let rows: Vec<CacheRow> = sqlx::query_as(&format!(
        "SELECT {} FROM actions_caches WHERE {WHERE}
          ORDER BY {sort} {dir}, id {dir} LIMIT $4 OFFSET $5",
        CacheRow::COLUMNS
    ))
    .bind(access.repo.id)
    .bind(&key)
    .bind(&git_ref)
    .bind(p.limit())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let items: Vec<CacheItem> = rows.iter().map(CacheItem::from).collect();
    Ok(wrapped(&p, total, "actions_caches", items))
}

#[derive(Debug, Default, Deserialize)]
pub struct DeleteQuery {
    pub key: Option<String>,
    #[serde(rename = "ref")]
    pub git_ref: Option<String>,
}

/// `DELETE /repos/{owner}/{repo}/actions/caches?key=&ref=` → the deleted
/// entries (exact key match).
pub async fn delete_by_key(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo)): Path<(String, String)>,
    Query(q): Query<DeleteQuery>,
) -> ApiResult<Json<Value>> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    access.require(Permission::Write)?;
    let Some(key) = q.key.filter(|k| !k.is_empty()) else {
        return Err(ApiError::invalid_field(FieldError::missing_field(
            "ActionsCache",
            "key",
        )));
    };
    let git_ref = q.git_ref.as_deref().filter(|r| !r.is_empty()).map(full_ref);
    let rows: Vec<CacheRow> = sqlx::query_as(&format!(
        "SELECT {} FROM actions_caches
          WHERE repo_id = $1 AND committed AND key = $2 AND ($3::text IS NULL OR ref = $3)
          ORDER BY id",
        CacheRow::COLUMNS
    ))
    .bind(access.repo.id)
    .bind(&key)
    .bind(&git_ref)
    .fetch_all(&state.db)
    .await?;
    if rows.is_empty() {
        return Err(ApiError::NotFound);
    }
    let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
    crate::cache::delete(&state, &ids).await?;
    let items: Vec<CacheItem> = rows.iter().map(CacheItem::from).collect();
    Ok(Json(
        json!({"total_count": items.len(), "actions_caches": items}),
    ))
}

/// `DELETE /repos/{owner}/{repo}/actions/caches/{cache_id}`
pub async fn delete(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, id)): Path<(String, String, i64)>,
) -> ApiResult<StatusCode> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    access.require(Permission::Write)?;
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM actions_caches WHERE id = $1 AND repo_id = $2 AND committed)",
    )
    .bind(id)
    .bind(access.repo.id)
    .fetch_one(&state.db)
    .await?;
    if !exists {
        return Err(ApiError::NotFound);
    }
    crate::cache::delete(&state, &[id]).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(sqlx::FromRow)]
struct Usage {
    size: i64,
    count: i64,
}

async fn repo_usage(state: &AppState, repo_id: i64) -> ApiResult<Usage> {
    Ok(sqlx::query_as(
        "SELECT COALESCE(sum(size_in_bytes), 0)::bigint AS size, count(*) AS count
           FROM actions_caches WHERE repo_id = $1 AND committed",
    )
    .bind(repo_id)
    .fetch_one(&state.db)
    .await?)
}

/// `GET /repos/{owner}/{repo}/actions/cache/usage`
pub async fn usage(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    if auth.0.is_none() {
        return Err(ApiError::requires_auth());
    }
    let u = repo_usage(&state, access.repo.id).await?;
    Ok(Json(json!({
        "full_name": format!("{}/{}", access.owner.login, access.repo.name),
        "active_caches_size_in_bytes": u.size,
        "active_caches_count": u.count,
    })))
}

fn gb_ceil(bytes: u64) -> i64 {
    bytes.div_ceil(1 << 30) as i64
}

/// `GET /repos/{owner}/{repo}/actions/cache/usage-policy`
pub async fn get_policy(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    access.require(Permission::Write)?;
    let limit = crate::cache::size_limit(&state, access.repo.id).await?;
    Ok(Json(
        json!({ "repo_cache_size_limit_in_gb": gb_ceil(limit) }),
    ))
}

#[derive(Debug, Deserialize)]
pub struct PolicyBody {
    pub repo_cache_size_limit_in_gb: Option<i64>,
}

/// `PATCH /repos/{owner}/{repo}/actions/cache/usage-policy` (admin) → 204.
/// The limit may not exceed the site maximum (`BGH_ACTIONS_CACHE_SIZE_LIMIT_GB`).
pub async fn set_policy(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo)): Path<(String, String)>,
    Json(body): Json<PolicyBody>,
) -> ApiResult<StatusCode> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    access.require(Permission::Admin)?;
    let Some(gb) = body.repo_cache_size_limit_in_gb else {
        return Err(ApiError::invalid_field(FieldError::missing_field(
            "ActionsCacheUsagePolicy",
            "repo_cache_size_limit_in_gb",
        )));
    };
    let max = gb_ceil(state.config.actions.cache_size_limit);
    if gb < 1 || gb > max {
        return Err(ApiError::invalid_field(FieldError::custom(
            "ActionsCacheUsagePolicy",
            "repo_cache_size_limit_in_gb",
            format!("must be between 1 and {max}"),
        )));
    }
    sqlx::query(
        "INSERT INTO actions_cache_policies (repo_id, repo_cache_size_limit_in_gb) VALUES ($1, $2)
         ON CONFLICT (repo_id) DO UPDATE
            SET repo_cache_size_limit_in_gb = EXCLUDED.repo_cache_size_limit_in_gb, updated_at = now()",
    )
    .bind(access.repo.id)
    .bind(gb as i32)
    .execute(&state.db)
    .await?;
    bgh_core::audit::log(
        &state.db,
        Some(&auth.user),
        "repo.actions_cache_policy",
        bgh_core::audit::Target::Repo {
            id: access.repo.id,
            org_id: None,
        },
        json!({ "repo_cache_size_limit_in_gb": gb }),
    )
    .await?;
    crate::cache::evict(&state, access.repo.id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Organization owner (or site admin) with `read:org`.
async fn require_org_owner(state: &AppState, auth: &AuthContext, org: &str) -> ApiResult<db::User> {
    let org = load_org(state, org).await?;
    if auth.user.site_admin {
        return Ok(org);
    }
    match bgh_core::perms::org_role(&state.db, org.id, auth.user.id).await? {
        Some(r) if r.is_admin() => {
            auth.require_scope("read:org")?;
            Ok(org)
        }
        Some(_) => Err(ApiError::forbidden("Must be an organization owner.")),
        None => Err(ApiError::NotFound),
    }
}

/// `GET /orgs/{org}/actions/cache/usage`
pub async fn org_usage(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(org): Path<String>,
) -> ApiResult<Json<Value>> {
    let org = require_org_owner(&state, &auth, &org).await?;
    let u: Usage = sqlx::query_as(
        "SELECT COALESCE(sum(c.size_in_bytes), 0)::bigint AS size, count(*) AS count
           FROM actions_caches c JOIN repositories r ON r.id = c.repo_id
          WHERE r.owner_id = $1 AND c.committed",
    )
    .bind(org.id)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(json!({
        "total_active_caches_count": u.count,
        "total_active_caches_size_in_bytes": u.size,
    })))
}

#[derive(sqlx::FromRow)]
struct RepoUsage {
    name: String,
    size: i64,
    count: i64,
}

/// `GET /orgs/{org}/actions/cache/usage-by-repository`
pub async fn org_usage_by_repo(
    State(state): State<AppState>,
    auth: RequireUser,
    p: Pagination,
    Path(org): Path<String>,
) -> ApiResult<Response> {
    let org = require_org_owner(&state, &auth, &org).await?;
    let total: i64 = sqlx::query_scalar(
        "SELECT count(DISTINCT c.repo_id) FROM actions_caches c
           JOIN repositories r ON r.id = c.repo_id
          WHERE r.owner_id = $1 AND c.committed",
    )
    .bind(org.id)
    .fetch_one(&state.db)
    .await?;
    let rows: Vec<RepoUsage> = sqlx::query_as(
        "SELECT r.name, COALESCE(sum(c.size_in_bytes), 0)::bigint AS size, count(*) AS count
           FROM actions_caches c JOIN repositories r ON r.id = c.repo_id
          WHERE r.owner_id = $1 AND c.committed
          GROUP BY r.id, r.name ORDER BY lower(r.name), r.id LIMIT $2 OFFSET $3",
    )
    .bind(org.id)
    .bind(p.limit())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let items: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "full_name": format!("{}/{}", org.login, r.name),
                "active_caches_size_in_bytes": r.size,
                "active_caches_count": r.count,
            })
        })
        .collect();
    Ok(wrapped(&p, total, "repository_cache_usages", items))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refs_and_sizes() {
        assert_eq!(full_ref("main"), "refs/heads/main");
        assert_eq!(full_ref("refs/pull/1/merge"), "refs/pull/1/merge");
        assert_eq!(gb_ceil(10 << 30), 10);
        assert_eq!(gb_ceil(1), 1);
    }
}
