//! Queue processing (P39.2): the `pulls.merge_queue` job, one run per
//! `(repo, base)` under a pg advisory lock, coalesced by [`kick`].
//!
//! Each run loops over [`step`] until nothing changes:
//!
//! * **No live group** (`merge_groups.state = 'checking'`): take the first
//!   `max_entries_to_build` queued entries and stack them on the base tip
//!   per `merge_method` (merge commit / squash commit / rebase). Each
//!   cumulative commit is written to
//!   `refs/heads/gh-readonly-queue/{base}/pr-{n}-{head_sha}` and recorded
//!   on its entry (`group_ref`, `group_sha`; the queue merges that commit,
//!   never whatever the ref points at later). A conflict ejects the entry
//!   (`unmergeable`, "merge conflict"). One `merge_groups` row covers the
//!   batch (`deadline_at` = now + `check_response_timeout_minutes`) and
//!   [`Event::MergeGroupChecksRequested`] is emitted per ref.
//! * **Live group**: destroyed and rebuilt when the base tip moved
//!   (`invalidated`). When one of its entries left the queue (dequeue,
//!   close, head push, merge outside the queue) the group is cut before
//!   it: that entry and the ones behind it lose their commits
//!   (`dequeued`, behind ones back to `queued`), the ones ahead keep
//!   their commits and checks ([`truncate`]); the requeued entries are
//!   built into the next group once this one is resolved.
//!   Otherwise every entry's commit is evaluated ([`verdict`]): the
//!   required status checks of the base branch, or, with none configured,
//!   every check present on the commit (none at all = success). ALLGREEN
//!   merges the longest all-green prefix; HEADGREEN merges up to the last
//!   green entry (bounded by `max_entries_to_merge`). Merging fast-forwards
//!   the base to the prefix's last commit, marks the PRs merged
//!   (`merge_commit_sha` = their entry's commit), deletes their refs and
//!   emits `MergeGroupDestroyed{reason: "merged"}`; the rest of the batch
//!   stays live in a new group on the new base. With nothing to merge, the
//!   first failing entry ("checks failed") or, after the deadline, the
//!   first entry without green checks ("timed out") is ejected and the
//!   group rebuilt.
//! * **Merging** locks the PRs and entries, re-checks them (still queued
//!   in this group, open, head unchanged) and moves the base while the
//!   locks are held, cutting the prefix before the first entry that left.
//!   A run that moved the base but failed before recording the merge is
//!   finished by the next run: a base tip containing a prefix's last
//!   commit marks that prefix merged without moving the base again.
//!
//! Kicked by enqueue / dequeue ([`super::schedule`]), check and status
//! changes on a group commit (`jobs::checks_changed`), pushes to a queued
//! base branch ([`super::on_event`]) and the `pulls.merge_queue_sweep`
//! service (deadlines, `min_entries_to_merge_wait_minutes`).

use std::time::Duration;

use bgh_core::events::RefUpdate;
use bgh_core::jobs::{JobPayload, NOTIFY_CHANNEL};
use bgh_core::prelude::*;
use bgh_git::merge::{MergeTree, Person, RebaseResult};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::{Entry, GroupingStrategy, QueueConfig};
use crate::merge::{MergeMethod, MergeRequest};
use crate::model::{self, Pull};
use crate::protection::{self, CheckOutcome, CheckOutcomes, RequiredCheck};
use crate::{git, json as pull_json, timeline};

/// Process the merge queue of `repo_id`'s `base` branch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessQueue {
    pub repo_id: i64,
    pub base: String,
}

impl JobPayload for ProcessQueue {
    const KIND: &'static str = "pulls.merge_queue";
    const MAX_ATTEMPTS: i32 = 5;
}

/// Upper bound of [`step`]s per run (then the run re-kicks itself).
const MAX_STEPS: usize = 50;

/// Enqueue a `pulls.merge_queue` run for `(repo_id, base)` unless one is
/// already waiting (bursts of changes coalesce into one run).
pub async fn kick(
    db: impl sqlx::PgExecutor<'_>,
    repo_id: i64,
    base: &str,
) -> Result<(), sqlx::Error> {
    let payload = serde_json::to_value(ProcessQueue {
        repo_id,
        base: base.to_string(),
    })
    .map_err(|e| sqlx::Error::Encode(Box::new(e)))?;
    sqlx::query(
        "WITH j AS (
            INSERT INTO jobs (kind, payload, run_at, max_attempts)
            SELECT $1, $2, now(), $3
             WHERE NOT EXISTS (SELECT 1 FROM jobs WHERE kind = $1 AND payload = $2
                                 AND locked_at IS NULL AND failed_at IS NULL)
            RETURNING id
         )
         SELECT pg_notify($4, $1) FROM j",
    )
    .bind(ProcessQueue::KIND)
    .bind(&payload)
    .bind(ProcessQueue::MAX_ATTEMPTS)
    .bind(NOTIFY_CHANNEL)
    .execute(db)
    .await?;
    Ok(())
}

