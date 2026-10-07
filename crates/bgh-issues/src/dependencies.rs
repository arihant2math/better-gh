//! Issue dependencies (GitHub "relationships"): an issue is *blocked by*
//! other issues, possibly in other repositories.
//!
//! `/repos/{o}/{r}/issues/{n}/dependencies/blocked_by` (GET, POST,
//! DELETE `/{issue_id}`) and `/dependencies/blocking` (GET). Cycles are
//! refused with 422. Each change writes `blocked_by_added|removed` on the
//! blocked issue and `blocking_added|removed` on the blocking one.

use std::collections::HashMap;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use bgh_core::perms::{self, RepoAccess};
use bgh_core::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::json::{self, BodyFormat, IssueOpts, RepoInfo};
use crate::{issues, service};

/// GitHub's limit per relationship direction.
pub const MAX_DEPENDENCIES: i64 = 50;

/// `issue_dependencies_summary` on the issue shape.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct Summary {
    /// Open issues blocking this one.
    pub blocked_by: i64,
    /// Open issues this one blocks.
    pub blocking: i64,
    pub total_blocked_by: i64,
    pub total_blocking: i64,
}

/// Summaries for `ids` (issues without dependencies are absent).
pub async fn summaries(
    db: impl sqlx::PgExecutor<'_>,
    ids: &[i64],
) -> ApiResult<HashMap<i64, Summary>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows: Vec<(i64, i64, i64, i64, i64)> = sqlx::query_as(
        "SELECT x.id,
                count(*) FILTER (WHERE d.blocked_id = x.id AND o.state = 'open'),
                count(*) FILTER (WHERE d.blocking_id = x.id AND o.state = 'open'),
                count(*) FILTER (WHERE d.blocked_id = x.id),
                count(*) FILTER (WHERE d.blocking_id = x.id)
           FROM unnest($1::bigint[]) AS x(id)
           JOIN issue_dependencies d ON d.blocked_id = x.id OR d.blocking_id = x.id
           JOIN issues o ON o.id = CASE WHEN d.blocked_id = x.id THEN d.blocking_id ELSE d.blocked_id END
          GROUP BY x.id",
    )
    .bind(ids)
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, b, bl, tb, tbl)| {
            (
                id,
                Summary {
                    blocked_by: b,
                    blocking: bl,
                    total_blocked_by: tb,
                    total_blocking: tbl,
                },
            )
        })
        .collect())
}

/// Re-sync issues blocked by `issue_id` (their `openBlockedBy` follows the
/// blocker's state). Called when an issue is closed or reopened.
pub async fn sync_dependents(tx: &mut Tx, issue_id: i64) -> ApiResult<()> {
    let ids: Vec<i64> = sqlx::query_scalar(
        "SELECT blocked_id FROM issue_dependencies WHERE blocking_id = $1
         UNION SELECT blocking_id FROM issue_dependencies WHERE blocked_id = $1",
    )
    .bind(issue_id)
    .fetch_all(&mut **tx)
    .await?;
    if !ids.is_empty() {
        tx.sync_models(SyncModel::Issue, &ids, SyncAction::Update)
            .await?;
    }
    Ok(())
}

/// Drop issues whose repository the caller can't read.
async fn readable(
    state: &AppState,
    auth: Option<&AuthContext>,
    rows: Vec<db::Issue>,
) -> ApiResult<(Vec<db::Issue>, json::RepoMap)> {
    let repos = json::load_repos(state, rows.iter().map(|i| i.repo_id)).await?;
    let list: Vec<db::Repository> = repos.values().map(|r| r.repo.clone()).collect();
    let raw = perms::repo_permissions(&state.db, auth.map(|a| a.user.id), &list).await?;
    let rows = rows
        .into_iter()
        .filter(|i| {
            repos.get(&i.repo_id).is_some_and(|r| {
                (i.is_pull_request || r.repo.has_issues)
                    && perms::effective(
                        auth,
                        &r.repo,
                        raw.get(&i.repo_id).copied().unwrap_or(Permission::None),
                    ) >= Permission::Read
            })
        })
        .collect();
    Ok((rows, repos))
}

