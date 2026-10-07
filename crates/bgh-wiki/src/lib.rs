//! bgh-wiki: git-backed repository wikis.
//!
//! Pages are files at the root of `{data_dir}/repos/{xx}/{id}.wiki.git`
//! (branch `master`), served through the private JSON API under
//! `/_bgh/repos/{owner}/{repo}/wiki` and git smart HTTP at
//! `/{owner}/{repo}.wiki.git` (routed via bgh-repos, see [`git`]).
//! Contract: `docs/packages/projects-wiki.md` ("Wiki"). Migrations: 1200-1299.

pub mod access;
pub mod api;
pub mod git;
pub mod jobs;
pub mod pages;
pub mod render;

use axum::Router;
use axum::routing::{get, post};
use bgh_core::{AppState, Registry};

/// REST API routes (relative to `/api/v3`): none (GitHub has no wiki API).
pub fn router() -> Router<AppState> {
    Router::new()
}

/// Private JSON API (absolute paths).
pub fn web_router() -> Router<AppState> {
    const BASE: &str = "/_bgh/repos/{owner}/{repo}/wiki";
    let r = |p: &str| format!("{BASE}{p}");
    Router::new()
        .route(BASE, get(api::overview))
        .route(&r("/pages"), post(api::create_page))
        .route(
            &r("/pages/{slug}"),
            get(api::get_page)
                .put(api::update_page)
                .delete(api::delete_page),
        )
        .route(&r("/pages/{slug}/raw"), get(api::raw_page))
        .route(&r("/pages/{slug}/history"), get(api::page_history))
        .route(&r("/pages/{slug}/revert"), post(api::revert_page))
        .route(&r("/compare/{range}"), get(api::compare))
        .route(&r("/history"), get(api::wiki_history))
        .route(&r("/search"), get(api::search))
        .route(
            &r("/settings"),
            get(api::get_settings).patch(api::update_settings),
        )
}

/// Wiki storage cleanup on repository deletion.
pub fn register(reg: &mut Registry) {
    reg.job(jobs::delete_storage);
    reg.on_event("wiki.delete_storage", jobs::on_event);
}
