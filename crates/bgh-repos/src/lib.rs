//! bgh-repos: repositories REST API and git transport routes.
//!
//! Each feature module exposes `routes()` (paths relative to `/api/v3`),
//! merged by [`router`]. See `docs/packages/repos-api.md` for the endpoint
//! list. Migrations: 0200-0249 (repos-api).
//!
//! Shared helpers: [`gitjson`] (GitHub shapes for git data), [`identity`]
//! (commit identities, email → user batch mapping), [`media`] (`Accept`
//! media types), [`cache`] (Redis cache for SHA-keyed data), [`refs`]
//! (API ref writes with branch protection), [`protection`] (rules engine).

pub mod autolinks;
pub mod branches;
pub mod browse;
pub mod cache;
pub mod collaborators;
pub mod commits;
pub mod contents;
pub mod create;
pub mod download;
pub mod forks;
pub mod git_http;
pub mod gitdb;
pub mod gitjson;
pub mod identity;
pub mod import;
pub mod jobs;
pub mod json;
pub mod keys;
pub mod lfs;
pub mod maintenance;
pub mod media;
pub mod mirrors;
pub mod protection;
pub mod protection_api;
pub mod refs;
pub mod repos;
pub mod rulesets;
pub mod settings;
pub mod ssh;
pub mod stars;
pub mod stats;
pub mod watching;

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
            get(repos::get_repo)
                .delete(repos::delete_repo)
                .patch(settings::update_repo),
        )
        .merge(settings::routes())
        .merge(stats::routes())
        .merge(forks::routes())
        .merge(stars::routes())
        .merge(watching::routes())
        .merge(collaborators::routes())
        .merge(keys::routes())
        .merge(autolinks::routes())
        .merge(contents::routes())
        .merge(gitdb::routes())
        .merge(commits::routes())
        .merge(branches::routes())
        .merge(protection_api::routes())
        .merge(rulesets::routes())
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
        .merge(import::web_routes())
        .merge(mirrors::web_routes())
}

/// Job handlers: post-receive processing, storage cleanup, languages.
pub fn register(reg: &mut Registry) {
    reg.job(jobs::post_receive);
    reg.job(jobs::delete_storage);
    reg.job(stats::compute_languages);
    reg.job(lfs::gc::run);
    reg.on_event("repos.transport_cleanup", lfs::gc::on_event);
    reg.job(maintenance::pack_refs);
    reg.on_event("repos.pack_refs", maintenance::on_event);
    reg.service("ssh", ssh::service);
    reg.service("repos.config_upgrade", maintenance::config_upgrade_service);
    reg.job(import::run_import_job);
    reg.job(mirrors::sync_job);
    reg.service("repos.mirrors", mirrors::service);
}