/// Job handler.
pub async fn process(state: AppState, job: ProcessQueue) -> anyhow::Result<()> {
    run(&state, job.repo_id, &job.base)
        .await
        .map_err(|e| anyhow::anyhow!("merge queue {}:{}: {e:?}", job.repo_id, job.base))
}

/// Process one queue until nothing changes. Holds one pool connection for
/// the whole run (the advisory lock's transaction).
pub async fn run(state: &AppState, repo_id: i64, base: &str) -> ApiResult<()> {
    // One run per queue at a time; held until the run ends.
    let mut lock = state.db.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("bgh.merge_queue:{repo_id}:{base}"))
        .execute(&mut *lock)
        .await?;
    let mut settled = false;
    for _ in 0..MAX_STEPS {
        if !step(state, repo_id, base).await? {
            settled = true;
            break;
        }
    }
    if !settled {
        kick(&state.db, repo_id, base).await?;
    }
    lock.commit().await?;
    Ok(())
}

/// Live group of a queue.
#[derive(Debug, Clone, sqlx::FromRow)]
struct Group {
    id: i64,
    base_ref: String,
    base_sha: String,
    head_ref: String,
    head_sha: String,
    entry_ids: Vec<i64>,
    checks_requested_at: Option<DateTime<Utc>>,
    deadline_at: Option<DateTime<Utc>>,
}

/// An entry of a group, in group order.
#[derive(Debug, Clone, sqlx::FromRow)]
struct GroupEntry {
    id: i64,
    pull_id: i64,
    enqueuer_id: Option<i64>,
    state: String,
    group_id: Option<i64>,
    group_ref: Option<String>,
    group_sha: Option<String>,
}

async fn live_group(db: &sqlx::PgPool, repo_id: i64, base: &str) -> ApiResult<Option<Group>> {
    Ok(sqlx::query_as::<_, Group>(
        "SELECT id, base_ref, base_sha, head_ref, head_sha, entry_ids, checks_requested_at,
                deadline_at
           FROM merge_groups
          WHERE repo_id = $1 AND base_ref = $2 AND state = 'checking'
          ORDER BY id DESC LIMIT 1",
    )
    .bind(repo_id)
    .bind(base)
    .fetch_optional(db)
    .await?)
}

async fn group_entries(db: &sqlx::PgPool, ids: &[i64]) -> ApiResult<Vec<GroupEntry>> {
    Ok(sqlx::query_as::<_, GroupEntry>(
        "SELECT id, pull_id, enqueuer_id, state, group_id, group_ref, group_sha
           FROM merge_queue_entries WHERE id = ANY($1)
          ORDER BY array_position($1, id)",
    )
    .bind(ids)
    .fetch_all(db)
    .await?)
}

/// One state transition; `false` when the queue is waiting (or empty).
async fn step(state: &AppState, repo_id: i64, base: &str) -> ApiResult<bool> {
    let Some(repo) = db::Repository::find(&state.db, repo_id).await? else {
        return Ok(false);
    };
    let rules = protection::rules_for(&state.db, repo_id, base).await?;
    let config = QueueConfig::from_rules(&rules);
    let store = git::store(state);
    let tip = git::branch_tip(&store, repo_id, base).await?;

    if let Some(g) = live_group(&state.db, repo_id, base).await? {
        let entries = group_entries(&state.db, &g.entry_ids).await?;
        if tip.as_deref() != Some(g.base_sha.as_str()) {
            // A run that moved the base but failed before recording the
            // merge (crash, DB error): the base now contains a prefix of
            // the group. Finish that merge instead of rebuilding on top.
            if let Some(tip) = tip.as_deref()
                && let Some(k) = landed(&store, repo_id, &entries, tip).await?
            {
                let landing = Landing::Landed {
                    tip: tip.to_string(),
                };
                merge_prefix(state, &repo, &g, &entries, k, landing).await?;
                return Ok(true);
            }
            // The base moved under the group.
            destroy(state, repo_id, &g, &entries, "invalidated", None).await?;
            return Ok(true);
        }
        let Some(config) = config else {
            // The branch lost its queue.
            destroy(state, repo_id, &g, &entries, "invalidated", None).await?;
            return Ok(true);
        };
        // An entry that left the queue invalidates itself and the entries
        // behind it (their commits contain its changes); the ones ahead
        // keep their commits and checks.
        let kept = g
            .entry_ids
            .iter()
            .zip(&entries)
            .take_while(|(id, e)| {
                e.id == **id
                    && e.state == "awaiting_checks"
                    && e.group_id == Some(g.id)
                    && e.group_sha.is_some()
            })
            .count();
        if kept < g.entry_ids.len() || g.entry_ids.is_empty() {
            truncate(state, repo_id, &g, &entries, kept).await?;
            return Ok(true);
        }
        let required = rules
            .checks
            .as_ref()
            .map(|c| c.checks.clone())
            .unwrap_or_default();
        let mut outcomes = Vec::with_capacity(entries.len());
        for e in &entries {
            let sha = e.group_sha.as_deref().unwrap_or_default();
            let o = protection::check_outcomes(&state.db, repo_id, sha).await?;
            outcomes.push(verdict(&o, &required));
        }
        let green = match config.grouping_strategy {
            GroupingStrategy::AllGreen => outcomes
                .iter()
                .take_while(|o| **o == CheckOutcome::Success)
                .count(),
            GroupingStrategy::HeadGreen => outcomes
                .iter()
                .rposition(|o| *o == CheckOutcome::Success)
                .map_or(0, |i| i + 1),
        };
        let k = green.min(config.max_entries_to_merge.max(1) as usize);
        if k > 0 {
            merge_prefix(state, &repo, &g, &entries, k, Landing::Swap).await?;
            return Ok(true);
        }
        if let Some(i) = outcomes.iter().position(|o| *o == CheckOutcome::Failure) {
            destroy(
                state,
                repo_id,
                &g,
                &entries,
                "invalidated",
                Some((i, "checks failed")),
            )
            .await?;
            return Ok(true);
        }
        if g.deadline_at.is_some_and(|d| d <= Utc::now()) {
            let i = outcomes
                .iter()
                .position(|o| *o != CheckOutcome::Success)
                .unwrap_or(0);
            destroy(
                state,
                repo_id,
                &g,
                &entries,
                "invalidated",
                Some((i, "timed out")),
            )
            .await?;
            return Ok(true);
        }
        return Ok(false);
    }

    match (config, tip) {
        (Some(config), Some(tip)) => build(state, &repo, base, &config, &tip).await,
        _ => Ok(false),
    }
}

