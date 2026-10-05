//! Post-push maintenance: pack loose refs once enough accumulate, so ref
//! listings (ref picker, info/refs, branch APIs) stay fast, and warm the
//! code browser's last-commit cache for the new default-branch head (best
//! effort, in-process) so the first visitor doesn't pay for the walk.

use std::sync::Arc;

use bgh_core::events::Event;
use bgh_core::jobs::JobPayload;
use bgh_core::prelude::*;
use serde::{Deserialize, Serialize};

/// Pack refs when at least this many loose ref files exist.
pub const LOOSE_REFS_THRESHOLD: usize = 64;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackRefs {
    pub repo_id: i64,
}

impl JobPayload for PackRefs {
    const KIND: &'static str = "repos.pack_refs";
}

/// Listener: after a push, schedule `pack-refs` when loose refs pile up.
pub async fn on_event(state: AppState, event: Arc<Event>) -> anyhow::Result<()> {
    let Event::Push(push) = &*event else {
        return Ok(());
    };
    let store = crate::store(&state);
    let path = store.path(push.repo_id);
    let n = tokio::task::spawn_blocking(move || {
        bgh_git::maintenance::loose_ref_count(&path, LOOSE_REFS_THRESHOLD)
    })
    .await?;
    if n >= LOOSE_REFS_THRESHOLD {
        bgh_core::jobs::enqueue_job(
            &state.db,
            &PackRefs {
                repo_id: push.repo_id,
            },
        )
        .await?;
    }
    warm_browse_cache(&state, push).await
}

/// Compute the root last-commit map of the default branch's new head.
async fn warm_browse_cache(
    state: &AppState,
    push: &bgh_core::events::PushEvent,
) -> anyhow::Result<()> {
    let Some(repo) = db::Repository::find(&state.db, push.repo_id).await? else {
        return Ok(());
    };
    let default_ref = format!("refs/heads/{}", repo.default_branch);
    let Some(update) = push
        .updates
        .iter()
        .find(|u| u.refname == default_ref && !u.is_delete())
    else {
        return Ok(());
    };
    let Some(owner) = db::User::find(&state.db, repo.owner_id).await? else {
        return Ok(());
    };
    let commit = update.new.clone();
    let target = crate::browse::Target {
        access: RepoAccess {
            repo,
            owner,
            permission: Permission::Read,
            authenticated: false,
        },
        refname: commit.clone(),
        commit,
        path: String::new(),
    };
    if let Err(err) = crate::browse::tree::last_commit_map(state, &target).await {
        tracing::debug!(?err, "browse cache warm-up failed");
    }
    Ok(())
}

pub async fn pack_refs(state: AppState, job: PackRefs) -> anyhow::Result<()> {
    let store = crate::store(&state);
    if !store.exists(job.repo_id) {
        return Ok(()); // deleted meanwhile
    }
    bgh_git::maintenance::pack_refs(&store, job.repo_id).await?;
    Ok(())
}
