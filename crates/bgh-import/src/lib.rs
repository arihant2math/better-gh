//! bgh-import: metadata importer. Part 1 (P18): GitHub.com / GHES issues,
//! labels, milestones, releases (with assets), repository settings, teams
//! and users (mapped or mannequins). Part 2 (P51): pull requests with
//! reviews, review comments and requested reviewers (`pulls`), wikis and
//! git refs (`gitops`), webhooks / branch protection / rulesets
//! (`repo_config`), GitLab as a source (`gitlab`) and mannequin reclaim
//! (`reclaim`). Git comes through P11's import.
//!
//! Private endpoints under `/_bgh/metadata-imports` and `/_bgh/…mannequin…`,
//! the `import.run` job, the `import.sweeper` service and the `bgh import
//! github|gitlab` CLI. Migrations: 3000-3099, 6300-6399. Status:
//! `docs/packages/p18-metadata-import.md`, `docs/packages/p51-metadata-import-2.md`.

pub mod api;
pub mod cli;
pub mod client;
pub mod gitlab;
pub mod gitops;
pub mod pipeline;
pub mod pulls;
pub mod reclaim;
pub mod repo_config;
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
        // P51: mannequin reclaim.
        .route("/_bgh/orgs/{org}/mannequins", get(reclaim::list_org))
        .route("/_bgh/admin/mannequins", get(reclaim::list_admin))
        .route("/_bgh/mannequins/{id}/reclaims", post(reclaim::invite))
        .route(
            "/_bgh/mannequin-reclaims/{id}",
            axum::routing::delete(reclaim::cancel),
        )
        .route("/_bgh/user/mannequin-reclaims", get(reclaim::list_mine))
        .route(
            "/_bgh/user/mannequin-reclaims/{id}/accept",
            post(reclaim::accept),
        )
        .route(
            "/_bgh/user/mannequin-reclaims/{id}/decline",
            post(reclaim::decline),
        )
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
