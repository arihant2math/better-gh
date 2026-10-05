//! Repository maintenance.
//!
//! * Post-push: pack loose refs once enough accumulate, so ref listings
//!   (ref picker, info/refs, branch APIs) stay fast, and warm the code
//!   browser's last-commit cache for the new default-branch head (best
//!   effort, in-process) so the first visitor doesn't pay for the walk.
//! * Fork-network-aware object maintenance ([`run_task`], used by the admin
//!   operations and the scheduler): see `bgh_git::maintenance` for the
//!   per-role git commands. Every run holds a per-repository advisory lock.
//! * The `repos.maintenance` service ([`service`]): once a minute the
//!   elected leader (pg advisory lock) picks due repositories (pushed since
//!   the last run, full repack due, or never maintained), writes
//!   commit-graphs, repacks geometrically when loose objects / packs pile up
//!   or the interval passed, fully repacks on the longer cadence, and prunes
//!   the archive cache. State lives in `repo_maintenance`.

use std::sync::Arc;
use std::time::Duration;

use bgh_core::events::Event;
use bgh_core::jobs::JobPayload;
use bgh_core::prelude::*;
use bgh_git::maintenance::{NetworkRole, ObjectStats, Task};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::pool::PoolConnection;
use sqlx::{FromRow, Postgres};
use tokio_util::sync::CancellationToken;

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

// ----- locks ------------------------------------------------------------------

/// Advisory-lock key of the scheduler's leader election.
const LEADER_KEY: i64 = 0x4247_4d41_494e_0001; // "BGMAIN" 1
/// Advisory-lock namespace of per-repository maintenance locks.
const REPO_LOCK_NS: i64 = 0x4d4e << 40;

/// A session-level pg advisory lock held on a dedicated connection.
/// Released by [`AdvisoryLock::release`]; if dropped instead, the connection
/// is closed (not returned to the pool), which releases the lock too.
pub struct AdvisoryLock {
    conn: Option<PoolConnection<Postgres>>,
    key: i64,
}

impl AdvisoryLock {
    /// Take `key`, waiting for it when `wait`, else `None` when it's held.
    pub async fn acquire(state: &AppState, key: i64, wait: bool) -> sqlx::Result<Option<Self>> {
        let mut conn = state.db.acquire().await?;
        let got: bool = if wait {
            sqlx::query("SELECT pg_advisory_lock($1)")
                .bind(key)
                .execute(&mut *conn)
                .await?;
            true
        } else {
            sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
                .bind(key)
                .fetch_one(&mut *conn)
                .await?
        };
        Ok(got.then_some(Self {
            conn: Some(conn),
            key,
        }))
    }

    pub async fn release(mut self) {
        if let Some(mut conn) = self.conn.take() {
            let ok = sqlx::query("SELECT pg_advisory_unlock($1)")
                .bind(self.key)
                .execute(&mut *conn)
                .await
                .is_ok();
            if !ok {
                drop(conn.detach());
            }
        }
    }
}

impl Drop for AdvisoryLock {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            drop(conn.detach()); // closes the session → lock released
        }
    }
}

/// The per-repository maintenance lock.
pub async fn lock_repo(
    state: &AppState,
    repo_id: i64,
    wait: bool,
) -> sqlx::Result<Option<AdvisoryLock>> {
    AdvisoryLock::acquire(state, REPO_LOCK_NS | (repo_id & ((1 << 40) - 1)), wait).await
}

// ----- roles and single runs ---------------------------------------------------

/// The repository's role in its fork network: alternates from disk;
/// dependents from the database (`parent_id`) or, as a fallback for
/// relationships the database lost, from the alternates files on disk.
pub async fn network_role(state: &AppState, repo_id: i64) -> anyhow::Result<NetworkRole> {
    let store = crate::store(state);
    let dir = store.path(repo_id);
    let db_forks: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM repositories WHERE parent_id = $1)")
            .bind(repo_id)
            .fetch_one(&state.db)
            .await?;
    let root = store.root.clone();
    let (has_alternates, on_disk) = tokio::task::spawn_blocking(move || {
        (
            bgh_git::maintenance::has_alternates(&dir),
            !db_forks && !bgh_git::maintenance::dependents_on_disk(&root, &dir).is_empty(),
        )
    })
    .await?;
    Ok(NetworkRole {
        has_alternates,
        has_dependents: db_forks || on_disk,
    })
}

