//! `GET /repositories/{id}` (same body as `/repos/{owner}/{repo}`) and
//! `GET /repositories?since=` (every readable public repository by id).

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use bgh_core::models::api::Repository;
use bgh_core::prelude::*;
use bgh_core::views;
use serde::Deserialize;

use crate::json::full_repo;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/repositories", get(list_all))
        .route("/repositories/{id}", get(get_by_id))
}

/// `GET /repositories/{id}`: 404 for unknown ids and repositories the
/// caller can't read.
async fn get_by_id(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path(id): Path<String>,
) -> ApiResult<Json<Repository>> {
    let id: i64 = id.parse().map_err(|_| ApiError::NotFound)?;
    let repo = db::Repository::find(&state.db, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let owner = db::User::find(&state.db, repo.owner_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let access = RepoAccess::for_repo(&state, auth.as_ref(), repo, owner).await?;
    Ok(Json(full_repo(&state, auth.as_ref(), &access).await?))
}

#[derive(Debug, Default, Deserialize)]
struct SinceQuery {
    since: Option<i64>,
    per_page: Option<u32>,
}

/// `GET /repositories?since=`: public (and, for those who can read them,
/// internal) repositories with `id > since`, ordered by id. Pagination is
/// by `since` only (Link `next`), like GitHub.
async fn list_all(
    State(state): State<AppState>,
    auth: MaybeUser,
    Query(q): Query<SinceQuery>,
) -> ApiResult<Response> {
    let per_page = q.per_page.unwrap_or(100).clamp(1, 100);
    let mut rows: Vec<db::Repository> = sqlx::query_as(&format!(
        "SELECT {} FROM repositories
          WHERE id > $1 AND visibility IN ('public', 'internal')
          ORDER BY id LIMIT $2",
        db::Repository::COLUMNS
    ))
    .bind(q.since.unwrap_or(0))
    .bind(i64::from(per_page) + 1)
    .fetch_all(&state.db)
    .await?;
    let has_next = rows.len() > per_page as usize;
    rows.truncate(per_page as usize);
    let last = rows.last().map(|r| r.id);
    // Drops what the caller can't read (internal repos, private mode).
    let items = views::minimal_repos(&state, auth.as_ref(), rows).await?;
    let base = state.urls.api("/repositories");
    let mut links = Vec::new();
    if has_next && let Some(last) = last {
        let pp = q
            .per_page
            .map(|_| format!("per_page={per_page}&"))
            .unwrap_or_default();
        links.push(format!("<{base}?{pp}since={last}>; rel=\"next\""));
    }
    links.push(format!("<{base}{{?since}}>; rel=\"first\""));
    let mut resp = Json(items).into_response();
    if let Ok(v) = HeaderValue::from_str(&links.join(", ")) {
        resp.headers_mut().insert(header::LINK, v);
    }
    Ok(resp)
}
