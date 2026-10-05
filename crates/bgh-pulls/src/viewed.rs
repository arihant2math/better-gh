//! Server-side "Viewed" state of pull request files (P38).
//!
//! `pull_viewed_files` keeps, per reviewer and path, the diff blob SHA the
//! file had when it was marked (the `sha` of the REST `/pulls/{n}/files`
//! entry). The file counts as viewed only while the PR diff still shows
//! that blob; once the file changes it reads as [`ViewedState::Dismissed`]
//! (GitHub's `FileViewedState`), i.e. unviewed in the UI.
//!
//! Rows are synced as the extension model `viewedFile` in the owner's
//! `user:{id}` scope and returned for the viewer by the PR page's `/sync`.
//!
//! Endpoints (`/_bgh/repos/{o}/{r}/pulls/{n}/viewed`):
//! * `GET` → `[{path, blob_sha, state}]` for the viewer (files in the diff)
//! * `PUT {path, blob_sha?}` → mark (blob defaults to the current diff's)
//! * `DELETE ?path=` → unmark
//!
//! [`mark`], [`unmark`] and [`states`] are the service functions for
//! GraphQL (`markFileAsViewed`, `unmarkFileAsViewed`, `viewerViewedState`).

use std::collections::HashMap;

use axum::extract::State;
use axum::http::StatusCode;
use bgh_core::prelude::*;
use bgh_core::sync;
use serde::{Deserialize, Serialize};

use crate::git;
use crate::model::Pull;
use crate::pulls::load_pull;

/// GitHub's `FileViewedState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ViewedState {
    Viewed,
    /// Marked viewed, but the file changed since.
    Dismissed,
    Unviewed,
}

impl ViewedState {
    /// State of a file whose current diff blob is `current`, given the blob
    /// stored when it was marked (if any).
    pub fn of(stored: Option<&str>, current: &str) -> Self {
        match stored {
            None => Self::Unviewed,
            Some(s) if s == current => Self::Viewed,
            Some(_) => Self::Dismissed,
        }
    }
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ViewedFile {
    pub id: i64,
    pub pull_id: i64,
    pub user_id: i64,
    pub path: String,
    pub blob_sha: String,
}

const COLUMNS: &str = "id, pull_id, user_id, path, blob_sha";

#[derive(sqlx::FromRow)]
struct Upserted {
    #[sqlx(flatten)]
    row: ViewedFile,
    inserted: bool,
}

/// Current diff blob SHA of every file of the PR diff (the REST entry's
/// `sha`), keyed by path.
pub async fn current_blobs(state: &AppState, pull: &Pull) -> ApiResult<HashMap<String, String>> {
    let diff = git::pull_diff(state, pull).await?;
    Ok(diff
        .files
        .iter()
        .map(|f| (f.filename.clone(), entry_sha(f)))
        .collect())
}

/// The REST diff entry `sha` (see `pulls::diff_entry`).
pub fn entry_sha(f: &bgh_git::patch::FileDiff) -> String {
    f.new_sha
        .clone()
        .or_else(|| f.old_sha.clone())
        .unwrap_or_else(|| bgh_git::ZERO_SHA.to_string())
}

/// Rows of `user_id` for `pull`.
pub async fn rows(state: &AppState, pull_id: i64, user_id: i64) -> ApiResult<Vec<ViewedFile>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM pull_viewed_files WHERE pull_id = $1 AND user_id = $2 ORDER BY path"
    ))
    .bind(pull_id)
    .bind(user_id)
    .fetch_all(&state.db)
    .await?)
}

/// Viewed state of every file in the PR diff for `user_id` (files never
/// marked are `Unviewed`).
pub async fn states(
    state: &AppState,
    pull: &Pull,
    user_id: i64,
) -> ApiResult<HashMap<String, ViewedState>> {
    let current = current_blobs(state, pull).await?;
    let stored: HashMap<String, String> = rows(state, pull.id(), user_id)
        .await?
        .into_iter()
        .map(|r| (r.path, r.blob_sha))
        .collect();
    Ok(current
        .into_iter()
        .map(|(path, sha)| {
            let s = ViewedState::of(stored.get(&path).map(String::as_str), &sha);
            (path, s)
        })
        .collect())
}

