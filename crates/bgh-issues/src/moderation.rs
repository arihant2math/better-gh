//! Comment moderation (P42): hide ("minimize") comments, edit history of
//! bodies and comments, and issue deletion. Web-client endpoints under
//! `/_bgh` (GitHub exposes these only through GraphQL, whose mutations call
//! the handlers here).
//!
//! One handler covers every comment kind (issue comments, reviews, review
//! comments, commit comments): the rows live in core tables and the logic
//! is `bgh_core::moderation`.

use axum::extract::State;
use axum::http::StatusCode;
use bgh_core::audit;
use bgh_core::moderation::{self, ContentKind};
use bgh_core::perms::RepoAccess;
use bgh_core::prelude::*;
use bgh_core::sync;
use serde::{Deserialize, Serialize};

use crate::issues;
use crate::json::{self, BodyFormat};
use crate::service;

fn kind_of(s: &str) -> ApiResult<ContentKind> {
    ContentKind::parse(s).ok_or(ApiError::NotFound)
}

fn audit_target(access: &RepoAccess) -> audit::Target {
    audit::Target::Repo {
        id: access.repo.id,
        org_id: access.owner.is_org().then_some(access.owner.id),
    }
}

/// Load a target the caller can see: 404 when missing, not in the
/// repository, or part of someone else's pending review.
async fn visible_target(
    state: &AppState,
    access: &RepoAccess,
    auth: Option<&AuthContext>,
    kind: ContentKind,
    id: i64,
) -> ApiResult<moderation::Target> {
    let t = moderation::find_target(&state.db, kind, access.repo.id, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if t.pending
        && t.author_id
            .is_none_or(|a| Some(a) != auth.map(|a| a.user.id))
    {
        return Err(ApiError::NotFound);
    }
    if kind == ContentKind::Issue || kind == ContentKind::Comment {
        // Issue (or the comment's issue) of a repository with issues off.
        let issue_id = t.parent_id.unwrap_or(t.id);
        let is_pr: bool = sqlx::query_scalar("SELECT is_pull_request FROM issues WHERE id = $1")
            .bind(issue_id)
            .fetch_one(&state.db)
            .await?;
        if !is_pr {
            issues::require_issues_enabled(access)?;
        }
    }
    Ok(t)
}

#[derive(Debug, Default, Deserialize)]
pub struct MinimizeBody {
    /// `spam`, `abuse`, `off-topic`, `outdated`, `duplicate`, `resolved`
    /// (or the GraphQL classifier: `OFF_TOPIC`, ...).
    pub reason: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MinimizedState {
    pub id: i64,
    pub minimized_reason: Option<String>,
}

/// `PUT /_bgh/repos/{owner}/{repo}/minimized/{kind}/{id}` — hide a comment.
pub async fn minimize(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, kind, id)): Path<(String, String, String, i64)>,
    Json(body): Json<MinimizeBody>,
) -> ApiResult<Json<MinimizedState>> {
    let reason = body
        .reason
        .as_deref()
        .ok_or_else(|| ApiError::invalid_field(FieldError::missing_field("Comment", "reason")))
        .and_then(|r| {
            moderation::parse_reason(r)
                .ok_or_else(|| ApiError::invalid_field(FieldError::invalid("Comment", "reason")))
        })?;
    set_minimized(&state, &auth, &owner, &repo, &kind, id, Some(reason)).await
}

/// `DELETE /_bgh/repos/{owner}/{repo}/minimized/{kind}/{id}` — unhide.
pub async fn unminimize(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, kind, id)): Path<(String, String, String, i64)>,
) -> ApiResult<Json<MinimizedState>> {
    set_minimized(&state, &auth, &owner, &repo, &kind, id, None).await
}

