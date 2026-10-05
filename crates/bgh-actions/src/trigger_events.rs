//! Workflow triggers beyond push / pull request / issues basics: the
//! remaining `pull_request` activity types, reviews and review comments,
//! `issues` / `issue_comment` activity, `release` types, and the events
//! evaluated against the default branch's workflows (`label`, `milestone`,
//! `watch`, `fork`, `public`, `gollum`, `check_run`, `check_suite`,
//! `workflow_run`, `repository_dispatch`).
//!
//! [`map_event`] turns a domain event into a [`TriggerKind`] (called by
//! [`crate::trigger::on_event`]); [`on_repo_event`] evaluates the
//! default-branch ones. Ids travel in `extra` ("hints") and are expanded
//! into payload objects by [`apply_hints`] when the trigger job runs.

use bgh_core::AppState;
use bgh_core::events::Event;
use bgh_core::models::db;
use serde_json::{Value, json};

use crate::context;
use crate::models::RunRow;
use crate::trigger::{self, TriggerKind, default_head, pr, run_default_branch_matching, user};

/// Issue activity that is `pull_request` activity when the issue is a PR.
pub const PR_ISSUE_ACTIONS: &[&str] = &[
    "labeled",
    "unlabeled",
    "assigned",
    "unassigned",
    "milestoned",
    "demilestoned",
    "locked",
    "unlocked",
];

/// GitHub stops `workflow_run` chains after this many levels.
pub const MAX_WORKFLOW_RUN_DEPTH: usize = 3;

fn pr_with(
    pull_id: i64,
    event: Option<&str>,
    action: &str,
    actor_id: Option<i64>,
    extra: Value,
) -> TriggerKind {
    match pr(pull_id, action, actor_id, None) {
        TriggerKind::PullRequest {
            pull_id,
            action,
            actor_id,
            before,
            ..
        } => TriggerKind::PullRequest {
            pull_id,
            action,
            actor_id,
            before,
            event: event.map(String::from),
            extra,
        },
        other => other,
    }
}

fn issue_with(issue_id: i64, action: &str, actor_id: i64, extra: Value) -> TriggerKind {
    TriggerKind::Issue {
        issue_id,
        action: action.into(),
        actor_id,
        extra,
    }
}

fn repo_event(
    event: &str,
    action: Option<&str>,
    actor_id: Option<i64>,
    extra: Value,
) -> TriggerKind {
    TriggerKind::Repo {
        event: event.into(),
        action: action.map(String::from),
        actor_id,
        extra,
    }
}

