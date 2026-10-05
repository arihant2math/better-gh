//! Internal insert API for importers (P18 metadata import).
//!
//! Unlike the REST handlers these keep the source's numbers, authors and
//! timestamps and emit **no** domain events (no webhooks, notifications,
//! activity, mention processing or subscriptions): an import is history,
//! not news. They still record sync actions so open clients see the rows.
//! Callers own idempotency (`bgh-import` maps source ids to local ids in
//! the same transaction) and call [`finish_repo`] at the end.

use bgh_core::prelude::*;
use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::service;

/// A label as it exists on the source. Re-importing a name updates it.
pub async fn upsert_label(
    tx: &mut Tx,
    repo_id: i64,
    name: &str,
    color: &str,
    description: Option<&str>,
    is_default: bool,
) -> ApiResult<i64> {
    let color = if color.len() == 6 && color.bytes().all(|b| b.is_ascii_hexdigit()) {
        color.to_ascii_lowercase()
    } else {
        "ededed".to_string()
    };
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO labels (repo_id, name, color, description, is_default)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (repo_id, lower(name)) DO UPDATE
            SET color = EXCLUDED.color, description = EXCLUDED.description, updated_at = now()
         RETURNING id",
    )
    .bind(repo_id)
    .bind(name)
    .bind(&color)
    .bind(description)
    .bind(is_default)
    .fetch_one(&mut **tx)
    .await?;
    tx.sync_model(SyncModel::Label, id, SyncAction::Insert)
        .await?;
    Ok(id)
}

/// A milestone with its source number and timestamps.
pub struct NewMilestone<'a> {
    pub number: i64,
    pub title: &'a str,
    pub description: Option<&'a str>,
    pub state: &'a str,
    pub creator_id: Option<i64>,
    pub due_on: Option<DateTime<Utc>>,
    pub closed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub async fn insert_milestone(tx: &mut Tx, repo_id: i64, m: &NewMilestone<'_>) -> ApiResult<i64> {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO milestones (repo_id, number, title, description, state, creator_id,
                                 due_on, closed_at, created_at, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING id",
    )
    .bind(repo_id)
    .bind(m.number)
    .bind(m.title)
    .bind(m.description)
    .bind(if m.state == "closed" {
        "closed"
    } else {
        "open"
    })
    .bind(m.creator_id)
    .bind(m.due_on)
    .bind(m.closed_at)
    .bind(m.created_at)
    .bind(m.updated_at)
    .fetch_one(&mut **tx)
    .await?;
    sqlx::query(
        "UPDATE repositories SET next_milestone_number = GREATEST(next_milestone_number, $2 + 1)
         WHERE id = $1",
    )
    .bind(repo_id)
    .bind(m.number)
    .execute(&mut **tx)
    .await?;
    tx.sync_model(SyncModel::Milestone, id, SyncAction::Insert)
        .await?;
    Ok(id)
}

/// An issue with its source number, state and timestamps.
pub struct NewIssue<'a> {
    pub number: i64,
    pub title: &'a str,
    pub body: Option<&'a str>,
    pub state: &'a str,
    pub state_reason: Option<&'a str>,
    pub author_id: Option<i64>,
    pub milestone_id: Option<i64>,
    pub locked: bool,
    pub active_lock_reason: Option<&'a str>,
    pub closed_at: Option<DateTime<Utc>>,
    pub closed_by_id: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub label_ids: &'a [i64],
    pub assignee_ids: &'a [i64],
}

const STATE_REASONS: &[&str] = &["completed", "not_planned", "reopened", "duplicate"];
const LOCK_REASONS: &[&str] = &["off-topic", "too heated", "resolved", "spam"];

pub async fn insert_issue(tx: &mut Tx, repo_id: i64, i: &NewIssue<'_>) -> ApiResult<i64> {
    let open = i.state != "closed";
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO issues (repo_id, number, title, body, state, state_reason, author_id,
                             milestone_id, locked, active_lock_reason, closed_at, closed_by_id,
                             created_at, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14) RETURNING id",
    )
    .bind(repo_id)
    .bind(i.number)
    .bind(i.title)
    .bind(i.body)
    .bind(if open { "open" } else { "closed" })
    .bind(i.state_reason.filter(|r| STATE_REASONS.contains(r)))
    .bind(i.author_id)
    .bind(i.milestone_id)
    .bind(i.locked)
    .bind(
        i.active_lock_reason
            .filter(|r| i.locked && LOCK_REASONS.contains(r)),
    )
    .bind(if open {
        None
    } else {
        i.closed_at.or(Some(i.updated_at))
    })
    .bind(if open { None } else { i.closed_by_id })
    .bind(i.created_at)
    .bind(i.updated_at)
    .fetch_one(&mut **tx)
    .await?;
    if !i.label_ids.is_empty() {
        sqlx::query(
            "INSERT INTO issue_labels (issue_id, label_id, created_at)
             SELECT $1, unnest($2::bigint[]), $3 ON CONFLICT DO NOTHING",
        )
        .bind(id)
        .bind(i.label_ids)
        .bind(i.created_at)
        .execute(&mut **tx)
        .await?;
    }
    if !i.assignee_ids.is_empty() {
        sqlx::query(
            "INSERT INTO issue_assignees (issue_id, user_id, created_at)
             SELECT $1, unnest($2::bigint[]), $3 ON CONFLICT DO NOTHING",
        )
        .bind(id)
        .bind(i.assignee_ids)
        .bind(i.created_at)
        .execute(&mut **tx)
        .await?;
    }
    sqlx::query(
        "UPDATE repositories SET next_issue_number = GREATEST(next_issue_number, $2 + 1),
                open_issues_count = open_issues_count + $3
         WHERE id = $1",
    )
    .bind(repo_id)
    .bind(i.number)
    .bind(i64::from(open))
    .execute(&mut **tx)
    .await?;
    tx.sync_issue(id, SyncAction::Insert, true).await?;
    Ok(id)
}

