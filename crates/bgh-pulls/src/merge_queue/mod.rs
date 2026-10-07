//! Merge queue (P39). A `merge_queue` ruleset rule on a branch refuses
//! direct merges ([`protection::MERGE_QUEUE_REQUIRED`]); PRs are added to
//! the branch's queue instead and merged by the queue, which bypasses just
//! that blocker (`MergeRequest::via_merge_queue`).
//!
//! This module holds the queue itself: settings ([`config_for`]), entries
//! (`merge_queue_entries`, ordered jump entries first, then by enqueue
//! time) and enqueue / dequeue with their timeline events
//! (`added_to_merge_queue`, `removed_from_merge_queue` `{"reason"}`).
//! Entries leave the queue automatically when the PR is closed or its head
//! changes. Building merge groups and merging is the queue processing
//! service (P39.2), started through [`schedule`].
//!
//! `/_bgh` endpoints: [`web`].

mod config;
pub mod web;

use bgh_core::prelude::*;
use bgh_repos::protection::Actor;
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::json;

use crate::model::Pull;
use crate::protection::{self, BlockerKind};
use crate::{json as pull_json, timeline};

pub use config::{GroupingStrategy, QueueConfig, config_for};

/// Entry states in which a PR is in the queue.
pub const ACTIVE_STATES: [&str; 3] = ["queued", "awaiting_checks", "mergeable"];

/// One `merge_queue_entries` row with its PR's number, title and author,
/// its merge group's head and its 1-based position (active entries only;
/// 0 otherwise).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Entry {
    pub id: i64,
    pub repo_id: i64,
    pub pull_id: i64,
    pub base_ref: String,
    pub head_sha: String,
    pub enqueuer_id: Option<i64>,
    pub state: String,
    pub jump: bool,
    pub group_id: Option<i64>,
    pub failure_reason: Option<String>,
    pub enqueued_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub number: i64,
    pub title: String,
    pub author_id: Option<i64>,
    pub group_head_sha: Option<String>,
    pub position: i64,
}

impl Entry {
    pub fn is_active(&self) -> bool {
        ACTIVE_STATES.contains(&self.state.as_str())
    }
}

/// Active entries in queue order; `{filter}` narrows the window.
const ENTRIES_SQL: &str = "
    SELECT e.id, e.repo_id, e.pull_id, e.base_ref, e.head_sha, e.enqueuer_id, e.state,
           e.jump, e.group_id, e.failure_reason, e.enqueued_at, e.updated_at,
           i.number::bigint AS number, i.title, i.author_id, g.head_sha AS group_head_sha,
           row_number() OVER (ORDER BY e.jump DESC, e.enqueued_at, e.id) AS position
      FROM merge_queue_entries e
      JOIN issues i ON i.id = e.pull_id
      LEFT JOIN merge_groups g ON g.id = e.group_id
     WHERE e.repo_id = $1 AND e.base_ref = $2
       AND e.state IN ('queued', 'awaiting_checks', 'mergeable')
     ORDER BY position";

/// The active entries of `repo_id`'s `base` queue, in order (positions
/// 1, 2, ...).
pub async fn entries_for(
    db: impl sqlx::PgExecutor<'_>,
    repo_id: i64,
    base: &str,
) -> ApiResult<Vec<Entry>> {
    Ok(sqlx::query_as::<_, Entry>(ENTRIES_SQL)
        .bind(repo_id)
        .bind(base)
        .fetch_all(db)
        .await?)
}

/// The active entry of `pull_id`, with its position.
pub async fn entry_for_pull(db: &sqlx::PgPool, pull_id: i64) -> ApiResult<Option<Entry>> {
    let key: Option<(i64, String)> = sqlx::query_as(
        "SELECT repo_id, base_ref FROM merge_queue_entries
          WHERE pull_id = $1 AND state IN ('queued', 'awaiting_checks', 'mergeable')",
    )
    .bind(pull_id)
    .fetch_optional(db)
    .await?;
    let Some((repo_id, base)) = key else {
        return Ok(None);
    };
    Ok(entries_for(db, repo_id, &base)
        .await?
        .into_iter()
        .find(|e| e.pull_id == pull_id))
}

