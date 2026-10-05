//! TODO

use axum::Router;
use bgh_core::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
}

pub async fn update_repo() -> bgh_core::ApiResult<()> {
    Err(bgh_core::ApiError::NotFound)
}
