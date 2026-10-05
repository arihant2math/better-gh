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
use serde_json::{Map, Value, json};

/// `issueEvent.data` in the sync protocol's camelCase vocabulary.
fn client_data(data: &Value, commit_id: Option<&str>) -> Value {
    let mut out = Map::new();
    let get = |k: &str| data.get(k).cloned();
    if let Some(v) = get("requested_reviewer_id") {
        out.insert("reviewerId".into(), v);
    }
    if let Some(v) = get("requested_team_id") {
        out.insert("teamId".into(), v);
    }
    if let Some(r) = data.get("rename") {
        out.insert("from".into(), r.get("from").cloned().unwrap_or(Value::Null));
        out.insert("to".into(), r.get("to").cloned().unwrap_or(Value::Null));
    }
    for k in ["from", "to", "before", "after", "ref"] {
        if let Some(v) = get(k) {
            out.insert(k.into(), v);
        }
    }
    if let Some(d) = data.get("dismissed_review") {
        out.insert(
            "reviewId".into(),
            d.get("review_id").cloned().unwrap_or(Value::Null),
        );
        out.insert(
            "dismissalMessage".into(),
            d.get("dismissal_message").cloned().unwrap_or(Value::Null),
        );
    }
    if let Some(v) = get("merge_method") {
        out.insert("mergeMethod".into(), v);
    }
    if let Some(c) = commit_id {
        out.insert("commitId".into(), json!(c));
    }
    Value::Object(out)
}

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
        "issueEvent",
        id,
        SyncAction::Insert,
        &json!({
            "id": id,
            "repoId": repo_id,
            "issueId": issue_id,
            "actorId": actor_id,
            "event": event,
            "data": client_data(&data, commit_id),
            "createdAt": Timestamp::from(created_at),
        }),
    )
    .await?;
    Ok(id)
}
