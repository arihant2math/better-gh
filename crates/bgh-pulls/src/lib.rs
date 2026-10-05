//! bgh-pulls: pull requests, reviews, review comments, merging, commit
//! statuses, check runs/suites, CODEOWNERS and auto-merge.
//!
//! Pull requests share the `issues` table (and numbering) with issues; the
//! PR-only data lives in `pull_requests`. The head of every PR is mirrored
//! into the base repository as `refs/pull/{n}/head` (fetched from forks),
//! and the test merge as `refs/pull/{n}/merge`. Pushes reach this crate
//! through `Event::Push` (→ `pulls.push` job → [`synchronize`]).
//! Migrations: 0400-0499. See `docs/packages/pulls.md`.

pub mod automerge;
pub mod checks;
pub mod codeowners;
pub mod comments;
pub mod commits;
pub mod git;
pub mod jobs;
pub mod json;
pub mod merge;
pub mod mergeability;
pub mod model;
pub mod protection;
pub mod pulls;
pub mod ranges;
pub mod reviewers;
pub mod reviews;
pub mod statuses;
pub mod suggestions;
pub mod synchronize;
pub mod timeline;
pub mod viewed;
pub mod web;

use axum::Router;
use axum::routing::{get, patch, post, put};
use bgh_core::{AppState, Registry};

/// REST API routes (relative to `/api/v3`).
pub fn router() -> Router<AppState> {
    const R: &str = "/repos/{owner}/{repo}";
    let r = |p: &str| format!("{R}{p}");
    Router::new()
        // pulls
        .route(&r("/pulls"), get(pulls::list).post(pulls::create))
        .route(&r("/pulls/{number}"), get(pulls::get).patch(pulls::update))
        .route(&r("/pulls/{number}/commits"), get(pulls::commits))
        .route(&r("/pulls/{number}/files"), get(pulls::files))
        .route(
            &r("/pulls/{number}/merge"),
            get(pulls::check_merged).put(merge::merge),
        )
        .route(
            &r("/pulls/{number}/update-branch"),
            put(merge::update_branch),
        )
        // review comments
        .route(&r("/pulls/comments"), get(comments::list_for_repo))
        .route(
            &r("/pulls/comments/{id}"),
            get(comments::get)
                .patch(comments::edit)
                .delete(comments::delete),
        )
        .route(
            &r("/pulls/comments/{id}/reactions"),
            get(comments::list_reactions).post(comments::create_reaction),
        )
        .route(
            &r("/pulls/comments/{id}/reactions/{reaction_id}"),
            axum::routing::delete(comments::delete_reaction),
        )
        .route(
            &r("/pulls/{number}/comments"),
            get(comments::list_for_pull).post(comments::create),
        )
        .route(
            &r("/pulls/{number}/comments/{id}/replies"),
            post(comments::reply),
        )
        // reviews
        .route(
            &r("/pulls/{number}/reviews"),
            get(reviews::list).post(reviews::create),
        )
        .route(
            &r("/pulls/{number}/reviews/{id}"),
            get(reviews::get)
                .put(reviews::update)
                .delete(reviews::delete_pending),
        )
        .route(
            &r("/pulls/{number}/reviews/{id}/comments"),
            get(reviews::comments),
        )
        .route(
            &r("/pulls/{number}/reviews/{id}/events"),
            post(reviews::submit),
        )
        .route(
            &r("/pulls/{number}/reviews/{id}/dismissals"),
            put(reviews::dismiss),
        )
        // requested reviewers
        .route(
            &r("/pulls/{number}/requested_reviewers"),
            get(reviewers::list)
                .post(reviewers::request)
                .delete(reviewers::remove),
        )
        // statuses
        .route(
            &r("/statuses/{sha}"),
            post(statuses::create).get(statuses::list),
        )
        .route(&r("/commits/{ref}/statuses"), get(statuses::list))
        .route(&r("/commits/{ref}/status"), get(statuses::combined))
        .route(&r("/commits/{ref}/pulls"), get(pulls::for_commit))
        // checks
        .route(&r("/check-runs"), post(checks::create))
        .route(
            &r("/check-runs/{id}"),
            get(checks::get_run).patch(checks::update),
        )
        .route(&r("/check-runs/{id}/annotations"), get(checks::annotations))
        .route(
            &r("/check-runs/{id}/rerequest"),
            post(checks::rerequest_run),
        )
        .route(&r("/check-suites"), post(checks::create_suite))
        .route(&r("/check-suites/preferences"), patch(checks::preferences))
        .route(&r("/check-suites/{id}"), get(checks::get_suite))
        .route(
            &r("/check-suites/{id}/check-runs"),
            get(checks::list_for_suite),
        )
        .route(
            &r("/check-suites/{id}/rerequest"),
            post(checks::rerequest_suite),
        )
        .route(&r("/commits/{ref}/check-runs"), get(checks::list_for_ref))
        .route(
            &r("/commits/{ref}/check-suites"),
            get(checks::suites_for_ref),
        )
}

/// `/_bgh` routes for the web client (GraphQL-only features on GitHub).
pub fn web_router() -> Router<AppState> {
    const P: &str = "/_bgh/repos/{owner}/{repo}/pulls/{number}";
    let p = |s: &str| format!("{P}{s}");
    Router::new()
        .route(&p("/threads"), get(web::list_threads))
        .route(&p("/threads/{id}/resolve"), post(web::resolve_thread))
        .route(&p("/threads/{id}/unresolve"), post(web::unresolve_thread))
        .route(&p("/ready_for_review"), post(web::ready_for_review))
        .route(&p("/convert_to_draft"), post(web::convert_to_draft))
        .route(
            &p("/auto_merge"),
            put(automerge::put).delete(automerge::delete),
        )
        .route(&p("/requirements"), get(web::requirements))
        .route(&p("/sync"), get(web::pull_sync))
        .route(
            &p("/reviews/pending/comments"),
            post(web::create_pending_comment),
        )
        .route(&p("/patch"), get(web::file_patch))
        // review workflow (P38)
        .route(&p("/files"), get(ranges::range_files))
        .route(
            &p("/viewed"),
            get(viewed::list).put(viewed::put).delete(viewed::delete),
        )
        .route(&p("/suggestions/apply"), post(suggestions::apply_handler))
        .route(
            "/_bgh/repos/{owner}/{repo}/check-runs/{id}/requested-action",
            post(checks::request_action),
        )
        .route(
            "/_bgh/repos/{owner}/{repo}/pulls/comments/{id}/reactions/{content}",
            axum::routing::delete(comments::delete_own_reaction),
        )
}

/// Jobs and event listeners.
pub fn register(reg: &mut Registry) {
    reg.job(jobs::refresh);
    reg.job(jobs::sync_pull);
    reg.job(jobs::push);
    reg.job(jobs::checks_changed);
    reg.on_event("pulls.push", jobs::on_event);
    reg.on_event("pulls.checks", jobs::on_checks_event);
}
