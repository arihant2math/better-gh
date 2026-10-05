//! Internal insert API for importers (P51 metadata import, part 2).
//!
//! Like `bgh_issues::import`, these keep the source's numbers, authors,
//! states and timestamps and emit **no** domain events (no webhooks,
//! notifications, activity, Actions, mention processing or review
//! requests): an import is history, not news. They record sync actions so
//! open clients see the rows. Callers own idempotency (`bgh-import` maps
//! source ids to local ids in the same transaction).

use bgh_core::prelude::*;
use chrono::{DateTime, Utc};

use crate::comments::SUBJECT;
use crate::jobs::Refresh;

/// A pull request with its source number, state, refs and timestamps.
pub struct NewPull<'a> {
    pub number: i64,
    pub title: &'a str,
    pub body: Option<&'a str>,
    /// `open` | `closed` (merged PRs are `closed` with `merged`).
    pub state: &'a str,
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
    /// `None` when the head lived in a fork that was not imported (only
    /// `refs/pull/{n}/head` remains, like a deleted fork).
    pub head_repo_id: Option<i64>,
    pub head_ref: &'a str,
    pub head_sha: &'a str,
    pub base_ref: &'a str,
    pub base_sha: &'a str,
    pub merge_base_sha: Option<&'a str>,
    pub merged: bool,
    pub merge_commit_sha: Option<&'a str>,
    pub merged_at: Option<DateTime<Utc>>,
    pub merged_by_id: Option<i64>,
    pub draft: bool,
    pub maintainer_can_modify: bool,
    pub additions: i64,
    pub deletions: i64,
    pub changed_files: i64,
    pub commits: i64,
}

const LOCK_REASONS: &[&str] = &["off-topic", "too heated", "resolved", "spam"];

/// Insert the `issues` + `pull_requests` rows. Open PRs get a
/// mergeability refresh (without CODEOWNERS review requests).
pub async fn insert_pull(tx: &mut Tx, repo_id: i64, p: &NewPull<'_>) -> ApiResult<i64> {
    let open = p.state != "closed" && !p.merged;
    let closed_at = if open {
        None
    } else {
        p.closed_at.or(p.merged_at).or(Some(p.updated_at))
    };
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO issues (repo_id, number, title, body, state, author_id, milestone_id,
                             locked, active_lock_reason, closed_at, closed_by_id,
                             created_at, updated_at, is_pull_request)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, true) RETURNING id",
    )
    .bind(repo_id)
    .bind(p.number)
    .bind(p.title)
    .bind(p.body)
    .bind(if open { "open" } else { "closed" })
    .bind(p.author_id)
    .bind(p.milestone_id)
    .bind(p.locked)
    .bind(
        p.active_lock_reason
            .filter(|r| p.locked && LOCK_REASONS.contains(r)),
    )
    .bind(closed_at)
    .bind(if open {
        None
    } else {
        p.closed_by_id.or(p.merged_by_id)
    })
    .bind(p.created_at)
    .bind(p.updated_at)
    .fetch_one(&mut **tx)
    .await?;
    sqlx::query(
        "INSERT INTO pull_requests (issue_id, repo_id, head_repo_id, head_ref, head_sha,
                base_ref, base_sha, merge_base_sha, merge_commit_sha, merged, merged_at,
                merged_by_id, draft, maintainer_can_modify, additions, deletions,
                changed_files, commits, mergeable_state)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17,
                 $18, $19)",
    )
    .bind(id)
    .bind(repo_id)
    .bind(p.head_repo_id)
    .bind(p.head_ref)
    .bind(p.head_sha)
    .bind(p.base_ref)
    .bind(p.base_sha)
    .bind(p.merge_base_sha)
    .bind(p.merge_commit_sha.filter(|_| p.merged))
    .bind(p.merged)
    .bind(if p.merged {
        p.merged_at.or(closed_at)
    } else {
        None
    })
    .bind(p.merged_by_id.filter(|_| p.merged))
    .bind(p.draft && open)
    .bind(p.maintainer_can_modify)
    .bind(p.additions)
    .bind(p.deletions)
    .bind(p.changed_files)
    .bind(p.commits)
    .bind(if p.draft && open { "draft" } else { "unknown" })
    .execute(&mut **tx)
    .await?;
    for (table, ids) in [
        ("issue_labels (issue_id, label_id", p.label_ids),
        ("issue_assignees (issue_id, user_id", p.assignee_ids),
    ] {
        if ids.is_empty() {
            continue;
        }
        sqlx::query(&format!(
            "INSERT INTO {table}, created_at)
             SELECT $1, unnest($2::bigint[]), $3 ON CONFLICT DO NOTHING"
        ))
        .bind(id)
        .bind(ids)
        .bind(p.created_at)
        .execute(&mut **tx)
        .await?;
    }
    sqlx::query(
        "UPDATE repositories SET next_issue_number = GREATEST(next_issue_number, $2 + 1),
                open_issues_count = open_issues_count + $3
         WHERE id = $1",
    )
    .bind(repo_id)
    .bind(p.number)
    .bind(i64::from(open))
    .execute(&mut **tx)
    .await?;
    tx.sync_issue(id, SyncAction::Insert, true).await?;
    if open {
        tx.enqueue(&Refresh {
            pull_id: id,
            codeowners: false,
        })
        .await?;
    }
    Ok(id)
}

