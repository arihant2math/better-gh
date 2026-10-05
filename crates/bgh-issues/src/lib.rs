//! bgh-issues: issues, labels, milestones, assignees, comments, reactions,
//! locking, events and timeline, issue templates, sub-issues, pinned
//! issues, transfers, mentions and cross-references, issue types and
//! dependencies (blocked by / blocking).
//!
//! See `docs/packages/issues.md` for the endpoint list and design notes.
//! Migrations for this crate use the 0300-0399 range.

pub mod assignees;
pub mod comments;
pub mod dependencies;
pub mod deployed;
pub mod events;
pub mod import;
pub mod issue_types;
pub mod issues;
pub mod json;
pub mod labels;
pub mod links;
pub mod milestones;
pub mod moderation;
pub mod pins;
pub mod reactions;
pub mod refs;
pub mod service;
pub mod sub_issues;
pub mod templates;
pub mod transfer;

use axum::Router;
use axum::routing::{delete, get, patch, post, put};
use bgh_core::{AppState, Registry};

/// REST API routes (relative to `/api/v3`).
pub fn router() -> Router<AppState> {
    const R: &str = "/repos/{owner}/{repo}";
    const I: &str = "/repos/{owner}/{repo}/issues/{issue_number}";
    let r = |p: &str| format!("{R}{p}");
    let i = |p: &str| format!("{I}{p}");
    Router::new()
        // Cross-repository lists.
        .route("/issues", get(issues::list_for_authenticated_user))
        .route("/user/issues", get(issues::list_for_user_repos))
        .route("/orgs/{org}/issues", get(issues::list_for_org))
        // Issues.
        .route(
            &r("/issues"),
            get(issues::list_for_repo).post(issues::create),
        )
        .route(I, get(issues::get).patch(issues::update))
        .route(&i("/lock"), put(events::lock).delete(events::unlock))
        .route(&i("/transfer"), post(transfer::transfer))
        // Comments.
        .route(&r("/issues/comments"), get(comments::list_for_repo))
        .route(
            &r("/issues/comments/{comment_id}"),
            get(comments::get)
                .patch(comments::update)
                .delete(comments::delete),
        )
        .route(
            &i("/comments"),
            get(comments::list_for_issue).post(comments::create),
        )
        // Reactions.
        .route(
            &i("/reactions"),
            get(reactions::list_for_issue).post(reactions::create_for_issue),
        )
        .route(
            &i("/reactions/{reaction_id}"),
            delete(reactions::delete_for_issue),
        )
        .route(
            &r("/issues/comments/{comment_id}/reactions"),
            get(reactions::list_for_comment).post(reactions::create_for_comment),
        )
        .route(
            &r("/issues/comments/{comment_id}/reactions/{reaction_id}"),
            delete(reactions::delete_for_comment),
        )
        // Events and timeline.
        .route(&r("/issues/events"), get(events::list_for_repo))
        .route(&r("/issues/events/{event_id}"), get(events::get))
        .route(&i("/events"), get(events::list_for_issue))
        .route(&i("/timeline"), get(events::timeline))
        // Labels.
        .route(&r("/labels"), get(labels::list).post(labels::create))
        .route(
            &r("/labels/{name}"),
            get(labels::get)
                .patch(labels::update)
                .delete(labels::delete),
        )
        .route(
            &i("/labels"),
            get(labels::list_for_issue)
                .post(labels::add_to_issue)
                .put(labels::set_for_issue)
                .delete(labels::remove_all_from_issue),
        )
        .route(&i("/labels/{name}"), delete(labels::remove_from_issue))
        // Milestones.
        .route(
            &r("/milestones"),
            get(milestones::list).post(milestones::create),
        )
        .route(
            &r("/milestones/{milestone_number}"),
            get(milestones::get)
                .patch(milestones::update)
                .delete(milestones::delete),
        )
        .route(
            &r("/milestones/{milestone_number}/labels"),
            get(labels::list_for_milestone),
        )
        // Assignees.
        .route(&r("/assignees"), get(assignees::list))
        .route(&r("/assignees/{assignee}"), get(assignees::check_repo))
        .route(
            &i("/assignees"),
            post(assignees::add).delete(assignees::remove),
        )
        .route(&i("/assignees/{assignee}"), get(assignees::check_issue))
        // Sub-issues.
        .route(
            &i("/sub_issues"),
            get(sub_issues::list).post(sub_issues::add),
        )
        .route(&i("/sub_issue"), delete(sub_issues::remove))
        .route(&i("/sub_issues/priority"), patch(sub_issues::reprioritize))
        .route(&i("/parent"), get(sub_issues::parent))
        // Issue dependencies (blocked by / blocking).
        .route(
            &i("/dependencies/blocked_by"),
            get(dependencies::list_blocked_by).post(dependencies::add),
        )
        .route(
            &i("/dependencies/blocked_by/{issue_id}"),
            delete(dependencies::remove),
        )
        .route(
            &i("/dependencies/blocking"),
            get(dependencies::list_blocking),
        )
        // Organization issue types.
        .route(
            "/orgs/{org}/issue-types",
            get(issue_types::list).post(issue_types::create),
        )
        .route(
            "/orgs/{org}/issue-types/{issue_type_id}",
            put(issue_types::update).delete(issue_types::delete),
        )
}

