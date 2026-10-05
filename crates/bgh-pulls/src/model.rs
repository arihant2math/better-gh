//! Row types and loaders for pull requests (an `issues` row joined with
//! its `pull_requests` row) and the other tables this crate owns.

use bgh_core::prelude::*;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgExecutor};

/// A pull request: conversation (`issues`) + PR data (`pull_requests`).
#[derive(Debug, Clone, FromRow)]
pub struct Pull {
    #[sqlx(flatten)]
    pub issue: db::Issue,
    #[sqlx(flatten)]
    pub pr: db::PullRequest,
}

impl Pull {
    pub fn id(&self) -> i64 {
        self.issue.id
    }

    pub fn number(&self) -> i64 {
        self.issue.number
    }

    pub fn is_open(&self) -> bool {
        self.issue.state == "open"
    }

    /// `refs/pull/{n}/head` in the base repository.
    pub fn head_ref_name(&self) -> String {
        format!("refs/pull/{}/head", self.issue.number)
    }

    /// `refs/pull/{n}/merge` (test merge commit) in the base repository.
    pub fn merge_ref_name(&self) -> String {
        format!("refs/pull/{}/merge", self.issue.number)
    }

    pub fn is_cross_repo(&self) -> bool {
        self.pr.head_repo_id != Some(self.pr.repo_id)
    }
}

/// `SELECT` list for [`Pull`] with `issues i JOIN pull_requests p`.
pub fn pull_columns() -> String {
    format!(
        "{}, {}",
        db::prefixed("i", db::Issue::COLUMNS),
        db::prefixed("p", PR_COLUMNS_NO_REPO)
    )
}

/// `pull_requests` columns minus `repo_id` (already selected from `issues`).
const PR_COLUMNS_NO_REPO: &str = "issue_id, head_repo_id, head_ref, head_sha, \
    base_ref, base_sha, merge_base_sha, merge_commit_sha, merged, merged_at, merged_by_id, \
    mergeable, rebaseable, mergeable_state, draft, maintainer_can_modify, auto_merge, \
    additions, deletions, changed_files, commits, review_comments_count";

pub const PULL_FROM: &str = "issues i JOIN pull_requests p ON p.issue_id = i.id";

pub async fn find_by_number(
    db: impl PgExecutor<'_>,
    repo_id: i64,
    number: i64,
) -> Result<Option<Pull>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT {} FROM {PULL_FROM} WHERE i.repo_id = $1 AND i.number = $2",
        pull_columns()
    ))
    .bind(repo_id)
    .bind(number)
    .fetch_optional(db)
    .await
}

pub async fn find_by_id(db: impl PgExecutor<'_>, id: i64) -> Result<Option<Pull>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT {} FROM {PULL_FROM} WHERE i.id = $1",
        pull_columns()
    ))
    .bind(id)
    .fetch_optional(db)
    .await
}

/// Lock and load inside a transaction (`FOR UPDATE` on both rows).
pub async fn lock(db: impl PgExecutor<'_>, id: i64) -> Result<Option<Pull>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT {} FROM {PULL_FROM} WHERE i.id = $1 FOR UPDATE OF i, p",
        pull_columns()
    ))
    .bind(id)
    .fetch_optional(db)
    .await
}

/// Load a PR by number or 404.
pub async fn load(state: &AppState, repo_id: i64, number: i64) -> ApiResult<Pull> {
    find_by_number(&state.db, repo_id, number)
        .await?
        .ok_or(ApiError::NotFound)
}

/// `pr_reviews` row.
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct Review {
    pub id: i64,
    pub pull_id: i64,
    pub repo_id: i64,
    pub user_id: Option<i64>,
    pub body: String,
    pub state: String,
    pub commit_id: Option<String>,
    pub submitted_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub dismissed_at: Option<DateTime<Utc>>,
    pub dismissal_message: Option<String>,
}

impl Review {
    pub const COLUMNS: &'static str = "id, pull_id, repo_id, user_id, body, state, commit_id, \
        submitted_at, created_at, updated_at, dismissed_at, dismissal_message";
}

/// `pr_review_comments` row.
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct ReviewComment {
    pub id: i64,
    pub pull_id: i64,
    pub repo_id: i64,
    pub review_id: Option<i64>,
    pub in_reply_to_id: Option<i64>,
    pub user_id: Option<i64>,
    pub body: String,
    pub path: String,
    pub commit_id: String,
    pub original_commit_id: String,
    pub diff_hunk: String,
    pub subject_type: String,
    pub side: Option<String>,
    pub start_side: Option<String>,
    pub line: Option<i32>,
    pub original_line: Option<i32>,
    pub start_line: Option<i32>,
    pub original_start_line: Option<i32>,
    pub position: Option<i32>,
    pub original_position: Option<i32>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub resolved_by_id: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ReviewComment {
    pub const COLUMNS: &'static str = "id, pull_id, repo_id, review_id, in_reply_to_id, user_id, \
        body, path, commit_id, original_commit_id, diff_hunk, subject_type, side, start_side, \
        line, original_line, start_line, original_start_line, position, original_position, \
        resolved_at, resolved_by_id, created_at, updated_at";

    /// Id of the thread's root comment.
    pub fn thread_id(&self) -> i64 {
        self.in_reply_to_id.unwrap_or(self.id)
    }

    pub fn is_outdated(&self) -> bool {
        self.subject_type == "line" && self.position.is_none()
    }
}

/// Review state of a pending review (not yet visible to others).
pub const PENDING: &str = "PENDING";

/// The repo id of a pull request's head (falls back to the base repo id for
/// deleted forks, where only `refs/pull/{n}/head` remains).
pub fn head_repo(p: &Pull) -> i64 {
    p.pr.head_repo_id.unwrap_or(p.pr.repo_id)
}