/// Outcome of a group commit: the worst of the `required` checks (missing
/// = pending); with no required checks, the worst of every check present
/// on it (none = success).
pub fn verdict(o: &CheckOutcomes, required: &[RequiredCheck]) -> CheckOutcome {
    let all: Vec<CheckOutcome> = if required.is_empty() {
        o.by_name().into_values().collect()
    } else {
        required
            .iter()
            .map(|c| o.get(&c.context, c.app_id).unwrap_or(CheckOutcome::Pending))
            .collect()
    };
    if all.contains(&CheckOutcome::Failure) {
        CheckOutcome::Failure
    } else if all.contains(&CheckOutcome::Pending) {
        CheckOutcome::Pending
    } else {
        CheckOutcome::Success
    }
}

fn destroyed_event(repo_id: i64, g: &Group, e: &GroupEntry, reason: &str) -> Option<Event> {
    Some(Event::MergeGroupDestroyed {
        repo_id,
        group_id: g.id,
        actor_id: e.enqueuer_id,
        head_ref: e.group_ref.clone()?,
        head_sha: e.group_sha.clone()?,
        base_ref: format!("refs/heads/{}", g.base_ref),
        base_sha: g.base_sha.clone(),
        reason: reason.to_string(),
    })
}

/// Destroy group `g` (`reason`), optionally ejecting `entries[i]` as
/// unmergeable first; its other active entries go back to `queued`.
async fn destroy(
    state: &AppState,
    repo_id: i64,
    g: &Group,
    entries: &[GroupEntry],
    reason: &str,
    eject: Option<(usize, &str)>,
) -> ApiResult<()> {
    let mut tx = Tx::begin(state).await?;
    // PRs before entries (lock order, see `super::lock_pulls`).
    let pull_ids: Vec<i64> = entries.iter().map(|e| e.pull_id).collect();
    super::lock_pulls(&mut tx, &pull_ids).await?;
    let mut touched: Vec<i64> = Vec::new();
    if let Some((i, why)) = eject
        && let Some(e) = entries.get(i)
    {
        let done = sqlx::query(
            "UPDATE merge_queue_entries SET state = 'unmergeable', failure_reason = $2,
                    updated_at = now()
              WHERE id = $1 AND state IN ('queued', 'awaiting_checks', 'mergeable')",
        )
        .bind(e.id)
        .bind(why)
        .execute(&mut *tx)
        .await?;
        if done.rows_affected() > 0 {
            timeline::record(
                &mut tx,
                repo_id,
                e.pull_id,
                None,
                "removed_from_merge_queue",
                None,
                json!({"reason": why}),
            )
            .await?;
            touched.push(e.pull_id);
        }
    }
    sqlx::query(
        "UPDATE merge_groups SET state = 'destroyed', updated_at = now()
          WHERE id = $1 AND state = 'checking'",
    )
    .bind(g.id)
    .execute(&mut *tx)
    .await?;
    let reset: Vec<i64> = sqlx::query_scalar(
        "UPDATE merge_queue_entries
            SET state = 'queued', group_id = NULL, group_ref = NULL, group_sha = NULL,
                updated_at = now()
          WHERE group_id = $1 AND state IN ('awaiting_checks', 'mergeable')
          RETURNING pull_id",
    )
    .bind(g.id)
    .fetch_all(&mut *tx)
    .await?;
    touched.extend(reset);
    for e in entries {
        if let Some(ev) = destroyed_event(repo_id, g, e, reason) {
            tx.emit(ev);
        }
    }
    let scope = bgh_core::sync::repo_scope(repo_id);
    for pull_id in touched {
        pull_json::sync_pull(&mut tx, &scope, pull_id).await?;
    }
    tx.commit().await?;
    remove_refs(state, repo_id, entries).await;
    Ok(())
}

