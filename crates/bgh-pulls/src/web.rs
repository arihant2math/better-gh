//! Private web-client endpoints (`/_bgh/...`) for features without a
//! GitHub REST equivalent (GraphQL-only on GitHub): review threads
//! (list / resolve / unresolve), draft ⇄ ready for review, auto-merge, and
//! a detailed merge-requirements view for the merge box. The underlying
//! service functions are public for bgh-graphql.

use axum::extract::State;
use bgh_core::models::api::SimpleUser;
use bgh_core::node_id::{self, NodeType};
use bgh_core::prelude::*;
use serde::Serialize;
use serde_json::json;

use crate::comments::{self, ReviewCommentJson};
use crate::jobs::Refresh;
use crate::json::{self, PullRequest};
use crate::model::{Pull, ReviewComment};
use crate::pulls::load_pull;
use crate::{protection, timeline};

#[derive(Debug, Serialize)]
pub struct ThreadJson {
    pub id: i64,
    pub node_id: String,
    pub path: String,
    pub subject_type: String,
    pub line: Option<i32>,
    pub original_line: Option<i32>,
    pub start_line: Option<i32>,
    pub original_start_line: Option<i32>,
    pub side: Option<String>,
    pub start_side: Option<String>,
    pub is_resolved: bool,
    pub is_outdated: bool,
    pub resolved_by: Option<SimpleUser>,
    pub comments: Vec<ReviewCommentJson>,
}

pub async fn list_threads(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, number)): Path<(String, String, i64)>,
) -> ApiResult<axum::Json<Vec<ThreadJson>>> {
    let (access, pull) = load_pull(&state, auth.as_ref(), &owner, &repo, number).await?;
    let rows: Vec<ReviewComment> = sqlx::query_as(&format!(
        "SELECT {} FROM pr_review_comments c
          WHERE c.pull_id = $1 AND (c.review_id IS NULL OR NOT EXISTS (
                SELECT 1 FROM pr_reviews r WHERE r.id = c.review_id AND r.state = 'PENDING'
                   AND r.user_id IS DISTINCT FROM $2))
          ORDER BY c.id",
        db::prefixed("c", ReviewComment::COLUMNS)
    ))
    .bind(pull.id())
    .bind(auth.user_id())
    .fetch_all(&state.db)
    .await?;
    let rendered = comments::render(&state, &access, &rows).await?;
    let resolvers =
        bgh_core::views::users_by_id(&state, rows.iter().map(|c| c.resolved_by_id)).await?;
    let mut threads: Vec<ThreadJson> = Vec::new();
    for (row, json) in rows.iter().zip(rendered) {
        if row.in_reply_to_id.is_none() {
            threads.push(ThreadJson {
                id: row.id,
                node_id: node_id::encode(NodeType::PullRequestReviewThread, row.id),
                path: row.path.clone(),
                subject_type: row.subject_type.clone(),
                line: row.line,
                original_line: row.original_line,
                start_line: row.start_line,
                original_start_line: row.original_start_line,
                side: row.side.clone(),
                start_side: row.start_side.clone(),
                is_resolved: row.resolved_at.is_some(),
                is_outdated: row.is_outdated(),
                resolved_by: row
                    .resolved_by_id
                    .and_then(|u| resolvers.get(&u))
                    .map(|u| SimpleUser::new(&state.urls, u)),
                comments: vec![json],
            });
        } else if let Some(t) = threads
            .iter_mut()
            .find(|t| Some(t.id) == row.in_reply_to_id)
        {
            t.comments.push(json);
        }
    }
    Ok(axum::Json(threads))
}

/// Resolve or unresolve the thread rooted at `comment_id` (PR author or
/// users with write access).
pub async fn set_resolved(
    state: &AppState,
    access: &RepoAccess,
    pull: &Pull,
    user: &db::User,
    comment_id: i64,
    resolved: bool,
) -> ApiResult<()> {
    if pull.issue.author_id != Some(user.id) {
        access.require(Permission::Write)?;
    }
    let root: ReviewComment = sqlx::query_as(&format!(
        "SELECT {} FROM pr_review_comments WHERE id = $1 AND pull_id = $2",
        ReviewComment::COLUMNS
    ))
    .bind(comment_id)
    .bind(pull.id())
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)?;
    let root_id = root.thread_id();
    let mut tx = Tx::begin(state).await?;
    let row: ReviewComment = sqlx::query_as(&format!(
        "UPDATE pr_review_comments
            SET resolved_at = CASE WHEN $2 THEN coalesce(resolved_at, now()) END,
                resolved_by_id = CASE WHEN $2 THEN coalesce(resolved_by_id, $3) END
          WHERE id = $1 RETURNING {}",
        ReviewComment::COLUMNS
    ))
    .bind(root_id)
    .bind(resolved)
    .bind(user.id)
    .fetch_one(&mut *tx)
    .await?;
    tx.sync(
        &access.scope(),
        "review_comment",
        row.id,
        SyncAction::Update,
        &comments::sync_json(&row),
    )
    .await?;
    tx.enqueue(&Refresh {
        pull_id: pull.id(),
        codeowners: false,
    })
    .await?;
    tx.emit(if resolved {
        Event::PullRequestReviewThreadResolved {
            repo_id: access.repo.id,
            pull_id: pull.id(),
            comment_id: root_id,
            actor_id: user.id,
        }
    } else {
        Event::PullRequestReviewThreadUnresolved {
            repo_id: access.repo.id,
            pull_id: pull.id(),
            comment_id: root_id,
            actor_id: user.id,
        }
    });
    tx.commit().await?;
    Ok(())
}

