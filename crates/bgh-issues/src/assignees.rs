//! Assignees: assignable users, checks, and issue assignment.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use bgh_core::models::api::SimpleUser;
use bgh_core::perms::RepoAccess;
use bgh_core::prelude::*;
use serde::Deserialize;

use crate::json::{self, BodyFormat};
use crate::{issues, service};

/// Users with at least triage permission on the repository (owner,
/// collaborators, org admins / members with a write+ base permission,
/// members of teams granted triage+ including child teams).
const ASSIGNABLE_SQL: &str = r#"
    WITH RECURSIVE granted AS (
        SELECT tr.team_id AS id FROM team_repos tr
         WHERE tr.repo_id = $1 AND tr.permission IN ('triage', 'write', 'maintain', 'admin')
        UNION
        SELECT t.id FROM teams t JOIN granted g ON t.parent_id = g.id
    ), ids AS (
        SELECT r.owner_id AS user_id FROM repositories r WHERE r.id = $1
        UNION
        SELECT c.user_id FROM collaborators c
         WHERE c.repo_id = $1 AND c.permission IN ('triage', 'write', 'maintain', 'admin')
        UNION
        SELECT m.user_id FROM org_members m
          JOIN repositories r ON r.owner_id = m.org_id
          LEFT JOIN org_settings s ON s.org_id = m.org_id
         WHERE r.id = $1 AND (m.role = 'admin' OR s.default_repository_permission IN ('write', 'admin'))
        UNION
        SELECT tm.user_id FROM team_members tm WHERE tm.team_id IN (SELECT id FROM granted)
    )
"#;

/// `GET /repos/{owner}/{repo}/assignees`
pub async fn list(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<Page<SimpleUser>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let rows: Vec<db::User> = sqlx::query_as(&format!(
        "{ASSIGNABLE_SQL}
         SELECT {} FROM users u WHERE u.id IN (SELECT user_id FROM ids)
            AND u.type = 'User' AND u.suspended_at IS NULL
          ORDER BY lower(u.login), u.id LIMIT $2 OFFSET $3",
        db::prefixed("u", db::User::COLUMNS)
    ))
    .bind(access.repo.id)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    Ok(p.page(rows).map(|u| SimpleUser::new(&state.urls, &u)))
}

async fn check(state: &AppState, access: &RepoAccess, login: &str) -> ApiResult<StatusCode> {
    let user = db::User::find_by_login(&state.db, login)
        .await?
        .ok_or(ApiError::NotFound)?;
    if service::is_assignable(state, &access.repo, &user).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound)
    }
}

/// `GET /repos/{owner}/{repo}/assignees/{assignee}`: 204 or 404.
pub async fn check_repo(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, login)): Path<(String, String, String)>,
) -> ApiResult<StatusCode> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    check(&state, &access, &login).await
}

/// `GET /repos/{owner}/{repo}/issues/{issue_number}/assignees/{assignee}`
pub async fn check_issue(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, number, login)): Path<(String, String, i64, String)>,
) -> ApiResult<StatusCode> {
    let (access, _) = issues::load(&state, auth.as_ref(), &owner, &repo, number).await?;
    check(&state, &access, &login).await
}

#[derive(Debug, Default, Deserialize)]
pub struct AssigneesBody {
    pub assignees: Option<Vec<String>>,
}

/// Resolve logins, silently dropping users who can't be assigned.
async fn assignable_ids(
    state: &AppState,
    repo: &db::Repository,
    logins: &[String],
) -> ApiResult<Vec<i64>> {
    let mut out = Vec::new();
    for login in logins.iter().take(service::MAX_ASSIGNEES * 2) {
        if let Some(u) = db::User::find_by_login(&state.db, login).await?
            && !out.contains(&u.id)
            && service::is_assignable(state, repo, &u).await?
        {
            out.push(u.id);
        }
    }
    Ok(out)
}

/// `POST /repos/{owner}/{repo}/issues/{issue_number}/assignees` → 201.
pub async fn add(
    State(state): State<AppState>,
    auth: RequireUser,
    fmt: BodyFormat,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    Json(body): Json<AssigneesBody>,
) -> ApiResult<Response> {
    let (access, issue) = issues::load(&state, Some(&auth), &owner, &repo, number).await?;
    access.require_not_archived()?;
    let mut issue = issue;
    // Without triage permission the request is accepted but ignored.
    if access.permission >= Permission::Triage {
        let ids = assignable_ids(
            &state,
            &access.repo,
            body.assignees.as_deref().unwrap_or(&[]),
        )
        .await?;
        let mut tx = Tx::begin(&state).await?;
        let locked = service::lock_issue(&mut tx, issue.id).await?;
        if !service::add_assignees(&mut tx, &locked, auth.user.id, &ids)
            .await?
            .is_empty()
        {
            issue = service::touch_and_sync(&mut tx, issue.id, SyncAction::Update).await?;
        }
        tx.commit().await?;
    }
    let rendered = json::issue(&state, fmt, &issue, &json::repo_map(&access)).await?;
    Ok((StatusCode::CREATED, Json(rendered)).into_response())
}

/// `DELETE /repos/{owner}/{repo}/issues/{issue_number}/assignees` → 200.
pub async fn remove(
    State(state): State<AppState>,
    auth: RequireUser,
    fmt: BodyFormat,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    Json(body): Json<AssigneesBody>,
) -> ApiResult<Json<json::Issue>> {
    let (access, issue) = issues::load(&state, Some(&auth), &owner, &repo, number).await?;
    access.require_not_archived()?;
    let mut issue = issue;
    if access.permission >= Permission::Triage {
        let logins: Vec<String> = body
            .assignees
            .unwrap_or_default()
            .into_iter()
            .map(|l| l.to_lowercase())
            .collect();
        let ids: Vec<i64> = sqlx::query_scalar(
            "SELECT u.id FROM users u JOIN issue_assignees a ON a.user_id = u.id
              WHERE a.issue_id = $1 AND lower(u.login) = ANY($2)",
        )
        .bind(issue.id)
        .bind(&logins)
        .fetch_all(&state.db)
        .await?;
        let mut tx = Tx::begin(&state).await?;
        let locked = service::lock_issue(&mut tx, issue.id).await?;
        if !service::remove_assignees(&mut tx, &locked, auth.user.id, &ids)
            .await?
            .is_empty()
        {
            issue = service::touch_and_sync(&mut tx, issue.id, SyncAction::Update).await?;
        }
        tx.commit().await?;
    }
    Ok(Json(
        json::issue(&state, fmt, &issue, &json::repo_map(&access)).await?,
    ))
}
