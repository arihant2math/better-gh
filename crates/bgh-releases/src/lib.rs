//! bgh-releases: releases and release assets.
//!
//! Implemented: release CRUD (`/repos/{o}/{r}/releases`, `/latest`,
//! `/tags/{tag}`, `/{id}`) with draft/prerelease/`make_latest` semantics,
//! tag creation from `target_commitish` on publish, generated release notes
//! (`/releases/generate-notes`), assets (list, streaming upload on the API
//! path and the `{base}/api/uploads/...` uploads host advertised in
//! `upload_url`, JSON or `application/octet-stream` download with download
//! counts, update, delete, browser download
//! `/{o}/{r}/releases/download/{tag}/{name}`), reactions on releases.
//! Asset blobs live in a content-addressed [`storage::AssetStorage`]
//! (disk by default). Migrations: 0600-0699.

pub mod assets;
pub mod git;
pub mod import;
pub mod model;
pub mod notes;
pub mod reactions;
pub mod releases;
pub mod storage;

use axum::Router;
use axum::routing::{delete, get, post};
use bgh_core::{AppState, Registry};

/// REST API routes. Paths are relative to `/api/v3`.
pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/repos/{owner}/{repo}/releases",
            get(releases::list).post(releases::create),
        )
        .route(
            "/repos/{owner}/{repo}/releases/latest",
            get(releases::latest),
        )
        .route(
            "/repos/{owner}/{repo}/releases/tags/{tag}",
            get(releases::by_tag),
        )
        .route(
            "/repos/{owner}/{repo}/releases/generate-notes",
            post(notes::generate_notes),
        )
        .route(
            "/repos/{owner}/{repo}/releases/{release_id}",
            get(releases::get)
                .patch(releases::update)
                .delete(releases::delete),
        )
        .route(
            "/repos/{owner}/{repo}/releases/{release_id}/assets",
            get(assets::list).post(assets::upload),
        )
        .route(
            "/repos/{owner}/{repo}/releases/assets/{asset_id}",
            get(assets::get)
                .patch(assets::update)
                .delete(assets::delete),
        )
        .route(
            "/repos/{owner}/{repo}/releases/{release_id}/reactions",
            get(reactions::list).post(reactions::create),
        )
        .route(
            "/repos/{owner}/{repo}/releases/{release_id}/reactions/{reaction_id}",
            delete(reactions::delete),
        )
}

/// The uploads host (`upload_url`) and browser downloads (absolute paths).
pub fn web_router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/uploads/repos/{owner}/{repo}/releases/{release_id}/assets",
            post(assets::upload),
        )
        .route(
            "/{owner}/{repo}/releases/download/{tag}/{name}",
            get(assets::browser_download),
        )
}

/// No background jobs or listeners.
pub fn register(_reg: &mut Registry) {}
