//! Raw files and source archives:
//!
//! * `GET /{owner}/{repo}/raw/{ref}/{path}`: file contents with safe
//!   content types; LFS pointers resolve to the stored object.
//! * `GET /{owner}/{repo}/archive/{ref}.tar.gz|.zip`: `git archive`,
//!   streamed and cached by commit SHA (`{repo}-{ref}/` prefix).
//! * `GET /{owner}/{repo}/legacy.tar.gz|legacy.zip/{ref}`: the targets of
//!   the REST `tarball`/`zipball` redirects (`{owner}-{repo}-{sha7}/`).
//! * REST `GET /repos/{owner}/{repo}/tarball|zipball[/{ref}]` → 302.
//!
//! Private repositories accept normal credentials (session cookie, token,
//! Basic) or a short-lived `?token=` (see [`token`]).

pub mod archive;
pub mod mime;
pub mod raw;
pub mod token;

use axum::Router;
use axum::routing::get;
use bgh_core::prelude::*;
use serde::Deserialize;

/// Absolute routes.
pub fn web_router() -> Router<AppState> {
    Router::new()
        .route("/{owner}/{repo}/raw/{*spec}", get(raw::get))
        .route("/{owner}/{repo}/archive/{*name}", get(archive::get))
        .route(
            "/{owner}/{repo}/legacy.tar.gz/{*spec}",
            get(archive::legacy_tar),
        )
        .route(
            "/{owner}/{repo}/legacy.zip/{*spec}",
            get(archive::legacy_zip),
        )
}

/// REST routes (relative to `/api/v3`).
pub fn api_router() -> Router<AppState> {
    Router::new()
        .route(
            "/repos/{owner}/{repo}/tarball",
            get(archive::tarball_default),
        )
        .route(
            "/repos/{owner}/{repo}/tarball/{*spec}",
            get(archive::tarball),
        )
        .route(
            "/repos/{owner}/{repo}/zipball",
            get(archive::zipball_default),
        )
        .route(
            "/repos/{owner}/{repo}/zipball/{*spec}",
            get(archive::zipball),
        )
}

#[derive(Debug, Default, Deserialize)]
pub struct TokenQuery {
    pub token: Option<String>,
}

/// Authorize a download: a valid `?token=` for this repository grants
/// read access; otherwise the caller's credentials decide (404 without
/// read access).
pub async fn access(
    state: &AppState,
    auth: Option<&AuthContext>,
    owner: &str,
    repo: &str,
    token: Option<&str>,
) -> ApiResult<RepoAccess> {
    if let Some(token) = token.filter(|t| !t.is_empty()) {
        let name = repo.strip_suffix(".git").unwrap_or(repo);
        let owner_row = db::User::find_by_login(&state.db, owner)
            .await?
            .ok_or(ApiError::NotFound)?;
        let repo_row = db::Repository::find_by_name(&state.db, owner_row.id, name)
            .await?
            .ok_or(ApiError::NotFound)?;
        if token::check(state, token, repo_row.id).await {
            return Ok(RepoAccess {
                repo: repo_row,
                owner: owner_row,
                permission: Permission::Read,
                authenticated: true,
            });
        }
        return Err(ApiError::NotFound);
    }
    RepoAccess::load(state, auth, owner, repo).await
}
