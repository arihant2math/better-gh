//! bgh-repos: repositories and git transport.
//!
//! Implemented: create (`POST /user/repos`, `POST /orgs/{org}/repos`),
//! `GET`/`DELETE /repos/{owner}/{repo}`, listing (`GET /user/repos`,
//! `GET /users/{username}/repos`, `GET /orgs/{org}/repos`), git smart HTTP
//! (`/{owner}/{repo}.git/info/refs`, `git-upload-pack`, `git-receive-pack`)
//! with permission checks and basic branch protection, and post-receive
//! processing (`repos.post_receive` job).
//! Planned here: PATCH/transfer/fork, collaborators, stars, watching,
//! topics, contents/trees/blobs/commits/refs APIs, branches & protection
//! APIs, compare, deploy keys. Migrations: 0200-0299.

pub mod browse;
pub mod create;
pub mod download;
pub mod git_http;
pub mod jobs;
pub mod json;
pub mod lfs;
pub mod maintenance;
pub mod protection;
pub mod repos;
pub mod ssh;

use axum::Router;
use axum::routing::{get, post};
use bgh_core::{AppState, Registry};
use bgh_git::RepoStore;

/// The repository store configured for this process.
pub fn store(state: &AppState) -> RepoStore {
    RepoStore::from_config(&state.config)
}

/// REST routes (relative to `/api/v3`).
pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/user/repos",
            post(create::create_for_user).get(repos::list_for_authenticated_user),
        )
        .route(
            "/orgs/{org}/repos",
            post(create::create_for_org).get(repos::list_for_org),
        )
        .route("/users/{username}/repos", get(repos::list_for_user))
        .route(
            "/repos/{owner}/{repo}",
            get(repos::get_repo).delete(repos::delete_repo),
        )
        .merge(download::api_router())
}

/// Git smart-HTTP routes (absolute paths). `{repo}` may carry `.git`.
pub fn web_router() -> Router<AppState> {
    Router::new()
        .route("/{owner}/{repo}/info/refs", get(git_http::info_refs))
        .route(
            "/{owner}/{repo}/git-upload-pack",
            post(git_http::upload_pack),
        )
        .route(
            "/{owner}/{repo}/git-receive-pack",
            post(git_http::receive_pack),
        )
        .merge(browse::web_router())
        .merge(download::web_router())
        .merge(lfs::web_router())
}

/// Job handlers: post-receive processing and storage cleanup.
pub fn register(reg: &mut Registry) {
    reg.job(jobs::post_receive);
    reg.job(jobs::delete_storage);
    reg.job(lfs::gc::run);
    reg.on_event("repos.transport_cleanup", lfs::gc::on_event);
    reg.job(maintenance::pack_refs);
    reg.on_event("repos.pack_refs", maintenance::on_event);
    reg.service("ssh", ssh::service);
}
