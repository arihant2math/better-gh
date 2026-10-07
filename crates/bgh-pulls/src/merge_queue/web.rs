//! `/_bgh` merge queue endpoints for the web client:
//!
//! * `PUT /_bgh/repos/{o}/{r}/pulls/{n}/queue` `{"jump"?}` → 201 Entry
//!   (200 with the existing entry when already queued)
//! * `DELETE /_bgh/repos/{o}/{r}/pulls/{n}/queue` → 204 (404 if not queued)
//! * `GET /_bgh/repos/{o}/{r}/queue/{*branch}` → Queue
//!
//! The requirements view (`/pulls/{n}/requirements`) carries
//! [`Requirement`] as `merge_queue`.

use axum::extract::State;
use axum::http::StatusCode;
use bgh_core::prelude::*;
use serde::{Deserialize, Serialize};

use super::{EntryJson, QueueConfig, config_for, dequeue, enqueue, entries_for, entry_for_pull};
use crate::model::Pull;
use crate::pulls::load_pull;

#[derive(Debug, Default, Deserialize)]
pub struct EnqueueBody {
    #[serde(default)]
    pub jump: bool,
}

pub async fn put(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    Json(body): Json<EnqueueBody>,
) -> ApiResult<(StatusCode, axum::Json<EntryJson>)> {
    let (access, pull) = load_pull(&state, Some(&auth), &owner, &repo, number).await?;
    let (entry, created) = enqueue(&state, &access, &pull, &auth.user, body.jump).await?;
    let json = super::render(&state, std::slice::from_ref(&entry))
        .await?
        .pop()
        .ok_or(ApiError::NotFound)?;
    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, axum::Json(json)))
}

pub async fn delete(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, number)): Path<(String, String, i64)>,
) -> ApiResult<StatusCode> {
    let (access, pull) = load_pull(&state, Some(&auth), &owner, &repo, number).await?;
    // The author may take their own PR out; anyone else needs write access.
    if pull.issue.author_id != Some(auth.user.id) {
        access.require(Permission::Write)?;
    }
    if !dequeue(
        &state,
        access.repo.id,
        pull.id(),
        Some(auth.user.id),
        "dequeued",
    )
    .await?
    {
        return Err(ApiError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Serialize)]
pub struct Queue {
    pub branch: String,
    /// A `merge_queue` rule is active for the branch.
    pub enabled: bool,
    pub config: Option<QueueConfig>,
    pub entries: Vec<EntryJson>,
}

pub async fn get_queue(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, branch)): Path<(String, String, String)>,
) -> ApiResult<axum::Json<Queue>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let branch = branch.trim_start_matches('/').to_string();
    let config = config_for(&state.db, access.repo.id, &branch).await?;
    let entries = entries_for(&state.db, access.repo.id, &branch).await?;
    Ok(axum::Json(Queue {
        enabled: config.is_some(),
        config,
        entries: super::render(&state, &entries).await?,
        branch,
    }))
}

/// `merge_queue` of the requirements view.
#[derive(Debug, Serialize)]
pub struct Requirement {
    /// The base branch has a merge queue: merge by adding to it.
    pub required: bool,
    pub branch: String,
    pub entry: Option<EntryJson>,
}

pub async fn requirement(
    state: &AppState,
    rules: &crate::protection::Rules,
    pull: &Pull,
) -> ApiResult<Requirement> {
    let entry = match entry_for_pull(&state.db, pull.id()).await? {
        Some(e) => super::render(state, std::slice::from_ref(&e)).await?.pop(),
        None => None,
    };
    Ok(Requirement {
        required: QueueConfig::from_rules(rules).is_some(),
        branch: pull.pr.base_ref.clone(),
        entry,
    })
}
