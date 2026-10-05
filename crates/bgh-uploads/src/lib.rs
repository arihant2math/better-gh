//! bgh-uploads: user attachments (P6).
//!
//! `POST /_bgh/uploads?repository_id=&owner_id=` stores an image, video or
//! file pasted into a comment, PR, release or wiki page and answers
//! `{id, href, markdown, ...}`. Attachments are served at GitHub's paths,
//! `/user-attachments/assets/{uuid}` (images, videos) and
//! `/user-attachments/files/{id}/{name}`, with read access to the owning
//! repository required for private ones. Blobs are content-addressed under
//! `{data_dir}/files/attachments/`. Migrations: 1800-1899.

pub mod gc;
pub mod model;
pub mod policy;
pub mod serve;
pub mod storage;
pub mod upload;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::routing::{get, post};
use bgh_core::{AppState, Registry};

/// REST API routes (none: uploads are a web endpoint, like on GitHub).
pub fn router() -> Router<AppState> {
    Router::new()
}

/// Absolute paths.
pub fn web_router() -> Router<AppState> {
    Router::new()
        .route(
            "/_bgh/uploads",
            // Size limits are per file type and enforced while streaming.
            post(upload::upload).layer(DefaultBodyLimit::disable()),
        )
        .route("/user-attachments/assets/{uuid}", get(serve::asset))
        .route("/user-attachments/files/{id}/{name}", get(serve::file))
}

pub fn register(reg: &mut Registry) {
    reg.job(gc::run);
    reg.on_event("uploads.gc", gc::on_event);
}