/// Add `pull` to its base branch's merge queue as `actor` (write access;
/// `jump` = put it in front, admin only). Returns the entry and whether
/// it was added now (an already queued PR returns its entry, `false`). 422 when the base has no merge queue, the PR is
/// not open, a draft, conflicting, or blocked by requirements other than
/// status checks (those run on the merge group).
pub async fn enqueue(
    state: &AppState,
    access: &RepoAccess,
    pull: &Pull,
    actor: &db::User,
    jump: bool,
) -> ApiResult<(Entry, bool)> {
    access.require(Permission::Write)?;
    access.require_not_archived()?;
    if jump {
        access.require(Permission::Admin)?;
    }
    if !pull.is_open() || pull.pr.merged {
        return Err(ApiError::unprocessable("Pull request is closed"));
    }
    if pull.pr.draft {
        return Err(ApiError::unprocessable("Pull request is still a draft"));
    }
    if let Some(e) = entry_for_pull(&state.db, pull.id()).await? {
        return Ok((e, false));
    }
    let repo = &access.repo;
    let rules = protection::rules_for(&state.db, repo.id, &pull.pr.base_ref).await?;
    if QueueConfig::from_rules(&rules).is_none() {
        return Err(ApiError::unprocessable(format!(
            "Merge queue is not enabled for branch {}",
            pull.pr.base_ref
        )));
    }
    if pull.pr.mergeable == Some(false) {
        return Err(ApiError::unprocessable(
            "Pull request has merge conflicts that must be resolved",
        ));
    }
    let ev = protection::evaluate(state, repo, pull, &rules).await?;
    let who = Actor::for_user(state, &access.owner, actor.id, access.permission).await?;
    let blocking: Vec<String> = ev
        .unbypassed(&rules, &who)
        .into_iter()
        .filter(|b| !matches!(b.kind, BlockerKind::Check | BlockerKind::MergeQueue))
        .map(|b| b.message.clone())
        .fold(Vec::new(), |mut v, m| {
            if !v.contains(&m) {
                v.push(m);
            }
            v
        });
    if !blocking.is_empty() {
        return Err(ApiError::unprocessable(format!(
            "Pull request is not ready for the merge queue: {}",
            blocking.join(" ")
        )));
    }

    let mut tx = Tx::begin(state).await?;
    let Some(locked) = crate::model::lock(&mut *tx, pull.id()).await? else {
        return Err(ApiError::NotFound);
    };
    if !locked.is_open() || locked.pr.merged || locked.pr.base_ref != pull.pr.base_ref {
        return Err(ApiError::conflict(
            "Pull request was modified. Review and try again.",
        ));
    }
    let id: Option<i64> = sqlx::query_scalar(
        "INSERT INTO merge_queue_entries (repo_id, pull_id, base_ref, head_sha, enqueuer_id, jump)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (pull_id) WHERE state IN ('queued', 'awaiting_checks', 'mergeable')
         DO NOTHING RETURNING id",
    )
    .bind(repo.id)
    .bind(pull.id())
    .bind(&locked.pr.base_ref)
    .bind(&locked.pr.head_sha)
    .bind(actor.id)
    .bind(jump)
    .fetch_optional(&mut *tx)
    .await?;
    if id.is_some() {
        timeline::record(
            &mut tx,
            repo.id,
            pull.id(),
            Some(actor.id),
            "added_to_merge_queue",
            None,
            json!({}),
        )
        .await?;
        pull_json::sync_pull(&mut tx, &access.scope(), pull.id()).await?;
        schedule(&mut tx, repo.id, &locked.pr.base_ref).await?;
    }
    tx.commit().await?;
    let entry = entry_for_pull(&state.db, pull.id())
        .await?
        .ok_or_else(|| ApiError::conflict("Pull request left the merge queue"))?;
    Ok((entry, id.is_some()))
}

/// Remove `pull_id`'s active entry inside `tx` (`actor_id` None = system),
/// recording `removed_from_merge_queue` `{"reason"}`. The caller syncs the
/// PR (`json::sync_pull`). Returns whether an entry was removed.
pub async fn remove_in_tx(
    tx: &mut Tx,
    repo_id: i64,
    pull_id: i64,
    actor_id: Option<i64>,
    reason: &str,
) -> ApiResult<bool> {
    let base: Option<String> = sqlx::query_scalar(
        "UPDATE merge_queue_entries SET state = 'removed', failure_reason = $2, updated_at = now()
          WHERE pull_id = $1 AND state IN ('queued', 'awaiting_checks', 'mergeable')
          RETURNING base_ref",
    )
    .bind(pull_id)
    .bind(reason)
    .fetch_optional(&mut **tx)
    .await?;
    let Some(base) = base else {
        return Ok(false);
    };
    timeline::record(
        tx,
        repo_id,
        pull_id,
        actor_id,
        "removed_from_merge_queue",
        None,
        json!({"reason": reason}),
    )
    .await?;
    schedule(tx, repo_id, &base).await?;
    Ok(true)
}