/// Cut group `g` before `entries[kept]` (it left the queue): that entry
/// and every entry behind it lose their group commits
/// (`MergeGroupDestroyed{reason: "dequeued"}`, refs deleted) and the
/// active ones go back to `queued`; `entries[..kept]` stay in `g` with
/// their commits and checks. With nothing kept the group is destroyed.
async fn truncate(
    state: &AppState,
    repo_id: i64,
    g: &Group,
    entries: &[GroupEntry],
    kept: usize,
) -> ApiResult<()> {
    if kept == 0 {
        return destroy(state, repo_id, g, entries, "dequeued", None).await;
    }
    let (prefix, cut) = entries.split_at(kept);
    let last = &prefix[kept - 1];
    let mut tx = Tx::begin(state).await?;
    let cut_pulls: Vec<i64> = cut.iter().map(|e| e.pull_id).collect();
    super::lock_pulls(&mut tx, &cut_pulls).await?;
    let prefix_ids: Vec<i64> = prefix.iter().map(|e| e.id).collect();
    sqlx::query(
        "UPDATE merge_groups SET entry_ids = $2, head_ref = $3, head_sha = $4, updated_at = now()
          WHERE id = $1 AND state = 'checking'",
    )
    .bind(g.id)
    .bind(&prefix_ids)
    .bind(last.group_ref.as_deref().unwrap_or_default())
    .bind(last.group_sha.as_deref().unwrap_or_default())
    .execute(&mut *tx)
    .await?;
    let cut_ids: Vec<i64> = cut.iter().map(|e| e.id).collect();
    let reset: Vec<i64> = sqlx::query_scalar(
        "UPDATE merge_queue_entries
            SET state = 'queued', group_id = NULL, group_ref = NULL, group_sha = NULL,
                updated_at = now()
          WHERE id = ANY($1) AND group_id = $2 AND state IN ('awaiting_checks', 'mergeable')
          RETURNING pull_id",
    )
    .bind(&cut_ids)
    .bind(g.id)
    .fetch_all(&mut *tx)
    .await?;
    for e in cut {
        if let Some(ev) = destroyed_event(repo_id, g, e, "dequeued") {
            tx.emit(ev);
        }
    }
    let scope = bgh_core::sync::repo_scope(repo_id);
    for pull_id in reset {
        pull_json::sync_pull(&mut tx, &scope, pull_id).await?;
    }
    tx.commit().await?;
    remove_refs(state, repo_id, cut).await;
    Ok(())
}

/// Delete the group refs of `entries` (best effort: a leftover ref is
/// overwritten by the next build of that entry).
async fn remove_refs(state: &AppState, repo_id: i64, entries: &[GroupEntry]) {
    let store = git::store(state);
    for r in entries.iter().filter_map(|e| e.group_ref.as_deref()) {
        if let Err(err) = bgh_git::merge::remove_ref(&store, repo_id, r).await {
            tracing::warn!(?err, repo_id, r, "merge queue: deleting group ref");
        }
    }
}

/// How [`merge_prefix`] lands its prefix on the base branch.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Landing {
    /// Fast-forward the base (compare-and-swap from the group's base).
    Swap,
    /// An earlier run already moved the base (now at `tip`) to the
    /// prefix's last commit but failed before recording it: only the
    /// bookkeeping is left.
    Landed { tip: String },
}

/// Length of the longest prefix of `entries` whose last commit is already
/// in `tip`'s history (see [`Landing::Landed`]).
async fn landed(
    store: &bgh_git::RepoStore,
    repo_id: i64,
    entries: &[GroupEntry],
    tip: &str,
) -> ApiResult<Option<usize>> {
    for (i, e) in entries.iter().enumerate().rev() {
        let Some(sha) = e.group_sha.as_deref() else {
            continue;
        };
        if sha == tip || bgh_git::merge::is_ancestor(store, repo_id, sha, tip).await? {
            return Ok(Some(i + 1));
        }
    }
    Ok(None)
}

/// A group entry and its PR as locked by [`merge_prefix`].
#[derive(Debug, sqlx::FromRow)]
struct LockedEntry {
    id: i64,
    state: String,
    group_id: Option<i64>,
    head_sha: String,
    pull_open: bool,
    pull_merged: bool,
    pull_head_sha: String,
}

/// Why locked entry `l` can't be merged by group `g` (`None` = it can).
fn unmergeable_reason(g: &Group, l: &LockedEntry) -> Option<&'static str> {
    if !l.pull_open || l.pull_merged {
        Some("closed")
    } else if l.state != "awaiting_checks" || l.group_id != Some(g.id) {
        Some("dequeued")
    } else if l.pull_head_sha != l.head_sha {
        Some("head changed")
    } else {
        None
    }
}

