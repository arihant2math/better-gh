//! Container registry (OCI Distribution v2 at `/v2/`) and the GitHub
//! Packages REST API.
//!
//! * `oci`: the registry protocol and the `/v2/token` endpoint;
//! * `rest`: `/user|users/{u}|orgs/{org}/packages[...]` (GitHub shapes);
//! * `web`: private `/_bgh/` endpoints of the web client;
//! * `gc`: periodic cleanup of uploads, expired deletes and unused blobs.
//!
//! Storage, access rules and quotas: `docs/packages/p15-container-registry.md`.

pub mod access;
pub mod digest;
pub mod gc;
pub mod model;
mod oci;
mod ops;
pub mod rest;
pub mod storage;
pub mod token;
mod visible;
mod web;

use axum::Router;
use bgh_core::{AppState, Registry};

/// REST routes (relative to `/api/v3`).
pub fn router() -> Router<AppState> {
    rest::router()
}

/// The registry at `/v2/` and the `/_bgh/` web endpoints.
pub fn web_router() -> Router<AppState> {
    Router::new().merge(oci::routes()).merge(web::router())
}

pub fn register(reg: &mut Registry) {
    reg.service("packages.gc", gc::service);
}
