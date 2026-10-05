//! TODO

use axum::Router;
use bgh_core::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
}

/// Raw file downloads (absolute paths).
pub fn web_routes() -> Router<AppState> {
    Router::new()
}
