//! TEMPORARY stub; replaced by the real payload builders.

use bgh_core::{AppState, events::Event};

#[derive(Debug, Clone)]
pub struct HookEvent {
    pub event: &'static str,
    pub action: Option<String>,
    pub repo_id: Option<i64>,
    pub org_id: Option<i64>,
    pub payload: serde_json::Value,
}

pub fn event_names(_event: &Event) -> Vec<&'static str> {
    vec![]
}

pub async fn for_event(_state: &AppState, _event: &Event) -> anyhow::Result<Vec<HookEvent>> {
    Ok(vec![])
}

pub async fn ping(
    _state: &AppState,
    hook_id: i64,
    hook_json: serde_json::Value,
    _repo_id: Option<i64>,
    _org_id: Option<i64>,
    _sender_id: Option<i64>,
) -> anyhow::Result<serde_json::Value> {
    Ok(serde_json::json!({"zen": "x", "hook_id": hook_id, "hook": hook_json}))
}

pub async fn test_push(
    _state: &AppState,
    _repo_id: i64,
    _sender_id: i64,
) -> anyhow::Result<Option<serde_json::Value>> {
    Ok(None)
}
