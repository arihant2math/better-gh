//! Blob garbage collection. Attachment rows cascade with their repository
//! (and owner); this job then removes blobs nothing references any more.

use std::sync::Arc;
use std::time::Duration;

use bgh_core::events::Event;
use bgh_core::jobs::JobPayload;
use bgh_core::prelude::*;
use serde::{Deserialize, Serialize};

use crate::storage;

/// Unreferenced blobs younger than this are kept: an upload stores the
/// blob just before inserting its row.
pub const GRACE: Duration = Duration::from_secs(3600);

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UploadsGc {}

impl JobPayload for UploadsGc {
    const KIND: &'static str = "uploads.gc";
}

/// Event listener: a deleted repository (or account) may leave orphaned
/// blobs behind.
pub async fn on_event(state: AppState, event: Arc<Event>) -> anyhow::Result<()> {
    if matches!(&*event, Event::RepositoryDeleted { .. })
        && tokio::fs::try_exists(storage::root(&state))
            .await
            .unwrap_or(false)
    {
        bgh_core::jobs::enqueue_job(&state.db, &UploadsGc {}).await?;
    }
    Ok(())
}

pub async fn run(state: AppState, _job: UploadsGc) -> anyhow::Result<()> {
    let removed = collect(&state, GRACE).await?;
    if removed > 0 {
        tracing::info!(removed, "uploads gc removed unreferenced attachment blobs");
    }
    Ok(())
}

/// Delete unreferenced blobs older than `grace`; returns how many.
pub async fn collect(state: &AppState, grace: Duration) -> anyhow::Result<usize> {
    let root = storage::root(state);
    let candidates: Vec<String> = tokio::task::spawn_blocking(move || {
        let mut out = Vec::new();
        let now = std::time::SystemTime::now();
        let Ok(dirs) = std::fs::read_dir(&root) else {
            return out;
        };
        for dir in dirs.flatten().filter(|e| e.file_name() != "tmp") {
            let Ok(files) = std::fs::read_dir(dir.path()) else {
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
                if old && storage::is_digest(&name) {
                    out.push(name);
                }
            }
        }
        out
    })
    .await?;
    if candidates.is_empty() {
        return Ok(0);
    }
    let orphaned: Vec<String> = sqlx::query_scalar(
        "SELECT d FROM unnest($1::text[]) d
          WHERE NOT EXISTS (SELECT 1 FROM attachments a WHERE a.sha256 = d)
            AND NOT EXISTS (SELECT 1 FROM deleted_repositories r WHERE d = ANY(r.blob_shas))",
    )
    .bind(&candidates)
    .fetch_all(&state.db)
    .await?;
    for d in &orphaned {
        storage::delete(state, d).await?;
    }
    Ok(orphaned.len())
}
