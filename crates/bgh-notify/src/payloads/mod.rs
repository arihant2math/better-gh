//! GitHub webhook payloads ("Webhook events and payloads") built from
//! database rows.
//!
//! [`for_event`] maps a domain [`Event`] to the deliveries GitHub would make
//! (`X-GitHub-Event` name, `action`, payload); [`event_names`] is the cheap
//! no-IO pre-check used to skip building payloads nobody subscribes to.
//! The per-entity builders (`issue`, `pull_request`, `repository`, ...) are
//! public so other code (hook tests, redeliveries, API views) can reuse them.
//!
//! Every builder tolerates rows deleted between commit and dispatch: they
//! return `Ok(None)` / an empty vec instead of failing.

mod checks;
mod common;
mod issues;
mod packages;
mod pulls;
mod push;
mod releases;

use bgh_core::AppState;
use bgh_core::events::Event;
use bgh_core::models::{api, db};
use bgh_core::node_id::{self, NodeType};
use serde_json::{Map, Value, json};

pub use checks::{
    ACTIONS_APP_ID, app_json, check_run, check_suite, pull_requests_for_sha, status_payload,
};
pub use common::{
    RepoCtx, association, author_associations, commit_node_id, organization, reactions, repository,
    sender, user_json, user_or_ghost, user_or_null,
};
pub use issues::{
    comment_json, deleted_comment_json, issue, issue_comment, issue_json, issue_row, label,
    label_json, milestone, milestone_json,
};
pub use pulls::{
    deleted_review_comment_json, pull_json, pull_request, pull_row, review, review_comment,
    review_comment_html_url, review_comment_url, review_html_url, team, team_json,
    webhook_review_state,
};
pub use push::{
    LogEntry, MAX_PUSH_COMMITS, commit_json, compare_url, parse_log, push_repository,
    timestamp_with_offset, unquote_path,
};
pub use releases::release;

use common::envelope;

/// One webhook delivery to make for a domain event.
#[derive(Debug, Clone)]
pub struct HookEvent {
    /// `X-GitHub-Event` name: "push", "issues", "issue_comment", ...
    pub event: &'static str,
    /// Payload `action` (None for push/create/delete/status/fork/ping).
    pub action: Option<String>,
    /// Repository the delivery belongs to (for repo hooks + delivery.repository_id).
    pub repo_id: Option<i64>,
    /// Organization for org hooks: repo owner if it is an Organization, or the org of org-level events.
    pub org_id: Option<i64>,
    pub payload: serde_json::Value,
}

/// GitHub's zen phrases, sent in `ping` payloads.
const ZEN: &[&str] = &[
    "Responsive is better than fast.",
    "It's not fully shipped until it's fast.",
    "Anything added dilutes everything else.",
    "Practicality beats purity.",
    "Approachable is better than simple.",
    "Mind your words, they are important.",
    "Speak like a human.",
    "Half measures are as bad as nothing at all.",
    "Encourage flow.",
    "Non-blocking is better than blocking.",
    "Favor focus over features.",
    "Design for failure.",
    "Keep it logically awesome.",
];

