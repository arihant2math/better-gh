//! `/repos/{o}/{r}/actions/artifacts[...]` and `/actions/runs/{id}/artifacts`.

use std::collections::HashMap;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use bgh_core::pagination::Pagination;
use bgh_core::prelude::*;
use serde::Deserialize;
use serde_json::Value;

use super::wrapped;
use crate::json::artifact_json;
use crate::models::{ArtifactRow, RunRow};

#[derive(Debug, Default, Deserialize)]
pub struct ArtifactFilter {
    pub name: Option<String>,
}

async fn render(
    state: &AppState,
    access: &RepoAccess,
    rows: &[ArtifactRow],
) -> ApiResult<Vec<Value>> {
    let ids: Vec<i64> = rows.iter().map(|a| a.run_id).collect();
    let runs: HashMap<i64, RunRow> = sqlx::query_as::<_, RunRow>(&format!(
        "SELECT {} FROM actions_runs WHERE id = ANY($1)",
        RunRow::COLUMNS
    ))
    .bind(&ids)
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .map(|r| (r.id, r))
    .collect();
    Ok(rows
        .iter()
        .map(|a| artifact_json(state, access, a, runs.get(&a.run_id)))
        .collect())
}

async fn list_inner(
    state: &AppState,
    access: &RepoAccess,
    p: &Pagination,
    run_id: Option<i64>,
    name: Option<&str>,
) -> ApiResult<Response> {
    let total: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM actions_artifacts
          WHERE repo_id = $1 AND ($2::bigint IS NULL OR run_id = $2) AND ($3::text IS NULL OR name = $3)",
    )
    .bind(access.repo.id)
    .bind(run_id)
    .bind(name)
    .fetch_one(&state.db)
    .await?;
    let rows: Vec<ArtifactRow> = sqlx::query_as(&format!(
        "SELECT {} FROM actions_artifacts
          WHERE repo_id = $1 AND ($2::bigint IS NULL OR run_id = $2) AND ($3::text IS NULL OR name = $3)
          ORDER BY id DESC LIMIT $4 OFFSET $5",
        ArtifactRow::COLUMNS
    ))
    .bind(access.repo.id)
    .bind(run_id)
    .bind(name)
    .bind(p.limit())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let items = render(state, access, &rows).await?;
    Ok(wrapped(p, total, "artifacts", items))
}

/// `GET /repos/{owner}/{repo}/actions/artifacts`
pub async fn list(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo)): Path<(String, String)>,
    Query(f): Query<ArtifactFilter>,
) -> ApiResult<Response> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    list_inner(&state, &access, &p, None, f.name.as_deref()).await
}

/// `GET /repos/{owner}/{repo}/actions/runs/{run_id}/artifacts`
pub async fn list_for_run(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo, run_id)): Path<(String, String, i64)>,
    Query(f): Query<ArtifactFilter>,
) -> ApiResult<Response> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    super::runs::load_run(&state, &access, run_id).await?;
    list_inner(&state, &access, &p, Some(run_id), f.name.as_deref()).await
}

async fn load(state: &AppState, access: &RepoAccess, id: i64) -> ApiResult<ArtifactRow> {
    sqlx::query_as(&format!(
        "SELECT {} FROM actions_artifacts WHERE id = $1 AND repo_id = $2",
        ArtifactRow::COLUMNS
    ))
    .bind(id)
    .bind(access.repo.id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)
}

/// `GET /repos/{owner}/{repo}/actions/artifacts/{artifact_id}`
pub async fn get(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, id)): Path<(String, String, i64)>,
) -> ApiResult<Json<Value>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let a = load(&state, &access, id).await?;
    let mut v = render(&state, &access, std::slice::from_ref(&a)).await?;
    Ok(Json(v.remove(0)))
}

/// `DELETE /repos/{owner}/{repo}/actions/artifacts/{artifact_id}`
pub async fn delete(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, id)): Path<(String, String, i64)>,
) -> ApiResult<StatusCode> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    access.require(Permission::Write)?;
    let a = load(&state, &access, id).await?;
    sqlx::query("DELETE FROM actions_artifacts WHERE id = $1")
        .bind(a.id)
        .execute(&state.db)
        .await?;
    let _ = tokio::fs::remove_file(crate::server::artifact_path(&state, a.id)).await;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET .../actions/artifacts/{artifact_id}/{archive_format}` (only `zip`)
/// → 302 to a short-lived download URL; 410 when expired.
pub async fn download(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, id, format)): Path<(String, String, i64, String)>,
) -> ApiResult<Response> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    if auth.0.is_none() {
        return Err(ApiError::requires_auth());
    }
    if format != "zip" {
        return Err(ApiError::NotFound);
    }
    let a = load(&state, &access, id).await?;
    if a.expired {
        return Err(ApiError::Gone("Artifact has expired".into()));
    }
    crate::web::redirect_download(&state, crate::web::Download::Artifact(a.id)).await
}