/// A requested reviewer (user or team) as it stands on the source.
pub async fn insert_requested_reviewer(
    tx: &mut Tx,
    pull_id: i64,
    user_id: Option<i64>,
    team_id: Option<i64>,
) -> ApiResult<()> {
    if user_id.is_none() == team_id.is_none() {
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO pr_requested_reviewers (pull_id, user_id, team_id) VALUES ($1, $2, $3)
         ON CONFLICT DO NOTHING",
    )
    .bind(pull_id)
    .bind(user_id)
    .bind(team_id)
    .execute(&mut **tx)
    .await?;
    tx.sync_issue(pull_id, SyncAction::Update, false).await?;
    Ok(())
}

/// A submitted review.
pub struct NewReview<'a> {
    pub user_id: Option<i64>,
    pub body: &'a str,
    /// `COMMENTED` | `APPROVED` | `CHANGES_REQUESTED` | `DISMISSED`.
    pub state: &'a str,
    pub commit_id: Option<&'a str>,
    pub submitted_at: DateTime<Utc>,
}

const REVIEW_STATES: &[&str] = &["COMMENTED", "APPROVED", "CHANGES_REQUESTED", "DISMISSED"];

pub async fn insert_review(tx: &mut Tx, pull_id: i64, r: &NewReview<'_>) -> ApiResult<i64> {
    let state = if REVIEW_STATES.contains(&r.state) {
        r.state
    } else {
        "COMMENTED"
    };
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO pr_reviews (pull_id, repo_id, user_id, body, state, commit_id, submitted_at,
                                 created_at, updated_at, dismissed_at)
         SELECT $1, repo_id, $2, $3, $4, $5, $6, $6, $6, CASE WHEN $4 = 'DISMISSED' THEN $6 END
           FROM pull_requests WHERE issue_id = $1
         RETURNING id",
    )
    .bind(pull_id)
    .bind(r.user_id)
    .bind(r.body)
    .bind(state)
    .bind(r.commit_id)
    .bind(r.submitted_at)
    .fetch_one(&mut **tx)
    .await?;
    tx.sync_model(SyncModel::Review, id, SyncAction::Insert)
        .await?;
    Ok(id)
}

