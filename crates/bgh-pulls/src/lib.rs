//! bgh-pulls: Pull requests, reviews, review comments, merging, commit statuses, check runs/suites, CODEOWNERS, auto-merge.
//!
//! Status: stub. Owned by the pulls feature work; see
//! `docs/BACKEND_PATTERNS.md` for how to add routes, jobs and listeners.
//! Migrations for this crate use the 0400-0499 range.

use axum::Router;
use bgh_core::{AppState, Registry};

/// REST API routes. Paths are relative to `/api/v3` (bgh-server nests them),
/// e.g. `.route("/repos/{owner}/{repo}/things", get(list))`.
pub fn router() -> Router<AppState> {
    Router::new()
}

/// Non-API routes with absolute paths (`/_bgh/...`, raw/archive downloads),
/// merged at the root by bgh-server.
pub fn web_router() -> Router<AppState> {
    Router::new()
}

/// Register background job handlers and event listeners.
pub fn register(_reg: &mut Registry) {}