/// Map a domain event not handled by [`trigger::on_event`] itself.
pub fn map_event(event: &Event) -> Option<(i64, TriggerKind)> {
    use Event as E;
    Some(match event {
        // ----- pull_request activity -----
        E::PullRequestEdited {
            repo_id,
            pull_id,
            actor_id,
            changes,
        } => (
            *repo_id,
            pr_with(
                *pull_id,
                None,
                "edited",
                Some(*actor_id),
                json!({"changes": changes}),
            ),
        ),
        E::PullRequestReadyForReview {
            repo_id,
            pull_id,
            actor_id,
        } => (
            *repo_id,
            pr_with(
                *pull_id,
                None,
                "ready_for_review",
                Some(*actor_id),
                Value::Null,
            ),
        ),
        E::PullRequestConvertedToDraft {
            repo_id,
            pull_id,
            actor_id,
        } => (
            *repo_id,
            pr_with(
                *pull_id,
                None,
                "converted_to_draft",
                Some(*actor_id),
                Value::Null,
            ),
        ),
        E::PullRequestReviewRequested {
            repo_id,
            pull_id,
            actor_id,
            reviewer_id,
            team_id,
        } => (
            *repo_id,
            pr_with(
                *pull_id,
                None,
                "review_requested",
                Some(*actor_id),
                json!({"reviewer_id": reviewer_id, "team_id": team_id}),
            ),
        ),
        E::PullRequestReviewRequestRemoved {
            repo_id,
            pull_id,
            actor_id,
            reviewer_id,
            team_id,
        } => (
            *repo_id,
            pr_with(
                *pull_id,
                None,
                "review_request_removed",
                Some(*actor_id),
                json!({"reviewer_id": reviewer_id, "team_id": team_id}),
            ),
        ),
        E::PullRequestAutoMergeEnabled {
            repo_id,
            pull_id,
            actor_id,
        } => (
            *repo_id,
            pr_with(
                *pull_id,
                None,
                "auto_merge_enabled",
                Some(*actor_id),
                Value::Null,
            ),
        ),
        E::PullRequestAutoMergeDisabled {
            repo_id,
            pull_id,
            actor_id,
        } => (
            *repo_id,
            pr_with(
                *pull_id,
                None,
                "auto_merge_disabled",
                *actor_id,
                Value::Null,
            ),
        ),
        // ----- pull_request_review -----
        E::PullRequestReviewSubmitted {
            repo_id,
            pull_id,
            review_id,
            actor_id,
        } => (
            *repo_id,
            pr_with(
                *pull_id,
                Some("pull_request_review"),
                "submitted",
                Some(*actor_id),
                json!({"review_id": review_id}),
            ),
        ),
        E::PullRequestReviewEdited {
            repo_id,
            pull_id,
            review_id,
            actor_id,
            changes,
        } => (
            *repo_id,
            pr_with(
                *pull_id,
                Some("pull_request_review"),
                "edited",
                Some(*actor_id),
                json!({"review_id": review_id, "changes": changes}),
            ),
        ),
        E::PullRequestReviewDismissed {
            repo_id,
            pull_id,
            review_id,
            actor_id,
        } => (
            *repo_id,
            pr_with(
                *pull_id,
                Some("pull_request_review"),
                "dismissed",
                *actor_id,
                json!({"review_id": review_id}),
            ),
        ),
        // ----- pull_request_review_comment -----
        E::PullRequestReviewCommentCreated {
            repo_id,
            pull_id,
            comment_id,
            actor_id,
        } => (
            *repo_id,
            pr_with(
                *pull_id,
                Some("pull_request_review_comment"),
                "created",
                Some(*actor_id),
                json!({"review_comment_id": comment_id}),
            ),
        ),
        E::PullRequestReviewCommentEdited {
            repo_id,
            pull_id,
            comment_id,
            actor_id,
            changes,
        } => (
            *repo_id,
            pr_with(
                *pull_id,
                Some("pull_request_review_comment"),
                "edited",
                Some(*actor_id),
                json!({"review_comment_id": comment_id, "changes": changes}),
            ),
        ),
        E::PullRequestReviewCommentDeleted {
            repo_id,
            pull_id,
            comment_id,
            actor_id,
            comment,
        } => (
            *repo_id,
            pr_with(
                *pull_id,
                Some("pull_request_review_comment"),
                "deleted",
                Some(*actor_id),
                json!({"review_comment_id": comment_id, "review_comment": comment}),
            ),
        ),
        // ----- issues (PR issues are routed to pull_request) -----
        E::IssueLabeled {
            repo_id,
            issue_id,
            label_id,
            actor_id,
        } => (
            *repo_id,
            issue_with(
                *issue_id,
                "labeled",
                *actor_id,
                json!({"label_id": label_id}),
            ),
        ),
        E::IssueUnlabeled {
            repo_id,
            issue_id,
            label_id,
            actor_id,
        } => (
            *repo_id,
            issue_with(
                *issue_id,
                "unlabeled",
                *actor_id,
                json!({"label_id": label_id}),
            ),
        ),
        E::IssueAssigned {
            repo_id,
            issue_id,
            assignee_id,
            actor_id,
        } => (
            *repo_id,
            issue_with(
                *issue_id,
                "assigned",
                *actor_id,
                json!({"assignee_id": assignee_id}),
            ),
        ),
        E::IssueUnassigned {
            repo_id,
            issue_id,
            assignee_id,
            actor_id,
        } => (
            *repo_id,
            issue_with(
                *issue_id,
                "unassigned",
                *actor_id,
                json!({"assignee_id": assignee_id}),
            ),
        ),
        E::IssueMilestoned {
            repo_id,
            issue_id,
            milestone_id,
            actor_id,
        } => (
            *repo_id,
            issue_with(
                *issue_id,
                "milestoned",
                *actor_id,
                json!({"milestone_id": milestone_id}),
            ),
        ),
        E::IssueDemilestoned {
            repo_id,
            issue_id,
            milestone_id,
            actor_id,
        } => (
            *repo_id,
            issue_with(
                *issue_id,
                "demilestoned",
                *actor_id,
                json!({"milestone_id": milestone_id}),
            ),
        ),
        E::IssueLocked {
            repo_id,
            issue_id,
            actor_id,
        } => (
            *repo_id,
            issue_with(*issue_id, "locked", *actor_id, Value::Null),
        ),
        E::IssueUnlocked {
            repo_id,
            issue_id,
            actor_id,
        } => (
            *repo_id,
            issue_with(*issue_id, "unlocked", *actor_id, Value::Null),
        ),
        E::IssuePinned {
            repo_id,
            issue_id,
            actor_id,
        } => (
            *repo_id,
            issue_with(*issue_id, "pinned", *actor_id, Value::Null),
        ),
        E::IssueUnpinned {
            repo_id,
            issue_id,
            actor_id,
        } => (
            *repo_id,
            issue_with(*issue_id, "unpinned", *actor_id, Value::Null),
        ),
        // GitHub fires `transferred` in the repository the issue left.
        E::IssueTransferred {
            issue_id,
            old_repo_id,
            actor_id,
            ..
        } => (
            *old_repo_id,
            issue_with(*issue_id, "transferred", *actor_id, Value::Null),
        ),
        E::IssueDeleted {
            repo_id,
            issue_id,
            actor_id,
            issue,
        } => (
            *repo_id,
            issue_with(*issue_id, "deleted", *actor_id, json!({"issue": issue})),
        ),
        // ----- issue_comment -----
        E::IssueCommentEdited {
            repo_id,
            issue_id,
            comment_id,
            actor_id,
            changes,
        } => (
            *repo_id,
            TriggerKind::IssueComment {
                issue_id: *issue_id,
                comment_id: *comment_id,
                action: "edited".into(),
                actor_id: *actor_id,
                extra: json!({"changes": changes}),
            },
        ),
        E::IssueCommentDeleted {
            repo_id,
            issue_id,
            comment_id,
            actor_id,
            comment,
        } => (
            *repo_id,
            TriggerKind::IssueComment {
                issue_id: *issue_id,
                comment_id: *comment_id,
                action: "deleted".into(),
                actor_id: *actor_id,
                extra: json!({"comment": comment}),
            },
        ),
        // ----- release (published is mapped by trigger::on_event) -----
        E::ReleaseCreated {
            repo_id,
            release_id,
            actor_id,
        } => (
            *repo_id,
            release(*release_id, "created", *actor_id, Value::Null),
        ),
        E::ReleaseEdited {
            repo_id,
            release_id,
            actor_id,
            changes,
        } => (
            *repo_id,
            release(
                *release_id,
                "edited",
                *actor_id,
                json!({"changes": changes}),
            ),
        ),
        E::ReleaseDeleted {
            repo_id,
            release_id,
            actor_id,
            release: snapshot,
            ..
        } => (
            *repo_id,
            release(
                *release_id,
                "deleted",
                *actor_id,
                json!({"release": snapshot}),
            ),
        ),
        E::ReleaseStateChanged {
            repo_id,
            release_id,
            actor_id,
            action,
        } => (
            *repo_id,
            release(*release_id, action, *actor_id, Value::Null),
        ),
        // ----- label / milestone -----
        E::LabelCreated {
            repo_id,
            label_id,
            actor_id,
        } => (
            *repo_id,
            repo_event(
                "label",
                Some("created"),
                Some(*actor_id),
                json!({"label_id": label_id}),
            ),
        ),
        E::LabelEdited {
            repo_id,
            label_id,
            actor_id,
            changes,
        } => (
            *repo_id,
            repo_event(
                "label",
                Some("edited"),
                Some(*actor_id),
                json!({"label_id": label_id, "changes": changes}),
            ),
        ),
        E::LabelDeleted {
            repo_id,
            label_id,
            actor_id,
            label,
            ..
        } => (
            *repo_id,
            repo_event(
                "label",
                Some("deleted"),
                Some(*actor_id),
                json!({"label_id": label_id, "label": label}),
            ),
        ),
        E::MilestoneCreated {
            repo_id,
            milestone_id,
            actor_id,
        } => milestone(*repo_id, *milestone_id, "created", *actor_id, Value::Null),
        E::MilestoneEdited {
            repo_id,
            milestone_id,
            actor_id,
            changes,
        } => milestone(
            *repo_id,
            *milestone_id,
            "edited",
            *actor_id,
            json!({"changes": changes}),
        ),
        E::MilestoneClosed {
            repo_id,
            milestone_id,
            actor_id,
        } => milestone(*repo_id, *milestone_id, "closed", *actor_id, Value::Null),
        E::MilestoneOpened {
            repo_id,
            milestone_id,
            actor_id,
        } => milestone(*repo_id, *milestone_id, "opened", *actor_id, Value::Null),
        E::MilestoneDeleted {
            repo_id,
            milestone_id,
            actor_id,
            milestone: snapshot,
            ..
        } => milestone(
            *repo_id,
            *milestone_id,
            "deleted",
            *actor_id,
            json!({"milestone": snapshot}),
        ),
        // ----- watch / fork / public / gollum -----
        E::StarCreated { repo_id, actor_id }
        | E::RepositoryStarred {
            repo_id,
            actor_id,
            starred: true,
        } => (
            *repo_id,
            repo_event("watch", Some("started"), Some(*actor_id), Value::Null),
        ),
        E::RepositoryForked {
            repo_id,
            fork_id,
            actor_id,
        } => (
            *repo_id,
            repo_event("fork", None, Some(*actor_id), json!({"fork_id": fork_id})),
        ),
        E::RepositoryPublicized { repo_id, actor_id } => (
            *repo_id,
            repo_event("public", None, Some(*actor_id), Value::Null),
        ),
        E::WikiPagesUpdated {
            repo_id,
            actor_id,
            pages,
        } => (
            *repo_id,
            repo_event("gollum", None, Some(*actor_id), json!({"pages": pages})),
        ),
        // ----- checks written by other integrations -----
        E::CheckRunCreated {
            repo_id,
            check_run_id,
            actor_id,
        } => check_run(*repo_id, *check_run_id, "created", *actor_id, Value::Null),
        E::CheckRunCompleted {
            repo_id,
            check_run_id,
            actor_id,
        } => check_run(*repo_id, *check_run_id, "completed", *actor_id, Value::Null),
        E::CheckRunRerequested {
            repo_id,
            check_run_id,
            actor_id,
        } => check_run(
            *repo_id,
            *check_run_id,
            "rerequested",
            Some(*actor_id),
            Value::Null,
        ),
        E::CheckRunActionRequested {
            repo_id,
            check_run_id,
            actor_id,
            identifier,
        } => check_run(
            *repo_id,
            *check_run_id,
            "requested_action",
            Some(*actor_id),
            json!({"requested_action": {"identifier": identifier}}),
        ),
        E::CheckSuiteCompleted {
            repo_id,
            check_suite_id,
        } => (
            *repo_id,
            repo_event(
                "check_suite",
                Some("completed"),
                None,
                json!({"check_suite_id": check_suite_id}),
            ),
        ),
        // ----- repository_dispatch -----
        E::RepositoryDispatch {
            repo_id,
            actor_id,
            event_type,
            client_payload,
            ..
        } => (
            *repo_id,
            repo_event(
                "repository_dispatch",
                Some(event_type),
                Some(*actor_id),
                json!({"client_payload": client_payload}),
            ),
        ),
        // ----- workflow_run -----
        E::WorkflowRunUpdated {
            repo_id,
            run_id,
            action,
            actor_id,
            workflow_run,
            workflow,
        } if matches!(action.as_str(), "requested" | "in_progress" | "completed") => (
            *repo_id,
            repo_event(
                "workflow_run",
                Some(action),
                *actor_id,
                json!({"run_id": run_id, "workflow_run": workflow_run, "workflow": workflow}),
            ),
        ),
        _ => return None,
    })
}