/// Cheap, no-IO: the webhook event names `event` maps to (used to skip
/// payload building when no hook subscribes). E.g. StarCreated -> ["star", "watch"].
pub fn event_names(event: &Event) -> Vec<&'static str> {
    use Event as E;
    match event {
        E::Push(p) => {
            let relevant =
                |u: &&bgh_core::events::RefUpdate| u.branch().is_some() || u.tag().is_some();
            let mut v = vec!["push"];
            if p.updates.iter().filter(relevant).any(|u| u.is_create()) {
                v.push("create");
            }
            if p.updates.iter().filter(relevant).any(|u| u.is_delete()) {
                v.push("delete");
            }
            v
        }
        E::IssueOpened { .. }
        | E::IssueEdited { .. }
        | E::IssueClosed { .. }
        | E::IssueReopened { .. }
        | E::IssueDeleted { .. } => vec!["issues"],
        // These apply to pull requests too (delivered as `pull_request`).
        E::IssueAssigned { .. }
        | E::IssueUnassigned { .. }
        | E::IssueLabeled { .. }
        | E::IssueUnlabeled { .. }
        | E::IssueMilestoned { .. }
        | E::IssueDemilestoned { .. }
        | E::IssueLocked { .. }
        | E::IssueUnlocked { .. } => vec!["issues", "pull_request"],
        E::IssueCommentCreated { .. }
        | E::IssueCommentEdited { .. }
        | E::IssueCommentDeleted { .. } => vec!["issue_comment"],
        E::PullRequestOpened { .. }
        | E::PullRequestSynchronized { .. }
        | E::PullRequestClosed { .. }
        | E::PullRequestReopened { .. }
        | E::PullRequestMerged { .. }
        | E::PullRequestEdited { .. }
        | E::PullRequestReadyForReview { .. }
        | E::PullRequestConvertedToDraft { .. }
        | E::PullRequestReviewRequested { .. }
        | E::PullRequestReviewRequestRemoved { .. } => vec!["pull_request"],
        E::PullRequestReviewSubmitted { .. } | E::PullRequestReviewDismissed { .. } => {
            vec!["pull_request_review"]
        }
        E::PullRequestReviewCommentCreated { .. }
        | E::PullRequestReviewCommentEdited { .. }
        | E::PullRequestReviewCommentDeleted { .. } => vec!["pull_request_review_comment"],
        E::ReleaseCreated { .. }
        | E::ReleasePublished { .. }
        | E::ReleaseEdited { .. }
        | E::ReleaseDeleted { .. } => vec!["release"],
        E::PackagePublished { .. } | E::PackageUpdated { .. } => vec!["package"],
        E::StarCreated { .. } => vec!["star", "watch"],
        E::StarDeleted { .. } => vec!["star"],
        E::RepositoryForked { .. } => vec!["fork"],
        E::CollaboratorAdded { .. }
        | E::CollaboratorEdited { .. }
        | E::CollaboratorRemoved { .. } => {
            vec!["member"]
        }
        E::RepositoryCreated { .. }
        | E::RepositoryDeleted { .. }
        | E::RepositoryUpdated { .. }
        | E::RepositoryRenamed { .. }
        | E::RepositoryArchived { .. }
        | E::RepositoryUnarchived { .. }
        | E::RepositoryPublicized { .. }
        | E::RepositoryPrivatized { .. } => vec!["repository"],
        E::LabelCreated { .. } | E::LabelEdited { .. } | E::LabelDeleted { .. } => vec!["label"],
        E::MilestoneCreated { .. }
        | E::MilestoneEdited { .. }
        | E::MilestoneClosed { .. }
        | E::MilestoneOpened { .. }
        | E::MilestoneDeleted { .. } => vec!["milestone"],
        E::CommitStatusCreated { .. } => vec!["status"],
        E::CheckRunUpdated { .. } => vec!["check_run"],
        E::CheckSuiteUpdated { .. } => vec!["check_suite"],
        E::WorkflowRunUpdated { .. } => vec!["workflow_run"],
        E::WorkflowJobUpdated { .. } => vec!["workflow_job"],
        E::OrgMemberAdded { .. } => vec!["organization"],
        // Site-level events: delivered to global (site admin) hooks only.
        E::UserAccountChanged { .. } => vec!["user"],
        E::OrganizationChanged { .. } => vec!["organization"],
        _ => Vec::new(),
    }
}

/// A site-level delivery (global hooks only: no repository or org scope).
fn global(event: &'static str, action: &str, payload: Value) -> HookEvent {
    HookEvent {
        event,
        action: Some(action.to_string()),
        repo_id: None,
        org_id: None,
        payload,
    }
}

/// `changes` for a rename (`{"login": {"from": old}}`) from the event data.
fn rename_changes(action: &str, data: &Value) -> Option<Value> {
    (action == "renamed")
        .then(|| data.get("from").cloned())
        .flatten()
        .map(|from| json!({ "login": { "from": from } }))
}

/// GHES global webhook `user` event.
async fn user_account_changed(
    state: &AppState,
    user_id: i64,
    login: &str,
    action: &str,
    actor_id: i64,
    data: &Value,
) -> anyhow::Result<Vec<HookEvent>> {
    let user = match db::User::find(&state.db, user_id).await? {
        Some(u) => user_json(&state.urls, &u),
        // Deleted: render from the event.
        None => serde_json::to_value(api::SimpleUser::from_parts(
            &state.urls,
            user_id,
            login,
            "User",
            false,
            None,
        ))?,
    };
    let mut m = Map::new();
    m.insert("action".into(), json!(action));
    m.insert("user".into(), user);
    if let Some(changes) = rename_changes(action, data) {
        m.insert("changes".into(), changes);
    }
    m.insert("sender".into(), sender(state, Some(actor_id)).await?);
    Ok(vec![global("user", action, Value::Object(m))])
}

/// GHES global webhook `organization` event (created / deleted / renamed).
async fn organization_changed(
    state: &AppState,
    org_id: i64,
    login: &str,
    action: &str,
    actor_id: i64,
    data: &Value,
) -> anyhow::Result<Vec<HookEvent>> {
    let org = match organization(state, org_id).await? {
        Some(o) => o,
        None => {
            let u = api::SimpleUser::from_parts(
                &state.urls,
                org_id,
                login,
                "Organization",
                false,
                None,
            );
            json!({
                "login": u.login,
                "id": u.id,
                "node_id": u.node_id,
                "url": state.urls.org(login),
                "avatar_url": u.avatar_url,
                "description": null,
            })
        }
    };
    let mut m = Map::new();
    m.insert("action".into(), json!(action));
    if let Some(changes) = rename_changes(action, data) {
        m.insert("changes".into(), changes);
    }
    m.insert("organization".into(), org);
    m.insert("sender".into(), sender(state, Some(actor_id)).await?);
    Ok(vec![global("organization", action, Value::Object(m))])
}