/// An issue comment; bumps the issue's `comments_count`.
pub async fn insert_comment(
    tx: &mut Tx,
    issue_id: i64,
    author_id: Option<i64>,
    body: &str,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
) -> ApiResult<i64> {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO comments (issue_id, repo_id, author_id, body, created_at, updated_at)
         SELECT $1, repo_id, $2, $3, $4, $5 FROM issues WHERE id = $1 RETURNING id",
    )
    .bind(issue_id)
    .bind(author_id)
    .bind(body)
    .bind(created_at)
    .bind(updated_at)
    .fetch_one(&mut **tx)
    .await?;
    sqlx::query("UPDATE issues SET comments_count = comments_count + 1 WHERE id = $1")
        .bind(issue_id)
        .execute(&mut **tx)
        .await?;
    service::sync_comment(tx, id, SyncAction::Insert).await?;
    tx.sync_issue(issue_id, SyncAction::Update, false).await?;
    Ok(id)
}

/// A timeline event. `data` must use the client keys of
/// BACKEND_PATTERNS.md §8a (`label: {name, color}`, `assignee_id`,
/// `milestone: {title}`, `rename: {from, to}`, `state_reason`,
/// `lock_reason`).
pub async fn insert_event(
    tx: &mut Tx,
    issue_id: i64,
    actor_id: Option<i64>,
    event: &str,
    commit_id: Option<&str>,
    data: Value,
    created_at: DateTime<Utc>,
) -> ApiResult<i64> {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO issue_events (issue_id, repo_id, actor_id, event, commit_id, data, created_at)
         SELECT $1, repo_id, $2, $3, $4, $5, $6 FROM issues WHERE id = $1 RETURNING id",
    )
    .bind(issue_id)
    .bind(actor_id)
    .bind(event)
    .bind(commit_id)
    .bind(data)
    .bind(created_at)
    .fetch_one(&mut **tx)
    .await?;
    tx.sync_model(SyncModel::IssueEvent, id, SyncAction::Insert)
        .await?;
    Ok(id)
}

/// Reactions of one user on an issue (`"issue"`) or issue comment
/// (`"issue_comment"`). Re-syncs the subject (`reactions` rollup).
pub async fn insert_reactions(
    tx: &mut Tx,
    subject_type: &str,
    subject_id: i64,
    reactions: &[(i64, String, DateTime<Utc>)],
) -> ApiResult<()> {
    for (user_id, content, at) in reactions {
        sqlx::query(
            "INSERT INTO reactions (subject_type, subject_id, user_id, content, created_at)
             VALUES ($1, $2, $3, $4, $5) ON CONFLICT DO NOTHING",
        )
        .bind(subject_type)
        .bind(subject_id)
        .bind(user_id)
        .bind(content)
        .bind(at)
        .execute(&mut **tx)
        .await?;
    }
    match subject_type {
        "issue" => {
            tx.sync_issue(subject_id, SyncAction::Update, false).await?;
        }
        _ => service::sync_comment(tx, subject_id, SyncAction::Update).await?,
    }
    Ok(())
}

/// After an import: issue numbers continue after `max_number` (the
/// highest source issue *or* PR number, so PRs imported later keep
/// theirs), milestone counters are recomputed and the repository re-synced.
pub async fn finish_repo(tx: &mut Tx, repo_id: i64, max_number: i64) -> ApiResult<()> {
    sqlx::query(
        "UPDATE repositories SET next_issue_number = GREATEST(next_issue_number, $2 + 1)
         WHERE id = $1",
    )
    .bind(repo_id)
    .bind(max_number)
    .execute(&mut **tx)
    .await?;
    let ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM milestones WHERE repo_id = $1")
        .bind(repo_id)
        .fetch_all(&mut **tx)
        .await?;
    service::refresh_milestones(tx, &ids).await?;
    service::sync_repo_open_issues(tx, repo_id).await?;
    Ok(())
}