fn release(release_id: i64, action: &str, actor_id: i64, extra: Value) -> TriggerKind {
    TriggerKind::Release {
        release_id,
        action: action.into(),
        actor_id,
        extra,
    }
}

fn milestone(
    repo_id: i64,
    milestone_id: i64,
    action: &str,
    actor_id: i64,
    mut extra: Value,
) -> (i64, TriggerKind) {
    if extra.is_null() {
        extra = json!({});
    }
    extra["milestone_id"] = json!(milestone_id);
    (
        repo_id,
        repo_event("milestone", Some(action), Some(actor_id), extra),
    )
}

fn check_run(
    repo_id: i64,
    check_run_id: i64,
    action: &str,
    actor_id: Option<i64>,
    mut extra: Value,
) -> (i64, TriggerKind) {
    if extra.is_null() {
        extra = json!({});
    }
    extra["check_run_id"] = json!(check_run_id);
    (
        repo_id,
        repo_event("check_run", Some(action), actor_id, extra),
    )
}

pub async fn is_pull_request(state: &AppState, issue_id: i64) -> anyhow::Result<bool> {
    Ok(
        sqlx::query_scalar::<_, bool>("SELECT is_pull_request FROM issues WHERE id = $1")
            .bind(issue_id)
            .fetch_optional(&state.db)
            .await?
            .unwrap_or(false),
    )
}

