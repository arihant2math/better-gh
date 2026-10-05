//! Cleanup after repository deletion: remove LFS objects no repository
//! links any more (rows cascade with the repository) and the repository's
//! cached archives.

use std::sync::Arc;
use std::time::Duration;

use bgh_core::events::Event;
use bgh_core::jobs::JobPayload;
use bgh_core::prelude::*;
use serde::{Deserialize, Serialize};

/// Unreferenced objects younger than this are kept (in-flight uploads link
/// the object right after writing it).
const GRACE: Duration = Duration::from_secs(3600);

/// Remove unreferenced LFS objects from disk.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LfsGc {}

impl JobPayload for LfsGc {
    const KIND: &'static str = "repos.lfs_gc";
}

/// Event listener: on repository deletion, drop its archive cache and
/// schedule LFS garbage collection.
pub async fn on_event(state: AppState, event: Arc<Event>) -> anyhow::Result<()> {
    if let Event::RepositoryDeleted { repo_id, .. } = &*event {
        let dir = crate::download::archive::cache_dir(&state);
        bgh_git::archive::purge_repo(&dir, *repo_id).await?;
        // Only scan when this server stores LFS objects at all.
        if tokio::fs::try_exists(&super::object_store(&state).root)
            .await
            .unwrap_or(false)
        {
            bgh_core::jobs::enqueue_job(&state.db, &LfsGc {}).await?;
        }
    }
    Ok(())
}

pub async fn run(state: AppState, _job: LfsGc) -> anyhow::Result<()> {
    let removed = collect(&state, GRACE).await?;
    if removed > 0 {
        tracing::info!(removed, "lfs gc removed unreferenced objects");
    }
    Ok(())
}

/// Delete unreferenced objects older than `grace`. Returns how many.
pub async fn collect(state: &AppState, grace: Duration) -> anyhow::Result<usize> {
    let store = super::object_store(state);
    let root = store.root.clone();
    // Candidate oids (blocking directory walk).
    let candidates: Vec<String> = tokio::task::spawn_blocking(move || {
        let mut out = Vec::new();
        let now = std::time::SystemTime::now();
        let Ok(l1) = std::fs::read_dir(&root) else {
            return out;
        };
        for a in l1.flatten().filter(|e| e.file_name() != "tmp") {
            let Ok(l2) = std::fs::read_dir(a.path()) else {
                continue;
            };
            for b in l2.flatten() {
                let Ok(files) = std::fs::read_dir(b.path()) else {
                    continue;
                };
                for f in files.flatten() {
                    let name = f.file_name().to_string_lossy().into_owned();
                    let old = f
                        .metadata()
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| now.duration_since(t).ok())
                        .is_some_and(|age| age >= grace);
                    if old && bgh_git::lfs::is_valid_oid(&name) {
                        out.push(name);
                    }
                }
            }
        }
        out
    })
    .await?;
    let mut removed = 0;
    for chunk in candidates.chunks(500) {
        let referenced: Vec<String> =
            sqlx::query_scalar("SELECT DISTINCT oid FROM lfs_objects WHERE oid = ANY($1)")
                .bind(chunk)
                .fetch_all(&state.db)
                .await?;
        for oid in chunk.iter().filter(|o| !referenced.contains(o)) {
            store.remove(oid).await?;
            removed += 1;
        }
    }
    Ok(removed)
}