/// A delivery scoped to `ctx`'s repository (and its org for org hooks).
fn hook(ctx: &RepoCtx, event: &'static str, action: Option<&str>, payload: Value) -> HookEvent {
    HookEvent {
        event,
        action: action.map(str::to_string),
        repo_id: Some(ctx.repo.id),
        org_id: ctx.org_id(),
        payload,
    }
}

/// Build every webhook delivery for a domain event. Returns an empty vec when
/// the referenced rows no longer exist (deleted between commit and dispatch) or
/// the event has no webhook equivalent. Must not panic on missing data.
pub async fn for_event(state: &AppState, event: &Event) -> anyhow::Result<Vec<HookEvent>> {
    use Event as E;
    if event_names(event).is_empty() {
        return Ok(Vec::new());
    }
    // Events without a live repository row.
    match event {
        E::OrgMemberAdded {
            org_id,
            user_id,
            actor_id,
        } => return org_member_added(state, *org_id, *user_id, *actor_id).await,
        E::RepositoryDeleted {
            repo_id,
            owner_id,
            full_name,
            actor_id,
        } => return repository_deleted(state, *repo_id, *owner_id, full_name, *actor_id).await,
        E::UserAccountChanged {
            user_id,
            login,
            action,
            actor_id,
            data,
        } => return user_account_changed(state, *user_id, login, action, *actor_id, data).await,
        E::OrganizationChanged {
            org_id,
            login,
            action,
            actor_id,
            data,
        } => return organization_changed(state, *org_id, login, action, *actor_id, data).await,
        _ => {}
    }
    let Some(repo_id) = event.repo_id() else {
        return Ok(Vec::new());
    };
    let Some(ctx) = RepoCtx::load(state, repo_id).await? else {
        return Ok(Vec::new());
    };
    if let E::Push(p) = event {
        return push::push_events(state, &ctx, p.pusher_id, &p.updates).await;
    }
    let b = Builder {
        state,
        ctx: &ctx,
        sender: sender(state, event.actor_id()).await?,
    };

    match event {
        // ----- issues ---------------------------------------------------
        E::IssueOpened { issue_id, .. } => b.issue(*issue_id, "opened", vec![], false).await,
        E::IssueEdited {
            issue_id, changes, ..
        } => {
            b.issue(
                *issue_id,
                "edited",
                vec![("changes", changes.clone())],
                false,
            )
            .await
        }
        E::IssueClosed { issue_id, .. } => b.issue(*issue_id, "closed", vec![], false).await,
        E::IssueReopened { issue_id, .. } => b.issue(*issue_id, "reopened", vec![], false).await,
        E::IssueAssigned {
            issue_id,
            assignee_id,
            ..
        } => {
            let assignee = sender(state, Some(*assignee_id)).await?;
            b.issue(*issue_id, "assigned", vec![("assignee", assignee)], true)
                .await
        }
        E::IssueUnassigned {
            issue_id,
            assignee_id,
            ..
        } => {
            let assignee = sender(state, Some(*assignee_id)).await?;
            b.issue(*issue_id, "unassigned", vec![("assignee", assignee)], true)
                .await
        }
        E::IssueLabeled {
            issue_id, label_id, ..
        } => {
            let extra = label(state, &ctx, *label_id)
                .await?
                .map(|l| vec![("label", l)])
                .unwrap_or_default();
            b.issue(*issue_id, "labeled", extra, true).await
        }
        E::IssueUnlabeled {
            issue_id, label_id, ..
        } => {
            let extra = label(state, &ctx, *label_id)
                .await?
                .map(|l| vec![("label", l)])
                .unwrap_or_default();
            b.issue(*issue_id, "unlabeled", extra, true).await
        }
        E::IssueMilestoned {
            issue_id,
            milestone_id,
            ..
        } => {
            let extra = milestone(state, &ctx, *milestone_id)
                .await?
                .map(|m| vec![("milestone", m)])
                .unwrap_or_default();
            b.issue(*issue_id, "milestoned", extra, true).await
        }
        E::IssueDemilestoned {
            issue_id,
            milestone_id,
            ..
        } => {
            let extra = milestone(state, &ctx, *milestone_id)
                .await?
                .map(|m| vec![("milestone", m)])
                .unwrap_or_default();
            b.issue(*issue_id, "demilestoned", extra, true).await
        }
        E::IssueLocked { issue_id, .. } => b.issue(*issue_id, "locked", vec![], true).await,
        E::IssueUnlocked { issue_id, .. } => b.issue(*issue_id, "unlocked", vec![], true).await,
        E::IssueDeleted { issue, .. } => Ok(vec![b.emit(
            "issues",
            Some("deleted"),
            vec![("issue", issue.clone())],
        )]),

        // ----- issue comments ---------------------------------------------
        E::IssueCommentCreated {
            issue_id,
            comment_id,
            ..
        } => b.issue_comment(*issue_id, *comment_id, "created").await,
        E::IssueCommentEdited {
            issue_id,
            comment_id,
            ..
        } => b.issue_comment(*issue_id, *comment_id, "edited").await,
        E::IssueCommentDeleted {
            issue_id,
            comment_id,
            ..
        } => b.issue_comment(*issue_id, *comment_id, "deleted").await,

        // ----- pull requests ------------------------------------------------
        E::PullRequestOpened { pull_id, .. } => b.pull(*pull_id, "opened", vec![], false).await,
        E::PullRequestSynchronized {
            pull_id,
            before,
            after,
            ..
        } => {
            b.pull(
                *pull_id,
                "synchronize",
                vec![("before", json!(before)), ("after", json!(after))],
                false,
            )
            .await
        }
        E::PullRequestClosed { pull_id, .. } => b.pull(*pull_id, "closed", vec![], false).await,
        E::PullRequestReopened { pull_id, .. } => b.pull(*pull_id, "reopened", vec![], false).await,
        E::PullRequestMerged { pull_id, .. } => b.pull(*pull_id, "closed", vec![], true).await,
        E::PullRequestEdited {
            pull_id, changes, ..
        } => {
            b.pull(
                *pull_id,
                "edited",
                vec![("changes", changes.clone())],
                false,
            )
            .await
        }
        E::PullRequestReadyForReview { pull_id, .. } => {
            b.pull(*pull_id, "ready_for_review", vec![], false).await
        }
        E::PullRequestConvertedToDraft { pull_id, .. } => {
            b.pull(*pull_id, "converted_to_draft", vec![], false).await
        }
        E::PullRequestReviewRequested {
            pull_id,
            reviewer_id,
            team_id,
            ..
        } => {
            let extra = b.requested(*reviewer_id, *team_id).await?;
            b.pull(*pull_id, "review_requested", extra, false).await
        }
        E::PullRequestReviewRequestRemoved {
            pull_id,
            reviewer_id,
            team_id,
            ..
        } => {
            let extra = b.requested(*reviewer_id, *team_id).await?;
            b.pull(*pull_id, "review_request_removed", extra, false)
                .await
        }

        // ----- reviews and review comments ------------------------------------
        E::PullRequestReviewSubmitted {
            pull_id, review_id, ..
        } => b.review(*pull_id, *review_id, "submitted").await,
        E::PullRequestReviewDismissed {
            pull_id, review_id, ..
        } => b.review(*pull_id, *review_id, "dismissed").await,
        E::PullRequestReviewCommentCreated {
            pull_id,
            comment_id,
            ..
        } => b.review_comment(*pull_id, *comment_id, "created").await,
        E::PullRequestReviewCommentEdited {
            pull_id,
            comment_id,
            ..
        } => b.review_comment(*pull_id, *comment_id, "edited").await,
        E::PullRequestReviewCommentDeleted {
            pull_id,
            comment_id,
            ..
        } => b.review_comment(*pull_id, *comment_id, "deleted").await,

        // ----- releases -------------------------------------------------------
        E::ReleaseCreated { release_id, .. } => b.release(*release_id, &["created"], vec![]).await,
        E::ReleasePublished { release_id, .. } => {
            b.release(*release_id, &["published", "released"], vec![])
                .await
        }
        E::ReleaseEdited {
            release_id,
            changes,
            ..
        } => {
            b.release(*release_id, &["edited"], vec![("changes", changes.clone())])
                .await
        }
        E::ReleaseDeleted { release, .. } => Ok(vec![b.emit(
            "release",
            Some("deleted"),
            vec![("release", release.clone())],
        )]),

        // ----- packages -------------------------------------------------------
        E::PackagePublished {
            package_id,
            version_id,
            tag,
            ..
        } => {
            b.package(*package_id, *version_id, tag.as_deref(), "published")
                .await
        }
        E::PackageUpdated {
            package_id,
            version_id,
            tag,
            ..
        } => {
            b.package(*package_id, *version_id, tag.as_deref(), "updated")
                .await
        }

        // ----- stars, forks, collaborators --------------------------------------
        E::StarCreated { actor_id, .. } => {
            let starred_at: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar(
                "SELECT created_at FROM stars WHERE user_id = $1 AND repo_id = $2",
            )
            .bind(actor_id)
            .bind(repo_id)
            .fetch_optional(&state.db)
            .await?;
            Ok(vec![
                b.emit(
                    "star",
                    Some("created"),
                    vec![(
                        "starred_at",
                        json!(starred_at.map(bgh_core::time::Timestamp)),
                    )],
                ),
                b.emit("watch", Some("started"), vec![]),
            ])
        }
        E::StarDeleted { .. } => Ok(vec![b.emit(
            "star",
            Some("deleted"),
            vec![("starred_at", Value::Null)],
        )]),
        E::RepositoryForked { fork_id, .. } => {
            let Some(fork) = RepoCtx::load(state, *fork_id).await? else {
                return Ok(Vec::new());
            };
            Ok(vec![b.emit(
                "fork",
                None,
                vec![("forkee", fork.repository(&state.urls))],
            )])
        }
        E::CollaboratorAdded {
            user_id,
            permission,
            ..
        } => {
            let member = sender(state, Some(*user_id)).await?;
            Ok(vec![b.emit(
                "member",
                Some("added"),
                vec![
                    ("member", member),
                    ("changes", json!({ "permission": { "to": permission } })),
                ],
            )])
        }
        E::CollaboratorEdited {
            user_id,
            old_permission,
            permission,
            ..
        } => {
            let member = sender(state, Some(*user_id)).await?;
            Ok(vec![b.emit(
                "member",
                Some("edited"),
                vec![
                    ("member", member),
                    (
                        "changes",
                        json!({ "permission": { "from": old_permission, "to": permission } }),
                    ),
                ],
            )])
        }
        E::CollaboratorRemoved { user_id, .. } => {
            let member = sender(state, Some(*user_id)).await?;
            Ok(vec![b.emit(
                "member",
                Some("removed"),
                vec![("member", member)],
            )])
        }

        // ----- repository lifecycle ----------------------------------------------
        E::RepositoryCreated { .. } => Ok(vec![b.emit("repository", Some("created"), vec![])]),
        E::RepositoryUpdated { .. } => Ok(vec![b.emit(
            "repository",
            Some("edited"),
            vec![("changes", json!({}))],
        )]),
        E::RepositoryRenamed { old_name, .. } => Ok(vec![b.emit(
            "repository",
            Some("renamed"),
            vec![(
                "changes",
                json!({ "repository": { "name": { "from": old_name } } }),
            )],
        )]),
        E::RepositoryArchived { .. } => Ok(vec![b.emit("repository", Some("archived"), vec![])]),
        E::RepositoryUnarchived { .. } => {
            Ok(vec![b.emit("repository", Some("unarchived"), vec![])])
        }
        E::RepositoryPublicized { .. } => {
            Ok(vec![b.emit("repository", Some("publicized"), vec![])])
        }
        E::RepositoryPrivatized { .. } => {
            Ok(vec![b.emit("repository", Some("privatized"), vec![])])
        }

        // ----- labels and milestones -----------------------------------------------
        E::LabelCreated { label_id, .. } => b.label(*label_id, "created", vec![]).await,
        E::LabelEdited {
            label_id, changes, ..
        } => {
            b.label(*label_id, "edited", vec![("changes", changes.clone())])
                .await
        }
        E::LabelDeleted { label, .. } => Ok(vec![b.emit(
            "label",
            Some("deleted"),
            vec![("label", label.clone())],
        )]),
        E::MilestoneCreated { milestone_id, .. } => {
            b.milestone(*milestone_id, "created", vec![]).await
        }
        E::MilestoneEdited {
            milestone_id,
            changes,
            ..
        } => {
            b.milestone(*milestone_id, "edited", vec![("changes", changes.clone())])
                .await
        }
        E::MilestoneClosed { milestone_id, .. } => {
            b.milestone(*milestone_id, "closed", vec![]).await
        }
        E::MilestoneOpened { milestone_id, .. } => {
            b.milestone(*milestone_id, "opened", vec![]).await
        }
        E::MilestoneDeleted { milestone, .. } => Ok(vec![b.emit(
            "milestone",
            Some("deleted"),
            vec![("milestone", milestone.clone())],
        )]),

        // ----- statuses, checks, workflows ------------------------------------------
        E::CommitStatusCreated { status_id, .. } => {
            Ok(status_payload(state, &ctx, *status_id, b.sender.clone())
                .await?
                .map(|p| vec![hook(&ctx, "status", None, p)])
                .unwrap_or_default())
        }
        E::CheckRunUpdated {
            check_run_id,
            action,
            ..
        } => Ok(check_run(state, &ctx, *check_run_id)
            .await?
            .map(|run| vec![b.emit("check_run", Some(action), vec![("check_run", run)])])
            .unwrap_or_default()),
        E::CheckSuiteUpdated {
            check_suite_id,
            action,
            ..
        } => Ok(check_suite(state, &ctx, *check_suite_id)
            .await?
            .map(|s| vec![b.emit("check_suite", Some(action), vec![("check_suite", s)])])
            .unwrap_or_default()),
        // bgh-actions doesn't render the run JSON yet: nothing to deliver.
        E::WorkflowRunUpdated { workflow_run, .. } if workflow_run.is_null() => Ok(Vec::new()),
        E::WorkflowRunUpdated {
            action,
            workflow_run,
            workflow,
            ..
        } => Ok(vec![b.emit(
            "workflow_run",
            Some(action),
            vec![
                ("workflow_run", workflow_run.clone()),
                ("workflow", workflow.clone().unwrap_or(Value::Null)),
            ],
        )]),
        E::WorkflowJobUpdated { workflow_job, .. } if workflow_job.is_null() => Ok(Vec::new()),
        E::WorkflowJobUpdated {
            action,
            workflow_job,
            ..
        } => Ok(vec![b.emit(
            "workflow_job",
            Some(action),
            vec![("workflow_job", workflow_job.clone())],
        )]),
        _ => Ok(Vec::new()),
    }
}

