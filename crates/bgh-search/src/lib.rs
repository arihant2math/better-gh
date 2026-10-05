//! bgh-search: search API, code/commit index, activity events.
//!
//! Implemented:
//! * `/search/issues`, `/search/repositories`, `/search/users`,
//!   `/search/commits`, `/search/labels`, `/search/topics`, `/search/code`
//!   with GitHub qualifier syntax ([`query`]), `{total_count,
//!   incomplete_results, items[score]}` envelopes and the `text-match` media
//!   type. Issues/repos/users/commits use Postgres full-text search plus
//!   pg_trgm; code uses a trigram index over default-branch blobs
//!   ([`code`]), maintained by the `search.index_repo` job.
//! * `/_bgh/search`: compact command-palette results.
//! * Events API ([`activity`]) recorded from domain events, and the
//!   dashboard feed `/_bgh/feed`.
//!
//! Migrations: 0700-0799.

pub mod activity;
pub mod code;
pub mod commits;
pub mod common;
pub mod issues;
pub mod palette;
pub mod query;
pub mod render;
pub mod repos;
pub mod resolve;
pub mod sqlb;
pub mod users;

use axum::Router;
use axum::routing::get;
use bgh_core::{AppState, Registry};

/// REST API routes. Paths are relative to `/api/v3`.
pub fn router() -> Router<AppState> {
    use activity::api as ev;
    Router::new()
        .route("/search/issues", get(issues::search))
        .route("/search/repositories", get(repos::search))
        .route("/search/topics", get(repos::topics))
        .route("/search/users", get(users::search))
        .route("/search/labels", get(users::labels))
        .route("/search/commits", get(commits::search))
        .route("/search/code", get(code::search::search))
        .route("/events", get(ev::public_events))
        .route("/repos/{owner}/{repo}/events", get(ev::repo_events))
        .route("/networks/{owner}/{repo}/events", get(ev::network_events))
        .route("/orgs/{org}/events", get(ev::org_events))
        .route("/users/{username}/events", get(ev::user_events))
        .route(
            "/users/{username}/events/public",
            get(ev::user_public_events),
        )
        .route(
            "/users/{username}/events/orgs/{org}",
            get(ev::user_org_events),
        )
        .route(
            "/users/{username}/received_events",
            get(ev::received_events),
        )
        .route(
            "/users/{username}/received_events/public",
            get(ev::received_public_events),
        )
}

/// Web-client routes (absolute paths).
pub fn web_router() -> Router<AppState> {
    Router::new()
        .route("/_bgh/search", get(palette::search))
        .route("/_bgh/feed", get(activity::api::feed))
}

/// Code indexing jobs and the activity/indexing event listeners.
pub fn register(reg: &mut Registry) {
    reg.job(code::index::index_repo_job);
    reg.job(code::index::gc_job);
    reg.on_event("search.code_index", code::index::on_event);
    reg.on_event("search.activity", activity::record::on_event);
}
