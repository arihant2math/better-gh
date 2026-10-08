//! Background scans: history backfills (on enable, on custom pattern
//! changes, site-wide enablement) and incremental scans of every push.

use std::sync::Arc;

use bgh_core::db::AdvisoryLock;
use bgh_core::events::{Event, RefUpdate, ZERO_SHA};
use bgh_core::jobs::JobPayload;
use bgh_core::state::AppState;
use bgh_git::RepoStore;
use serde::{Deserialize, Serialize};

use crate::patterns::Engine;
use crate::scan;
use crate::settings::{self, Effective};

/// Scan every commit reachable from any ref.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanHistory {
    pub repo_id: i64,
    pub scan_id: i64,
}

impl JobPayload for ScanHistory {
    const KIND: &'static str = "security.scan_history";
    const MAX_ATTEMPTS: i32 = 5;
}

/// Scan the commits a push added.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanPush {
    pub repo_id: i64,
    pub scan_id: i64,
    pub updates: Vec<RefUpdate>,
}

impl JobPayload for ScanPush {
    const KIND: &'static str = "security.scan_push";
}

/// Record a pending scan of `kind` and enqueue its history job (inside the
/// caller's transaction).
pub async fn enqueue_history_scan(
    conn: &mut sqlx::PgConnection,
    repo_id: i64,
    kind: &str,
) -> Result<i64, sqlx::Error> {
    let scan_id: i64 = sqlx::query_scalar(
        "INSERT INTO secret_scanning_scans (repo_id, kind) VALUES ($1, $2) RETURNING id",
    )
    .bind(repo_id)
    .bind(kind)
    .fetch_one(&mut *conn)
    .await?;
    bgh_core::jobs::enqueue_job(&mut *conn, &ScanHistory { repo_id, scan_id }).await?;
    Ok(scan_id)
}

/// Effective settings and the engine (custom patterns included) of a
/// repository.
pub async fn engine_for(
    state: &AppState,
    repo_id: i64,
) -> anyhow::Result<(Effective, Arc<Engine>)> {
    let eff = settings::effective(state, repo_id)
        .await
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let custom = crate::custom::patterns_for_repo(&state.db, repo_id).await?;
    Ok((eff, Engine::for_repo(eff.non_provider_patterns, custom)))
}

async fn finish_scan(state: &AppState, scan_id: i64, out: &scan::Outcome) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE secret_scanning_scans SET status = 'completed', completed_at = now(),
                blobs_scanned = $2, bytes_scanned = $3
          WHERE id = $1",
    )
    .bind(scan_id)
    .bind(out.blobs_scanned)
    .bind(out.bytes_scanned)
    .execute(&state.db)
    .await?;
    Ok(())
}

fn store(state: &AppState) -> RepoStore {
    RepoStore::from_config(&state.config)
}

pub async fn scan_history(state: AppState, job: ScanHistory) -> anyhow::Result<()> {
    let (eff, engine) = engine_for(&state, job.repo_id).await?;
    let Ok(cli) = store(&state).cli(job.repo_id) else {
        return Ok(()); // repository gone
    };
    let mut out = scan::Outcome::default();
    if eff.secret_scanning {
        let commits = scan::rev_list(&cli, &["--all"], &[]).await?;
        out = scan::scan_commits(&cli, &commits, &[], engine.clone(), eff.max_blob, None).await?;
        crate::store::record(&state, job.repo_id, &engine, &out.hits)
            .await
            .map_err(|e| anyhow::anyhow!("recording alerts: {e:?}"))?;
    }
    finish_scan(&state, job.scan_id, &out).await
}

/// `rev-list` arguments for the commits `updates` added (after the refs
/// moved): new tips, minus old tips and every ref the push didn't touch.
pub fn push_rev_args(updates: &[RefUpdate]) -> Vec<String> {
    let mut args: Vec<String> = updates
        .iter()
        .filter(|u| u.new != ZERO_SHA && bgh_git::is_sha(&u.new))
        .map(|u| u.new.clone())
        .collect();
    if args.is_empty() {
        return args;
    }
    args.push("--not".into());
    args.extend(
        updates
            .iter()
            .filter(|u| u.old != ZERO_SHA && bgh_git::is_sha(&u.old))
            .map(|u| u.old.clone()),
    );
    for u in updates {
        // `--exclude` takes a glob; escape its metacharacters.
        let mut pat = String::with_capacity(u.refname.len());
        for c in u.refname.chars() {
            if matches!(c, '*' | '?' | '[' | '\\') {
                pat.push('\\');
            }
            pat.push(c);
        }
        args.push(format!("--exclude={pat}"));
    }
    // Not `--all`: it includes HEAD, which follows the pushed branch.
    args.push("--glob=refs/*".into());
    args
}

