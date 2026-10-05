//! bgh-import: metadata importer, part 1 (P18): GitHub.com / GHES issues,
//! labels, milestones, releases (with assets), repository settings, teams
//! and users (mapped or mannequins). Git comes through P11's import.
//! Pull requests, reviews, wikis and GitLab are P51.
//!
//! Private endpoints under `/_bgh/metadata-imports`, the `import.run` job,
//! the `import.sweeper` service and the `bgh import github` CLI
//! (`cli::github`). Migrations: 3000-3099. Status:
//! `docs/packages/p18-metadata-import.md`.

pub mod api;
pub mod client;
pub mod pipeline;
pub mod row;
pub mod users;

use axum::Router;
use axum::routing::{get, post};
use bgh_core::{AppState, Registry};

/// No public REST routes (GitHub's importer APIs are not emulated).
pub fn router() -> Router<AppState> {
    Router::new()
}

pub fn web_router() -> Router<AppState> {
    Router::new()
        .route("/_bgh/metadata-imports", post(api::create))
        .route("/_bgh/metadata-imports/{id}", get(api::get))
        .route("/_bgh/metadata-imports/{id}/log", get(api::log))
        .route("/_bgh/metadata-imports/{id}/cancel", post(api::cancel))
        .route("/_bgh/metadata-imports/{id}/resume", post(api::resume))
        .route("/_bgh/admin/metadata-imports", get(api::list_admin))
        .route("/_bgh/orgs/{org}/metadata-imports", get(api::list_org))
}

pub fn register(reg: &mut Registry) {
    reg.job(pipeline::run_job);
    reg.service("import.sweeper", |state, shutdown| async move {
        loop {
            if let Err(e) = pipeline::sweep_stale(&state).await {
                tracing::warn!("import sweeper: {e:#}");
            }
            tokio::select! {
                _ = shutdown.cancelled() => return Ok(()),
                _ = tokio::time::sleep(std::time::Duration::from_secs(60)) => {}
            }
        }
    });
}
