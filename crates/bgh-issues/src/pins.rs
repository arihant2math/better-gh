//! Pinned issues (web client endpoints under `/_bgh`).

use axum::extract::State;
use axum::http::StatusCode;
use bgh_core::perms::RepoAccess;
use bgh_core::prelude::*;
use serde_json::json;

use crate::json::{self, BodyFormat, IssueOpts};
use crate::{issues, service};

/// At most this many pinned issues per repository (GitHub's limit).
pub const MAX_PINNED: i64 = 3;

/// `GET /_bgh/repos/{owner}/{repo}/pinned-issues`
pub async fn list(
    State(state): State<AppState>,
    auth: MaybeUser,
    fmt: BodyFormat,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<Json<Vec<json::Issue>>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let rows: Vec<db::Issue> = sqlx::query_as(&format!(
        "SELECT {} FROM issues i JOIN pinned_issues p ON p.issue_id = i.id
          WHERE p.repo_id = $1 ORDER BY p.position, p.created_at",
        db::prefixed("i", db::Issue::COLUMNS)
    ))
    .bind(access.repo.id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(
        json::issues(
            &state,
            fmt,
            &rows,
            &json::repo_map(&access),
            IssueOpts::default(),
        )
        .await?,
    ))
}

/// `PUT /_bgh/repos/{owner}/{repo}/issues/{number}/pin` → 204.
pub async fn pin(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, number)): Path<(String, String, i64)>,
) -> ApiResult<StatusCode> {
    let (access, issue) = issues::load(&state, Some(&auth), &owner, &repo, number).await?;
    access.require(Permission::Write)?;
    access.require_not_archived()?;
    if issue.is_pull_request {
        return Err(ApiError::unprocessable("Pull requests cannot be pinned"));
    }
    let mut tx = Tx::begin(&state).await?;
    // Serialize pinning per repository.
    sqlx::query("SELECT id FROM repositories WHERE id = $1 FOR UPDATE")
        .bind(access.repo.id)
        .execute(&mut *tx)
        .await?;
    let (count, already): (i64, bool) = sqlx::query_as(
        "SELECT count(*), coalesce(bool_or(issue_id = $2), false) FROM pinned_issues WHERE repo_id = $1",
    )
    .bind(access.repo.id)
    .bind(issue.id)
    .fetch_one(&mut *tx)
    .await?;
    if already {
        return Ok(StatusCode::NO_CONTENT);
    }
    if count >= MAX_PINNED {
        return Err(ApiError::unprocessable(format!(
            "A repository can have at most {MAX_PINNED} pinned issues"
        )));
    }
    sqlx::query(
        "INSERT INTO pinned_issues (issue_id, repo_id, position, pinned_by_id) VALUES ($1, $2, $3, $4)",
    )
    .bind(issue.id)
    .bind(access.repo.id)
    .bind(count as i32)
    .bind(auth.user.id)
    .execute(&mut *tx)
    .await?;
    service::add_event(
        &mut tx,
        &issue,
        Some(auth.user.id),
        "pinned",
        None,
        json!({}),
    )
    .await?;
    service::touch_and_sync(&mut tx, issue.id, SyncAction::Update).await?;
    tx.emit(Event::IssuePinned {
        repo_id: issue.repo_id,
        issue_id: issue.id,
        actor_id: auth.user.id,
    });
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /_bgh/repos/{owner}/{repo}/issues/{number}/pin` → 204.
pub async fn unpin(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, number)): Path<(String, String, i64)>,
) -> ApiResult<StatusCode> {
    let (access, issue) = issues::load(&state, Some(&auth), &owner, &repo, number).await?;
    access.require(Permission::Write)?;
    access.require_not_archived()?;
    let mut tx = Tx::begin(&state).await?;
    let n = sqlx::query("DELETE FROM pinned_issues WHERE issue_id = $1")
        .bind(issue.id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    if n == 0 {
        return Err(ApiError::NotFound);
    }
    service::add_event(
        &mut tx,
        &issue,
        Some(auth.user.id),
        "unpinned",
        None,
        json!({}),
    )
    .await?;
    service::touch_and_sync(&mut tx, issue.id, SyncAction::Update).await?;
    tx.emit(Event::IssueUnpinned {
        repo_id: issue.repo_id,
        issue_id: issue.id,
        actor_id: auth.user.id,
    });
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
