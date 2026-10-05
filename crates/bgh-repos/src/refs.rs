//! Ref updates made through the REST API (git refs, contents, merges,
//! branch rename, merge-upstream): branch protection, old-value checked
//! `update-ref`, and the same post-receive processing as `git push`.

use bgh_core::events::{RefUpdate, ZERO_SHA};
use bgh_core::prelude::*;

use crate::jobs::PostReceive;
use crate::protection::{self, Actor, RepoRules};

/// Strip the hook prefix from a rule violation for API error messages.
fn api_reason(reason: &str) -> String {
    reason
        .strip_prefix("protected branch hook declined: ")
        .or_else(|| reason.strip_prefix("push declined due to repository rule violations: "))
        .unwrap_or(reason)
        .to_string()
}

/// Check branch protection / rulesets for an API ref update made by `user`
/// (422 with GitHub's message on violation).
pub async fn authorize(
    state: &AppState,
    access: &RepoAccess,
    user: &db::User,
    git: &bgh_git::GitCli,
    update: &RefUpdate,
) -> ApiResult<()> {
    let rules = RepoRules::load(&state.db, &access.repo).await?;
    if rules.is_empty() {
        return Ok(());
    }
    let actor = Actor::load(state, access, user).await?;
    let needs = protection::check_update(&rules, &actor, update)
        .map_err(|r| ApiError::unprocessable(api_reason(&r)))?;
    protection::verify_needs(state, git, access.repo.id, update, &needs).await
}

/// Move `refname` from `old` (None = create) to `new` (None = delete) on
/// behalf of `user`. Non-fast-forward updates need `force`. Enqueues
/// post-receive processing (`Event::Push`, pushed_at, ...).
pub async fn write_ref(
    state: &AppState,
    access: &RepoAccess,
    user: &db::User,
    refname: &str,
    old: Option<&str>,
    new: Option<&str>,
    force: bool,
) -> ApiResult<RefUpdate> {
    access.require_not_archived()?;
    if bgh_git::storage::is_hidden_ref(refname) {
        // Server-only namespaces (`refs/pull/*`, `refs/bgh/*`).
        return Err(ApiError::unprocessable(format!(
            "Reference update failed: {refname} is a hidden ref."
        )));
    }
    let store = crate::store(state);
    let git = store.cli(access.repo.id)?;
    let update = RefUpdate {
        old: old.unwrap_or(ZERO_SHA).to_string(),
        new: new.unwrap_or(ZERO_SHA).to_string(),
        refname: refname.to_string(),
    };
    if !force
        && !update.is_create()
        && !update.is_delete()
        && update.old != update.new
        && !git.is_ancestor(&update.old, &update.new).await?
    {
        return Err(ApiError::unprocessable("Update is not a fast forward"));
    }
    authorize(state, access, user, &git, &update).await?;
    if update.old == update.new {
        return Ok(update);
    }
    match new {
        Some(sha) => git.update_ref(refname, sha, old).await?,
        None => bgh_git::write::delete_ref(&store, access.repo.id, refname, old).await?,
    }
    bgh_core::jobs::enqueue_job(
        &state.db,
        &PostReceive {
            repo_id: access.repo.id,
            pusher_id: Some(user.id),
            updates: vec![update.clone()],
        },
    )
    .await?;
    Ok(update)
}
