//! Comment moderation and edit history (migration 5400), shared by the
//! crates that own the commented-on content (bgh-issues, bgh-pulls,
//! bgh-repos) and bgh-graphql.
//!
//! * Minimized ("hidden") comments: `minimized_reason` / `minimized_by_id`
//!   / `minimized_at` on `comments`, `pr_review_comments`, `pr_reviews`
//!   and `commit_comments`.
//! * Edit history: `user_content_edits`, one row per edit of an issue/PR
//!   body or any comment kind, written by [`record_edit`] in the edit's
//!   transaction.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgConnection;

use crate::error::{ApiError, ApiResult, FieldError};
use crate::models::api::SimpleUser;
use crate::models::db;
use crate::state::AppState;
use crate::time::Timestamp;

/// What an edit or minimization targets (`user_content_edits.target_type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContentKind {
    /// An issue or pull request body.
    Issue,
    /// An issue / PR conversation comment (`comments`).
    Comment,
    /// A pull request review body (`pr_reviews`).
    Review,
    /// An inline review comment (`pr_review_comments`).
    ReviewComment,
    /// A commit comment (`commit_comments`).
    CommitComment,
}

impl ContentKind {
    pub const ALL: [ContentKind; 5] = [
        Self::Issue,
        Self::Comment,
        Self::Review,
        Self::ReviewComment,
        Self::CommitComment,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Issue => "issue",
            Self::Comment => "comment",
            Self::Review => "review",
            Self::ReviewComment => "review_comment",
            Self::CommitComment => "commit_comment",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }

    /// The table holding the content.
    pub fn table(self) -> &'static str {
        match self {
            Self::Issue => "issues",
            Self::Comment => "comments",
            Self::Review => "pr_reviews",
            Self::ReviewComment => "pr_review_comments",
            Self::CommitComment => "commit_comments",
        }
    }

    /// The author column of [`Self::table`].
    fn author_column(self) -> &'static str {
        match self {
            Self::Issue | Self::Comment => "author_id",
            Self::Review | Self::ReviewComment | Self::CommitComment => "user_id",
        }
    }

    /// Everything but issue bodies can be minimized.
    pub fn minimizable(self) -> bool {
        self != Self::Issue
    }
}

/// GitHub's minimize reasons, stored and returned in GraphQL
/// `minimizedReason` spelling.
pub const REASONS: [&str; 6] = [
    "spam",
    "abuse",
    "off-topic",
    "outdated",
    "duplicate",
    "resolved",
];

/// Normalize a reason given as a GraphQL `ReportedContentClassifiers`
/// value (`OFF_TOPIC`) or in stored spelling (`off-topic`).
pub fn parse_reason(s: &str) -> Option<&'static str> {
    let norm = s.trim().to_ascii_lowercase().replace('_', "-");
    REASONS.into_iter().find(|r| *r == norm)
}

/// A content row as moderation sees it.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Target {
    pub id: i64,
    pub repo_id: i64,
    pub author_id: Option<i64>,
    /// Pull request (issue) id for reviews and review comments, issue id
    /// for comments, `None` for commit comments and issues.
    pub parent_id: Option<i64>,
    /// The review is pending (reviews / review comments of a pending review).
    pub pending: bool,
    pub minimized_reason: Option<String>,
}

/// Load a target of the repository (`None` if missing). Pending reviews
/// and their comments are only visible to their author: they come back
/// with `pending = true` for the caller to refuse.
pub async fn find_target(
    db: impl sqlx::PgExecutor<'_>,
    kind: ContentKind,
    repo_id: i64,
    id: i64,
) -> Result<Option<Target>, sqlx::Error> {
    let sql = match kind {
        ContentKind::Issue => "SELECT id, repo_id, author_id, NULL::bigint AS parent_id,
                    false AS pending, NULL::text AS minimized_reason
               FROM issues WHERE id = $1 AND repo_id = $2"
            .to_string(),
        ContentKind::Comment => "SELECT id, repo_id, author_id, issue_id AS parent_id,
                    false AS pending, minimized_reason
               FROM comments WHERE id = $1 AND repo_id = $2"
            .to_string(),
        ContentKind::Review => "SELECT id, repo_id, user_id AS author_id, pull_id AS parent_id,
                    state = 'PENDING' AS pending, minimized_reason
               FROM pr_reviews WHERE id = $1 AND repo_id = $2"
            .to_string(),
        ContentKind::ReviewComment => "SELECT c.id, c.repo_id, c.user_id AS author_id,
                    c.pull_id AS parent_id,
                    coalesce(v.state = 'PENDING', false) AS pending, c.minimized_reason
               FROM pr_review_comments c LEFT JOIN pr_reviews v ON v.id = c.review_id
              WHERE c.id = $1 AND c.repo_id = $2"
            .to_string(),
        ContentKind::CommitComment => "SELECT id, repo_id, user_id AS author_id,
                    NULL::bigint AS parent_id, false AS pending, minimized_reason
               FROM commit_comments WHERE id = $1 AND repo_id = $2"
            .to_string(),
    };
    sqlx::query_as::<_, Target>(&sql)
        .bind(id)
        .bind(repo_id)
        .fetch_optional(db)
        .await
}

