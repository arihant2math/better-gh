//! bgh-projects: Projects (GitHub Projects v2 semantics) owned by users and
//! organizations — items (issues, pull requests, draft issues), custom
//! fields, views, item ordering and workflows-lite.
//!
//! The web client uses the private JSON API under `/_bgh/projects`,
//! `/_bgh/owners/{owner}/projects` and `/_bgh/repos/{o}/{r}/projects`; every
//! write records sync actions in the owner's `org:{id}` / `user:{id}` scope
//! (models `project`, `projectField`, `projectView`, `projectItem`,
//! `projectWorkflow`; see `docs/SYNC_PROTOCOL.md` §11 and
//! `docs/packages/projects-wiki.md`). Migrations: 1100-1199.

pub mod access;
pub mod bootstrap;
pub mod compact;
pub mod fields;
pub mod filter;
pub mod items;
pub mod model;
pub mod position;
pub mod projects;
pub mod service;
mod util;
pub mod views;
pub mod workflows;

use axum::Router;
use axum::routing::{get, patch, post, put};
use bgh_core::{AppState, Registry};

/// REST API routes (relative to `/api/v3`). Projects have no REST surface yet.
pub fn router() -> Router<AppState> {
    Router::new()
}

/// Private web-client routes.
pub fn web_router() -> Router<AppState> {
    Router::new()
        .route("/_bgh/projects", post(projects::create))
        .route(
            "/_bgh/projects/{id}",
            get(projects::get)
                .patch(projects::update)
                .delete(projects::delete),
        )
        .route(
            "/_bgh/projects/{id}/repos/{repo_id}",
            put(projects::link_repo).delete(projects::unlink_repo),
        )
        .route("/_bgh/projects/{id}/fields", post(fields::create))
        .route(
            "/_bgh/projects/{id}/fields/{field_id}",
            patch(fields::update).delete(fields::delete),
        )
        .route("/_bgh/projects/{id}/items", post(items::create))
        .route(
            "/_bgh/projects/{id}/items/{item_id}",
            patch(items::update).delete(items::delete),
        )
        .route("/_bgh/projects/{id}/views", post(views::create))
        .route(
            "/_bgh/projects/{id}/views/{view_id}",
            patch(views::update).delete(views::delete),
        )
        .route("/_bgh/projects/{id}/workflows/{kind}", put(workflows::put))
        .route(
            "/_bgh/owners/{owner}/projects",
            get(projects::list_for_owner),
        )
        .route(
            "/_bgh/owners/{owner}/projects/{number}",
            get(projects::get_by_number),
        )
        .route(
            "/_bgh/repos/{owner}/{repo}/projects",
            get(projects::list_for_repo),
        )
}

/// Workflow listener and the bootstrap scope provider.
pub fn register(reg: &mut Registry) {
    reg.on_event("projects.workflows", workflows::on_event);
    reg.scope_provider(bootstrap::PROVIDER);
}