/// Merge `entries[..k]` (green): fast-forward the base to the last one's
/// commit and mark their PRs merged. The rest of the group stays live in
/// a new group based on the new tip.
///
/// Race-free against dequeue / close / head pushes: the PRs and entries
/// are locked (PRs first, then entries: the order of every entry writer,
/// see `super::lock_pulls`) and re-checked before the
/// base moves, and the base moves while the locks are held; the prefix is
/// cut before the first entry that can no longer merge. Idempotent: a run
/// that failed after moving the base is finished by [`Landing::Landed`].
async fn merge_prefix(
    state: &AppState,
    repo: &db::Repository,
    g: &Group,
    entries: &[GroupEntry],
    k: usize,
    landing: Landing,
) -> ApiResult<()> {
    let store = git::store(state);
    let base_ref = format!("refs/heads/{}", g.base_ref);
    let mut tx = Tx::begin(state).await?;
    // Every PR of the group (the rest's entries move to the new group),
    // then the prefix entries.
    let pull_ids: Vec<i64> = entries.iter().map(|e| e.pull_id).collect();
    let entry_ids: Vec<i64> = entries[..k].iter().map(|e| e.id).collect();
    super::lock_pulls(&mut tx, &pull_ids).await?;
    let locked: Vec<LockedEntry> = sqlx::query_as(
        "SELECT e.id, e.state, e.group_id, e.head_sha, i.state = 'open' AS pull_open,
                p.merged AS pull_merged, p.head_sha AS pull_head_sha
           FROM merge_queue_entries e
           JOIN issues i ON i.id = e.pull_id
           JOIN pull_requests p ON p.issue_id = e.pull_id
          WHERE e.id = ANY($1) ORDER BY e.id FOR UPDATE OF e",
    )
    .bind(&entry_ids)
    .fetch_all(&mut *tx)
    .await?;

    let k = if landing == Landing::Swap {
        // Cut the prefix before the first entry that can no longer merge.
        let reason = |e: &GroupEntry| match locked.iter().find(|l| l.id == e.id) {
            Some(l) => unmergeable_reason(g, l),
            None => Some("dequeued"),
        };
        let cut = entries[..k]
            .iter()
            .position(|e| reason(e).is_some())
            .unwrap_or(k);
        if cut == 0 {
            let why = reason(&entries[0]).unwrap_or("dequeued");
            tx.rollback().await?;
            return destroy(state, repo.id, g, entries, "dequeued", Some((0, why))).await;
        }
        let new_tip = entries[cut - 1].group_sha.as_deref().unwrap_or_default();
        // Moved while the rows are locked: nothing can leave the queue
        // between the check above and the commit below.
        if !bgh_git::merge::compare_and_swap_ref(&store, repo.id, &base_ref, new_tip, &g.base_sha)
            .await?
        {
            tx.rollback().await?;
            return destroy(state, repo.id, g, entries, "invalidated", None).await;
        }
        cut
    } else {
        k
    };
    let (prefix, rest) = entries.split_at(k);
    let last = &prefix[k - 1];
    let new_tip = last.group_sha.clone().unwrap_or_default();

    let scope = bgh_core::sync::repo_scope(repo.id);
    let mut prev = g.base_sha.clone();
    let mut merged: Vec<(i64, i64)> = Vec::new();
    for e in prefix {
        let sha = e.group_sha.clone().unwrap_or_default();
        let actor = e.enqueuer_id.unwrap_or(repo.owner_id);
        let done = sqlx::query(
            "UPDATE pull_requests SET merged = true, merged_at = now(), merged_by_id = $2,
                    merge_commit_sha = $3, base_sha = $4, mergeable = NULL, rebaseable = NULL,
                    mergeable_state = 'unknown', auto_merge = NULL
              WHERE issue_id = $1 AND NOT merged",
        )
        .bind(e.pull_id)
        .bind(actor)
        .bind(&sha)
        .bind(&prev)
        .execute(&mut *tx)
        .await?;
        // Swap: every entry was checked above. Landed: the commits are in
        // the base already, so the PR is merged even if it left the queue
        // after the base moved.
        sqlx::query(
            "UPDATE merge_queue_entries SET state = 'merged', updated_at = now()
              WHERE id = $1 AND (state = 'awaiting_checks' OR ($2 AND state <> 'merged'))",
        )
        .bind(e.id)
        .bind(landing != Landing::Swap)
        .execute(&mut *tx)
        .await?;
        prev = sha.clone();
        if done.rows_affected() == 0 {
            continue; // already merged
        }
        let closed = sqlx::query(
            "UPDATE issues SET state = 'closed', state_reason = NULL, closed_at = now(),
                    closed_by_id = $2, updated_at = now() WHERE id = $1 AND state = 'open'",
        )
        .bind(e.pull_id)
        .bind(actor)
        .execute(&mut *tx)
        .await?;
        if closed.rows_affected() > 0 {
            sqlx::query(
                "UPDATE repositories SET open_issues_count = greatest(open_issues_count - 1, 0)
                  WHERE id = $1",
            )
            .bind(repo.id)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query("DELETE FROM pr_requested_reviewers WHERE pull_id = $1")
            .bind(e.pull_id)
            .execute(&mut *tx)
            .await?;
        for event in ["merged", "closed"] {
            timeline::record(
                &mut tx,
                repo.id,
                e.pull_id,
                Some(actor),
                event,
                Some(&sha),
                json!({}),
            )
            .await?;
        }
        pull_json::sync_pull(&mut tx, &scope, e.pull_id).await?;
        tx.emit(Event::PullRequestMerged {
            repo_id: repo.id,
            pull_id: e.pull_id,
            actor_id: actor,
            merge_commit_sha: sha.clone(),
        });
        if let Some(ev) = destroyed_event(repo.id, g, e, "merged") {
            tx.emit(ev);
        }
        merged.push((e.pull_id, actor));
    }
    let prefix_ids: Vec<i64> = prefix.iter().map(|e| e.id).collect();
    sqlx::query(
        "UPDATE merge_groups SET state = 'merged', entry_ids = $2, head_ref = $3, head_sha = $4,
                updated_at = now()
          WHERE id = $1",
    )
    .bind(g.id)
    .bind(&prefix_ids)
    .bind(last.group_ref.as_deref().unwrap_or_default())
    .bind(&new_tip)
    .execute(&mut *tx)
    .await?;
    if !rest.is_empty() {
        // The remaining refs are stacked on the merged prefix: still valid
        // (entries that left the queue are dropped by the next step).
        let rest_ids: Vec<i64> = rest.iter().map(|e| e.id).collect();
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO merge_groups (repo_id, base_ref, base_sha, head_ref, head_sha, state,
                                       entry_ids, checks_requested_at, deadline_at)
             VALUES ($1, $2, $3, $4, $5, 'checking', $6, $7, $8) RETURNING id",
        )
        .bind(repo.id)
        .bind(&g.base_ref)
        .bind(&new_tip)
        .bind(&g.head_ref)
        .bind(&g.head_sha)
        .bind(&rest_ids)
        .bind(g.checks_requested_at)
        .bind(g.deadline_at)
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE merge_queue_entries SET group_id = $2, updated_at = now()
              WHERE id = ANY($1) AND group_id = $3",
        )
        .bind(&rest_ids)
        .bind(id)
        .bind(g.id)
        .execute(&mut *tx)
        .await?;
        for e in rest {
            pull_json::sync_pull(&mut tx, &scope, e.pull_id).await?;
        }
    }
    let pusher_id = merged.last().map(|(_, a)| *a);
    // pushed_at / size / Event::Push (re-syncs PRs targeting the base),
    // atomically with the merge. Also for a landed base that moved on
    // since: the queue's own update (group base -> prefix head) was never
    // processed.
    tx.enqueue(&bgh_repos::jobs::PostReceive {
        repo_id: repo.id,
        pusher_id,
        updates: vec![RefUpdate {
            old: g.base_sha.clone(),
            new: new_tip,
            refname: base_ref,
        }],
    })
    .await?;
    tx.commit().await?;
    remove_refs(state, repo.id, prefix).await;

    // Head branch deletion is git work outside the transaction (best
    // effort, with its own post-receive).
    if repo.delete_branch_on_merge {
        let mut updates = Vec::new();
        for (pull_id, actor) in &merged {
            let deleted = match model::find_by_id(&state.db, *pull_id).await? {
                Some(pull) => crate::merge::delete_head_branch(state, repo, &pull, *actor).await,
                None => Ok(None),
            };
            match deleted {
                Ok(Some(u)) => updates.push(u),
                Ok(None) => {}
                Err(err) => {
                    tracing::warn!(?err, repo_id = repo.id, pull_id, "merge queue: head branch");
                }
            }
        }
        if !updates.is_empty() {
            bgh_core::jobs::enqueue_job(
                &state.db,
                &bgh_repos::jobs::PostReceive {
                    repo_id: repo.id,
                    pusher_id,
                    updates,
                },
            )
            .await?;
        }
    }
    Ok(())
}

