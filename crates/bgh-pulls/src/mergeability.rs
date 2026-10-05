//! Mergeability computation (background): conflict detection with
//! `git merge-tree`, the test merge commit (`refs/pull/{n}/merge`, exposed
//! as `merge_commit_sha` while open), rebaseability, and `mergeable_state`
//! from branch protection. Ends by attempting auto-merge.

use bgh_core::prelude::*;
use bgh_git::merge::RebaseResult;

use crate::git;
use crate::model::{self, Pull};
use crate::protection;

/// Upper bound of commits replayed to decide `rebaseable`.
const REBASE_CHECK_LIMIT: i64 = 100;

pub struct Computed {
    pub mergeable: bool,
    pub rebaseable: bool,
    pub test_merge: Option<String>,
}

/// Compute mergeability of `pull` (current `base_sha` / `head_sha`).
pub async fn compute(state: &AppState, pull: &Pull) -> ApiResult<Computed> {
    let store = git::store(state);
    let repo_id = pull.pr.repo_id;
    let test_merge = bgh_git::merge::test_merge(
        &store,
        repo_id,
        &pull.merge_ref_name(),
        &pull.pr.base_sha,
        &pull.pr.head_sha,
        &git::site_committer(state),
    )
    .await?;
    let mergeable = test_merge.is_some();
    let rebaseable = if !mergeable || pull.pr.commits > REBASE_CHECK_LIMIT {
        false
    } else {
        let base = pull
            .pr
            .merge_base_sha
            .clone()
            .unwrap_or_else(|| pull.pr.base_sha.clone());
        matches!(
            bgh_git::merge::rebase(
                &store,
                repo_id,
                &pull.pr.base_sha,
                &base,
                &pull.pr.head_sha,
                &git::site_committer(state),
            )
            .await?,
            RebaseResult::Done { .. }
        )
    };
    Ok(Computed {
        mergeable,
        rebaseable,
        test_merge,
    })
}

/// Recompute and store mergeability for an open PR, optionally requesting
/// code owner reviews, then try auto-merge.
pub async fn refresh(state: &AppState, pull_id: i64, codeowners: bool) -> ApiResult<()> {
    let Some(pull) = model::find_by_id(&state.db, pull_id).await? else {
        return Ok(());
    };
    if !pull.is_open() || pull.pr.merged {
        return Ok(());
    }
    let Some(repo) = db::Repository::find(&state.db, pull.pr.repo_id).await? else {
        return Ok(());
    };
    if codeowners && !pull.pr.draft {
        crate::codeowners::request_owners(state, &pull).await?;
    }
    let c = compute(state, &pull).await?;
    let rules = protection::rules_for(&state.db, repo.id, &pull.pr.base_ref).await?;
    let ev = protection::evaluate(state, &repo, &pull, &rules).await?;
    let ms = protection::mergeable_state(pull.pr.draft, Some(c.mergeable), &ev);

    let mut tx = Tx::begin(state).await?;
    // Only store if the PR didn't move meanwhile (a newer refresh follows).
    let updated = sqlx::query(
        "UPDATE pull_requests SET mergeable = $2, rebaseable = $3, mergeable_state = $4,
                merge_commit_sha = $5
          WHERE issue_id = $1 AND head_sha = $6 AND base_sha = $7 AND NOT merged",
    )
    .bind(pull.id())
    .bind(c.mergeable)
    .bind(c.rebaseable)
    .bind(ms)
    .bind(&c.test_merge)
    .bind(&pull.pr.head_sha)
    .bind(&pull.pr.base_sha)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if updated == 0 {
        return Ok(());
    }
    let changed = pull.pr.mergeable != Some(c.mergeable)
        || pull.pr.rebaseable != Some(c.rebaseable)
        || pull.pr.mergeable_state != ms
        || pull.pr.merge_commit_sha != c.test_merge;
    if changed {
        crate::json::sync_pull(&mut tx, &bgh_core::sync::repo_scope(repo.id), pull.id()).await?;
    }
    tx.commit().await?;

    if pull.pr.auto_merge.is_some() {
        crate::automerge::try_merge(state, pull.id()).await?;
    }
    Ok(())
}