/// Run `task` on a repository under its maintenance lock (waiting for it)
/// and record the result in `repo_maintenance`. Returns git's output.
pub async fn run_task(state: &AppState, repo_id: i64, task: Task) -> anyhow::Result<String> {
    let store = crate::store(state);
    if !store.exists(repo_id) {
        anyhow::bail!("repository storage does not exist");
    }
    let settings = bgh_core::settings::load(state).await?;
    let lock = lock_repo(state, repo_id, true)
        .await?
        .ok_or_else(|| anyhow::anyhow!("maintenance lock unavailable"))?;
    let res = async {
        let role = network_role(state, repo_id).await?;
        let out = bgh_git::maintenance::run_task(
            &store.git_bin,
            &store.path(repo_id),
            task,
            role,
            settings.git_maintenance.prune_grace_days,
        )
        .await?;
        Ok::<_, anyhow::Error>((role, out))
    }
    .await;
    lock.release().await;
    let full = matches!(task, Task::Gc | Task::Repack | Task::PruneNow);
    match res {
        Ok((role, out)) => {
            let stats = bgh_git::maintenance::object_stats(&store.git_bin, &store.path(repo_id))
                .await
                .unwrap_or_default();
            record(state, repo_id, "succeeded", None, stats, role, full).await?;
            Ok(out)
        }
        Err(e) => {
            let msg = format!("{e:#}");
            let _ = record_status(state, repo_id, "failed", Some(&msg)).await;
            Err(e)
        }
    }
}

/// Make a repository self-contained (and, first, every repository borrowing
/// from it), under its maintenance lock.
pub async fn dissociate(state: &AppState, repo_id: i64) -> anyhow::Result<()> {
    let store = crate::store(state);
    if !store.exists(repo_id) {
        return Ok(());
    }
    let lock = lock_repo(state, repo_id, true)
        .await?
        .ok_or_else(|| anyhow::anyhow!("maintenance lock unavailable"))?;
    let res =
        bgh_git::maintenance::dissociate_network(&store.git_bin, &store.root, &store.path(repo_id))
            .await;
    lock.release().await;
    res?;
    Ok(())
}

async fn record(
    state: &AppState,
    repo_id: i64,
    status: &str,
    error: Option<&str>,
    stats: ObjectStats,
    role: NetworkRole,
    full: bool,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO repo_maintenance AS m
              (repo_id, last_run_at, last_full_at, status, error, pack_count, loose_count,
               has_alternates, has_dependents, updated_at)
         SELECT r.id, now(), CASE WHEN $8 THEN now() END, $2, $3, $4, $5, $6, $7, now()
           FROM repositories r WHERE r.id = $1
         ON CONFLICT (repo_id) DO UPDATE SET
              last_run_at = now(),
              last_full_at = CASE WHEN $8 THEN now() ELSE m.last_full_at END,
              status = $2, error = $3, pack_count = $4, loose_count = $5,
              has_alternates = $6, has_dependents = $7, updated_at = now()",
    )
    .bind(repo_id)
    .bind(status)
    .bind(error)
    .bind(stats.pack_count)
    .bind(stats.loose_count)
    .bind(role.has_alternates)
    .bind(role.has_dependents)
    .bind(full)
    .execute(&state.db)
    .await?;
    Ok(())
}

/// Status-only update (failures, skips): `last_run_at` moves so the
/// repository goes to the back of the queue.
async fn record_status(
    state: &AppState,
    repo_id: i64,
    status: &str,
    error: Option<&str>,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO repo_maintenance AS m (repo_id, last_run_at, status, error, updated_at)
         SELECT r.id, now(), $2, $3, now() FROM repositories r WHERE r.id = $1
         ON CONFLICT (repo_id) DO UPDATE SET
              last_run_at = now(), status = $2, error = $3, updated_at = now()",
    )
    .bind(repo_id)
    .bind(status)
    .bind(error.map(|e| e.chars().take(4096).collect::<String>()))
    .execute(&state.db)
    .await?;
    Ok(())
}

