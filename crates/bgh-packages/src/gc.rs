//! Registry garbage collection (`packages.gc` service, hourly, one leader
//! via a pg advisory lock):
//!
//! 1. abandoned upload sessions (idle for a day) and their files;
//! 2. versions and packages soft-deleted more than 30 days ago;
//! 3. blob links no version of the package references, older than a day
//!    (blobs uploaded but never used by a manifest, or left over by
//!    purged versions);
//! 4. blobs no package links any more, from the database and the disk.

use std::time::Duration;

use bgh_core::AppState;
use tokio_util::sync::CancellationToken;

use crate::digest::Digest;
use crate::storage;

/// Leader lock key ("pkggc").
const LEADER_KEY: i64 = 0x706b_6767_63;

pub async fn service(state: AppState, shutdown: CancellationToken) -> anyhow::Result<()> {
    let mut tick = tokio::time::interval(Duration::from_secs(3600));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return Ok(()),
            _ = tick.tick() => {}
        }
        match run(&state, "1 day", "30 days").await {
            Ok(r) if r != Report::default() => tracing::info!(?r, "package gc"),
            Ok(_) => {}
            Err(err) => tracing::warn!(?err, "package gc failed"),
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub uploads: u64,
    pub versions: u64,
    pub packages: u64,
    pub links: u64,
    pub blobs: u64,
}

/// One GC pass. `grace` and `retention` are Postgres intervals (tests use
/// `0 seconds`). Returns an empty report when another process leads.
pub async fn run(state: &AppState, grace: &str, retention: &str) -> anyhow::Result<Report> {
    let mut report = Report::default();
    let mut conn = state.db.acquire().await?;
    let leader: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
        .bind(LEADER_KEY)
        .fetch_one(&mut *conn)
        .await?;
    if !leader {
        return Ok(report);
    }
    let res = pass(state, grace, retention, &mut report).await;
    let _ = sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(LEADER_KEY)
        .execute(&mut *conn)
        .await;
    res.map(|()| report)
}

async fn pass(
    state: &AppState,
    grace: &str,
    retention: &str,
    report: &mut Report,
) -> anyhow::Result<()> {
    // 1. Abandoned uploads.
    let uploads: Vec<uuid::Uuid> = sqlx::query_scalar(
        "DELETE FROM package_uploads WHERE updated_at < now() - $1::interval RETURNING id",
    )
    .bind(grace)
    .fetch_all(&state.db)
    .await?;
    for id in &uploads {
        let _ = tokio::fs::remove_file(storage::upload_path(state, id)).await;
    }
    report.uploads = uploads.len() as u64;

    // 2. Expired soft deletes.
    report.versions =
        sqlx::query("DELETE FROM package_versions WHERE deleted_at < now() - $1::interval")
            .bind(retention)
            .execute(&state.db)
            .await?
            .rows_affected();
    report.packages = sqlx::query("DELETE FROM packages WHERE deleted_at < now() - $1::interval")
        .bind(retention)
        .execute(&state.db)
        .await?
        .rows_affected();

    // 3. Unreferenced links (manifests are stored in the database, so only
    //    config/layer references keep a blob).
    let touched: Vec<i64> = sqlx::query_scalar(
        "DELETE FROM package_blob_links l
          WHERE l.created_at < now() - $1::interval
            AND NOT EXISTS (SELECT 1 FROM package_versions v
                              JOIN package_version_blobs vb ON vb.version_id = v.id
                             WHERE v.package_id = l.package_id AND vb.digest = l.digest)
            AND NOT EXISTS (SELECT 1 FROM package_uploads u WHERE u.package_id = l.package_id)
          RETURNING l.package_id",
    )
    .bind(grace)
    .fetch_all(&state.db)
    .await?;
    report.links = touched.len() as u64;
    let mut touched = touched;
    touched.sort_unstable();
    touched.dedup();
    for id in touched {
        let mut conn = state.db.acquire().await?;
        crate::ops::recompute_size(&mut conn, id).await?;
    }

    // 4. Unlinked blobs, under the exclusive blob lock (uploads committing
    //    a blob hold it shared between moving the file and linking it).
    let mut tx = state.db.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(storage::BLOB_LOCK_KEY)
        .execute(&mut *tx)
        .await?;
    let gone: Vec<String> = sqlx::query_scalar(
        "DELETE FROM package_blobs b
          WHERE NOT EXISTS (SELECT 1 FROM package_blob_links l WHERE l.digest = b.digest)
          RETURNING b.digest",
    )
    .fetch_all(&mut *tx)
    .await?;
    for d in &gone {
        if let Some(d) = Digest::parse(d) {
            let _ = tokio::fs::remove_file(storage::blob_path(state, &d)).await;
        }
    }
    tx.commit().await?;
    report.blobs = gone.len() as u64;
    Ok(())
}