pub async fn resolve_thread(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, number, id)): Path<(String, String, i64, i64)>,
) -> ApiResult<axum::Json<serde_json::Value>> {
    let (access, pull) = load_pull(&state, Some(&auth), &owner, &repo, number).await?;
    set_resolved(&state, &access, &pull, &auth.user, id, true).await?;
    Ok(axum::Json(json!({"id": id, "is_resolved": true})))
}

pub async fn unresolve_thread(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, number, id)): Path<(String, String, i64, i64)>,
) -> ApiResult<axum::Json<serde_json::Value>> {
    let (access, pull) = load_pull(&state, Some(&auth), &owner, &repo, number).await?;
    set_resolved(&state, &access, &pull, &auth.user, id, false).await?;
    Ok(axum::Json(json!({"id": id, "is_resolved": false})))
}

/// Mark ready for review (`draft = false`) or convert to draft.
pub async fn set_draft(
    state: &AppState,
    access: &RepoAccess,
    pull: &Pull,
    user: &db::User,
    draft: bool,
) -> ApiResult<()> {
    if pull.issue.author_id != Some(user.id) {
        access.require(Permission::Write)?;
    }
    if !pull.is_open() {
        return Err(ApiError::unprocessable("Pull request is closed"));
    }
    if pull.pr.draft == draft {
        return Ok(());
    }
    let mut tx = Tx::begin(state).await?;
    sqlx::query(
        "UPDATE pull_requests SET draft = $2, mergeable_state = CASE WHEN $2 THEN 'draft' ELSE 'unknown' END
          WHERE issue_id = $1",
    )
    .bind(pull.id())
    .bind(draft)
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE issues SET updated_at = now() WHERE id = $1")
        .bind(pull.id())
        .execute(&mut *tx)
        .await?;
    timeline::record(
        &mut tx,
        access.repo.id,
        pull.id(),
        Some(user.id),
        if draft {
            "convert_to_draft"
        } else {
            "ready_for_review"
        },
        None,
        json!({}),
    )
    .await?;
    json::sync_pull(&mut tx, &access.scope(), pull.id()).await?;
    tx.enqueue(&Refresh {
        pull_id: pull.id(),
        codeowners: !draft,
    })
    .await?;
    tx.emit(if draft {
        Event::PullRequestConvertedToDraft {
            repo_id: access.repo.id,
            pull_id: pull.id(),
            actor_id: user.id,
        }
    } else {
        Event::PullRequestReadyForReview {
            repo_id: access.repo.id,
            pull_id: pull.id(),
            actor_id: user.id,
        }
    });
    tx.commit().await?;
    Ok(())
}

async fn draft_handler(
    state: AppState,
    auth: RequireUser,
    owner: String,
    repo: String,
    number: i64,
    draft: bool,
) -> ApiResult<axum::Json<PullRequest>> {
    let (access, pull) = load_pull(&state, Some(&auth), &owner, &repo, number).await?;
    set_draft(&state, &access, &pull, &auth.user, draft).await?;
    let pull = crate::model::load(&state, access.repo.id, number).await?;
    Ok(axum::Json(
        json::render_full(&state, Some(&auth), &access, &pull).await?,
    ))
}

pub async fn ready_for_review(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, number)): Path<(String, String, i64)>,
) -> ApiResult<axum::Json<PullRequest>> {
    draft_handler(state, auth, owner, repo, number, false).await
}

pub async fn convert_to_draft(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, number)): Path<(String, String, i64)>,
) -> ApiResult<axum::Json<PullRequest>> {
    draft_handler(state, auth, owner, repo, number, true).await
}

#[derive(Debug, Serialize)]
pub struct Requirements {
    pub mergeable: Option<bool>,
    pub rebaseable: Option<bool>,
    pub mergeable_state: String,
    pub protected: bool,
    pub blockers: Vec<String>,
    pub approvals: i64,
    pub required_approvals: i64,
    pub changes_requested: bool,
    pub behind: bool,
    pub unstable: bool,
    pub required_checks: Vec<String>,
    pub linear_history: bool,
    pub allowed_merge_methods: Vec<&'static str>,
    pub can_bypass: bool,
}

pub async fn requirements(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, number)): Path<(String, String, i64)>,
) -> ApiResult<axum::Json<Requirements>> {
    let (access, pull) = load_pull(&state, auth.as_ref(), &owner, &repo, number).await?;
    let rules = protection::rules_for(&state.db, access.repo.id, &pull.pr.base_ref).await?;
    let ev = protection::evaluate(&state, &access.repo, &pull, &rules).await?;
    let mut methods = Vec::new();
    if access.repo.allow_merge_commit && !rules.linear_history {
        methods.push("merge");
    }
    if access.repo.allow_squash_merge {
        methods.push("squash");
    }
    if access.repo.allow_rebase_merge {
        methods.push("rebase");
    }
    Ok(axum::Json(Requirements {
        mergeable: pull.pr.mergeable,
        rebaseable: pull.pr.rebaseable,
        mergeable_state: pull.pr.mergeable_state.clone(),
        protected: rules.protected,
        blockers: ev.blockers.clone(),
        approvals: ev.approvals,
        required_approvals: rules
            .reviews
            .as_ref()
            .map(|r| r.required_approving_review_count)
            .unwrap_or(0),
        changes_requested: ev.changes_requested,
        behind: ev.behind,
        unstable: ev.unstable,
        required_checks: rules
            .checks
            .as_ref()
            .map(|c| c.contexts.clone())
            .unwrap_or_default(),
        linear_history: rules.linear_history,
        allowed_merge_methods: methods,
        can_bypass: access.permission >= Permission::Admin && !rules.enforce_admins,
    }))
}