async fn set_minimized(
    state: &AppState,
    auth: &AuthContext,
    owner: &str,
    repo: &str,
    kind: &str,
    id: i64,
    reason: Option<&'static str>,
) -> ApiResult<Json<MinimizedState>> {
    let kind = kind_of(kind)?;
    if !kind.minimizable() {
        return Err(ApiError::NotFound);
    }
    let access = RepoAccess::load(state, Some(auth), owner, repo).await?;
    let target = visible_target(state, &access, Some(auth), kind, id).await?;
    access.require(Permission::Triage)?;
    access.require_not_archived()?;
    if target.pending {
        return Err(ApiError::unprocessable(
            "Comments of a pending review can't be minimized.",
        ));
    }
    let mut tx = Tx::begin(state).await?;
    moderation::set_minimized(&mut tx, kind, id, reason, auth.user.id).await?;
    let synced = match kind {
        ContentKind::Comment => Some(SyncModel::Comment),
        ContentKind::Review => Some(SyncModel::Review),
        ContentKind::ReviewComment => Some(SyncModel::ReviewComment),
        ContentKind::CommitComment | ContentKind::Issue => None,
    };
    if let Some(model) = synced {
        tx.sync_model(model, id, SyncAction::Update).await?;
    }
    audit::log(
        &mut *tx,
        Some(&auth.user),
        if reason.is_some() {
            "comment.minimize"
        } else {
            "comment.unminimize"
        },
        audit_target(&access),
        serde_json::json!({ "kind": kind.as_str(), "id": id, "reason": reason }),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(MinimizedState {
        id,
        minimized_reason: reason.map(str::to_string),
    }))
}

#[derive(Debug, Default, Deserialize)]
pub struct IdsQuery {
    pub ids: Option<String>,
}

/// `GET /_bgh/repos/{owner}/{repo}/minimized/{kind}?ids=1,2` — minimized
/// states of unsynced comments (commit comments).
pub async fn minimized_list(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, kind)): Path<(String, String, String)>,
    Query(q): Query<IdsQuery>,
) -> ApiResult<Json<Vec<MinimizedState>>> {
    let kind = kind_of(&kind)?;
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let ids: Vec<i64> = q
        .ids
        .as_deref()
        .unwrap_or("")
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .take(500)
        .collect();
    let rows = if kind.minimizable() && !ids.is_empty() {
        sqlx::query_as::<_, (i64, String)>(&format!(
            "SELECT id, minimized_reason FROM {} WHERE id = ANY($1) AND repo_id = $2
                AND minimized_reason IS NOT NULL ORDER BY id",
            kind.table()
        ))
        .bind(&ids)
        .bind(access.repo.id)
        .fetch_all(&state.db)
        .await?
    } else {
        vec![]
    };
    Ok(Json(
        rows.into_iter()
            .map(|(id, r)| MinimizedState {
                id,
                minimized_reason: Some(r),
            })
            .collect(),
    ))
}

/// `GET /_bgh/repos/{owner}/{repo}/edits/{kind}/{id}` — edit history,
/// newest first.
pub async fn edits(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, kind, id)): Path<(String, String, String, i64)>,
) -> ApiResult<Json<Vec<moderation::ContentEditJson>>> {
    let kind = kind_of(&kind)?;
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    visible_target(&state, &access, auth.as_ref(), kind, id).await?;
    let rows = moderation::edits_for(&state.db, kind, &[id]).await?;
    Ok(Json(moderation::render_edits(&state, rows).await?))
}