// ----- scheduler ----------------------------------------------------------------

/// Job: one scheduler pass now (admin "Run now").
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunPass {}

impl JobPayload for RunPass {
    const KIND: &'static str = "repos.maintenance_pass";
}

pub async fn run_pass_job(state: AppState, _job: RunPass) -> anyhow::Result<()> {
    run_pass(&state, true, None).await?;
    Ok(())
}

/// What one scheduler pass did.
#[derive(Debug, Clone, Default, Serialize)]
pub struct PassReport {
    /// Whether this process held the leader lock (otherwise nothing ran).
    pub leader: bool,
    pub full: Vec<i64>,
    pub incremental: Vec<i64>,
    pub commit_graph: Vec<i64>,
    pub skipped: Vec<i64>,
    pub failed: Vec<i64>,
    pub archives_pruned: usize,
}

#[derive(Debug, FromRow)]
struct DueRow {
    id: i64,
    last_run_at: Option<DateTime<Utc>>,
    last_full_at: Option<DateTime<Utc>>,
    maintained: bool,
}

/// `repos.maintenance` service: one [`run_pass`] per minute while enabled.
pub async fn service(state: AppState, shutdown: CancellationToken) -> anyhow::Result<()> {
    let mut tick = tokio::time::interval(Duration::from_secs(60));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_archive_prune: Option<std::time::Instant> = None;
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return Ok(()),
            _ = tick.tick() => {}
        }
        let enabled = match bgh_core::settings::load(&state).await {
            Ok(s) => s.git_maintenance.enabled,
            Err(err) => {
                tracing::warn!(?err, "loading settings failed");
                continue;
            }
        };
        if !enabled {
            continue;
        }
        let prune_archives =
            last_archive_prune.is_none_or(|t| t.elapsed() >= Duration::from_secs(3600));
        match run_pass(&state, prune_archives, Some(&shutdown)).await {
            Ok(report) => {
                if report.leader && prune_archives {
                    last_archive_prune = Some(std::time::Instant::now());
                }
                if !report.failed.is_empty() {
                    tracing::warn!(failed = ?report.failed, "git maintenance failed for some repositories");
                }
            }
            Err(err) => tracing::warn!(?err, "git maintenance pass failed"),
        }
    }
}

/// One scheduler pass (leader only): maintain due repositories, then (when
/// `prune_archives`) prune the archive cache.
pub async fn run_pass(
    state: &AppState,
    prune_archives: bool,
    shutdown: Option<&CancellationToken>,
) -> anyhow::Result<PassReport> {
    let mut report = PassReport::default();
    let Some(leader) = AdvisoryLock::acquire(state, LEADER_KEY, false).await? else {
        return Ok(report);
    };
    report.leader = true;
    let res = pass(state, prune_archives, shutdown, &mut report).await;
    leader.release().await;
    res.map(|()| report)
}

