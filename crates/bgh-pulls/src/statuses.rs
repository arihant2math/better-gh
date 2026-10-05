//! Commit statuses: create, list, combined status.

use std::collections::HashMap;

use axum::extract::State;
use axum::http::StatusCode;
use bgh_core::models::api::{MinimalRepository, SimpleUser};
use bgh_core::node_id::{self, NodeType};
use bgh_core::prelude::*;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::git;
use crate::jobs::ChecksChanged;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct StatusRow {
    pub id: i64,
    pub repo_id: i64,
    pub sha: String,
    pub state: String,
    pub context: String,
    pub description: Option<String>,
    pub target_url: Option<String>,
    pub avatar_url: Option<String>,
    pub creator_id: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

const COLUMNS: &str = "id, repo_id, sha, state, context, description, target_url, avatar_url, \
    creator_id, created_at, updated_at";

/// `status`.
#[derive(Debug, Clone, Serialize)]
pub struct StatusJson {
    pub url: String,
    pub avatar_url: Option<String>,
    pub id: i64,
    pub node_id: String,
    pub state: String,
    pub description: Option<String>,
    pub target_url: Option<String>,
    pub context: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub creator: Option<Option<SimpleUser>>,
}

fn render_rows(
    state: &AppState,
    access: &RepoAccess,
    rows: &[StatusRow],
    users: Option<&HashMap<i64, db::User>>,
) -> Vec<StatusJson> {
    let owner = access.owner.login.as_str();
    let name = access.repo.name.as_str();
    rows.iter()
        .map(|r| {
            let creator = r.creator_id.and_then(|c| users.and_then(|u| u.get(&c)));
            StatusJson {
                url: state
                    .urls
                    .api(&format!("/repos/{owner}/{name}/statuses/{}", r.sha)),
                avatar_url: r
                    .avatar_url
                    .clone()
                    .or_else(|| creator.map(|u| state.urls.avatar(u.id, u.avatar_url.as_deref()))),
                id: r.id,
                node_id: node_id::encode(NodeType::Status, r.id),
                state: r.state.clone(),
                description: r.description.clone(),
                target_url: r.target_url.clone(),
                context: r.context.clone(),
                created_at: r.created_at.into(),
                updated_at: r.updated_at.into(),
                creator: users.map(|_| creator.map(|u| SimpleUser::new(&state.urls, u))),
            }
        })
        .collect()
}

async fn render(
    state: &AppState,
    access: &RepoAccess,
    rows: &[StatusRow],
) -> ApiResult<Vec<StatusJson>> {
    let users = bgh_core::views::users_by_id(state, rows.iter().map(|r| r.creator_id)).await?;
    Ok(render_rows(state, access, rows, Some(&users)))
}

/// Resolve `{ref}` (SHA, branch, tag) or 422 / 404 like GitHub.
async fn resolve(state: &AppState, access: &RepoAccess, rev: &str) -> ApiResult<String> {
    git::resolve_commit(&git::store(state), access.repo.id, rev)
        .await?
        .ok_or(ApiError::NotFound)
}

#[derive(Debug, Deserialize)]
pub struct CreateBody {
    pub state: Option<String>,
    pub target_url: Option<String>,
    pub description: Option<String>,
    pub context: Option<String>,
}

/// Service entry point (also used by bgh-actions): record a status.
#[allow(clippy::too_many_arguments)]
pub async fn create_status(
    state: &AppState,
    repo_id: i64,
    sha: &str,
    actor_id: i64,
    status: &str,
    context: &str,
    description: Option<&str>,
    target_url: Option<&str>,
) -> ApiResult<StatusRow> {
    let mut tx = Tx::begin(state).await?;
    let row: StatusRow = sqlx::query_as(&format!(
        "INSERT INTO commit_statuses (repo_id, sha, state, context, description, target_url, creator_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING {COLUMNS}"
    ))
    .bind(repo_id)
    .bind(sha)
    .bind(status)
    .bind(context)
    .bind(description)
    .bind(target_url)
    .bind(actor_id)
    .fetch_one(&mut *tx)
    .await?;
    tx.sync(
        &bgh_core::sync::repo_scope(repo_id),
        "commit_status",
        row.id,
        SyncAction::Insert,
        &json!({"id": row.id, "sha": row.sha, "state": row.state, "context": row.context,
                "description": row.description, "target_url": row.target_url,
                "creator_id": row.creator_id, "created_at": Timestamp::from(row.created_at)}),
    )
    .await?;
    tx.enqueue(&ChecksChanged {
        repo_id,
        sha: sha.to_string(),
    })
    .await?;
    tx.emit(Event::CommitStatusCreated {
        repo_id,
        status_id: row.id,
        sha: sha.to_string(),
        actor_id,
    });
    tx.commit().await?;
    Ok(row)
}

pub async fn create(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, sha)): Path<(String, String, String)>,
    Json(body): Json<CreateBody>,
) -> ApiResult<(StatusCode, axum::Json<StatusJson>)> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    access.require(Permission::Write)?;
    access.require_not_archived()?;
    let st = body
        .state
        .as_deref()
        .ok_or_else(|| ApiError::invalid_field(FieldError::missing_field("Status", "state")))?;
    if !matches!(st, "error" | "failure" | "pending" | "success") {
        return Err(ApiError::invalid_field(FieldError::invalid(
            "Status", "state",
        )));
    }
    let sha = git::resolve_commit(&git::store(&state), access.repo.id, &sha)
        .await?
        .ok_or_else(|| ApiError::unprocessable(format!("No commit found for SHA: {sha}")))?;
    let context = body
        .context
        .as_deref()
        .filter(|c| !c.is_empty())
        .unwrap_or("default");
    let row = create_status(
        &state,
        access.repo.id,
        &sha,
        auth.user.id,
        st,
        context,
        body.description.as_deref(),
        body.target_url.as_deref(),
    )
    .await?;
    let out = render(&state, &access, &[row]).await?.remove(0);
    Ok((StatusCode::CREATED, axum::Json(out)))
}