/// `DELETE /_bgh/repos/{owner}/{repo}/edits/{kind}/{id}/{edit_id}` —
/// delete a revision's text (the content's author or a repo admin).
/// `edit_id` 0 deletes the original text.
pub async fn delete_edit(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, kind, id, edit_id)): Path<(String, String, String, i64, i64)>,
) -> ApiResult<StatusCode> {
    let kind = kind_of(&kind)?;
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    let target = visible_target(&state, &access, Some(&auth), kind, id).await?;
    access.require_not_archived()?;
    if target.author_id != Some(auth.user.id) && access.permission < Permission::Admin {
        return Err(ApiError::forbidden(
            "Only the author or a repository admin can delete edit history.",
        ));
    }
    let mut tx = Tx::begin(&state).await?;
    if edit_id == 0 {
        if !moderation::delete_original(&mut tx, kind, id).await? {
            return Err(ApiError::NotFound);
        }
    } else {
        let edit: moderation::ContentEdit = sqlx::query_as(&format!(
            "SELECT {} FROM user_content_edits
              WHERE id = $1 AND target_type = $2 AND target_id = $3 AND repo_id = $4",
            moderation::ContentEdit::COLUMNS
        ))
        .bind(edit_id)
        .bind(kind.as_str())
        .bind(id)
        .bind(access.repo.id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(ApiError::NotFound)?;
        moderation::delete_revision(&mut tx, &edit, auth.user.id).await?;
    }
    audit::log(
        &mut *tx,
        Some(&auth.user),
        "comment.delete_edit",
        audit_target(&access),
        serde_json::json!({ "kind": kind.as_str(), "id": id, "edit_id": edit_id }),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /_bgh/repos/{owner}/{repo}/issues/{issue_number}` (and GraphQL
/// `deleteIssue`) → 204. Repository admins only; pull requests can't be
/// deleted. The number stays reserved and answers 410 afterwards.
pub async fn delete_issue(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, number)): Path<(String, String, i64)>,
) -> ApiResult<StatusCode> {
    let (access, issue) = issues::load(&state, Some(&auth), &owner, &repo, number).await?;
    access.require(Permission::Admin)?;
    access.require_not_archived()?;
    if issue.is_pull_request {
        return Err(ApiError::unprocessable("Pull requests cannot be deleted."));
    }
    // Webhook `issues.deleted` carries the issue as it was.
    let snapshot = serde_json::to_value(
        json::issue(
            &state,
            BodyFormat::default(),
            &issue,
            &json::repo_map(&access),
        )
        .await?,
    )?;
    let mut tx = Tx::begin(&state).await?;
    let issue = service::lock_issue(&mut tx, issue.id).await?;
    // Rows whose synced shape shows this issue: sub-issue parent/children
    // and linked pull requests.
    let related: Vec<i64> = sqlx::query_scalar(
        "SELECT parent_id FROM sub_issues WHERE child_id = $1
         UNION SELECT child_id FROM sub_issues WHERE parent_id = $1
         UNION SELECT pull_id FROM issue_pr_links WHERE issue_id = $1",
    )
    .bind(issue.id)
    .fetch_all(&mut *tx)
    .await?;
    let notifications: Vec<(i64, i64)> = sqlx::query_as(
        "DELETE FROM notifications WHERE subject_type = 'Issue' AND subject_id = $1
         RETURNING id, user_id",
    )
    .bind(issue.id)
    .fetch_all(&mut *tx)
    .await?;
    sqlx::query(
        "DELETE FROM thread_subscriptions WHERE subject_type = 'Issue' AND subject_id = $1",
    )
    .bind(issue.id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "DELETE FROM reactions
          WHERE (subject_type = 'issue' AND subject_id = $1)
             OR (subject_type = 'issue_comment'
                 AND subject_id IN (SELECT id FROM comments WHERE issue_id = $1))",
    )
    .bind(issue.id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO deleted_issues (repo_id, number, deleted_by_id) VALUES ($1, $2, $3)
         ON CONFLICT (repo_id, number) DO UPDATE
            SET deleted_by_id = EXCLUDED.deleted_by_id, deleted_at = now()",
    )
    .bind(issue.repo_id)
    .bind(issue.number)
    .bind(auth.user.id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM issues WHERE id = $1")
        .bind(issue.id)
        .execute(&mut *tx)
        .await?;
    if issue.state == "open" {
        service::adjust_open_count(&mut tx, issue.repo_id, -1).await?;
    }
    service::refresh_milestones(&mut tx, &issue.milestone_id.into_iter().collect::<Vec<_>>())
        .await?;
    tx.sync_delete(&sync::repo_scope(issue.repo_id), SyncModel::Issue, issue.id)
        .await?;
    for id in related {
        tx.sync_issue(id, SyncAction::Update, false).await?;
    }
    for (id, user_id) in notifications {
        tx.sync_delete(&sync::user_scope(user_id), SyncModel::Notification, id)
            .await?;
    }
    service::sync_repo_open_issues(&mut tx, issue.repo_id).await?;
    audit::log(
        &mut *tx,
        Some(&auth.user),
        "issue.destroy",
        audit_target(&access),
        serde_json::json!({ "issue_id": issue.id, "number": issue.number, "title": issue.title }),
    )
    .await?;
    tx.emit(Event::IssueDeleted {
        repo_id: issue.repo_id,
        issue_id: issue.id,
        actor_id: auth.user.id,
        issue: snapshot,
    });
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// 410 for numbers of deleted issues (callers map their 404s through this).
pub async fn gone_if_deleted(
    db: impl sqlx::PgExecutor<'_>,
    repo_id: i64,
    number: i64,
) -> ApiResult<()> {
    let deleted: Option<i32> =
        sqlx::query_scalar("SELECT 1 FROM deleted_issues WHERE repo_id = $1 AND number = $2")
            .bind(repo_id)
            .bind(number)
            .fetch_optional(db)
            .await?;
    match deleted {
        Some(_) => Err(ApiError::Gone("This issue was deleted".into())),
        None => Ok(()),
    }
}