/// Web-client routes (absolute paths).
pub fn web_router() -> Router<AppState> {
    Router::new()
        .route(
            "/_bgh/repos/{owner}/{repo}/issue-templates",
            get(templates::get),
        )
        .route("/_bgh/repos/{owner}/{repo}/pinned-issues", get(pins::list))
        .route(
            "/_bgh/repos/{owner}/{repo}/issues/{issue_number}/pin",
            put(pins::pin).delete(pins::unpin),
        )
        .route(
            "/_bgh/repos/{owner}/{repo}/issues/{issue_number}/links",
            get(links::list).post(links::create),
        )
        .route(
            "/_bgh/repos/{owner}/{repo}/issues/{issue_number}/links/{linked_id}",
            delete(links::delete),
        )
        .route(
            "/_bgh/repos/{owner}/{repo}/issues/{issue_number}/viewer-reactions",
            get(reactions::viewer_reactions),
        )
        .route(
            "/_bgh/repos/{owner}/{repo}/issues/{issue_number}/reactions/{content}",
            delete(reactions::delete_own_for_issue),
        )
        .route(
            "/_bgh/repos/{owner}/{repo}/issues/comments/{comment_id}/reactions/{content}",
            delete(reactions::delete_own_for_comment),
        )
        // Moderation (P42): hide comments, edit history, issue deletion.
        .route(
            "/_bgh/repos/{owner}/{repo}/minimized/{kind}",
            get(moderation::minimized_list),
        )
        .route(
            "/_bgh/repos/{owner}/{repo}/minimized/{kind}/{id}",
            put(moderation::minimize).delete(moderation::unminimize),
        )
        .route(
            "/_bgh/repos/{owner}/{repo}/edits/{kind}/{id}",
            get(moderation::edits),
        )
        .route(
            "/_bgh/repos/{owner}/{repo}/edits/{kind}/{id}/{edit_id}",
            delete(moderation::delete_edit),
        )
        .route(
            "/_bgh/repos/{owner}/{repo}/issues/{issue_number}",
            delete(moderation::delete_issue),
        )
}

/// Event listeners: commit references / closing keywords from pushes, and
/// issue ↔ pull request links (closing keywords in PR bodies, closing
/// linked issues on merge). (Default labels are created with the
/// repository, `bgh_core::labels`.)
pub fn register(reg: &mut Registry) {
    reg.on_event("issues.commit_references", refs::on_event);
    reg.on_event("issues.pr_links", links::on_event);
    reg.on_event("issues.deployed", deployed::on_event);
}
