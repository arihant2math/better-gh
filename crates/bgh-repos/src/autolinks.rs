//! Autolink references (`JIRA-123` → URL). Admin only.
//!
//! * `GET|POST /repos/{o}/{r}/autolinks`
//! * `GET|DELETE /repos/{o}/{r}/autolinks/{id}`

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use bgh_core::audit;
use bgh_core::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::json;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/repos/{owner}/{repo}/autolinks", get(list).post(create))
        .route(
            "/repos/{owner}/{repo}/autolinks/{id}",
            get(get_one).delete(delete),
        )
}

/// `autolink` (also the row shape).
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Autolink {
    pub id: i64,
    pub key_prefix: String,
    pub url_template: String,
    pub is_alphanumeric: bool,
}

const COLUMNS: &str = "id, key_prefix, url_template, is_alphanumeric";

async fn admin_access(
    state: &AppState,
    auth: &AuthContext,
    owner: &str,
    repo: &str,
) -> ApiResult<RepoAccess> {
    let access = RepoAccess::load(state, Some(auth), owner, repo).await?;
    access.require(Permission::Admin)?;
    Ok(access)
}

/// `GET /repos/{owner}/{repo}/autolinks` (unpaginated, like GitHub).
async fn list(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<Json<Vec<Autolink>>> {
    let access = admin_access(&state, &auth, &owner, &repo).await?;
    let rows = sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM repo_autolinks WHERE repo_id = $1 ORDER BY id"
    ))
    .bind(access.repo.id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(rows))
}

#[derive(Debug, Deserialize)]
struct CreateBody {
    key_prefix: Option<String>,
    url_template: Option<String>,
    is_alphanumeric: Option<bool>,
}

/// Allowed key prefix characters (GitHub: letters, digits and `.-_+=:/#`).
fn valid_prefix(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= 100
        && p.chars()
            .all(|c| c.is_ascii_alphanumeric() || ".-_+=:/#".contains(c))
}

/// `POST /repos/{owner}/{repo}/autolinks`
async fn create(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo)): Path<(String, String)>,
    Json(body): Json<CreateBody>,
) -> ApiResult<(StatusCode, Json<Autolink>)> {
    let access = admin_access(&state, &auth, &owner, &repo).await?;
    let mut errors = Vec::new();
    let key_prefix = body.key_prefix.unwrap_or_default();
    let url_template = body.url_template.unwrap_or_default();
    if key_prefix.is_empty() {
        errors.push(FieldError::missing_field("Autolink", "key_prefix"));
    } else if !valid_prefix(&key_prefix) {
        errors.push(FieldError::invalid("Autolink", "key_prefix"));
    }
    if url_template.is_empty() {
        errors.push(FieldError::missing_field("Autolink", "url_template"));
    } else if !url_template.contains("<num>") {
        errors.push(FieldError::custom(
            "Autolink",
            "url_template",
            "url_template must contain <num>",
        ));
    }
    if !errors.is_empty() {
        return Err(ApiError::validation(errors));
    }
    let already = || ApiError::invalid_field(FieldError::already_exists("Autolink", "key_prefix"));

    let mut tx = Tx::begin(&state).await?;
    let link: Autolink = sqlx::query_as(&format!(
        "INSERT INTO repo_autolinks (repo_id, key_prefix, url_template, is_alphanumeric)
         VALUES ($1, $2, $3, $4) RETURNING {COLUMNS}"
    ))
    .bind(access.repo.id)
    .bind(&key_prefix)
    .bind(&url_template)
    .bind(body.is_alphanumeric.unwrap_or(true))
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| match bgh_core::db::unique_violation(&e).as_deref() {
        Some("repo_autolinks_prefix_key") => already(),
        _ => e.into(),
    })?;
    audit::log(
        &mut *tx,
        Some(&auth.user),
        "repository_autolink.create",
        target(&access),
        json!({ "key_prefix": key_prefix, "url_template": url_template }),
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(link)))
}

fn target(access: &RepoAccess) -> audit::Target {
    audit::Target::Repo {
        id: access.repo.id,
        org_id: access.owner.is_org().then_some(access.owner.id),
    }
}

/// `GET /repos/{owner}/{repo}/autolinks/{id}`
async fn get_one(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, id)): Path<(String, String, i64)>,
) -> ApiResult<Json<Autolink>> {
    let access = admin_access(&state, &auth, &owner, &repo).await?;
    let link = sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM repo_autolinks WHERE repo_id = $1 AND id = $2"
    ))
    .bind(access.repo.id)
    .bind(id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)?;
    Ok(Json(link))
}

/// `DELETE /repos/{owner}/{repo}/autolinks/{id}`
async fn delete(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, id)): Path<(String, String, i64)>,
) -> ApiResult<StatusCode> {
    let access = admin_access(&state, &auth, &owner, &repo).await?;
    let mut tx = Tx::begin(&state).await?;
    let prefix: String = sqlx::query_scalar(
        "DELETE FROM repo_autolinks WHERE repo_id = $1 AND id = $2 RETURNING key_prefix",
    )
    .bind(access.repo.id)
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(ApiError::NotFound)?;
    audit::log(
        &mut *tx,
        Some(&auth.user),
        "repository_autolink.destroy",
        target(&access),
        json!({ "key_prefix": prefix }),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