/// Name of the group ref of PR `number` queued at `head_sha`.
pub fn group_ref(base: &str, number: i64, head_sha: &str) -> String {
    format!("refs/heads/gh-readonly-queue/{base}/pr-{number}-{head_sha}")
}

/// Build a new group from the first queued entries on `tip`.
async fn build(
    state: &AppState,
    repo: &db::Repository,
    base: &str,
    config: &QueueConfig,
    tip: &str,
) -> ApiResult<bool> {
    let active = super::entries_for(&state.db, repo.id, base).await?;
    let scope = bgh_core::sync::repo_scope(repo.id);
    // Entries of a group that no longer exists (repair).
    let orphans: Vec<&Entry> = active.iter().filter(|e| e.state != "queued").collect();
    if !orphans.is_empty() {
        let mut tx = Tx::begin(state).await?;
        let pull_ids: Vec<i64> = orphans.iter().map(|e| e.pull_id).collect();
        super::lock_pulls(&mut tx, &pull_ids).await?;
        for e in &orphans {
            sqlx::query(
                "UPDATE merge_queue_entries
                    SET state = 'queued', group_id = NULL, group_ref = NULL, group_sha = NULL,
                        updated_at = now()
                  WHERE id = $1",
            )
            .bind(e.id)
            .execute(&mut *tx)
            .await?;
            pull_json::sync_pull(&mut tx, &scope, e.pull_id).await?;
        }
        tx.commit().await?;
        return Ok(true);
    }
    let queued: Vec<Entry> = active
        .into_iter()
        .take(config.max_entries_to_build.max(1) as usize)
        .collect();
    let Some(oldest) = queued.iter().map(|e| e.enqueued_at).min() else {
        return Ok(false);
    };
    if (queued.len() as i64) < config.min_entries_to_merge
        && oldest + chrono::Duration::minutes(config.min_entries_to_merge_wait_minutes) > Utc::now()
    {
        return Ok(false); // wait for more entries (the sweep comes back)
    }

    let store = git::store(state);
    let mut prev = tip.to_string();
    let mut built: Vec<(&Entry, String, String)> = Vec::new();
    let mut conflicts: Vec<&Entry> = Vec::new();
    for e in &queued {
        let Some(pull) = model::find_by_id(&state.db, e.pull_id).await? else {
            continue;
        };
        match build_one(state, repo, &pull, e, config.merge_method, &prev).await? {
            Some(sha) => {
                let r = group_ref(base, e.number, &e.head_sha);
                bgh_git::merge::force_ref(&store, repo.id, &r, &sha).await?;
                prev = sha.clone();
                built.push((e, r, sha));
            }
            None => conflicts.push(e),
        }
    }

    let mut tx = Tx::begin(state).await?;
    // PRs before entries (lock order, see `super::lock_pulls`).
    let pull_ids: Vec<i64> = conflicts
        .iter()
        .map(|e| e.pull_id)
        .chain(built.iter().map(|(e, _, _)| e.pull_id))
        .collect();
    super::lock_pulls(&mut tx, &pull_ids).await?;
    for e in &conflicts {
        sqlx::query(
            "UPDATE merge_queue_entries SET state = 'unmergeable', failure_reason = 'merge conflict',
                    updated_at = now()
              WHERE id = $1 AND state = 'queued'",
        )
        .bind(e.id)
        .execute(&mut *tx)
        .await?;
        timeline::record(
            &mut tx,
            repo.id,
            e.pull_id,
            None,
            "removed_from_merge_queue",
            None,
            json!({"reason": "merge conflict"}),
        )
        .await?;
        pull_json::sync_pull(&mut tx, &scope, e.pull_id).await?;
    }
    // Entries dequeued while building leave the batch, with every entry
    // built on top of them (their commits contain its changes).
    let built_ids: Vec<i64> = built.iter().map(|(e, _, _)| e.id).collect();
    let still_queued: Vec<i64> = sqlx::query_scalar(
        "SELECT id FROM merge_queue_entries WHERE id = ANY($1) AND state = 'queued'
          ORDER BY id FOR UPDATE",
    )
    .bind(&built_ids)
    .fetch_all(&mut *tx)
    .await?;
    let cut = built
        .iter()
        .position(|(e, _, _)| !still_queued.contains(&e.id))
        .unwrap_or(built.len());
    let stale = built.split_off(cut);
    if let Some((_, head_ref, head_sha)) = built.last() {
        let ids: Vec<i64> = built.iter().map(|(e, _, _)| e.id).collect();
        let group_id: i64 = sqlx::query_scalar(
            "INSERT INTO merge_groups (repo_id, base_ref, base_sha, head_ref, head_sha, state,
                                       entry_ids, checks_requested_at, deadline_at)
             VALUES ($1, $2, $3, $4, $5, 'checking', $6, now(),
                     now() + make_interval(mins => $7))
             RETURNING id",
        )
        .bind(repo.id)
        .bind(base)
        .bind(tip)
        .bind(head_ref)
        .bind(head_sha)
        .bind(&ids)
        .bind(config.check_response_timeout_minutes.clamp(1, 60 * 24 * 7) as i32)
        .fetch_one(&mut *tx)
        .await?;
        for (e, r, sha) in &built {
            sqlx::query(
                "UPDATE merge_queue_entries
                    SET state = 'awaiting_checks', group_id = $2, group_ref = $3, group_sha = $4,
                        updated_at = now()
                  WHERE id = $1 AND state = 'queued'",
            )
            .bind(e.id)
            .bind(group_id)
            .bind(r)
            .bind(sha)
            .execute(&mut *tx)
            .await?;
            pull_json::sync_pull(&mut tx, &scope, e.pull_id).await?;
            tx.emit(Event::MergeGroupChecksRequested {
                repo_id: repo.id,
                group_id,
                actor_id: e.enqueuer_id,
                head_ref: r.clone(),
                head_sha: sha.clone(),
                base_ref: format!("refs/heads/{base}"),
                base_sha: tip.to_string(),
            });
        }
    }
    tx.commit().await?;
    for (_, r, _) in &stale {
        if let Err(err) = bgh_git::merge::remove_ref(&store, repo.id, r).await {
            tracing::warn!(
                ?err,
                repo_id = repo.id,
                r,
                "merge queue: deleting group ref"
            );
        }
    }
    Ok(!built.is_empty() || !conflicts.is_empty() || !stale.is_empty())
}