async fn list_side(
    state: &AppState,
    auth: Option<&AuthContext>,
    fmt: BodyFormat,
    p: Pagination,
    (owner, repo, number): (String, String, i64),
    blocked_by: bool,
) -> ApiResult<Page<json::Issue>> {
    let (_, issue) = issues::load(state, auth, &owner, &repo, number).await?;
    if issue.is_pull_request {
        return Err(ApiError::NotFound);
    }
    let (join, filter) = if blocked_by {
        ("d.blocking_id", "d.blocked_id")
    } else {
        ("d.blocked_id", "d.blocking_id")
    };
    let rows: Vec<db::Issue> = sqlx::query_as(&format!(
        "SELECT {} FROM issues i JOIN issue_dependencies d ON {join} = i.id
          WHERE {filter} = $1 ORDER BY d.created_at, i.id LIMIT $2 OFFSET $3",
        db::prefixed("i", db::Issue::COLUMNS)
    ))
    .bind(issue.id)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let page = p.page(rows);
    let (rows, repos) = readable(state, auth, page.items).await?;
    Ok(Page {
        items: json::issues(state, fmt, &rows, &repos, IssueOpts::default()).await?,
        link: page.link,
    })
}

/// `GET /repos/{owner}/{repo}/issues/{issue_number}/dependencies/blocked_by`
pub async fn list_blocked_by(
    State(state): State<AppState>,
    auth: MaybeUser,
    fmt: BodyFormat,
    p: Pagination,
    Path(path): Path<(String, String, i64)>,
) -> ApiResult<Page<json::Issue>> {
    list_side(&state, auth.as_ref(), fmt, p, path, true).await
}

/// `GET /repos/{owner}/{repo}/issues/{issue_number}/dependencies/blocking`
pub async fn list_blocking(
    State(state): State<AppState>,
    auth: MaybeUser,
    fmt: BodyFormat,
    p: Pagination,
    Path(path): Path<(String, String, i64)>,
) -> ApiResult<Page<json::Issue>> {
    list_side(&state, auth.as_ref(), fmt, p, path, false).await
}

#[derive(Debug, Default, Deserialize)]
pub struct AddBody {
    pub issue_id: Option<i64>,
}

/// `{id, number, repository}` reference stored in event data.
pub fn ref_json(info: &RepoInfo, issue: &db::Issue) -> serde_json::Value {
    json!({
        "id": issue.id,
        "number": issue.number,
        "repository": format!("{}/{}", info.owner.login, info.repo.name),
    })
}

/// Load an issue by id for the caller; 422 (`field`) if it doesn't exist,
/// isn't readable or is a pull request.
pub async fn load_other(
    state: &AppState,
    auth: &AuthContext,
    id: Option<i64>,
    field: &str,
) -> ApiResult<(db::Issue, RepoInfo)> {
    let invalid = || ApiError::invalid_field(FieldError::invalid("Issue", field));
    let id =
        id.ok_or_else(|| ApiError::invalid_field(FieldError::missing_field("Issue", field)))?;
    let other = service::issue_by_id(&state.db, id)
        .await
        .map_err(|_| invalid())?;
    let mut repos = json::load_repos(state, [other.repo_id]).await?;
    let info = repos.remove(&other.repo_id).ok_or_else(invalid)?;
    let access = RepoAccess::for_repo(state, Some(auth), info.repo.clone(), info.owner.clone())
        .await
        .map_err(|_| invalid())?;
    if other.is_pull_request || !access.repo.has_issues {
        return Err(invalid());
    }
    Ok((other, info))
}