fn hint_id(extra: &Value, key: &str) -> Option<i64> {
    extra.get(key).and_then(Value::as_i64)
}

async fn label_json(
    state: &AppState,
    repo: &db::Repository,
    owner: &db::User,
    id: i64,
) -> anyhow::Result<Option<Value>> {
    let row: Option<(i64, String, String, Option<String>, bool)> =
        sqlx::query_as("SELECT id, name, color, description, is_default FROM labels WHERE id = $1")
            .bind(id)
            .fetch_optional(&state.db)
            .await?;
    Ok(row.map(|(id, name, color, description, default)| {
        json!({
            "id": id,
            "name": name,
            "color": color,
            "description": description,
            "default": default,
            "url": state.urls.api(&format!("/repos/{}/{}/labels/{}", owner.login, repo.name, name)),
        })
    }))
}

async fn milestone_json(
    state: &AppState,
    repo: &db::Repository,
    owner: &db::User,
    id: i64,
) -> anyhow::Result<Option<Value>> {
    #[derive(sqlx::FromRow)]
    struct M {
        id: i64,
        number: i64,
        title: String,
        description: Option<String>,
        state: String,
        open_issues: i64,
        closed_issues: i64,
        due_on: Option<chrono::DateTime<chrono::Utc>>,
        created_at: chrono::DateTime<chrono::Utc>,
        updated_at: chrono::DateTime<chrono::Utc>,
    }
    let m: Option<M> = sqlx::query_as(
        "SELECT id, number, title, description, state, open_issues, closed_issues, due_on,
                created_at, updated_at
           FROM milestones WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?;
    Ok(m.map(|m| {
        json!({
            "id": m.id,
            "number": m.number,
            "title": m.title,
            "description": m.description,
            "state": m.state,
            "open_issues": m.open_issues,
            "closed_issues": m.closed_issues,
            "due_on": m.due_on.map(bgh_core::time::Timestamp),
            "created_at": bgh_core::time::Timestamp(m.created_at),
            "updated_at": bgh_core::time::Timestamp(m.updated_at),
            "html_url": state.urls.html(&format!("/{}/{}/milestone/{}", owner.login, repo.name, m.number)),
        })
    }))
}

/// Expand payload hints (ids carried by the trigger) into GitHub payload
/// objects: `label`, `assignee`, `milestone`, `requested_reviewer`,
/// `requested_team`, `review`, `comment` (review comments), `changes`.
pub async fn apply_hints(
    state: &AppState,
    repo: &db::Repository,
    owner: &db::User,
    extra: &Value,
    payload: &mut Value,
) -> anyhow::Result<()> {
    if !extra.is_object() {
        return Ok(());
    }
    if let Some(changes) = extra.get("changes").filter(|c| c.is_object()) {
        payload["changes"] = changes.clone();
    }
    if let Some(id) = hint_id(extra, "label_id") {
        let label = label_json(state, repo, owner, id).await?;
        payload["label"] = label.unwrap_or_else(|| extra.get("label").cloned().unwrap_or_default());
    }
    if let Some(id) = hint_id(extra, "milestone_id") {
        let m = milestone_json(state, repo, owner, id).await?;
        payload["milestone"] =
            m.unwrap_or_else(|| extra.get("milestone").cloned().unwrap_or_default());
    }
    if let Some(id) = hint_id(extra, "assignee_id") {
        let u = user(state, Some(id)).await?;
        payload["assignee"] = context::sender_payload(state, u.as_ref());
    }
    if let Some(id) = hint_id(extra, "reviewer_id") {
        let u = user(state, Some(id)).await?;
        payload["requested_reviewer"] = context::sender_payload(state, u.as_ref());
    }
    if let Some(id) = hint_id(extra, "team_id") {
        let t: Option<(i64, String, String, Option<String>)> =
            sqlx::query_as("SELECT id, name, slug, description FROM teams WHERE id = $1")
                .bind(id)
                .fetch_optional(&state.db)
                .await?;
        if let Some((id, name, slug, description)) = t {
            payload["requested_team"] =
                json!({"id": id, "name": name, "slug": slug, "description": description});
        }
    }
    if let Some(id) = hint_id(extra, "review_id") {
        #[derive(sqlx::FromRow)]
        struct Review {
            id: i64,
            user_id: Option<i64>,
            body: String,
            state: String,
            commit_id: Option<String>,
            submitted_at: Option<chrono::DateTime<chrono::Utc>>,
            number: i64,
        }
        let r: Option<Review> = sqlx::query_as(
            "SELECT r.id, r.user_id, r.body, r.state, r.commit_id, r.submitted_at, i.number
                   FROM pr_reviews r JOIN issues i ON i.id = r.pull_id WHERE r.id = $1",
        )
        .bind(id)
        .fetch_optional(&state.db)
        .await?;
        if let Some(Review {
            id,
            user_id,
            body,
            state: st,
            commit_id,
            submitted_at,
            number,
        }) = r
        {
            let u = user(state, user_id).await?;
            payload["review"] = json!({
                "id": id,
                "user": context::sender_payload(state, u.as_ref()),
                "body": body,
                "state": st.to_ascii_lowercase(),
                "commit_id": commit_id,
                "submitted_at": submitted_at.map(bgh_core::time::Timestamp),
                "html_url": format!("{}#pullrequestreview-{id}", state.urls.pull_html(&owner.login, &repo.name, number)),
            });
        }
    }
    if let Some(id) = hint_id(extra, "review_comment_id") {
        #[derive(sqlx::FromRow)]
        struct ReviewComment {
            id: i64,
            review_id: Option<i64>,
            user_id: Option<i64>,
            body: String,
            path: String,
            commit_id: String,
            line: Option<i32>,
            number: i64,
            created_at: chrono::DateTime<chrono::Utc>,
            updated_at: chrono::DateTime<chrono::Utc>,
        }
        let c: Option<ReviewComment> = sqlx::query_as(
            "SELECT c.id, c.review_id, c.user_id, c.body, c.path, c.commit_id, c.line, i.number,
                        c.created_at, c.updated_at
                   FROM pr_review_comments c JOIN issues i ON i.id = c.pull_id WHERE c.id = $1",
        )
        .bind(id)
        .fetch_optional(&state.db)
        .await?;
        payload["comment"] = match c {
            Some(ReviewComment {
                id,
                review_id,
                user_id,
                body,
                path,
                commit_id,
                line,
                number,
                created_at: created,
                updated_at: updated,
            }) => {
                let u = user(state, user_id).await?;
                json!({
                    "id": id,
                    "pull_request_review_id": review_id,
                    "user": context::sender_payload(state, u.as_ref()),
                    "body": body,
                    "path": path,
                    "commit_id": commit_id,
                    "line": line,
                    "created_at": bgh_core::time::Timestamp(created),
                    "updated_at": bgh_core::time::Timestamp(updated),
                    "html_url": format!("{}#discussion_r{id}", state.urls.pull_html(&owner.login, &repo.name, number)),
                })
            }
            None => extra.get("review_comment").cloned().unwrap_or_default(),
        };
    }
    Ok(())
}

/// Evaluate a default-branch event ([`TriggerKind::Repo`]).
pub async fn on_repo_event(
    state: &AppState,
    repo: &db::Repository,
    owner: &db::User,
    event: &str,
    action: Option<&str>,
    actor_id: Option<i64>,
    extra: &Value,
) -> anyhow::Result<()> {
    let actor = user(state, actor_id).await?;
    let mut payload = json!({
        "repository": context::repo_payload(state, repo, owner),
        "sender": context::sender_payload(state, actor.as_ref()),
    });
    if let Some(a) = action {
        payload["action"] = json!(a);
    }
    let action = action.unwrap_or("");
    match event {
        "label" => {
            apply_hints(state, repo, owner, extra, &mut payload).await?;
        }
        "milestone" => {
            apply_hints(state, repo, owner, extra, &mut payload).await?;
        }
        "watch" | "public" => {}
        "repository_dispatch" => {
            payload["branch"] = json!(repo.default_branch);
            payload["client_payload"] = extra
                .get("client_payload")
                .cloned()
                .filter(|c| !c.is_null())
                .unwrap_or_else(|| json!({}));
        }
        "fork" => {
            let Some(fork_id) = hint_id(extra, "fork_id") else {
                return Ok(());
            };
            let Some(fork) = db::Repository::find(&state.db, fork_id).await? else {
                return Ok(());
            };
            let Some(fork_owner) = db::User::find(&state.db, fork.owner_id).await? else {
                return Ok(());
            };
            payload["forkee"] = context::repo_payload(state, &fork, &fork_owner);
        }
        "gollum" => {
            payload["pages"] = extra.get("pages").cloned().unwrap_or_else(|| json!([]));
        }
        "check_run" => {
            let Some(id) = hint_id(extra, "check_run_id") else {
                return Ok(());
            };
            let Some(check_run) = check_run_json(state, repo, owner, id).await? else {
                return Ok(());
            };
            payload["check_run"] = check_run;
            if let Some(ra) = extra.get("requested_action") {
                payload["requested_action"] = ra.clone();
            }
        }
        "check_suite" => {
            let Some(id) = hint_id(extra, "check_suite_id") else {
                return Ok(());
            };
            let Some(suite) = check_suite_json(state, id).await? else {
                return Ok(());
            };
            payload["check_suite"] = suite;
        }
        "workflow_run" => {
            return on_workflow_run(state, repo, owner, action, actor_id, extra, payload).await;
        }
        _ => return Ok(()),
    }
    let Some((git_ref, sha)) = default_head(state, repo).await else {
        return Ok(());
    };
    trigger::run_default_branch_event(
        state, repo, owner, event, action, &git_ref, &sha, actor_id, payload,
    )
    .await
}

/// Check run JSON for `check_run` payloads; `None` for Actions' own check
/// runs (they never trigger workflows, which would loop).
async fn check_run_json(
    state: &AppState,
    repo: &db::Repository,
    owner: &db::User,
    id: i64,
) -> anyhow::Result<Option<Value>> {
    #[derive(sqlx::FromRow)]
    struct C {
        id: i64,
        name: String,
        head_sha: String,
        status: String,
        conclusion: Option<String>,
        external_id: Option<String>,
        details_url: Option<String>,
        output: Value,
        started_at: Option<chrono::DateTime<chrono::Utc>>,
        completed_at: Option<chrono::DateTime<chrono::Utc>>,
        suite_id: i64,
        head_branch: Option<String>,
        app_slug: String,
    }
    let c: Option<C> = sqlx::query_as(
        "SELECT r.id, r.name, r.head_sha, r.status, r.conclusion, r.external_id, r.details_url,
                r.output, r.started_at, r.completed_at, s.id AS suite_id, s.head_branch, s.app_slug
           FROM check_runs r JOIN check_suites s ON s.id = r.check_suite_id WHERE r.id = $1",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?;
    let Some(c) = c.filter(|c| c.app_slug != "actions") else {
        return Ok(None);
    };
    Ok(Some(json!({
        "id": c.id,
        "name": c.name,
        "head_sha": c.head_sha,
        "status": c.status,
        "conclusion": c.conclusion,
        "external_id": c.external_id,
        "details_url": c.details_url,
        "output": c.output,
        "started_at": c.started_at.map(bgh_core::time::Timestamp),
        "completed_at": c.completed_at.map(bgh_core::time::Timestamp),
        "url": state.urls.api(&format!("/repos/{}/{}/check-runs/{}", owner.login, repo.name, c.id)),
        "check_suite": {"id": c.suite_id, "head_branch": c.head_branch, "head_sha": c.head_sha},
        "app": {"slug": c.app_slug},
    })))
}

/// Check suite JSON for `check_suite` payloads; `None` for Actions' suites.
async fn check_suite_json(state: &AppState, id: i64) -> anyhow::Result<Option<Value>> {
    #[derive(sqlx::FromRow)]
    struct Suite {
        id: i64,
        head_sha: String,
        head_branch: Option<String>,
        status: String,
        conclusion: Option<String>,
        app_slug: String,
    }
    let s: Option<Suite> = sqlx::query_as(
        "SELECT id, head_sha, head_branch, status, conclusion, app_slug FROM check_suites WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?;
    Ok(s.filter(|s| s.app_slug != "actions").map(|s| {
        json!({
            "id": s.id,
            "head_sha": s.head_sha,
            "head_branch": s.head_branch,
            "status": s.status,
            "conclusion": s.conclusion,
            "app": {"slug": s.app_slug},
        })
    }))
}

/// How many `workflow_run` levels led to `run` (1 = not triggered by
/// `workflow_run`).
async fn workflow_run_depth(state: &AppState, run: &RunRow) -> anyhow::Result<usize> {
    let mut depth = 1;
    let mut cur = run.clone();
    while cur.event == "workflow_run" && depth <= MAX_WORKFLOW_RUN_DEPTH {
        let Some(parent) = cur.event_payload["workflow_run"]["id"].as_i64() else {
            break;
        };
        let Some(p) = RunRow::find(&state.db, parent).await? else {
            break;
        };
        depth += 1;
        cur = p;
    }
    Ok(depth)
}

/// `workflow_run`: workflows on the default branch listening for the
/// triggering workflow's name (and branch, activity type).
#[allow(clippy::too_many_arguments)]
async fn on_workflow_run(
    state: &AppState,
    repo: &db::Repository,
    owner: &db::User,
    action: &str,
    actor_id: Option<i64>,
    extra: &Value,
    mut payload: Value,
) -> anyhow::Result<()> {
    let Some(run_id) = hint_id(extra, "run_id") else {
        return Ok(());
    };
    let Some(run) = RunRow::find(&state.db, run_id).await? else {
        return Ok(());
    };
    if workflow_run_depth(state, &run).await? >= MAX_WORKFLOW_RUN_DEPTH {
        return Ok(());
    }
    payload["workflow_run"] = extra.get("workflow_run").cloned().unwrap_or_default();
    payload["workflow"] = extra.get("workflow").cloned().unwrap_or_default();
    let Some((git_ref, sha)) = default_head(state, repo).await else {
        return Ok(());
    };
    let name = run.name.clone();
    let branch = run.head_branch.clone().unwrap_or_default();
    run_default_branch_matching(
        state,
        repo,
        owner,
        "workflow_run",
        &git_ref,
        &sha,
        actor_id,
        payload,
        |d| d.on.matches_workflow_run(&name, action, &branch),
    )
    .await
    .map(drop)
}
