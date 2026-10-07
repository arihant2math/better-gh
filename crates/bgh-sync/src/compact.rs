//! Compaction of `sync_actions` (job `sync.compact`, self-rescheduling).

use std::collections::HashSet;
use std::sync::Mutex;
use std::time::Duration;

use bgh_core::jobs::{self, JobPayload};
use bgh_core::state::AppState;
use chrono::Utc;
use serde::{Deserialize, Serialize};

/// Delete batch size (keeps each statement short).
const CHUNK: i64 = 10_000;

/// Periodic compaction job.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Compact {}

impl JobPayload for Compact {
    const KIND: &'static str = "sync.compact";
    const MAX_ATTEMPTS: i32 = 3;
}

/// What a compaction run did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    pub deleted: u64,
    pub min_retained_id: i64,
}

/// Prune actions older than `retention`.
///
/// * `keep_latest = false`: delete every action older than the window
///   (always keeping the newest action so the head is known) and advance
///   `sync_meta.min_retained_id`; clients behind it rebootstrap.
/// * `keep_latest = true`: only delete old actions superseded by a newer
///   action for the same row; every row's latest state stays replayable,
///   so `min_retained_id` doesn't move.
pub async fn compact(
    state: &AppState,
    retention: Duration,
    keep_latest: bool,
) -> anyhow::Result<Stats> {
    let cutoff_time = Utc::now() - chrono::Duration::from_std(retention)?;
    // First id inside the window; if every action is older, keep the newest.
    let cutoff: Option<i64> = sqlx::query_scalar(
        "SELECT coalesce(
             (SELECT min(id) FROM sync_actions WHERE created_at >= $1),
             (SELECT max(id) FROM sync_actions))",
    )
    .bind(cutoff_time)
    .fetch_one(&state.db)
    .await?;
    let Some(cutoff) = cutoff else {
        return Ok(Stats {
            deleted: 0,
            min_retained_id: crate::delta::min_retained_id(&state.db).await?,
        });
    };
    // Never delete above the commit-order watermark: a gap there may still
    // be in flight, and `seqlog` must find the rows it scans.
    let cutoff = cutoff.min(crate::delta::head(&state.db).await? + 1);
    let mut deleted = 0u64;
    if keep_latest {
        loop {
            let n = sqlx::query(
                "DELETE FROM sync_actions WHERE id IN (
                     SELECT a.id FROM sync_actions a
                      WHERE a.id < $1
                        AND EXISTS (SELECT 1 FROM sync_actions b
                                     WHERE b.scope = a.scope AND b.model = a.model
                                       AND b.model_id = a.model_id AND b.id > a.id)
                      ORDER BY a.id LIMIT $2)",
            )
            .bind(cutoff)
            .bind(CHUNK)
            .execute(&state.db)
            .await?
            .rows_affected();
            deleted += n;
            if n < CHUNK as u64 {
                break;
            }
        }
    } else {
        // Publish the new floor first: a replay racing with the delete then
        // rechecks it and asks the client to rebootstrap instead of silently
        // skipping pruned actions.
        sqlx::query(
            "UPDATE sync_meta SET min_retained_id = greatest(min_retained_id, $1), compacted_at = now()",
        )
        .bind(cutoff)
        .execute(&state.db)
        .await?;
        loop {
            let n = sqlx::query(
                "DELETE FROM sync_actions WHERE id IN (
                     SELECT id FROM sync_actions WHERE id < $1 ORDER BY id LIMIT $2)",
            )
            .bind(cutoff)
            .bind(CHUNK)
            .execute(&state.db)
            .await?
            .rows_affected();
            deleted += n;
            if n < CHUNK as u64 {
                break;
            }
        }
    }
    if keep_latest {
        sqlx::query("UPDATE sync_meta SET compacted_at = now()")
            .execute(&state.db)
            .await?;
    }
    let min_retained_id = crate::delta::min_retained_id(&state.db).await?;
    tracing::info!(deleted, min_retained_id, "sync log compacted");
    Ok(Stats {
        deleted,
        min_retained_id,
    })
}

/// Job handler: compact with the configured policy, then schedule the next run.
pub async fn run_job(state: AppState, _job: Compact) -> anyhow::Result<()> {
    let cfg = &state.config.sync;
    // Schedule first so a failing run doesn't stop the cycle.
    schedule(&state, cfg.compact_interval).await?;
    compact(&state, cfg.retention, cfg.keep_latest).await?;
    Ok(())
}

/// Enqueue a compaction run `after` from now unless one is already pending
/// (a running job is locked and doesn't count).
async fn schedule(state: &AppState, after: Duration) -> anyhow::Result<()> {
    let mut tx = state.db.begin().await?;
    // Serialize schedulers across processes.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('sync.compact.schedule'))")
        .execute(&mut *tx)
        .await?;
    let pending: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM jobs WHERE kind = $1 AND failed_at IS NULL AND locked_at IS NULL",
    )
    .bind(Compact::KIND)
    .fetch_one(&mut *tx)
    .await?;
    if pending == 0 {
        let run_at = Utc::now() + chrono::Duration::from_std(after)?;
        jobs::enqueue_at(
            &mut *tx,
            Compact::KIND,
            &serde_json::to_value(Compact {})?,
            run_at,
            Compact::MAX_ATTEMPTS,
        )
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Make sure the periodic compaction is scheduled (once per process and
/// database; called on the first bootstrap / WebSocket).
pub async fn ensure_scheduled(state: &AppState) {
    static DONE: Mutex<Option<HashSet<String>>> = Mutex::new(None);
    let key = state.config.redis_prefix.clone();
    {
        let mut done = DONE.lock().expect("compact schedule lock");
        if !done.get_or_insert_with(HashSet::new).insert(key.clone()) {
            return;
        }
    }
    if let Err(err) = schedule(state, state.config.sync.compact_interval).await {
        tracing::warn!(?err, "scheduling sync compaction");
        if let Some(done) = DONE.lock().expect("compact schedule lock").as_mut() {
            done.remove(&key);
        }
    }
}
