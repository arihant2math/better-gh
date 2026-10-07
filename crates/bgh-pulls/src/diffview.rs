//! Diff viewer data (P37): every check-run annotation of a commit in one
//! request, so the Files tab can show them inline at their lines.
//!
//! `GET /_bgh/repos/{o}/{r}/commits/{sha}/annotations`

use axum::extract::State;
use bgh_core::prelude::*;
use serde::Serialize;

/// Most annotations returned for one commit.
pub const MAX_ANNOTATIONS: i64 = 1000;

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct CommitAnnotation {
    pub check_run_id: i64,
    pub check_run_name: String,
    pub path: String,
    pub start_line: i32,
    pub end_line: i32,
    pub start_column: Option<i32>,
    pub end_column: Option<i32>,
    pub annotation_level: String,
    pub title: Option<String>,
    pub message: String,
    pub raw_details: Option<String>,
}

pub async fn commit_annotations(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, sha)): Path<(String, String, String)>,
) -> ApiResult<axum::Json<Vec<CommitAnnotation>>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    if !bgh_git::is_sha(&sha) {
        return Err(ApiError::NotFound);
    }
    // check_runs_sha_idx (repo_id, head_sha, name) + check_run_annotations_run_idx.
    let rows: Vec<CommitAnnotation> = sqlx::query_as::<_, CommitAnnotation>(
        "SELECT r.id AS check_run_id, r.name AS check_run_name, a.path, a.start_line,
                a.end_line, a.start_column, a.end_column, a.annotation_level, a.title,
                a.message, a.raw_details
           FROM check_runs r
           JOIN check_run_annotations a ON a.check_run_id = r.id
          WHERE r.repo_id = $1 AND r.head_sha = $2
          ORDER BY a.path COLLATE \"C\", a.start_line, a.id
          LIMIT $3",
    )
    .bind(access.repo.id)
    .bind(sha.to_ascii_lowercase())
    .bind(MAX_ANNOTATIONS)
    .fetch_all(&state.db)
    .await?;
    Ok(axum::Json(rows))
}