pub async fn list(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo, rev)): Path<(String, String, String)>,
) -> ApiResult<Page<StatusJson>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let sha = resolve(&state, &access, &rev).await?;
    let rows: Vec<StatusRow> = sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM commit_statuses WHERE repo_id = $1 AND sha = $2
          ORDER BY id DESC LIMIT $3 OFFSET $4"
    ))
    .bind(access.repo.id)
    .bind(&sha)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let page = p.page(rows);
    let items = render(&state, &access, &page.items).await?;
    Ok(Page {
        items,
        link: page.link,
    })
}

#[derive(Debug, Serialize)]
pub struct CombinedStatus {
    pub state: String,
    pub statuses: Vec<StatusJson>,
    pub sha: String,
    pub total_count: i64,
    pub repository: MinimalRepository,
    pub commit_url: String,
    pub url: String,
}

/// Combined state: failure if any error/failure, pending if any pending
/// or none, success otherwise.
pub fn combine(states: &[&str]) -> &'static str {
    if states.iter().any(|s| *s == "error" || *s == "failure") {
        "failure"
    } else if states.is_empty() || states.contains(&"pending") {
        "pending"
    } else {
        "success"
    }
}

pub async fn combined(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo, rev)): Path<(String, String, String)>,
) -> ApiResult<axum::Json<CombinedStatus>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let sha = resolve(&state, &access, &rev).await?;
    let rows: Vec<StatusRow> = sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM (
            SELECT DISTINCT ON (context) {COLUMNS} FROM commit_statuses
             WHERE repo_id = $1 AND sha = $2 ORDER BY context, id DESC) s
          ORDER BY id"
    ))
    .bind(access.repo.id)
    .bind(&sha)
    .fetch_all(&state.db)
    .await?;
    let states: Vec<&str> = rows.iter().map(|r| r.state.as_str()).collect();
    let combined = combine(&states).to_string();
    let total = rows.len() as i64;
    let page: Vec<StatusRow> = rows
        .into_iter()
        .skip(p.offset() as usize)
        .take(p.limit() as usize)
        .collect();
    let o = access.owner.login.as_str();
    let n = access.repo.name.as_str();
    Ok(axum::Json(CombinedStatus {
        state: combined,
        statuses: render_rows(&state, &access, &page, None),
        total_count: total,
        repository: MinimalRepository::new(
            &state.urls,
            &access.repo,
            &access.owner,
            access.api_permission(),
        ),
        commit_url: state.urls.commit(o, n, &sha),
        url: state
            .urls
            .api(&format!("/repos/{o}/{n}/commits/{sha}/status")),
        sha,
    }))
}