/// `POST /repos/{owner}/{repo}/issues/{issue_number}/dependencies/blocked_by`
/// (`{"issue_id": N}`) → 201 with the blocked issue.
pub async fn add(
    State(state): State<AppState>,
    auth: RequireUser,
    fmt: BodyFormat,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    Json(body): Json<AddBody>,
) -> ApiResult<Response> {
    let (access, blocked) = issues::load(&state, Some(&auth), &owner, &repo, number).await?;
    access.require(Permission::Triage)?;
    access.require_not_archived()?;
    if blocked.is_pull_request {
        return Err(ApiError::NotFound);
    }
    let (blocking, blocking_info) = load_other(&state, &auth, body.issue_id, "issue_id").await?;
    if blocking.id == blocked.id {
        return Err(ApiError::unprocessable(
            "An issue cannot be blocked by itself",
        ));
    }
    let blocked_info = RepoInfo::from_access(&access);
    let mut tx = Tx::begin(&state).await?;
    // Serialize dependency writes: the cycle check must see every edge.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('bgh:issue_dependencies'))")
        .execute(&mut *tx)
        .await?;
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM issue_dependencies WHERE blocked_id = $1 AND blocking_id = $2)",
    )
    .bind(blocked.id)
    .bind(blocking.id)
    .fetch_one(&mut *tx)
    .await?;
    if exists {
        return Err(ApiError::unprocessable(
            "Issue is already blocked by this issue",
        ));
    }
    // A cycle exists if `blocked` already (transitively) blocks `blocking`.
    let cycle: bool = sqlx::query_scalar(
        "WITH RECURSIVE up AS (
             SELECT blocking_id AS id, 1 AS depth FROM issue_dependencies WHERE blocked_id = $1
             UNION
             SELECT d.blocking_id, up.depth + 1 FROM issue_dependencies d JOIN up ON d.blocked_id = up.id
              WHERE up.depth < 1000
         ) SELECT EXISTS (SELECT 1 FROM up WHERE id = $2)",
    )
    .bind(blocking.id)
    .bind(blocked.id)
    .fetch_one(&mut *tx)
    .await?;
    if cycle {
        return Err(ApiError::unprocessable(
            "Adding this dependency would create a circular dependency",
        ));
    }
    let (n_blocked_by, n_blocking): (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM issue_dependencies WHERE blocked_id = $1),
                (SELECT count(*) FROM issue_dependencies WHERE blocking_id = $2)",
    )
    .bind(blocked.id)
    .bind(blocking.id)
    .fetch_one(&mut *tx)
    .await?;
    if n_blocked_by >= MAX_DEPENDENCIES || n_blocking >= MAX_DEPENDENCIES {
        return Err(ApiError::unprocessable(format!(
            "Issues can have at most {MAX_DEPENDENCIES} dependencies of each kind"
        )));
    }
    sqlx::query("INSERT INTO issue_dependencies (blocked_id, blocking_id) VALUES ($1, $2)")
        .bind(blocked.id)
        .bind(blocking.id)
        .execute(&mut *tx)
        .await?;
    service::add_event(
        &mut tx,
        &blocked,
        Some(auth.user.id),
        "blocked_by_added",
        None,
        json!({ "blocking_issue": ref_json(&blocking_info, &blocking) }),
    )
    .await?;
    service::add_event(
        &mut tx,
        &blocking,
        Some(auth.user.id),
        "blocking_added",
        None,
        json!({ "blocked_issue": ref_json(&blocked_info, &blocked) }),
    )
    .await?;
    service::touch_and_sync(&mut tx, blocking.id, SyncAction::Update).await?;
    let blocked = service::touch_and_sync(&mut tx, blocked.id, SyncAction::Update).await?;
    tx.commit().await?;
    let rendered = json::issue(&state, fmt, &blocked, &json::repo_map(&access)).await?;
    Ok((StatusCode::CREATED, Json(rendered)).into_response())
}

/// `DELETE /repos/{owner}/{repo}/issues/{issue_number}/dependencies/blocked_by/{issue_id}`
/// → 200 with the blocked issue.
pub async fn remove(
    State(state): State<AppState>,
    auth: RequireUser,
    fmt: BodyFormat,
    Path((owner, repo, number, issue_id)): Path<(String, String, i64, i64)>,
) -> ApiResult<Json<json::Issue>> {
    let (access, blocked) = issues::load(&state, Some(&auth), &owner, &repo, number).await?;
    access.require(Permission::Triage)?;
    access.require_not_archived()?;
    let mut tx = Tx::begin(&state).await?;
    let deleted =
        sqlx::query("DELETE FROM issue_dependencies WHERE blocked_id = $1 AND blocking_id = $2")
            .bind(blocked.id)
            .bind(issue_id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
    if deleted == 0 {
        return Err(ApiError::NotFound);
    }
    let blocking = service::issue_by_id(&mut *tx, issue_id).await?;
    let mut repos = json::load_repos(&state, [blocking.repo_id]).await?;
    let blocking_info = repos.remove(&blocking.repo_id).ok_or(ApiError::NotFound)?;
    let blocked_info = RepoInfo::from_access(&access);
    service::add_event(
        &mut tx,
        &blocked,
        Some(auth.user.id),
        "blocked_by_removed",
        None,
        json!({ "blocking_issue": ref_json(&blocking_info, &blocking) }),
    )
    .await?;
    service::add_event(
        &mut tx,
        &blocking,
        Some(auth.user.id),
        "blocking_removed",
        None,
        json!({ "blocked_issue": ref_json(&blocked_info, &blocked) }),
    )
    .await?;
    service::touch_and_sync(&mut tx, blocking.id, SyncAction::Update).await?;
    let blocked = service::touch_and_sync(&mut tx, blocked.id, SyncAction::Update).await?;
    tx.commit().await?;
    Ok(Json(
        json::issue(&state, fmt, &blocked, &json::repo_map(&access)).await?,
    ))
}