/// Mark `path` viewed for `user` at `blob_sha` (default: the file's blob in
/// the current PR diff). The path must be part of the PR diff (422).
pub async fn mark(
    state: &AppState,
    access: &RepoAccess,
    pull: &Pull,
    user_id: i64,
    path: &str,
    blob_sha: Option<&str>,
) -> ApiResult<ViewedFile> {
    let current = current_blobs(state, pull)
        .await?
        .remove(path)
        .ok_or_else(|| {
            ApiError::unprocessable(format!("{path} is not part of the pull request diff"))
        })?;
    let blob = match blob_sha.filter(|s| !s.is_empty()) {
        Some(s) if bgh_git::is_sha(s) => s.to_ascii_lowercase(),
        Some(_) => {
            return Err(ApiError::invalid_field(FieldError::invalid(
                "PullRequestViewedFile",
                "blob_sha",
            )));
        }
        None => current,
    };
    let mut tx = Tx::begin(state).await?;
    let row: Upserted = sqlx::query_as(&format!(
        "INSERT INTO pull_viewed_files (pull_id, repo_id, user_id, path, blob_sha)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (pull_id, user_id, path)
         DO UPDATE SET blob_sha = EXCLUDED.blob_sha, updated_at = now()
         RETURNING {COLUMNS}, (xmax = 0) AS inserted"
    ))
    .bind(pull.id())
    .bind(access.repo.id)
    .bind(user_id)
    .bind(path)
    .bind(&blob)
    .fetch_one(&mut *tx)
    .await?;
    let (row, inserted) = (row.row, row.inserted);
    let action = if inserted {
        SyncAction::Insert
    } else {
        SyncAction::Update
    };
    tx.sync_model(SyncModel::ViewedFile, row.id, action).await?;
    tx.commit().await?;
    Ok(row)
}

/// Unmark `path` for `user_id` (no-op when not marked).
pub async fn unmark(state: &AppState, pull: &Pull, user_id: i64, path: &str) -> ApiResult<()> {
    let mut tx = Tx::begin(state).await?;
    let id: Option<i64> = sqlx::query_scalar(
        "DELETE FROM pull_viewed_files WHERE pull_id = $1 AND user_id = $2 AND path = $3
         RETURNING id",
    )
    .bind(pull.id())
    .bind(user_id)
    .bind(path)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(id) = id {
        tx.sync_delete(&sync::user_scope(user_id), SyncModel::ViewedFile, id)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}

#[derive(Debug, Serialize)]
pub struct ViewedJson {
    pub path: String,
    pub blob_sha: String,
    pub state: ViewedState,
}

/// `GET .../viewed`: the viewer's marked files with their current state.
pub async fn list(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, number)): Path<(String, String, i64)>,
) -> ApiResult<axum::Json<Vec<ViewedJson>>> {
    let (_access, pull) = load_pull(&state, Some(&auth), &owner, &repo, number).await?;
    let current = current_blobs(&state, &pull).await?;
    let out = rows(&state, pull.id(), auth.user.id)
        .await?
        .into_iter()
        .filter_map(|r| {
            let cur = current.get(&r.path)?;
            Some(ViewedJson {
                state: ViewedState::of(Some(&r.blob_sha), cur),
                path: r.path,
                blob_sha: r.blob_sha,
            })
        })
        .collect();
    Ok(axum::Json(out))
}

#[derive(Debug, Deserialize)]
pub struct MarkBody {
    pub path: Option<String>,
    pub blob_sha: Option<String>,
}

fn require_path(path: Option<String>) -> ApiResult<String> {
    path.filter(|p| !p.is_empty()).ok_or_else(|| {
        ApiError::invalid_field(FieldError::missing_field("PullRequestViewedFile", "path"))
    })
}

/// `PUT .../viewed {path, blob_sha?}`.
pub async fn put(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    Json(body): Json<MarkBody>,
) -> ApiResult<axum::Json<ViewedJson>> {
    let (access, pull) = load_pull(&state, Some(&auth), &owner, &repo, number).await?;
    let path = require_path(body.path)?;
    let row = mark(
        &state,
        &access,
        &pull,
        auth.user.id,
        &path,
        body.blob_sha.as_deref(),
    )
    .await?;
    let current = current_blobs(&state, &pull).await?;
    Ok(axum::Json(ViewedJson {
        state: ViewedState::of(
            Some(&row.blob_sha),
            current.get(&row.path).map_or("", String::as_str),
        ),
        path: row.path,
        blob_sha: row.blob_sha,
    }))
}

#[derive(Debug, Deserialize)]
pub struct PathQuery {
    pub path: Option<String>,
}

/// `DELETE .../viewed?path=`.
pub async fn delete(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    Query(q): Query<PathQuery>,
) -> ApiResult<StatusCode> {
    let (_access, pull) = load_pull(&state, Some(&auth), &owner, &repo, number).await?;
    let path = require_path(q.path)?;
    unmark(&state, &pull, auth.user.id, &path).await?;
    Ok(StatusCode::NO_CONTENT)
}