/// Remove `pull_id` from the merge queue (`actor_id` None = system).
/// Returns whether it was queued.
pub async fn dequeue(
    state: &AppState,
    repo_id: i64,
    pull_id: i64,
    actor_id: Option<i64>,
    reason: &str,
) -> ApiResult<bool> {
    let mut tx = Tx::begin(state).await?;
    if !remove_in_tx(&mut tx, repo_id, pull_id, actor_id, reason).await? {
        return Ok(false);
    }
    pull_json::sync_pull(&mut tx, &bgh_core::sync::repo_scope(repo_id), pull_id).await?;
    tx.commit().await?;
    Ok(true)
}

/// The queue of `repo_id`'s `base` changed (entry added or removed):
/// (re)start its processing in `tx`. The processing service (building
/// merge groups, merging) lands in P39.2; until then entries just wait.
pub async fn schedule(tx: &mut Tx, repo_id: i64, base: &str) -> ApiResult<()> {
    let _ = (tx, repo_id, base);
    Ok(())
}

/// Event listener: a PR closed or merged outside the queue leaves it.
pub async fn on_event(state: AppState, event: std::sync::Arc<Event>) -> anyhow::Result<()> {
    let (repo_id, pull_id, actor_id, reason) = match &*event {
        Event::PullRequestClosed {
            repo_id,
            pull_id,
            actor_id,
        } => (*repo_id, *pull_id, *actor_id, "closed"),
        Event::PullRequestMerged {
            repo_id,
            pull_id,
            actor_id,
            ..
        } => (*repo_id, *pull_id, *actor_id, "merged"),
        _ => return Ok(()),
    };
    // Entries in a merge group are finished by the queue itself (P39.2).
    let queued: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM merge_queue_entries
                         WHERE pull_id = $1 AND state = 'queued' AND group_id IS NULL)",
    )
    .bind(pull_id)
    .fetch_one(&state.db)
    .await?;
    if queued {
        dequeue(&state, repo_id, pull_id, Some(actor_id), reason)
            .await
            .map_err(|e| anyhow::anyhow!("dequeue pull {pull_id}: {e:?}"))?;
    }
    Ok(())
}

/// `Entry` JSON (`/_bgh` contract shared with the web client).
#[derive(Debug, Serialize)]
pub struct EntryJson {
    pub id: i64,
    pub position: i64,
    pub state: String,
    pub base_ref: String,
    pub head_sha: String,
    pub jump: bool,
    pub pull: EntryPull,
    pub enqueuer: api::SimpleUser,
    pub enqueued_at: Timestamp,
    /// Seconds; not estimated yet.
    pub estimated_time_to_merge: Option<i64>,
    pub group_head_sha: Option<String>,
    pub failure_reason: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct EntryPull {
    pub number: i64,
    pub title: String,
    pub user: api::SimpleUser,
}

/// Render entries (one user query).
pub async fn render(state: &AppState, entries: &[Entry]) -> ApiResult<Vec<EntryJson>> {
    let users = bgh_core::views::users_by_id(
        state,
        entries.iter().flat_map(|e| [e.enqueuer_id, e.author_id]),
    )
    .await?;
    let user =
        |id: Option<i64>| api::SimpleUser::or_ghost(&state.urls, id.and_then(|i| users.get(&i)));
    Ok(entries
        .iter()
        .map(|e| EntryJson {
            id: e.id,
            position: e.position,
            state: e.state.clone(),
            base_ref: e.base_ref.clone(),
            head_sha: e.head_sha.clone(),
            jump: e.jump,
            pull: EntryPull {
                number: e.number,
                title: e.title.clone(),
                user: user(e.author_id),
            },
            enqueuer: user(e.enqueuer_id),
            enqueued_at: e.enqueued_at.into(),
            estimated_time_to_merge: None,
            group_head_sha: e.group_head_sha.clone(),
            failure_reason: e.failure_reason.clone(),
        })
        .collect())
}