/// Set (`Some(reason)`) or clear (`None`) a target's minimized state.
/// Doesn't touch `updated_at` (minimizing isn't an edit).
pub async fn set_minimized(
    conn: &mut PgConnection,
    kind: ContentKind,
    id: i64,
    reason: Option<&str>,
    actor_id: i64,
) -> ApiResult<()> {
    if !kind.minimizable() {
        return Err(ApiError::unprocessable("This content can't be minimized."));
    }
    sqlx::query(&format!(
        "UPDATE {} SET minimized_reason = $2,
                minimized_by_id = CASE WHEN $2::text IS NULL THEN NULL ELSE $3 END,
                minimized_at = CASE WHEN $2::text IS NULL THEN NULL ELSE now() END
          WHERE id = $1",
        kind.table()
    ))
    .bind(id)
    .bind(reason)
    .bind(actor_id)
    .execute(conn)
    .await?;
    Ok(())
}

/// Minimized reasons of many targets of one kind (`(id, reason)` for the
/// minimized ones only).
pub async fn minimized_reasons(
    db: impl sqlx::PgExecutor<'_>,
    kind: ContentKind,
    ids: &[i64],
) -> Result<Vec<(i64, String)>, sqlx::Error> {
    if !kind.minimizable() || ids.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query_as(&format!(
        "SELECT id, minimized_reason FROM {} WHERE id = ANY($1) AND minimized_reason IS NOT NULL",
        kind.table()
    ))
    .bind(ids)
    .fetch_all(db)
    .await
}

/// Record one edit of a body (no-op when the text didn't change). Call in
/// the edit's transaction, after validating it.
pub async fn record_edit(
    conn: &mut PgConnection,
    repo_id: i64,
    kind: ContentKind,
    target_id: i64,
    editor_id: i64,
    before: &str,
    after: &str,
) -> Result<(), sqlx::Error> {
    if before == after {
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO user_content_edits
             (repo_id, target_type, target_id, editor_id, body, previous_body, created_at)
         VALUES ($1, $2, $3, $4, $5, $6, clock_timestamp())",
    )
    .bind(repo_id)
    .bind(kind.as_str())
    .bind(target_id)
    .bind(editor_id)
    .bind(after)
    .bind(before)
    .execute(conn)
    .await?;
    Ok(())
}

/// One `user_content_edits` row.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ContentEdit {
    pub id: i64,
    pub repo_id: i64,
    pub target_type: String,
    pub target_id: i64,
    pub editor_id: Option<i64>,
    pub body: Option<String>,
    pub previous_body: Option<String>,
    pub created_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub deleted_by_id: Option<i64>,
}

impl ContentEdit {
    pub const COLUMNS: &'static str = "id, repo_id, target_type, target_id, editor_id, body, \
         previous_body, created_at, deleted_at, deleted_by_id";
}

/// Edits of many targets of one kind, oldest first.
pub async fn edits_for(
    db: impl sqlx::PgExecutor<'_>,
    kind: ContentKind,
    ids: &[i64],
) -> Result<Vec<ContentEdit>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT {} FROM user_content_edits
          WHERE target_type = $1 AND target_id = ANY($2) ORDER BY target_id, id",
        ContentEdit::COLUMNS
    ))
    .bind(kind.as_str())
    .bind(ids)
    .fetch_all(db)
    .await
}

