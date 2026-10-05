//! Wiki storage cleanup when a repository is deleted.

use std::sync::Arc;

use bgh_core::jobs::{self, JobPayload};
use bgh_core::prelude::*;
use serde::{Deserialize, Serialize};

/// Remove a deleted repository's wiki storage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteWikiStorage {
    pub repo_id: i64,
}

impl JobPayload for DeleteWikiStorage {
    const KIND: &'static str = "wiki.delete_storage";
}

/// Event listener: on `RepositoryDeleted`, enqueue [`DeleteWikiStorage`]
/// when the repository has wiki storage.
pub async fn on_event(state: AppState, event: Arc<Event>) -> anyhow::Result<()> {
    if let Event::RepositoryDeleted { repo_id, .. } = &*event
        && crate::access::store(&state).path(*repo_id).exists()
    {
        jobs::enqueue_job(&state.db, &DeleteWikiStorage { repo_id: *repo_id }).await?;
    }
    Ok(())
}

/// Idempotent; never deletes the wiki of a repository that (still) exists.
pub async fn delete_storage(state: AppState, job: DeleteWikiStorage) -> anyhow::Result<()> {
    if db::Repository::find(&state.db, job.repo_id)
        .await?
        .is_some()
    {
        return Ok(());
    }
    crate::access::store(&state).delete(job.repo_id).await?;
    Ok(())
}