/// Commit of entry `e` (its queued head) on `prev` per `method`; `None`
/// on a conflict.
async fn build_one(
    state: &AppState,
    repo: &db::Repository,
    pull: &Pull,
    e: &Entry,
    method: MergeMethod,
    prev: &str,
) -> ApiResult<Option<String>> {
    let store = git::store(state);
    let committer_id = git::site_committer(state);
    let committer = Person::from(&committer_id);
    if method == MergeMethod::Rebase {
        let base = bgh_git::merge::merge_base(&store, repo.id, prev, &e.head_sha)
            .await?
            .unwrap_or_else(|| prev.to_string());
        return Ok(
            match bgh_git::merge::rebase(&store, repo.id, prev, &base, &e.head_sha, &committer_id)
                .await?
            {
                RebaseResult::Done { head } => Some(head),
                RebaseResult::Conflict { .. } | RebaseResult::HasMerges => None,
            },
        );
    }
    let tree = match bgh_git::merge::merge_tree(&store, repo.id, prev, &e.head_sha, None).await? {
        MergeTree::Clean { tree } => tree,
        MergeTree::Conflict { .. } => return Ok(None),
    };
    let head_label = crate::merge::head_label(state, pull).await?;
    let req = MergeRequest {
        method,
        commit_title: None,
        commit_message: None,
        sha: None,
        via_merge_queue: true,
    };
    let (title, message) = crate::merge::messages(state, repo, pull, &head_label, &req).await?;
    let full_message = if message.is_empty() {
        format!("{title}\n")
    } else {
        format!("{title}\n\n{message}\n")
    };
    let sha = if method == MergeMethod::Merge {
        let author = person(state, e.enqueuer_id, &committer).await?;
        bgh_git::merge::commit_tree(
            &store,
            repo.id,
            &tree,
            &[prev, &e.head_sha],
            &full_message,
            &author,
            &committer,
        )
        .await?
    } else {
        let author = person(state, pull.issue.author_id, &committer).await?;
        bgh_git::merge::commit_tree(
            &store,
            repo.id,
            &tree,
            &[prev],
            &full_message,
            &author,
            &committer,
        )
        .await?
    };
    Ok(Some(sha))
}

