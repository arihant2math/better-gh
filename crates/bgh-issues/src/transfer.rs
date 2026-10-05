//! Issue transfer between repositories of the same owner.

use axum::extract::State;
use axum::http::StatusCode;
use bgh_core::perms::RepoAccess;
use bgh_core::prelude::*;
use bgh_core::sync;
use serde::Deserialize;
use serde_json::json;

use crate::json::{self, BodyFormat};
use crate::{issues, service};

#[derive(Debug, Default, Deserialize)]
pub struct TransferBody {
    /// Owner of the target repository (defaults to the current owner).
    pub new_owner: Option<String>,
    pub new_name: Option<String>,
}

/// `POST /repos/{owner}/{repo}/issues/{issue_number}/transfer` → 201 with
/// the issue at its new location. The old URL answers 301 afterwards.
///
/// Requires write access to both repositories; the target must belong to
/// the same owner, have issues enabled, and a private issue can't move to
/// a public repository. Comments, events, reactions and assignees who can
/// be assigned in the target move along; labels and the milestone are kept
/// when the target has ones with the same name/title.
pub async fn transfer(
    State(state): State<AppState>,
    auth: RequireUser,
    fmt: BodyFormat,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    Json(body): Json<TransferBody>,
) -> ApiResult<(StatusCode, Json<json::Issue>)> {
    let (source, issue) = issues::load(&state, Some(&auth), &owner, &repo, number).await?;
    source.require(Permission::Write)?;
    source.require_not_archived()?;
    if issue.is_pull_request {
        return Err(ApiError::unprocessable(
            "Pull requests cannot be transferred",
        ));
    }
    let new_name = body
        .new_name
        .as_deref()
        .filter(|n| !n.is_empty())
        .ok_or_else(|| ApiError::invalid_field(FieldError::missing_field("Issue", "new_name")))?;
    let new_owner = body.new_owner.as_deref().unwrap_or(&source.owner.login);
    let target = RepoAccess::load(&state, Some(&auth), new_owner, new_name).await?;
    target.require(Permission::Write)?;
    target.require_not_archived()?;
    issues::require_issues_enabled(&target)?;
    if target.repo.id == source.repo.id {
        return Err(ApiError::unprocessable(
            "Issue is already in the target repository",
        ));
    }
    if target.repo.owner_id != source.repo.owner_id {
        return Err(ApiError::unprocessable(
            "Issues can only be transferred between repositories of the same owner",
        ));
    }
    if source.repo.is_private() && !target.repo.is_private() {
        return Err(ApiError::unprocessable(
            "Issues cannot be transferred from a private to a public repository",
        ));
    }
    let (old_repo, new_repo) = (source.repo.id, target.repo.id);
    let mut tx = Tx::begin(&state).await?;
    let issue = service::lock_issue(&mut tx, issue.id).await?;
    let new_number = service::allocate_number(&mut tx, new_repo).await?;
    if issue.state == "open" {
        service::adjust_open_count(&mut tx, old_repo, -1).await?;
    } else {
        // allocate_number counted an open item.
        service::adjust_open_count(&mut tx, new_repo, -1).await?;
    }
    sqlx::query(
        "INSERT INTO issue_transfers (old_repo_id, old_number, issue_id) VALUES ($1, $2, $3)
         ON CONFLICT (old_repo_id, old_number) DO UPDATE SET issue_id = EXCLUDED.issue_id",
    )
    .bind(old_repo)
    .bind(issue.number)
    .bind(issue.id)
    .execute(&mut *tx)
    .await?;
    // Milestone: keep a same-titled one in the target.
    let new_milestone: Option<i64> =
        match issue.milestone_id {
            Some(m) => sqlx::query_scalar(
                "SELECT t.id FROM milestones t JOIN milestones s ON lower(s.title) = lower(t.title)
                  WHERE s.id = $1 AND t.repo_id = $2",
            )
            .bind(m)
            .bind(new_repo)
            .fetch_optional(&mut *tx)
            .await?,
            None => None,
        };
    sqlx::query("UPDATE issues SET repo_id = $2, number = $3, milestone_id = $4 WHERE id = $1")
        .bind(issue.id)
        .bind(new_repo)
        .bind(new_number)
        .bind(new_milestone)
        .execute(&mut *tx)
        .await?;
    // Labels: map by name, drop the rest.
    sqlx::query(
        "UPDATE issue_labels il SET label_id = t.id
           FROM labels s, labels t
          WHERE il.issue_id = $1 AND s.id = il.label_id AND t.repo_id = $2
            AND lower(t.name) = lower(s.name)",
    )
    .bind(issue.id)
    .bind(new_repo)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "DELETE FROM issue_labels il USING labels l
          WHERE il.issue_id = $1 AND l.id = il.label_id AND l.repo_id <> $2",
    )
    .bind(issue.id)
    .bind(new_repo)
    .execute(&mut *tx)
    .await?;
    // Assignees: keep those assignable in the target.
    let assignees: Vec<db::User> = sqlx::query_as(&format!(
        "SELECT {} FROM users u JOIN issue_assignees a ON a.user_id = u.id WHERE a.issue_id = $1",
        db::prefixed("u", db::User::COLUMNS)
    ))
    .bind(issue.id)
    .fetch_all(&mut *tx)
    .await?;
    for u in assignees {
        if !service::is_assignable(&state, &target.repo, &u).await? {
            sqlx::query("DELETE FROM issue_assignees WHERE issue_id = $1 AND user_id = $2")
                .bind(issue.id)
                .bind(u.id)
                .execute(&mut *tx)
                .await?;
        }
    }
    let comments: Vec<db::Comment> = sqlx::query_as(&format!(
        "UPDATE comments SET repo_id = $2 WHERE issue_id = $1 RETURNING {}",
        db::Comment::COLUMNS
    ))
    .bind(issue.id)
    .bind(new_repo)
    .fetch_all(&mut *tx)
    .await?;
    sqlx::query("UPDATE issue_events SET repo_id = $2 WHERE issue_id = $1")
        .bind(issue.id)
        .bind(new_repo)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE thread_subscriptions SET repo_id = $2 WHERE subject_id = $1 AND subject_type = 'Issue'")
        .bind(issue.id)
        .bind(new_repo)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM pinned_issues WHERE issue_id = $1")
        .bind(issue.id)
        .execute(&mut *tx)
        .await?;
    let milestones: Vec<i64> = issue
        .milestone_id
        .into_iter()
        .chain(new_milestone)
        .collect();
    service::refresh_milestones(&mut tx, &milestones).await?;

    // Sync: gone from the old scope, inserted into the new one.
    let old_scope = sync::repo_scope(old_repo);
    tx.sync_delete(&old_scope, SyncModel::Issue, issue.id)
        .await?;
    for c in &comments {
        tx.sync_delete(&old_scope, SyncModel::Comment, c.id).await?;
    }
    let comment_ids: Vec<i64> = comments.iter().map(|c| c.id).collect();
    tx.sync_models(SyncModel::Comment, &comment_ids, SyncAction::Insert)
        .await?;
    let moved = service::issue_by_id(&mut *tx, issue.id).await?;
    service::add_event(
        &mut tx,
        &moved,
        Some(auth.user.id),
        "transferred",
        None,
        json!({ "from_repository": source.full_name() }),
    )
    .await?;
    let moved = service::touch_and_sync(&mut tx, issue.id, SyncAction::Insert).await?;
    service::sync_repo_open_issues(&mut tx, old_repo).await?;
    service::sync_repo_open_issues(&mut tx, new_repo).await?;
    tx.emit(Event::IssueTransferred {
        repo_id: new_repo,
        issue_id: issue.id,
        old_repo_id: old_repo,
        old_number: issue.number,
        actor_id: auth.user.id,
    });
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        Json(json::issue(&state, fmt, &moved, &json::repo_map(&target)).await?),
    ))
}
