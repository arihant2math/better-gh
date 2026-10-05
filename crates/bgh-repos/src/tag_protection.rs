//! Legacy tag protection (`/repos/{o}/{r}/tags/protection`), mapped onto
//! tag rulesets like GitHub's migration did: each protected pattern is an
//! active tag ruleset (`tag_protection = true`) restricting creation,
//! update and deletion of matching tags to maintainers and admins.
//!
//! * `GET|POST /repos/{o}/{r}/tags/protection`
//! * `DELETE /repos/{o}/{r}/tags/protection/{id}`
//!
//! Repository admins only. The id of a tag protection is its ruleset's id.

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get};
use bgh_core::prelude::*;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::protection::RulesetRow;
use crate::rulesets::{Fields, delete_repo_ruleset, insert_repo_ruleset, user_of};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/repos/{owner}/{repo}/tags/protection",
            get(list).post(create),
        )
        .route(
            "/repos/{owner}/{repo}/tags/protection/{id}",
            delete(destroy),
        )
}

fn render(r: &RulesetRow) -> Value {
    json!({
        "id": r.id,
        "pattern": r.conditions["ref_name"]["include"][0],
        "created_at": Timestamp::from(r.created_at),
        "updated_at": Timestamp::from(r.updated_at),
        "enabled": r.enforcement == "active",
    })
}

async fn admin_access(
    state: &AppState,
    auth: &MaybeUser,
    owner: &str,
    repo: &str,
) -> ApiResult<RepoAccess> {
    let access = RepoAccess::load(state, auth.as_ref(), owner, repo).await?;
    access.require(Permission::Admin)?;
    Ok(access)
}

/// `GET /repos/{owner}/{repo}/tags/protection`
async fn list(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<Json<Vec<Value>>> {
    let access = admin_access(&state, &auth, &owner, &repo).await?;
    let rows: Vec<RulesetRow> = sqlx::query_as(&format!(
        "SELECT {} FROM repo_rulesets WHERE repo_id = $1 AND tag_protection ORDER BY id",
        RulesetRow::COLUMNS
    ))
    .bind(access.repo.id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(rows.iter().map(render).collect()))
}

#[derive(Debug, Deserialize)]
struct CreateInput {
    pattern: Option<String>,
}

/// `POST /repos/{owner}/{repo}/tags/protection`
async fn create(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo)): Path<(String, String)>,
    Json(input): Json<CreateInput>,
) -> ApiResult<Response> {
    let access = admin_access(&state, &auth, &owner, &repo).await?;
    access.require_not_archived()?;
    let user = user_of(&auth)?;
    let pattern = input
        .pattern
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .ok_or_else(|| {
            ApiError::invalid_field(FieldError::missing_field("TagProtection", "pattern"))
        })?;
    let f = Fields {
        name: format!("Tag protection: {pattern}"),
        target: "tag".into(),
        enforcement: "active".into(),
        conditions: json!({"ref_name": {"include": [pattern], "exclude": []}}),
        rules: json!([{"type": "creation"}, {"type": "update", "parameters":
            {"update_allows_fetch_and_merge": false}}, {"type": "deletion"}]),
        // Maintainers (and admins) may still manage protected tags.
        bypass_actors: json!([{"actor_id": 2, "actor_type": "RepositoryRole",
            "bypass_mode": "always"}]),
    };
    let row = insert_repo_ruleset(&state, &access, user, &f, true).await?;
    Ok((StatusCode::CREATED, Json(render(&row))).into_response())
}

/// `DELETE /repos/{owner}/{repo}/tags/protection/{id}`
async fn destroy(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, id)): Path<(String, String, i64)>,
) -> ApiResult<StatusCode> {
    let access = admin_access(&state, &auth, &owner, &repo).await?;
    access.require_not_archived()?;
    let user = user_of(&auth)?;
    delete_repo_ruleset(&state, &access, user, id, true).await?;
    Ok(StatusCode::NO_CONTENT)
}
