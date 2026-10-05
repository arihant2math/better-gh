//! bgh-actions: GitHub Actions compatible CI.
//!
//! See `docs/packages/actions.md` for the feature overview.

pub mod crypto;
pub mod expr;
pub mod logs;
pub mod models;
pub mod protocol;
pub mod workflow;

use axum::Router;
use bgh_core::{AppState, Registry};

/// REST API routes (relative to `/api/v3`).
pub fn router() -> Router<AppState> {
    Router::new()
}

/// Non-API routes (`/_bgh/actions/...`).
pub fn web_router() -> Router<AppState> {
    Router::new()
}

/// Background jobs, event listeners and services.
pub fn register(_reg: &mut Registry) {}
