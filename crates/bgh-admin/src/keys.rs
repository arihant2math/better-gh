//! GHES `GET /admin/keys` and `DELETE /admin/keys/{key_ids}`: every public
//! SSH key on the instance (user keys and deploy keys).

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use bgh_core::audit::Target;
use bgh_core::prelude::*;
use bgh_core::time::ts;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::FromRow;

use crate::common::{direction, log};

#[derive(Debug, Default, Deserialize)]
pub struct ListParams {
    /// Only keys used (accessed) after this time.
    pub since: Option<DateTime<Utc>>,
    /// `created` | `updated` | `accessed`
    pub sort: Option<String>,
    pub direction: Option<String>,
}

#[derive(Debug, FromRow)]
struct KeyRow {
    id: i64,
    key: String,
    title: String,
    read_only: bool,
    verified: bool,
    created_at: DateTime<Utc>,
    last_used_at: Option<DateTime<Utc>>,
    user_id: Option<i64>,
    repo_id: Option<i64>,
    owner_login: Option<String>,
    repo_name: Option<String>,
    added_by: Option<String>,
}

/// `public-key-full`
#[derive(Debug, Serialize)]
pub struct PublicKeyFull {
    pub id: i64,
    pub key: String,
    pub user_id: Option<i64>,
    pub repository_id: Option<i64>,
    pub url: String,
    pub title: String,
    pub read_only: bool,
    pub verified: bool,
    pub created_at: Timestamp,
    pub added_by: Option<String>,
    pub last_used: Option<Timestamp>,
}

/// `GET /admin/keys`
pub async fn list(
    State(state): State<AppState>,
    _auth: RequireSiteAdmin,
    p: Pagination,
    Query(q): Query<ListParams>,
) -> ApiResult<Page<PublicKeyFull>> {
    let order = match q.sort.as_deref() {
        Some("accessed") => "last_used_at",
        _ => "created_at",
    };
    let dir = direction(q.direction.as_deref(), true);
    let rows: Vec<KeyRow> = sqlx::query_as(&format!(
        "SELECT * FROM (
            SELECT k.id, k.key, k.title, k.read_only, k.verified, k.created_at, k.last_used_at,
                   k.user_id, NULL::bigint AS repo_id, u.login AS owner_login,
                   NULL::text AS repo_name, u.login AS added_by
              FROM ssh_keys k JOIN users u ON u.id = k.user_id
            UNION ALL
            SELECT d.id, d.key, d.title, d.read_only, d.verified, d.created_at, d.last_used_at,
                   NULL::bigint, d.repo_id, o.login, r.name, a.login
              FROM deploy_keys d
              JOIN repositories r ON r.id = d.repo_id
              JOIN users o ON o.id = r.owner_id
              LEFT JOIN users a ON a.id = d.added_by_id
         ) k
         WHERE ($1::timestamptz IS NULL OR k.last_used_at > $1)
         ORDER BY {order} {dir} NULLS LAST, id {dir}
         LIMIT $2 OFFSET $3"
    ))
    .bind(q.since)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    Ok(p.page(rows).map(|r| {
        let url = match (&r.repo_id, &r.owner_login, &r.repo_name) {
            (Some(_), Some(o), Some(n)) => {
                format!("{}/keys/{}", state.urls.repo(o, n), r.id)
            }
            _ => state.urls.api(&format!("/user/keys/{}", r.id)),
        };
        PublicKeyFull {
            id: r.id,
            key: r.key,
            user_id: r.user_id,
            repository_id: r.repo_id,
            url,
            title: r.title,
            read_only: r.read_only,
            verified: r.verified,
            created_at: r.created_at.into(),
            added_by: r.added_by,
            last_used: ts(r.last_used_at),
        }
    }))
}

/// `DELETE /admin/keys/{key_ids}` → 204. User keys take precedence when a
/// user key and a deploy key share the id.
pub async fn delete(
    State(state): State<AppState>,
    auth: RequireSiteAdmin,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    let id: i64 = id.parse().map_err(|_| ApiError::NotFound)?;
    let mut tx = Tx::begin(&state).await?;
    let user_key: Option<(i64, String)> =
        sqlx::query_as("DELETE FROM ssh_keys WHERE id = $1 RETURNING user_id, fingerprint")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    let (target, data) = match user_key {
        Some((user_id, fp)) => (
            Target::User(user_id),
            json!({ "key_id": id, "fingerprint": fp, "type": "user" }),
        ),
        None => {
            let deploy: Option<(i64, String)> = sqlx::query_as(
                "DELETE FROM deploy_keys WHERE id = $1 RETURNING repo_id, fingerprint",
            )
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
            let (repo_id, fp) = deploy.ok_or(ApiError::NotFound)?;
            (
                Target::Repo {
                    id: repo_id,
                    org_id: None,
                },
                json!({ "key_id": id, "fingerprint": fp, "type": "deploy" }),
            )
        }
    };
    log(&mut tx, &auth, &headers, "public_key.delete", target, data).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
