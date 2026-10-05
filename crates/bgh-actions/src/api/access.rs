//! `GET`/`PUT /repos/{o}/{r}/actions/permissions/access`: which other
//! repositories may use this (private or internal) repository's reusable
//! workflows (see [`crate::reusable::may_call`]).

use axum::extract::State;
use axum::http::StatusCode;
use bgh_core::prelude::*;
use serde::Deserialize;
use serde_json::{Value, json};

const LEVELS: &[&str] = &["none", "user", "organization", "enterprise"];

pub async fn get(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let a = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    a.require(Permission::Admin)?;
    let mut conn = state.db.acquire().await?;
    let level = crate::reusable::access_level(&mut conn, a.repo.id).await?;
    Ok(Json(json!({ "access_level": level })))
}

#[derive(Deserialize)]
pub struct PutAccess {
    access_level: Option<String>,
}

pub async fn put(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo)): Path<(String, String)>,
    Json(body): Json<PutAccess>,
) -> ApiResult<StatusCode> {
    let a = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    a.require(Permission::Admin)?;
    let level = body.access_level.unwrap_or_default();
    if !LEVELS.contains(&level.as_str()) {
        return Err(ApiError::unprocessable(format!(
            "Invalid access_level '{level}': expected one of {}",
            LEVELS.join(", ")
        )));
    }
    if a.repo.visibility == "public" {
        return Err(ApiError::unprocessable(
            "Access settings apply only to private and internal repositories.",
        ));
    }
    if level == "enterprise" && a.repo.visibility != "internal" {
        return Err(ApiError::unprocessable(
            "The enterprise access level applies only to internal repositories.",
        ));
    }
    let mut tx = Tx::begin(&state).await?;
    sqlx::query(
        "INSERT INTO actions_repo_access (repo_id, access_level) VALUES ($1, $2)
         ON CONFLICT (repo_id) DO UPDATE SET access_level = $2, updated_at = now()",
    )
    .bind(a.repo.id)
    .bind(&level)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