async fn pass(
    state: &AppState,
    prune_archives: bool,
    shutdown: Option<&CancellationToken>,
    report: &mut PassReport,
) -> anyhow::Result<()> {
    let settings = bgh_core::settings::load(state).await?;
    let cfg = &settings.git_maintenance;
    let due: Vec<DueRow> = sqlx::query_as(
        "SELECT r.id, m.last_run_at, m.last_full_at, m.repo_id IS NOT NULL AS maintained
           FROM repositories r
           LEFT JOIN repo_maintenance m ON m.repo_id = r.id
          WHERE m.repo_id IS NULL
             OR ((m.status <> 'failed' OR m.updated_at < now() - make_interval(hours => $1))
                 AND ((r.pushed_at IS NOT NULL
                       AND (m.last_run_at IS NULL OR r.pushed_at > m.last_run_at))
                      OR m.last_full_at IS NULL
                      OR m.last_full_at < now() - make_interval(days => $2)))
          ORDER BY m.last_run_at ASC NULLS FIRST, r.id
          LIMIT $3",
    )
    .bind(cfg.interval_hours as i32)
    .bind(cfg.full_interval_days as i32)
    .bind(cfg.max_repos_per_pass)
    .fetch_all(&state.db)
    .await?;

    let store = crate::store(state);
    let root = store.root.clone();
    let index =
        tokio::task::spawn_blocking(move || bgh_git::maintenance::dependents_index(&root)).await?;
    let ids: Vec<i64> = due.iter().map(|d| d.id).collect();
    let db_parents: Vec<i64> =
        sqlx::query_scalar("SELECT DISTINCT parent_id FROM repositories WHERE parent_id = ANY($1)")
            .bind(&ids)
            .fetch_all(&state.db)
            .await?;

    let now = Utc::now();
    for row in due {
        if shutdown.is_some_and(|s| s.is_cancelled()) {
            break;
        }
        let dir = store.path(row.id);
        if !store.exists(row.id) {
            record_status(
                state,
                row.id,
                "skipped",
                Some("repository storage does not exist"),
            )
            .await?;
            // Don't pick it again before the next full cycle.
            sqlx::query("UPDATE repo_maintenance SET last_full_at = now() WHERE repo_id = $1")
                .bind(row.id)
                .execute(&state.db)
                .await?;
            report.skipped.push(row.id);
            continue;
        }
        if bgh_git::maintenance::push_in_progress(&dir) {
            record_status(state, row.id, "skipped", Some("push in progress")).await?;
            report.skipped.push(row.id);
            continue;
        }
        let Some(lock) = lock_repo(state, row.id, false).await? else {
            report.skipped.push(row.id);
            continue;
        };
        let role = NetworkRole {
            has_alternates: bgh_git::maintenance::has_alternates(&dir),
            has_dependents: db_parents.contains(&row.id)
                || !bgh_git::maintenance::dependents_in(&index, &dir).is_empty(),
        };
        let res = async {
            let before = bgh_git::maintenance::object_stats(&store.git_bin, &dir).await?;
            let full_due = row
                .last_full_at
                .is_none_or(|t| now - t >= chrono::Duration::days(cfg.full_interval_days.into()));
            let repack_due = !row.maintained
                || before.loose_count >= cfg.loose_objects_threshold
                || before.pack_count >= cfg.pack_count_threshold
                || row
                    .last_run_at
                    .is_none_or(|t| now - t >= chrono::Duration::hours(cfg.interval_hours.into()));
            let task = if full_due {
                Task::Gc
            } else if repack_due {
                Task::Incremental
            } else {
                Task::CommitGraph
            };
            bgh_git::maintenance::run_task(&store.git_bin, &dir, task, role, cfg.prune_grace_days)
                .await?;
            let after = bgh_git::maintenance::object_stats(&store.git_bin, &dir).await?;
            Ok::<_, anyhow::Error>((task, after))
        }
        .await;
        lock.release().await;
        match res {
            Ok((task, stats)) => {
                record(
                    state,
                    row.id,
                    "succeeded",
                    None,
                    stats,
                    role,
                    task == Task::Gc,
                )
                .await?;
                match task {
                    Task::Gc => report.full.push(row.id),
                    Task::Incremental => report.incremental.push(row.id),
                    _ => report.commit_graph.push(row.id),
                }
            }
            Err(err) => {
                tracing::warn!(repo_id = row.id, ?err, "git maintenance failed");
                record_status(state, row.id, "failed", Some(&format!("{err:#}"))).await?;
                report.failed.push(row.id);
            }
        }
    }

    if prune_archives {
        let dir = crate::download::archive::cache_dir(state);
        let max_age = Duration::from_secs(u64::from(cfg.archive_cache_max_age_days) * 86_400);
        report.archives_pruned = bgh_git::archive::prune_cache(&dir, max_age).await?;
        report.archives_pruned += bgh_git::archive::prune_cache_to_size(
            &dir,
            cfg.archive_cache_max_size_mb.saturating_mul(1024 * 1024),
        )
        .await?;
    }
    Ok(())
}