/// A review comment with its source position. `position = None` (with a
/// line subject) marks it outdated, as on GitHub.
pub struct NewReviewComment<'a> {
    pub review_id: Option<i64>,
    pub in_reply_to_id: Option<i64>,
    pub user_id: Option<i64>,
    pub body: &'a str,
    pub path: &'a str,
    pub commit_id: &'a str,
    pub original_commit_id: &'a str,
    pub diff_hunk: &'a str,
    /// `line` | `file`.
    pub subject_type: &'a str,
    pub side: Option<&'a str>,
    pub start_side: Option<&'a str>,
    pub line: Option<i32>,
    pub original_line: Option<i32>,
    pub start_line: Option<i32>,
    pub original_start_line: Option<i32>,
    pub position: Option<i32>,
    pub original_position: Option<i32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

fn side(s: Option<&str>) -> Option<&'static str> {
    match s {
        Some("LEFT") => Some("LEFT"),
        Some("RIGHT") => Some("RIGHT"),
        _ => None,
    }
}

pub async fn insert_review_comment(
    tx: &mut Tx,
    pull_id: i64,
    c: &NewReviewComment<'_>,
) -> ApiResult<i64> {
    let file = c.subject_type == "file";
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO pr_review_comments (pull_id, repo_id, review_id, in_reply_to_id, user_id,
                body, path, commit_id, original_commit_id, diff_hunk, subject_type, side,
                start_side, line, original_line, start_line, original_start_line, position,
                original_position, created_at, updated_at)
         SELECT $1, repo_id, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16,
                $17, $18, $19, $20
           FROM pull_requests WHERE issue_id = $1
         RETURNING id",
    )
    .bind(pull_id)
    .bind(c.review_id)
    .bind(c.in_reply_to_id)
    .bind(c.user_id)
    .bind(c.body)
    .bind(c.path)
    .bind(c.commit_id)
    .bind(c.original_commit_id)
    .bind(c.diff_hunk)
    .bind(if file { "file" } else { "line" })
    .bind(if file {
        None
    } else {
        side(c.side).or(Some("RIGHT"))
    })
    .bind(side(c.start_side).filter(|_| c.start_line.is_some()))
    .bind(c.line)
    .bind(c.original_line.or(c.line))
    .bind(c.start_line)
    .bind(c.original_start_line.or(c.start_line))
    .bind(c.position)
    .bind(c.original_position.or(c.position))
    .bind(c.created_at)
    .bind(c.updated_at)
    .fetch_one(&mut **tx)
    .await?;
    sqlx::query(
        "UPDATE pull_requests SET review_comments_count = review_comments_count + 1
          WHERE issue_id = $1",
    )
    .bind(pull_id)
    .execute(&mut **tx)
    .await?;
    tx.sync_model(SyncModel::ReviewComment, id, SyncAction::Insert)
        .await?;
    Ok(id)
}

/// Mark a review thread (its root comment) resolved.
pub async fn resolve_thread(
    tx: &mut Tx,
    root_id: i64,
    by: Option<i64>,
    at: DateTime<Utc>,
) -> ApiResult<()> {
    sqlx::query(
        "UPDATE pr_review_comments SET resolved_at = $2, resolved_by_id = $3 WHERE id = $1",
    )
    .bind(root_id)
    .bind(at)
    .bind(by)
    .execute(&mut **tx)
    .await?;
    tx.sync_model(SyncModel::ReviewComment, root_id, SyncAction::Update)
        .await?;
    Ok(())
}

/// Reactions on a review comment.
pub async fn insert_comment_reactions(
    tx: &mut Tx,
    comment_id: i64,
    reactions: &[(i64, String, DateTime<Utc>)],
) -> ApiResult<()> {
    for (user_id, content, at) in reactions {
        sqlx::query(
            "INSERT INTO reactions (subject_type, subject_id, user_id, content, created_at)
             VALUES ($1, $2, $3, $4, $5) ON CONFLICT DO NOTHING",
        )
        .bind(SUBJECT)
        .bind(comment_id)
        .bind(user_id)
        .bind(content)
        .bind(at)
        .execute(&mut **tx)
        .await?;
    }
    tx.sync_model(SyncModel::ReviewComment, comment_id, SyncAction::Update)
        .await?;
    Ok(())
}
