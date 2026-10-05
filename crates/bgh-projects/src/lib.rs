//! bgh-projects.

use axum::Router;
use bgh_core::{AppState, Registry};

/// REST API routes (relative to `/api/v3`).
pub fn router() -> Router<AppState> {
    Router::new()
}

/// Non-API routes with absolute paths.
pub fn web_router() -> Router<AppState> {
    Router::new()
}

/// Register background job handlers and event listeners.
pub fn register(_reg: &mut Registry) {}