/// Git identity of user `id`, `fallback` without one.
async fn person(state: &AppState, id: Option<i64>, fallback: &Person) -> ApiResult<Person> {
    let user = match id {
        Some(id) => db::User::find(&state.db, id).await?,
        None => None,
    };
    Ok(match user {
        Some(u) => Person::from(&git::identity(state, &u).await?),
        None => fallback.clone(),
    })
}

/// Statuses / checks changed for `sha`: kick the queues whose live group
/// built it.
pub async fn on_checks_changed(state: &AppState, repo_id: i64, sha: &str) -> ApiResult<()> {
    let bases: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT g.base_ref FROM merge_queue_entries e
           JOIN merge_groups g ON g.id = e.group_id
          WHERE e.repo_id = $1 AND e.group_sha = $2 AND g.state = 'checking'",
    )
    .bind(repo_id)
    .bind(sha)
    .fetch_all(&state.db)
    .await?;
    for base in bases {
        kick(&state.db, repo_id, &base).await?;
    }
    Ok(())
}

/// Kick queues that need time-based progress: live groups past their
/// deadline, and queued entries without a live group (e.g. waiting for
/// `min_entries_to_merge`). Returns the number of queues kicked.
pub async fn sweep(state: &AppState) -> ApiResult<usize> {
    let queues: Vec<(i64, String)> = sqlx::query_as(
        "SELECT repo_id, base_ref FROM merge_groups
          WHERE state = 'checking' AND deadline_at <= now()
         UNION
         SELECT e.repo_id, e.base_ref FROM merge_queue_entries e
          WHERE e.state = 'queued'
            AND NOT EXISTS (SELECT 1 FROM merge_groups g
                             WHERE g.repo_id = e.repo_id AND g.base_ref = e.base_ref
                               AND g.state = 'checking')",
    )
    .fetch_all(&state.db)
    .await?;
    for (repo_id, base) in &queues {
        kick(&state.db, *repo_id, base).await?;
    }
    Ok(queues.len())
}

/// The `pulls.merge_queue_sweep` service: [`sweep`] every minute.
pub async fn sweep_service(state: AppState, shutdown: CancellationToken) -> anyhow::Result<()> {
    let mut tick = tokio::time::interval(Duration::from_secs(60));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return Ok(()),
            _ = tick.tick() => {}
        }
        if let Err(err) = sweep(&state).await {
            tracing::warn!(?err, "merge queue sweep failed");
        }
    }
}
