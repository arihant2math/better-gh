//! Minimal deployment environments (`/repos/{o}/{r}/environments`), the
//! scope of environment secrets and variables. Protection rules and
//! deployment branch policies are not implemented.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use bgh_core::pagination::Pagination;
use bgh_core::prelude::*;
use serde_json::Value;

use super::wrapped;
use crate::json::environment_json;
use crate::models::EnvironmentRow;

pub async fn list(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<Response> {
    let a = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let total: i64 =
        sqlx::query_scalar("SELECT count(*) FROM actions_environments WHERE repo_id = $1")
            .bind(a.repo.id)
            .fetch_one(&state.db)
            .await?;
    let rows: Vec<EnvironmentRow> = sqlx::query_as(&format!(
        "SELECT {} FROM actions_environments WHERE repo_id = $1 ORDER BY lower(name), id
          LIMIT $2 OFFSET $3",
        EnvironmentRow::COLUMNS
    ))
    .bind(a.repo.id)
    .bind(p.limit())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let items: Vec<Value> = rows
        .iter()
        .map(|e| environment_json(&state, &a, e))
        .collect();
    Ok(wrapped(&p, total, "environments", items))
}

async fn find(state: &AppState, a: &RepoAccess, name: &str) -> ApiResult<EnvironmentRow> {
    sqlx::query_as(&format!(
        "SELECT {} FROM actions_environments WHERE repo_id = $1 AND lower(name) = lower($2)",
        EnvironmentRow::COLUMNS
    ))
    .bind(a.repo.id)
    .bind(name)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)
}

pub async fn get(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, name)): Path<(String, String, String)>,
) -> ApiResult<Json<Value>> {
    let a = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let e = find(&state, &a, &name).await?;
    Ok(Json(environment_json(&state, &a, &e)))
}

/// `PUT /repos/{o}/{r}/environments/{name}`: create or update (200).
pub async fn put(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, name)): Path<(String, String, String)>,
) -> ApiResult<Json<Value>> {
    let a = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    a.require(Permission::Admin)?;
    if name.is_empty() || name.len() > 255 || name.contains('/') {
        return Err(ApiError::invalid_field(FieldError::invalid(
            "Environment",
            "name",
        )));
    }
    let e: EnvironmentRow = sqlx::query_as(&format!(
        "INSERT INTO actions_environments (repo_id, name) VALUES ($1, $2)
         ON CONFLICT (repo_id, lower(name)) DO UPDATE SET updated_at = now()
         RETURNING {}",
        EnvironmentRow::COLUMNS
    ))
    .bind(a.repo.id)
    .bind(&name)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(environment_json(&state, &a, &e)))
}

pub async fn delete(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, name)): Path<(String, String, String)>,
) -> ApiResult<StatusCode> {
    let a = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    a.require(Permission::Admin)?;
    let e = find(&state, &a, &name).await?;
    sqlx::query("DELETE FROM actions_environments WHERE id = $1")
        .bind(e.id)
        .execute(&state.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
