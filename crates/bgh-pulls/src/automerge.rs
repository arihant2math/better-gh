//! Auto-merge: enable / disable (`/_bgh` endpoints and service functions
//! for GraphQL's `enablePullRequestAutoMerge`), and the merge attempt run
//! after every mergeability refresh (triggered by pushes, statuses, check
//! runs and reviews).

use axum::extract::State;
use axum::http::StatusCode;
use bgh_core::prelude::*;
use serde::Deserialize;
use serde_json::json;

use crate::json::{self, PullRequest};
use crate::merge::{MergeMethod, MergeRequest, perform_merge};
use crate::model::{self, Pull};
use crate::pulls::load_pull;
use crate::{protection, timeline};

#[derive(Debug, Deserialize, Default)]
pub struct EnableBody {
    pub merge_method: Option<String>,
    pub commit_title: Option<String>,
    pub commit_message: Option<String>,
}

/// Enable auto-merge on `pull` for `actor` (needs write access).
pub async fn enable(
    state: &AppState,
    access: &RepoAccess,
    pull: &Pull,
    actor: &db::User,
    body: &EnableBody,
) -> ApiResult<()> {
    access.require(Permission::Write)?;
    if !access.repo.allow_auto_merge {
        return Err(ApiError::unprocessable(
            "Auto-merge is not allowed for this repository",
        ));
    }
    if !pull.is_open() || pull.pr.merged {
        return Err(ApiError::unprocessable("Pull request is closed"));
    }
    let method = match body.merge_method.as_deref() {
        None => MergeMethod::Merge,
        Some(m) => MergeMethod::parse(m).ok_or_else(|| {
            ApiError::invalid_field(FieldError::invalid("PullRequest", "merge_method"))
        })?,
    };
    let mut tx = Tx::begin(state).await?;
    sqlx::query("UPDATE pull_requests SET auto_merge = $2 WHERE issue_id = $1")
        .bind(pull.id())
        .bind(json!({
            "enabled_by_id": actor.id,
            "merge_method": method.as_str(),
            "commit_title": body.commit_title,
            "commit_message": body.commit_message,
        }))
        .execute(&mut *tx)
        .await?;
    timeline::record(
        &mut tx,
        access.repo.id,
        pull.id(),
        Some(actor.id),
        "auto_merge_enabled",
        None,
        json!({"merge_method": method.as_str()}),
    )
    .await?;
    json::sync_pull(&mut tx, &access.scope(), pull.id()).await?;
    tx.emit(Event::PullRequestAutoMergeEnabled {
        repo_id: access.repo.id,
        pull_id: pull.id(),
        actor_id: actor.id,
    });
    // Evaluate right away (merges if requirements are already met).
    tx.enqueue(&crate::jobs::Refresh {
        pull_id: pull.id(),
        codeowners: false,
    })
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Disable auto-merge (`actor_id` None = system, e.g. after a failure).
pub async fn disable(
    state: &AppState,
    repo_id: i64,
    pull_id: i64,
    actor_id: Option<i64>,
    reason: &str,
) -> ApiResult<bool> {
    let mut tx = Tx::begin(state).await?;
    let n = sqlx::query(
        "UPDATE pull_requests SET auto_merge = NULL WHERE issue_id = $1 AND auto_merge IS NOT NULL",
    )
    .bind(pull_id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if n == 0 {
        return Ok(false);
    }
    timeline::record(
        &mut tx,
        repo_id,
        pull_id,
        actor_id,
        "auto_merge_disabled",
        None,
        json!({"reason": reason}),
    )
    .await?;
    json::sync_pull(&mut tx, &bgh_core::sync::repo_scope(repo_id), pull_id).await?;
    tx.emit(Event::PullRequestAutoMergeDisabled {
        repo_id,
        pull_id,
        actor_id,
    });
    tx.commit().await?;
    Ok(true)
}

/// Merge the PR if auto-merge is enabled and every requirement passes.
pub async fn try_merge(state: &AppState, pull_id: i64) -> ApiResult<()> {
    let Some(pull) = model::find_by_id(&state.db, pull_id).await? else {
        return Ok(());
    };
    let Some(cfg) = pull.pr.auto_merge.clone() else {
        return Ok(());
    };
    if !pull.is_open() || pull.pr.merged || pull.pr.draft || pull.pr.mergeable != Some(true) {
        return Ok(());
    }
    let Some(repo) = db::Repository::find(&state.db, pull.pr.repo_id).await? else {
        return Ok(());
    };
    let rules = protection::rules_for(&state.db, repo.id, &pull.pr.base_ref).await?;
    let ev = protection::evaluate(state, &repo, &pull, &rules).await?;
    // Auto-merge waits for every requirement and for pending checks.
    let pending = protection::check_outcomes(&state.db, &[repo.id], &pull.pr.head_sha)
        .await?
        .values()
        .any(|o| *o == protection::CheckOutcome::Pending);
    if !ev.blockers.is_empty() || pending {
        return Ok(());
    }
    let Some(user_id) = cfg.get("enabled_by_id").and_then(|v| v.as_i64()) else {
        return Ok(());
    };
    let Some(user) = db::User::find(&state.db, user_id).await? else {
        disable(state, repo.id, pull.id(), None, "user_deleted").await?;
        return Ok(());
    };
    let Some(owner) = db::User::find(&state.db, repo.owner_id).await? else {
        return Ok(());
    };
    let perm = crate::pulls::member_permission(state, &repo, user.id).await?;
    if perm < Permission::Write {
        disable(state, repo.id, pull.id(), None, "permission_revoked").await?;
        return Ok(());
    }
    let req = MergeRequest {
        method: cfg
            .get("merge_method")
            .and_then(|v| v.as_str())
            .and_then(MergeMethod::parse)
            .unwrap_or(MergeMethod::Merge),
        commit_title: cfg
            .get("commit_title")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        commit_message: cfg
            .get("commit_message")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        sha: Some(pull.pr.head_sha.clone()),
    };
    match perform_merge(
        state,
        &repo,
        &owner,
        &pull,
        &user,
        perm.min(Permission::Write),
        &req,
    )
    .await
    {
        Ok(_) => Ok(()),
        // Moved meanwhile: the next refresh retries.
        Err(ApiError::Conflict(_)) => Ok(()),
        Err(ApiError::Status(s, msg)) if s == StatusCode::METHOD_NOT_ALLOWED => {
            tracing::info!(pull = pull.id(), %msg, "auto-merge not possible");
            if msg.contains("not allowed") || msg.contains("can't be rebased") {
                disable(state, repo.id, pull.id(), None, "merge_method_failed").await?;
            }
            Ok(())
        }
        Err(e) => Err(e),
    }
}

// ----- web endpoints ------------------------------------------------------

pub async fn put(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    Json(body): Json<EnableBody>,
) -> ApiResult<axum::Json<PullRequest>> {
    let (access, pull) = load_pull(&state, Some(&auth), &owner, &repo, number).await?;
    enable(&state, &access, &pull, &auth.user, &body).await?;
    let pull = model::load(&state, access.repo.id, number).await?;
    Ok(axum::Json(
        json::render_full(&state, Some(&auth), &access, &pull).await?,
    ))
}

pub async fn delete(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, number)): Path<(String, String, i64)>,
) -> ApiResult<axum::Json<PullRequest>> {
    let (access, pull) = load_pull(&state, Some(&auth), &owner, &repo, number).await?;
    if pull.issue.author_id != Some(auth.user.id) {
        access.require(Permission::Write)?;
    }
    disable(
        &state,
        access.repo.id,
        pull.id(),
        Some(auth.user.id),
        "disabled",
    )
    .await?;
    let pull = model::load(&state, access.repo.id, number).await?;
    Ok(axum::Json(
        json::render_full(&state, Some(&auth), &access, &pull).await?,
    ))
}