/// Delete the content of one revision (GitHub keeps the entry, marked
/// deleted). The current body can't be deleted: refuse the latest edit.
/// Clears the same text from the next revision's `previous_body`.
pub async fn delete_revision(
    conn: &mut PgConnection,
    edit: &ContentEdit,
    actor_id: i64,
) -> ApiResult<()> {
    let latest: Option<i64> = sqlx::query_scalar(
        "SELECT max(id) FROM user_content_edits WHERE target_type = $1 AND target_id = $2",
    )
    .bind(&edit.target_type)
    .bind(edit.target_id)
    .fetch_one(&mut *conn)
    .await?;
    if latest == Some(edit.id) {
        return Err(ApiError::invalid_field(FieldError::custom(
            "UserContentEdit",
            "id",
            "the current revision can't be deleted",
        )));
    }
    sqlx::query(
        "UPDATE user_content_edits SET body = NULL, deleted_at = coalesce(deleted_at, now()),
                deleted_by_id = coalesce(deleted_by_id, $2)
          WHERE id = $1",
    )
    .bind(edit.id)
    .bind(actor_id)
    .execute(&mut *conn)
    .await?;
    sqlx::query(
        "UPDATE user_content_edits SET previous_body = NULL
          WHERE id = (SELECT min(id) FROM user_content_edits
                       WHERE target_type = $1 AND target_id = $2 AND id > $3)",
    )
    .bind(&edit.target_type)
    .bind(edit.target_id)
    .bind(edit.id)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Delete the original (pre-first-edit) text of a target.
pub async fn delete_original(
    conn: &mut PgConnection,
    kind: ContentKind,
    target_id: i64,
) -> Result<bool, sqlx::Error> {
    let n = sqlx::query(
        "UPDATE user_content_edits SET previous_body = NULL
          WHERE id = (SELECT min(id) FROM user_content_edits
                       WHERE target_type = $1 AND target_id = $2)",
    )
    .bind(kind.as_str())
    .bind(target_id)
    .execute(conn)
    .await?
    .rows_affected();
    Ok(n > 0)
}

/// Author of the target (who, besides admins, may delete revisions).
pub async fn target_author(
    db: impl sqlx::PgExecutor<'_>,
    kind: ContentKind,
    id: i64,
) -> Result<Option<i64>, sqlx::Error> {
    sqlx::query_scalar(&format!(
        "SELECT {} FROM {} WHERE id = $1",
        kind.author_column(),
        kind.table()
    ))
    .bind(id)
    .fetch_optional(db)
    .await
    .map(Option::flatten)
}

/// `_bgh` JSON of an edit history entry.
#[derive(Debug, Clone, Serialize)]
pub struct ContentEditJson {
    pub id: i64,
    pub editor: Option<SimpleUser>,
    /// The text after this edit (`null` once deleted).
    pub body: Option<String>,
    /// The text before this edit (`null` once deleted).
    pub previous_body: Option<String>,
    pub edited_at: Timestamp,
    pub deleted_at: Option<Timestamp>,
    pub deleted_by: Option<SimpleUser>,
}

/// Render edits (newest first, as the "edited" dropdown lists them).
pub async fn render_edits(
    state: &AppState,
    mut edits: Vec<ContentEdit>,
) -> ApiResult<Vec<ContentEditJson>> {
    let users = crate::views::users_by_id(
        state,
        edits.iter().flat_map(|e| [e.editor_id, e.deleted_by_id]),
    )
    .await?;
    let user = |id: Option<i64>| -> Option<SimpleUser> {
        id.and_then(|id| users.get(&id))
            .map(|u: &db::User| SimpleUser::new(&state.urls, u))
    };
    edits.sort_by_key(|e| std::cmp::Reverse(e.id));
    Ok(edits
        .into_iter()
        .map(|e| ContentEditJson {
            id: e.id,
            editor: user(e.editor_id),
            body: e.body,
            previous_body: e.previous_body,
            edited_at: Timestamp(e.created_at),
            deleted_at: e.deleted_at.map(Timestamp),
            deleted_by: user(e.deleted_by_id),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reasons_parse_both_spellings() {
        assert_eq!(parse_reason("OFF_TOPIC"), Some("off-topic"));
        assert_eq!(parse_reason("off-topic"), Some("off-topic"));
        assert_eq!(parse_reason("Spam"), Some("spam"));
        assert_eq!(parse_reason("rude"), None);
        for k in ContentKind::ALL {
            assert_eq!(ContentKind::parse(k.as_str()), Some(k));
        }
    }
}
