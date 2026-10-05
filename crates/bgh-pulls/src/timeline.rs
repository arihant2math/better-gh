//! PR timeline events, stored in the shared `issue_events` table (rendered
//! by bgh-issues' events/timeline APIs). Event names follow GitHub; `data`
//! carries the event-specific fields:
//!
//! | event | data |
//! |-------|------|
//! | `review_requested` / `review_request_removed` | `{"requested_reviewer_id"}` or `{"requested_team_id"}`, `"as_code_owner"` |
//! | `review_dismissed` | `{"dismissed_review": {"review_id", "state", "dismissal_message"}}` |
//! | `merged` | `commit_id` = merge commit |
//! | `closed` / `reopened` | `commit_id` = merge commit for merged PRs |
//! | `head_ref_force_pushed` | `{"before", "after"}`, `commit_id` = after |
//! | `head_ref_deleted` / `head_ref_restored` | `{"ref"}` |
//! | `base_ref_changed` | `{"from", "to"}` |
//! | `renamed` | `{"rename": {"from", "to"}}` |
//! | `convert_to_draft` / `ready_for_review` | `{}` |
//! | `auto_merge_enabled` / `auto_merge_disabled` | `{"merge_method"}` / `{"reason"}` |

use bgh_core::prelude::*;
use serde_json::{Value, json};

/// Insert a timeline event and record its sync action (model `issue_event`).
pub async fn record(
    tx: &mut Tx,
    repo_id: i64,
    issue_id: i64,
    actor_id: Option<i64>,
    event: &str,
    commit_id: Option<&str>,
    data: Value,
) -> ApiResult<i64> {
    let (id, created_at): (i64, chrono::DateTime<chrono::Utc>) = sqlx::query_as(
        "INSERT INTO issue_events (issue_id, repo_id, actor_id, event, commit_id, data)
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING id, created_at",
    )
    .bind(issue_id)
    .bind(repo_id)
    .bind(actor_id)
    .bind(event)
    .bind(commit_id)
    .bind(&data)
    .fetch_one(&mut **tx)
    .await?;
    tx.sync(
        &bgh_core::sync::repo_scope(repo_id),
        "issue_event",
        id,
        SyncAction::Insert,
        &json!({
            "id": id,
            "issue_id": issue_id,
            "actor_id": actor_id,
            "event": event,
            "commit_id": commit_id,
            "data": data,
            "created_at": Timestamp::from(created_at),
        }),
    )
    .await?;
    Ok(id)
}
