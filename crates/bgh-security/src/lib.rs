//! Security features: secret scanning (pattern engine, history and push
//! scans, alerts API, custom patterns) and push protection (P65). Code
//! scanning and the rest of the Security tab build on this crate (P66).
//!
//! * `patterns`: built-in, non-provider and custom patterns ([`patterns::Engine`]).
//! * `scan`: which blobs commits introduce, read in bounded batches.
//! * `push`: the receive-pack object check (wired by bgh-repos).
//! * `jobs`: `security.scan_history` / `security.scan_push`, the push
//!   listener and the `security.backfill` service.
//! * `alerts`, `custom`: REST and internal routes.
//! * `settings`: `security_and_analysis` and the site-wide switches.

pub mod alerts;
pub mod custom;
pub mod jobs;
pub mod patterns;
pub mod push;
pub mod scan;
pub mod settings;
pub mod store;

use axum::Router;
use axum::routing::{get, patch, post};
use bgh_core::registry::Registry;
use bgh_core::state::AppState;

/// REST routes (relative to `/api/v3`).
pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/repos/{owner}/{repo}/secret-scanning/alerts",
            get(alerts::list_repo),
        )
        .route(
            "/repos/{owner}/{repo}/secret-scanning/alerts/{number}",
            get(alerts::get).patch(alerts::update),
        )
        .route(
            "/repos/{owner}/{repo}/secret-scanning/alerts/{number}/locations",
            get(alerts::locations),
        )
        .route(
            "/repos/{owner}/{repo}/secret-scanning/push-protection-bypasses",
            post(alerts::bypass),
        )
        .route(
            "/repos/{owner}/{repo}/secret-scanning/scan-history",
            get(alerts::scan_history),
        )
        .route("/orgs/{org}/secret-scanning/alerts", get(alerts::list_org))
}

/// Internal routes of the web client (absolute paths).
pub fn web_router() -> Router<AppState> {
    Router::new()
        .route(
            "/_bgh/repos/{owner}/{repo}/secret-scanning/settings",
            get(alerts::get_settings),
        )
        .route(
            "/_bgh/repos/{owner}/{repo}/secret-scanning/scan",
            post(alerts::scan_now),
        )
        .route(
            "/_bgh/repos/{owner}/{repo}/secret-scanning/push-blocks/{placeholder}",
            get(alerts::get_block),
        )
        .route(
            "/_bgh/repos/{owner}/{repo}/secret-scanning/custom-patterns",
            get(custom::list_repo).post(custom::create_repo),
        )
        .route(
            "/_bgh/repos/{owner}/{repo}/secret-scanning/custom-patterns/{id}",
            patch(custom::update_repo).delete(custom::delete_repo),
        )
        .route(
            "/_bgh/orgs/{org}/secret-scanning/custom-patterns",
            get(custom::list_org).post(custom::create_org),
        )
        .route(
            "/_bgh/orgs/{org}/secret-scanning/custom-patterns/{id}",
            patch(custom::update_org).delete(custom::delete_org),
        )
        .route(
            "/_bgh/secret-scanning/custom-patterns/test",
            post(custom::test_pattern),
        )
        .route("/_bgh/secret-scanning/patterns", get(custom::builtin))
}

pub fn register(reg: &mut Registry) {
    reg.job(jobs::scan_history);
    reg.job(jobs::scan_push);
    reg.on_event("security.secret_scanning", jobs::on_event);
    reg.service("security.backfill", jobs::backfill_service);
}
