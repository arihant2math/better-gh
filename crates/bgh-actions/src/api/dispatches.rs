//! `POST /repos/{owner}/{repo}/dispatches` (`repository_dispatch`).

use axum::extract::{Path, State};
use axum::http::StatusCode;
use bgh_core::prelude::*;
use serde::Deserialize;
use serde_json::Value;

/// GitHub caps `client_payload` at 10 top-level properties.
const MAX_CLIENT_PAYLOAD_KEYS: usize = 10;
const MAX_EVENT_TYPE_LEN: usize = 100;

#[derive(Debug, Deserialize)]
pub struct Body {
    pub event_type: Option<String>,
    #[serde(default)]
    pub client_payload: Option<Value>,
}

/// Emits `RepositoryDispatch`: the `repository_dispatch` webhook and the
/// workflows listening for it on the default branch. 204.
pub async fn create(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo)): Path<(String, String)>,
    Json(body): Json<Body>,
) -> ApiResult<StatusCode> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    access.require(Permission::Write)?;
    access.require_not_archived()?;
    let event_type = body.event_type.unwrap_or_default();
    if event_type.is_empty() {
        return Err(ApiError::unprocessable(
            "Invalid request.\n\n\"event_type\" wasn't supplied.",
        ));
    }
    if event_type.chars().count() > MAX_EVENT_TYPE_LEN {
        return Err(ApiError::unprocessable(
            "Invalid request.\n\n\"event_type\" is too long (maximum is 100 characters).",
        ));
    }
    let client_payload = match body.client_payload {
        None | Some(Value::Null) => Value::Object(Default::default()),
        Some(Value::Object(o)) if o.len() > MAX_CLIENT_PAYLOAD_KEYS => {
            return Err(ApiError::unprocessable(
                "Invalid request.\n\nNo more than 10 properties are allowed; 11 were supplied."
                    .replace("11", &o.len().to_string()),
            ));
        }
        Some(v @ Value::Object(_)) => v,
        Some(_) => {
            return Err(ApiError::unprocessable(
                "Invalid request.\n\nFor 'properties/client_payload', is not an object.",
            ));
        }
    };
    let mut tx = Tx::begin(&state).await?;
    tx.emit(Event::RepositoryDispatch {
        repo_id: access.repo.id,
        actor_id: auth.user.id,
        event_type,
        client_payload,
        branch: access.repo.default_branch.clone(),
    });
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