/// Per-event builder state: repository context + rendered sender.
struct Builder<'a> {
    state: &'a AppState,
    ctx: &'a RepoCtx,
    sender: Value,
}

impl Builder<'_> {
    /// `package` delivery (`published` | `updated`).
    async fn package(
        &self,
        package_id: i64,
        version_id: i64,
        tag: Option<&str>,
        action: &str,
    ) -> anyhow::Result<Vec<HookEvent>> {
        let Some(pkg) =
            packages::package(self.state, self.ctx, package_id, version_id, tag).await?
        else {
            return Ok(Vec::new());
        };
        Ok(vec![self.emit(
            "package",
            Some(action),
            vec![("package", pkg)],
        )])
    }

    fn emit(
        &self,
        event: &'static str,
        action: Option<&str>,
        entries: Vec<(&str, Value)>,
    ) -> HookEvent {
        hook(
            self.ctx,
            event,
            action,
            envelope(
                &self.state.urls,
                self.ctx,
                action,
                entries,
                self.sender.clone(),
            ),
        )
    }

    /// `issues` delivery, or `pull_request` for PRs when `pr_too` (else
    /// nothing: pulls emit their own opened/edited/closed/reopened events).
    async fn issue(
        &self,
        issue_id: i64,
        action: &str,
        extra: Vec<(&'static str, Value)>,
        pr_too: bool,
    ) -> anyhow::Result<Vec<HookEvent>> {
        let Some(row) = issue_row(self.state, self.ctx, issue_id).await? else {
            return Ok(Vec::new());
        };
        if row.is_pull_request {
            if !pr_too {
                return Ok(Vec::new());
            }
            let Some(pr) = pull_row(self.state, self.ctx, issue_id).await? else {
                return Ok(Vec::new());
            };
            let pj = pull_json(self.state, self.ctx, &row, &pr).await?;
            let mut entries = vec![("number", json!(row.number)), ("pull_request", pj)];
            entries.extend(extra);
            return Ok(vec![self.emit("pull_request", Some(action), entries)]);
        }
        let ij = issue_json(self.state, self.ctx, &row).await?;
        let mut entries = vec![("issue", ij)];
        entries.extend(extra);
        Ok(vec![self.emit("issues", Some(action), entries)])
    }

    async fn issue_comment(
        &self,
        issue_id: i64,
        comment_id: i64,
        action: &str,
    ) -> anyhow::Result<Vec<HookEvent>> {
        let Some(row) = issue_row(self.state, self.ctx, issue_id).await? else {
            return Ok(Vec::new());
        };
        let comment = if action == "deleted" {
            deleted_comment_json(&self.state.urls, self.ctx, &row, comment_id)
        } else {
            match issues::comment_row(self.state, self.ctx, comment_id).await? {
                Some(c) => comment_json(self.state, self.ctx, &row, &c).await?,
                None => return Ok(Vec::new()),
            }
        };
        let ij = issue_json(self.state, self.ctx, &row).await?;
        let mut entries = vec![("issue", ij), ("comment", comment)];
        if action == "edited" {
            entries.push(("changes", json!({})));
        }
        Ok(vec![self.emit("issue_comment", Some(action), entries)])
    }

    async fn pull_value(&self, pull_id: i64) -> anyhow::Result<Option<(db::Issue, Value)>> {
        let Some(row) = issue_row(self.state, self.ctx, pull_id).await? else {
            return Ok(None);
        };
        let Some(pr) = pull_row(self.state, self.ctx, pull_id).await? else {
            return Ok(None);
        };
        let pj = pull_json(self.state, self.ctx, &row, &pr).await?;
        Ok(Some((row, pj)))
    }

    async fn pull(
        &self,
        pull_id: i64,
        action: &str,
        extra: Vec<(&'static str, Value)>,
        merged: bool,
    ) -> anyhow::Result<Vec<HookEvent>> {
        let Some((row, mut pj)) = self.pull_value(pull_id).await? else {
            return Ok(Vec::new());
        };
        if merged {
            pj["merged"] = json!(true);
        }
        let mut entries = vec![("number", json!(row.number)), ("pull_request", pj)];
        entries.extend(extra);
        Ok(vec![self.emit("pull_request", Some(action), entries)])
    }

    /// `requested_reviewer` or `requested_team` entry.
    async fn requested(
        &self,
        reviewer_id: Option<i64>,
        team_id: Option<i64>,
    ) -> anyhow::Result<Vec<(&'static str, Value)>> {
        if let Some(id) = reviewer_id {
            return Ok(vec![(
                "requested_reviewer",
                sender(self.state, Some(id)).await?,
            )]);
        }
        if let Some(id) = team_id
            && let Some(t) = team(self.state, self.ctx, id).await?
        {
            return Ok(vec![("requested_team", t)]);
        }
        Ok(Vec::new())
    }

    async fn review(
        &self,
        pull_id: i64,
        review_id: i64,
        action: &str,
    ) -> anyhow::Result<Vec<HookEvent>> {
        let Some(mut rv) = review(self.state, self.ctx, review_id).await? else {
            return Ok(Vec::new());
        };
        if action == "dismissed" {
            rv["state"] = json!("dismissed");
        }
        let Some((_, pj)) = self.pull_value(pull_id).await? else {
            return Ok(Vec::new());
        };
        Ok(vec![self.emit(
            "pull_request_review",
            Some(action),
            vec![("review", rv), ("pull_request", pj)],
        )])
    }

    async fn review_comment(
        &self,
        pull_id: i64,
        comment_id: i64,
        action: &str,
    ) -> anyhow::Result<Vec<HookEvent>> {
        let Some((row, pj)) = self.pull_value(pull_id).await? else {
            return Ok(Vec::new());
        };
        let comment = if action == "deleted" {
            deleted_review_comment_json(&self.state.urls, self.ctx, row.number, comment_id)
        } else {
            match review_comment(self.state, self.ctx, comment_id).await? {
                Some(c) => c,
                None => return Ok(Vec::new()),
            }
        };
        let mut entries = vec![("comment", comment), ("pull_request", pj)];
        if action == "edited" {
            entries.push(("changes", json!({})));
        }
        Ok(vec![self.emit(
            "pull_request_review_comment",
            Some(action),
            entries,
        )])
    }

    /// `release` deliveries; for `["published", "released"]` the second
    /// action becomes `prereleased` for pre-releases.
    async fn release(
        &self,
        release_id: i64,
        actions: &[&str],
        extra: Vec<(&'static str, Value)>,
    ) -> anyhow::Result<Vec<HookEvent>> {
        let Some(rel) = release(self.state, self.ctx, release_id).await? else {
            return Ok(Vec::new());
        };
        let prerelease = rel["prerelease"].as_bool().unwrap_or(false);
        Ok(actions
            .iter()
            .map(|a| {
                let action = if *a == "released" && prerelease {
                    "prereleased"
                } else {
                    a
                };
                let mut entries = vec![("release", rel.clone())];
                entries.extend(extra.iter().cloned());
                self.emit("release", Some(action), entries)
            })
            .collect())
    }

    async fn label(
        &self,
        label_id: i64,
        action: &str,
        extra: Vec<(&'static str, Value)>,
    ) -> anyhow::Result<Vec<HookEvent>> {
        let Some(l) = label(self.state, self.ctx, label_id).await? else {
            return Ok(Vec::new());
        };
        let mut entries = vec![("label", l)];
        entries.extend(extra);
        Ok(vec![self.emit("label", Some(action), entries)])
    }

    async fn milestone(
        &self,
        milestone_id: i64,
        action: &str,
        extra: Vec<(&'static str, Value)>,
    ) -> anyhow::Result<Vec<HookEvent>> {
        let Some(m) = milestone(self.state, self.ctx, milestone_id).await? else {
            return Ok(Vec::new());
        };
        let mut entries = vec![("milestone", m)];
        entries.extend(extra);
        Ok(vec![self.emit("milestone", Some(action), entries)])
    }
}

/// `repository` `deleted`: the row is gone, so build a minimal object from
/// the event. Delivered to org/global hooks only (`repo_id: None`).
async fn repository_deleted(
    state: &AppState,
    repo_id: i64,
    owner_id: i64,
    full_name: &str,
    actor_id: i64,
) -> anyhow::Result<Vec<HookEvent>> {
    let Some(owner) = db::User::find(&state.db, owner_id).await? else {
        return Ok(Vec::new());
    };
    let urls = &state.urls;
    let name = full_name
        .split_once('/')
        .map(|(_, n)| n)
        .unwrap_or(full_name)
        .to_string();
    let mut m = Map::new();
    m.insert("action".into(), json!("deleted"));
    m.insert(
        "repository".into(),
        json!({
            "id": repo_id,
            "node_id": node_id::encode(NodeType::Repository, repo_id),
            "name": name,
            "full_name": format!("{}/{name}", owner.login),
            "owner": user_json(urls, &owner),
            "private": false,
            "html_url": urls.repo_html(&owner.login, &name),
            "url": urls.repo(&owner.login, &name),
        }),
    );
    let org_id = owner.is_org().then_some(owner.id);
    if let Some(org_id) = org_id
        && let Some(org) = organization(state, org_id).await?
    {
        m.insert("organization".into(), org);
    }
    m.insert("sender".into(), sender(state, Some(actor_id)).await?);
    Ok(vec![HookEvent {
        event: "repository",
        action: Some("deleted".into()),
        repo_id: None,
        org_id,
        payload: Value::Object(m),
    }])
}

/// `organization` `member_added`.
async fn org_member_added(
    state: &AppState,
    org_id: i64,
    user_id: i64,
    actor_id: i64,
) -> anyhow::Result<Vec<HookEvent>> {
    let Some(org_json) = organization(state, org_id).await? else {
        return Ok(Vec::new());
    };
    let role: Option<String> =
        sqlx::query_scalar("SELECT role FROM org_members WHERE org_id = $1 AND user_id = $2")
            .bind(org_id)
            .bind(user_id)
            .fetch_optional(&state.db)
            .await?;
    let Some(role) = role else {
        return Ok(Vec::new());
    };
    let Some(member) = db::User::find(&state.db, user_id).await? else {
        return Ok(Vec::new());
    };
    let urls = &state.urls;
    let org_login = org_json["login"].as_str().unwrap_or_default().to_string();
    let org_url = urls.org(&org_login);
    let payload = json!({
        "action": "member_added",
        "membership": {
            "url": format!("{org_url}/memberships/{}", member.login),
            "state": "active",
            "role": role,
            "organization_url": org_url,
            "user": user_json(urls, &member),
        },
        "organization": org_json,
        "sender": sender(state, Some(actor_id)).await?,
    });
    Ok(vec![HookEvent {
        event: "organization",
        action: Some("member_added".into()),
        repo_id: None,
        org_id: Some(org_id),
        payload,
    }])
}

/// `ping` payload: {"zen", "hook_id", "hook": <hook json passed in>, "repository"? , "organization"?, "sender"}.
pub async fn ping(
    state: &AppState,
    hook_id: i64,
    hook_json: serde_json::Value,
    repo_id: Option<i64>,
    org_id: Option<i64>,
    sender_id: Option<i64>,
) -> anyhow::Result<serde_json::Value> {
    let mut m = Map::new();
    let zen = ZEN[(hook_id.unsigned_abs() % ZEN.len() as u64) as usize];
    m.insert("zen".into(), json!(zen));
    m.insert("hook_id".into(), json!(hook_id));
    m.insert("hook".into(), hook_json);
    let mut org = None;
    if let Some(repo_id) = repo_id
        && let Some(ctx) = RepoCtx::load(state, repo_id).await?
    {
        m.insert("repository".into(), ctx.repository(&state.urls));
        org = ctx.organization(&state.urls);
    }
    if org.is_none()
        && let Some(org_id) = org_id
    {
        org = organization(state, org_id).await?;
    }
    if let Some(org) = org {
        m.insert("organization".into(), org);
    }
    m.insert("sender".into(), sender(state, sender_id).await?);
    Ok(Value::Object(m))
}

/// Synthetic `push` payload for `POST /repos/{o}/{r}/hooks/{id}/tests`: latest commit on the
/// default branch (before = its parent or zero sha). None if the repo has no commits.
pub async fn test_push(
    state: &AppState,
    repo_id: i64,
    sender_id: i64,
) -> anyhow::Result<Option<serde_json::Value>> {
    let Some(ctx) = RepoCtx::load(state, repo_id).await? else {
        return Ok(None);
    };
    push::test_push(state, &ctx, sender_id).await
}

/// Ghost simple-user JSON.
pub fn ghost(state: &AppState) -> Value {
    serde_json::to_value(api::SimpleUser::ghost(&state.urls)).unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bgh_core::events::{PushEvent, RefUpdate, ZERO_SHA};

    #[test]
    fn maps_event_names() {
        let star = Event::StarCreated {
            repo_id: 1,
            actor_id: 2,
        };
        assert_eq!(event_names(&star), vec!["star", "watch"]);
        let assigned = Event::IssueAssigned {
            repo_id: 1,
            issue_id: 2,
            assignee_id: 3,
            actor_id: 4,
        };
        assert_eq!(event_names(&assigned), vec!["issues", "pull_request"]);
        let opened = Event::PullRequestOpened {
            repo_id: 1,
            pull_id: 2,
            actor_id: 3,
        };
        assert_eq!(event_names(&opened), vec!["pull_request"]);
        let member = Event::OrgMemberAdded {
            org_id: 1,
            user_id: 2,
            actor_id: 3,
        };
        assert_eq!(event_names(&member), vec!["organization"]);
        let status = Event::CommitStatusCreated {
            repo_id: 1,
            status_id: 2,
            sha: "a".repeat(40),
            actor_id: None,
        };
        assert_eq!(event_names(&status), vec!["status"]);

        let sha = "a".repeat(40);
        let push = |updates: Vec<RefUpdate>| {
            Event::Push(PushEvent {
                repo_id: 1,
                pusher_id: None,
                updates,
                origin: None,
            })
        };
        let create = RefUpdate {
            old: ZERO_SHA.into(),
            new: sha.clone(),
            refname: "refs/heads/main".into(),
        };
        let delete = RefUpdate {
            old: sha.clone(),
            new: ZERO_SHA.into(),
            refname: "refs/tags/v1".into(),
        };
        let update = RefUpdate {
            old: sha.clone(),
            new: "b".repeat(40),
            refname: "refs/heads/main".into(),
        };
        assert_eq!(event_names(&push(vec![update.clone()])), vec!["push"]);
        assert_eq!(
            event_names(&push(vec![create, delete, update])),
            vec!["push", "create", "delete"]
        );
    }
}