pub async fn scan_push(state: AppState, job: ScanPush) -> anyhow::Result<()> {
    let (eff, engine) = engine_for(&state, job.repo_id).await?;
    let Ok(cli) = store(&state).cli(job.repo_id) else {
        return Ok(());
    };
    let mut out = scan::Outcome::default();
    let args = push_rev_args(&job.updates);
    if eff.secret_scanning && !args.is_empty() {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let commits = scan::rev_list(&cli, &args, &[]).await?;
        out = scan::scan_commits(&cli, &commits, &[], engine.clone(), eff.max_blob, None).await?;
        crate::store::record(&state, job.repo_id, &engine, &out.hits)
            .await
            .map_err(|e| anyhow::anyhow!("recording alerts: {e:?}"))?;
    }
    finish_scan(&state, job.scan_id, &out).await
}

/// Listener `security.secret_scanning`: scan every push of repositories
/// with secret scanning on.
pub async fn on_event(state: AppState, event: Arc<Event>) -> anyhow::Result<()> {
    let Event::Push(p) = &*event else {
        return Ok(());
    };
    let eff = settings::effective(&state, p.repo_id)
        .await
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;
    if !eff.secret_scanning || push_rev_args(&p.updates).is_empty() {
        return Ok(());
    }
    let mut tx = state.db.begin().await?;
    if bgh_core::events::claim_effect(&mut tx).await? {
        let scan_id: i64 = sqlx::query_scalar(
            "INSERT INTO secret_scanning_scans (repo_id, kind) VALUES ($1, 'incremental')
             RETURNING id",
        )
        .bind(p.repo_id)
        .fetch_one(&mut *tx)
        .await?;
        bgh_core::jobs::enqueue_job(
            &mut *tx,
            &ScanPush {
                repo_id: p.repo_id,
                scan_id,
                updates: p.updates.clone(),
            },
        )
        .await?;
        tx.commit().await?;
    }
    Ok(())
}

/// Enqueue a backfill for every repository whose secret scanning is on but
/// that was never scanned (site-wide enablement, repositories enabled
/// while the feature was unavailable). Returns how many were queued.
pub async fn backfill_pending(state: &AppState) -> anyhow::Result<usize> {
    let site = bgh_core::settings::load(state)
        .await
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let s = &site.secret_scanning;
    if !s.available {
        return Ok(0);
    }
    let ids: Vec<i64> = sqlx::query_scalar(
        "SELECT r.id FROM repositories r
          LEFT JOIN repo_security_settings s ON s.repo_id = r.id
          WHERE ($1 OR coalesce(s.secret_scanning, false))
            AND NOT EXISTS (SELECT 1 FROM secret_scanning_scans x
                             WHERE x.repo_id = r.id AND x.kind = 'backfill')
          ORDER BY r.id LIMIT 500",
    )
    .bind(s.enable_all)
    .fetch_all(&state.db)
    .await?;
    let mut tx = state.db.begin().await?;
    for id in &ids {
        enqueue_history_scan(&mut tx, *id, "backfill").await?;
    }
    tx.commit().await?;
    Ok(ids.len())
}

/// Service `security.backfill`: [`backfill_pending`] every minute on one
/// node (pg advisory lock).
pub async fn backfill_service(
    state: AppState,
    shutdown: tokio_util::sync::CancellationToken,
) -> anyhow::Result<()> {
    const LOCK: i64 = 0x7700_0000_0001;
    loop {
        if let Some(leader) = AdvisoryLock::try_acquire(&state.db, LOCK).await? {
            loop {
                if let Err(e) = backfill_pending(&state).await {
                    tracing::warn!("secret scanning backfill: {e:#}");
                }
                tokio::select! {
                    _ = shutdown.cancelled() => {
                        leader.release().await;
                        return Ok(());
                    }
                    _ = tokio::time::sleep(std::time::Duration::from_secs(60)) => {}
                }
            }
        }
        tokio::select! {
            _ = shutdown.cancelled() => return Ok(()),
            _ = tokio::time::sleep(std::time::Duration::from_secs(60)) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_rev_args_exclude_pushed_refs() {
        let a = "a".repeat(40);
        let b = "b".repeat(40);
        let args = push_rev_args(&[
            RefUpdate {
                old: ZERO_SHA.into(),
                new: a.clone(),
                refname: "refs/heads/new*".into(),
            },
            RefUpdate {
                old: b.clone(),
                new: a.clone(),
                refname: "refs/heads/main".into(),
            },
            RefUpdate {
                old: b.clone(),
                new: ZERO_SHA.into(),
                refname: "refs/heads/gone".into(),
            },
        ]);
        assert_eq!(
            args,
            [
                a.as_str(),
                &a,
                "--not",
                &b,
                &b,
                "--exclude=refs/heads/new\\*",
                "--exclude=refs/heads/main",
                "--exclude=refs/heads/gone",
                "--glob=refs/*"
            ]
        );
        assert!(
            push_rev_args(&[RefUpdate {
                old: b.clone(),
                new: ZERO_SHA.into(),
                refname: "refs/heads/x".into()
            }])
            .is_empty()
        );
    }
}
