//! Issue, comment, label, assignee and lock mutations (bgh-issues).

use std::sync::Arc;

use async_graphql::{Context, ID, InputObject, Interface, MaybeUndefined, Object, SimpleObject};
use axum::extract::State;
use bgh_core::auth::RequireUser;
use bgh_core::node_id::NodeType;
use bgh_core::prelude::*;
use bgh_issues::json::BodyFormat;
use serde_json::json;

use super::{
    body, decode, guard, into_json, issue_by_node, label_names, logins, milestone_number,
    owner_repo, reload_issue, repo_by_node,
};
use crate::ctx::{GResult, api_err, gql, not_found};
use crate::model::enums::{IssueClosedStateReason, IssueState, LockReason};
use crate::model::issue::{IssueComment, IssueCommentEdge};
use crate::model::pull::from_issues;
use crate::model::{Issue, PullRequest};

/// Issue or pull request, as returned by `Labelable` / `Assignable` /
/// `Lockable` / `Closable` payload fields.
#[derive(Interface, Clone)]
#[graphql(
    name = "Lockable",
    field(name = "id", ty = "ID"),
    field(name = "locked", ty = "bool"),
    field(name = "active_lock_reason", ty = "Option<LockReason>")
)]
pub enum IssueLike {
    Issue(Issue),
    PullRequest(PullRequest),
}

pub async fn issue_like(ctx: &Context<'_>, id: i64) -> GResult<IssueLike> {
    let i = reload_issue(ctx, id).await?;
    if i.is_pull_request {
        let p = from_issues(ctx, vec![i])
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| not_found("pull request not found"))?;
        Ok(IssueLike::PullRequest(p))
    } else {
        Ok(IssueLike::Issue(Issue::new(Arc::new(i))))
    }
}

/// Name of the issue type `id` (REST takes names).
async fn issue_type_name(ctx: &Context<'_>, id: &ID) -> GResult<String> {
    let n = decode(id, &[NodeType::IssueType], "an IssueType")?;
    bgh_issues::issue_types::by_id(&gql(ctx).state.db, n)
        .await
        .map_err(api_err)?
        .map(|t| t.name)
        .ok_or_else(|| {
            not_found(format!(
                "Could not resolve to an IssueType with the global id of '{}'.",
                id.0
            ))
        })
}

fn st(ctx: &Context<'_>) -> State<AppState> {
    State(gql(ctx).state.clone())
}

fn user(a: &bgh_core::auth::AuthContext) -> RequireUser {
    RequireUser(a.clone())
}

#[derive(InputObject)]
pub struct CreateIssueInput {
    pub repository_id: ID,
    pub title: String,
    pub body: Option<String>,
    pub assignee_ids: Option<Vec<ID>>,
    pub label_ids: Option<Vec<ID>>,
    pub milestone_id: Option<ID>,
    pub project_ids: Option<Vec<ID>>,
    #[graphql(name = "projectV2Ids")]
    pub project_v2_ids: Option<Vec<ID>>,
    pub issue_template: Option<String>,
    pub issue_type_id: Option<ID>,
    pub parent_issue_id: Option<ID>,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
pub struct CreateIssuePayload {
    pub issue: Option<Issue>,
    pub client_mutation_id: Option<String>,
}

#[derive(InputObject)]
pub struct UpdateIssueInput {
    pub id: ID,
    pub title: Option<String>,
    pub body: Option<String>,
    pub assignee_ids: Option<Vec<ID>>,
    pub label_ids: Option<Vec<ID>>,
    pub milestone_id: Option<ID>,
    pub state: Option<IssueState>,
    pub project_ids: Option<Vec<ID>>,
    /// `null` removes the issue type.
    pub issue_type_id: MaybeUndefined<ID>,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
pub struct UpdateIssuePayload {
    pub issue: Option<Issue>,
    pub actor: Option<crate::model::Actor>,
    pub client_mutation_id: Option<String>,
}

#[derive(InputObject)]
pub struct CloseIssueInput {
    pub issue_id: ID,
    pub state_reason: Option<IssueClosedStateReason>,
    pub duplicate_issue_id: Option<ID>,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
pub struct CloseIssuePayload {
    pub issue: Option<Issue>,
    pub client_mutation_id: Option<String>,
}

#[derive(InputObject)]
pub struct ReopenIssueInput {
    pub issue_id: ID,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
pub struct ReopenIssuePayload {
    pub issue: Option<Issue>,
    pub client_mutation_id: Option<String>,
}

#[derive(InputObject)]
pub struct AddCommentInput {
    pub subject_id: ID,
    pub body: String,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
pub struct AddCommentPayload {
    pub comment_edge: Option<IssueCommentEdge>,
    pub subject: Option<IssueLike>,
    pub client_mutation_id: Option<String>,
}

#[derive(InputObject)]
pub struct UpdateIssueCommentInput {
    pub id: ID,
    pub body: String,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
pub struct UpdateIssueCommentPayload {
    pub issue_comment: Option<IssueComment>,
    pub client_mutation_id: Option<String>,
}

#[derive(InputObject)]
pub struct DeleteIssueCommentInput {
    pub id: ID,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
pub struct DeleteIssueCommentPayload {
    pub client_mutation_id: Option<String>,
}

#[derive(InputObject)]
pub struct AddLabelsToLabelableInput {
    pub labelable_id: ID,
    pub label_ids: Vec<ID>,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
pub struct AddLabelsToLabelablePayload {
    pub labelable: Option<IssueLike>,
    pub client_mutation_id: Option<String>,
}

#[derive(InputObject)]
pub struct RemoveLabelsFromLabelableInput {
    pub labelable_id: ID,
    pub label_ids: Vec<ID>,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
pub struct RemoveLabelsFromLabelablePayload {
    pub labelable: Option<IssueLike>,
    pub client_mutation_id: Option<String>,
}

#[derive(InputObject)]
pub struct AddAssigneesToAssignableInput {
    pub assignable_id: ID,
    pub assignee_ids: Vec<ID>,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
pub struct AddAssigneesToAssignablePayload {
    pub assignable: Option<IssueLike>,
    pub client_mutation_id: Option<String>,
}

#[derive(InputObject)]
pub struct RemoveAssigneesFromAssignableInput {
    pub assignable_id: ID,
    pub assignee_ids: Vec<ID>,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
pub struct RemoveAssigneesFromAssignablePayload {
    pub assignable: Option<IssueLike>,
    pub client_mutation_id: Option<String>,
}

#[derive(InputObject)]
pub struct ReplaceActorsForAssignableInput {
    pub assignable_id: ID,
    pub actor_ids: Option<Vec<ID>>,
    pub actor_logins: Option<Vec<String>>,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
pub struct ReplaceActorsForAssignablePayload {
    pub assignable: Option<IssueLike>,
    pub client_mutation_id: Option<String>,
}

#[derive(InputObject)]
pub struct LockLockableInput {
    pub lockable_id: ID,
    pub lock_reason: Option<LockReason>,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
pub struct LockLockablePayload {
    pub locked_record: Option<IssueLike>,
    pub actor: Option<crate::model::Actor>,
    pub client_mutation_id: Option<String>,
}

#[derive(InputObject)]
pub struct UnlockLockableInput {
    pub lockable_id: ID,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
pub struct UnlockLockablePayload {
    pub unlocked_record: Option<IssueLike>,
    pub actor: Option<crate::model::Actor>,
    pub client_mutation_id: Option<String>,
}

#[derive(InputObject)]
pub struct PinIssueInput {
    pub issue_id: ID,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
pub struct PinIssuePayload {
    pub issue: Option<Issue>,
    pub client_mutation_id: Option<String>,
}

#[derive(InputObject)]
pub struct UnpinIssueInput {
    pub issue_id: ID,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
pub struct UnpinIssuePayload {
    pub issue: Option<Issue>,
    pub client_mutation_id: Option<String>,
}

#[derive(InputObject)]
pub struct TransferIssueInput {
    pub issue_id: ID,
    pub repository_id: ID,
    pub create_labels_if_missing: Option<bool>,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
pub struct TransferIssuePayload {
    pub issue: Option<Issue>,
    pub client_mutation_id: Option<String>,
}

fn as_issue(x: IssueLike) -> Option<Issue> {
    match x {
        IssueLike::Issue(i) => Some(i),
        IssueLike::PullRequest(_) => None,
    }
}

/// `PATCH /repos/{o}/{r}/issues/{n}` through bgh-issues.
pub async fn patch_issue(
    ctx: &Context<'_>,
    issue: &db::Issue,
    repo: &crate::loaders::RepoRow,
    patch: serde_json::Value,
) -> GResult<()> {
    let a = guard(ctx)?;
    let (o, r) = owner_repo(repo);
    into_json(
        bgh_issues::issues::update(
            st(ctx),
            user(a),
            BodyFormat::default(),
            Path((o, r, issue.number)),
            Json(body(patch)?),
        )
        .await,
    )
    .await?;
    Ok(())
}

#[derive(InputObject)]
pub struct CreateLinkedBranchInput {
    pub issue_id: ID,
    pub oid: crate::scalars::GitObjectID,
    pub name: Option<String>,
    pub repository_id: Option<ID>,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
pub struct CreateLinkedBranchPayload {
    pub linked_branch: Option<crate::model::issue::LinkedBranch>,
    pub issue: Option<Issue>,
    pub client_mutation_id: Option<String>,
}

/// `{number}-{slug of title}`, like GitHub's "create a branch" default.
fn branch_name(number: i64, title: &str) -> String {
    let mut slug = String::new();
    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
    }
    let slug: String = slug.trim_end_matches('-').chars().take(60).collect();
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() {
        number.to_string()
    } else {
        format!("{number}-{slug}")
    }
}

#[derive(Default)]
pub struct IssueMutations;

#[Object]
impl IssueMutations {
    /// Create a branch linked to an issue (named `{number}-...`).
    pub async fn create_linked_branch(
        &self,
        ctx: &Context<'_>,
        input: CreateLinkedBranchInput,
    ) -> GResult<CreateLinkedBranchPayload> {
        let a = guard(ctx)?;
        let (issue, issue_repo) = issue_by_node(ctx, &input.issue_id).await?;
        let repo = match &input.repository_id {
            Some(id) => repo_by_node(ctx, id).await?,
            None => issue_repo,
        };
        let mut name = input
            .name
            .clone()
            .unwrap_or_else(|| branch_name(issue.number, &issue.title));
        if !name.starts_with(&format!("{}-", issue.number)) {
            name = format!("{}-{name}", issue.number);
        }
        let (o, r) = owner_repo(&repo);
        into_json(
            bgh_repos::gitdb::create_ref(
                st(ctx),
                user(a),
                Path((o, r)),
                Json(body(
                    json!({"ref": format!("refs/heads/{name}"), "sha": input.oid.0}),
                )?),
            )
            .await,
        )
        .await?;
        let repo = crate::model::Repository(repo);
        let full = format!("refs/heads/{name}");
        let linked = crate::model::Ref::load(ctx, repo, &full)
            .await?
            .map(|r| crate::model::issue::LinkedBranch { r });
        Ok(CreateLinkedBranchPayload {
            linked_branch: linked,
            issue: as_issue(issue_like(ctx, issue.id).await?),
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Creates a new issue.
    pub async fn create_issue(
        &self,
        ctx: &Context<'_>,
        input: CreateIssueInput,
    ) -> GResult<CreateIssuePayload> {
        let a = guard(ctx)?;
        let repo = repo_by_node(ctx, &input.repository_id).await?;
        let mut b = json!({"title": input.title, "body": input.body});
        if let Some(ids) = &input.assignee_ids {
            b["assignees"] = json!(logins(ctx, ids).await?);
        }
        if let Some(ids) = &input.label_ids {
            b["labels"] = json!(label_names(ctx, repo.repo.id, ids).await?);
        }
        if let Some(m) = &input.milestone_id {
            b["milestone"] = json!(milestone_number(ctx, repo.repo.id, m).await?);
        }
        if let Some(t) = &input.issue_type_id {
            b["type"] = json!(issue_type_name(ctx, t).await?);
        }
        if let Some(t) = &input.issue_template {
            b["template"] = json!(t);
        }
        let (o, r) = owner_repo(&repo);
        let v = into_json(
            bgh_issues::issues::create(
                st(ctx),
                user(a),
                BodyFormat::default(),
                Path((o, r)),
                Json(body(b)?),
            )
            .await,
        )
        .await?;
        let id = v["id"]
            .as_i64()
            .ok_or_else(|| not_found("issue not created"))?;
        if let Some(projects) = &input.project_v2_ids {
            super::projects::add_to_projects(ctx, id, projects).await?;
        }
        Ok(CreateIssuePayload {
            issue: as_issue(issue_like(ctx, id).await?),
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Updates an issue.
    pub async fn update_issue(
        &self,
        ctx: &Context<'_>,
        input: UpdateIssueInput,
    ) -> GResult<UpdateIssuePayload> {
        let (issue, repo) = issue_by_node(ctx, &input.id).await?;
        let mut b = json!({});
        if let Some(t) = input.title {
            b["title"] = json!(t);
        }
        if let Some(t) = input.body {
            b["body"] = json!(t);
        }
        if let Some(s) = input.state {
            b["state"] = json!(if s == IssueState::Closed {
                "closed"
            } else {
                "open"
            });
        }
        if let Some(ids) = &input.assignee_ids {
            b["assignees"] = json!(logins(ctx, ids).await?);
        }
        if let Some(ids) = &input.label_ids {
            b["labels"] = json!(label_names(ctx, repo.repo.id, ids).await?);
        }
        if let Some(m) = &input.milestone_id {
            b["milestone"] = json!(milestone_number(ctx, repo.repo.id, m).await?);
        }
        match &input.issue_type_id {
            MaybeUndefined::Value(t) => b["type"] = json!(issue_type_name(ctx, t).await?),
            MaybeUndefined::Null => b["type"] = serde_json::Value::Null,
            MaybeUndefined::Undefined => {}
        }
        patch_issue(ctx, &issue, &repo, b).await?;
        let a = guard(ctx)?;
        Ok(UpdateIssuePayload {
            issue: as_issue(issue_like(ctx, issue.id).await?),
            actor: Some(crate::model::Actor::from_user(Arc::new(a.user.clone()))),
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Close an issue.
    pub async fn close_issue(
        &self,
        ctx: &Context<'_>,
        input: CloseIssueInput,
    ) -> GResult<CloseIssuePayload> {
        let (issue, repo) = issue_by_node(ctx, &input.issue_id).await?;
        let reason = input
            .state_reason
            .unwrap_or(IssueClosedStateReason::Completed)
            .rest();
        let mut b = json!({"state": "closed", "state_reason": reason});
        if let Some(d) = &input.duplicate_issue_id {
            b["duplicate_of"] = json!(decode(d, &[NodeType::Issue], "an Issue")?);
            b["state_reason"] = json!("duplicate");
        }
        patch_issue(ctx, &issue, &repo, b).await?;
        Ok(CloseIssuePayload {
            issue: as_issue(issue_like(ctx, issue.id).await?),
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Reopen an issue.
    pub async fn reopen_issue(
        &self,
        ctx: &Context<'_>,
        input: ReopenIssueInput,
    ) -> GResult<ReopenIssuePayload> {
        let (issue, repo) = issue_by_node(ctx, &input.issue_id).await?;
        patch_issue(ctx, &issue, &repo, json!({"state": "open"})).await?;
        Ok(ReopenIssuePayload {
            issue: as_issue(issue_like(ctx, issue.id).await?),
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Adds a comment to an Issue or Pull Request.
    pub async fn add_comment(
        &self,
        ctx: &Context<'_>,
        input: AddCommentInput,
    ) -> GResult<AddCommentPayload> {
        let a = guard(ctx)?;
        let (issue, repo) = issue_by_node(ctx, &input.subject_id).await?;
        let (o, r) = owner_repo(&repo);
        let v = into_json(
            bgh_issues::comments::create(
                st(ctx),
                user(a),
                BodyFormat::default(),
                Path((o, r, issue.number)),
                Json(body(json!({"body": input.body}))?),
            )
            .await,
        )
        .await?;
        let cid = v["id"]
            .as_i64()
            .ok_or_else(|| not_found("comment not created"))?;
        let c = load_comment(ctx, cid).await?;
        Ok(AddCommentPayload {
            comment_edge: Some(IssueCommentEdge {
                cursor: crate::conn::encode_cursor(issue.comments_count + 1),
                node: c,
            }),
            subject: Some(issue_like(ctx, issue.id).await?),
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Updates an IssueComment object.
    pub async fn update_issue_comment(
        &self,
        ctx: &Context<'_>,
        input: UpdateIssueCommentInput,
    ) -> GResult<UpdateIssueCommentPayload> {
        let a = guard(ctx)?;
        let cid = decode(&input.id, &[NodeType::IssueComment], "an IssueComment")?;
        let c = load_comment(ctx, cid).await?;
        let repo = super::repo_by_id(ctx, c.0.repo_id).await?;
        let (o, r) = owner_repo(&repo);
        into_json(
            bgh_issues::comments::update(
                st(ctx),
                user(a),
                BodyFormat::default(),
                Path((o, r, cid)),
                Json(body(json!({"body": input.body}))?),
            )
            .await,
        )
        .await?;
        Ok(UpdateIssueCommentPayload {
            issue_comment: Some(load_comment(ctx, cid).await?),
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Deletes an IssueComment object.
    pub async fn delete_issue_comment(
        &self,
        ctx: &Context<'_>,
        input: DeleteIssueCommentInput,
    ) -> GResult<DeleteIssueCommentPayload> {
        let a = guard(ctx)?;
        let cid = decode(&input.id, &[NodeType::IssueComment], "an IssueComment")?;
        let c = load_comment(ctx, cid).await?;
        let repo = super::repo_by_id(ctx, c.0.repo_id).await?;
        let (o, r) = owner_repo(&repo);
        into_json(bgh_issues::comments::delete(st(ctx), user(a), Path((o, r, cid))).await).await?;
        Ok(DeleteIssueCommentPayload {
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Adds labels to a labelable object.
    pub async fn add_labels_to_labelable(
        &self,
        ctx: &Context<'_>,
        input: AddLabelsToLabelableInput,
    ) -> GResult<AddLabelsToLabelablePayload> {
        let a = guard(ctx)?;
        let (issue, repo) = issue_by_node(ctx, &input.labelable_id).await?;
        let names = label_names(ctx, repo.repo.id, &input.label_ids).await?;
        let (o, r) = owner_repo(&repo);
        into_json(
            bgh_issues::labels::add_to_issue(
                st(ctx),
                user(a),
                Path((o, r, issue.number)),
                Json(body(json!({"labels": names}))?),
            )
            .await,
        )
        .await?;
        Ok(AddLabelsToLabelablePayload {
            labelable: Some(issue_like(ctx, issue.id).await?),
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Removes labels from a Labelable object.
    pub async fn remove_labels_from_labelable(
        &self,
        ctx: &Context<'_>,
        input: RemoveLabelsFromLabelableInput,
    ) -> GResult<RemoveLabelsFromLabelablePayload> {
        let a = guard(ctx)?;
        let (issue, repo) = issue_by_node(ctx, &input.labelable_id).await?;
        let names = label_names(ctx, repo.repo.id, &input.label_ids).await?;
        for name in names {
            let (o, r) = owner_repo(&repo);
            into_json(
                bgh_issues::labels::remove_from_issue(
                    st(ctx),
                    user(a),
                    Path((o, r, issue.number, name)),
                )
                .await,
            )
            .await?;
        }
        Ok(RemoveLabelsFromLabelablePayload {
            labelable: Some(issue_like(ctx, issue.id).await?),
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Adds assignees to an assignable object.
    pub async fn add_assignees_to_assignable(
        &self,
        ctx: &Context<'_>,
        input: AddAssigneesToAssignableInput,
    ) -> GResult<AddAssigneesToAssignablePayload> {
        let a = guard(ctx)?;
        let (issue, repo) = issue_by_node(ctx, &input.assignable_id).await?;
        let names = logins(ctx, &input.assignee_ids).await?;
        let (o, r) = owner_repo(&repo);
        into_json(
            bgh_issues::assignees::add(
                st(ctx),
                user(a),
                BodyFormat::default(),
                Path((o, r, issue.number)),
                Json(body(json!({"assignees": names}))?),
            )
            .await,
        )
        .await?;
        Ok(AddAssigneesToAssignablePayload {
            assignable: Some(issue_like(ctx, issue.id).await?),
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Removes assignees from an assignable object.
    pub async fn remove_assignees_from_assignable(
        &self,
        ctx: &Context<'_>,
        input: RemoveAssigneesFromAssignableInput,
    ) -> GResult<RemoveAssigneesFromAssignablePayload> {
        let a = guard(ctx)?;
        let (issue, repo) = issue_by_node(ctx, &input.assignable_id).await?;
        let names = logins(ctx, &input.assignee_ids).await?;
        let (o, r) = owner_repo(&repo);
        into_json(
            bgh_issues::assignees::remove(
                st(ctx),
                user(a),
                BodyFormat::default(),
                Path((o, r, issue.number)),
                Json(body(json!({"assignees": names}))?),
            )
            .await,
        )
        .await?;
        Ok(RemoveAssigneesFromAssignablePayload {
            assignable: Some(issue_like(ctx, issue.id).await?),
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Replaces the assignees of an assignable object.
    pub async fn replace_actors_for_assignable(
        &self,
        ctx: &Context<'_>,
        input: ReplaceActorsForAssignableInput,
    ) -> GResult<ReplaceActorsForAssignablePayload> {
        let (issue, repo) = issue_by_node(ctx, &input.assignable_id).await?;
        let mut names = match &input.actor_ids {
            Some(ids) => logins(ctx, ids).await?,
            None => vec![],
        };
        names.extend(input.actor_logins.clone().unwrap_or_default());
        patch_issue(ctx, &issue, &repo, json!({"assignees": names})).await?;
        Ok(ReplaceActorsForAssignablePayload {
            assignable: Some(issue_like(ctx, issue.id).await?),
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Lock a lockable object.
    pub async fn lock_lockable(
        &self,
        ctx: &Context<'_>,
        input: LockLockableInput,
    ) -> GResult<LockLockablePayload> {
        let a = guard(ctx)?;
        let (issue, repo) = issue_by_node(ctx, &input.lockable_id).await?;
        let (o, r) = owner_repo(&repo);
        let reason = input.lock_reason.map(LockReason::rest);
        into_json(
            bgh_issues::events::lock(
                st(ctx),
                user(a),
                Path((o, r, issue.number)),
                Json(body(json!({"lock_reason": reason}))?),
            )
            .await,
        )
        .await?;
        Ok(LockLockablePayload {
            locked_record: Some(issue_like(ctx, issue.id).await?),
            actor: Some(crate::model::Actor::from_user(Arc::new(a.user.clone()))),
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Unlock a lockable object.
    pub async fn unlock_lockable(
        &self,
        ctx: &Context<'_>,
        input: UnlockLockableInput,
    ) -> GResult<UnlockLockablePayload> {
        let a = guard(ctx)?;
        let (issue, repo) = issue_by_node(ctx, &input.lockable_id).await?;
        let (o, r) = owner_repo(&repo);
        into_json(bgh_issues::events::unlock(st(ctx), user(a), Path((o, r, issue.number))).await)
            .await?;
        Ok(UnlockLockablePayload {
            unlocked_record: Some(issue_like(ctx, issue.id).await?),
            actor: Some(crate::model::Actor::from_user(Arc::new(a.user.clone()))),
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Pin an issue to a repository.
    pub async fn pin_issue(
        &self,
        ctx: &Context<'_>,
        input: PinIssueInput,
    ) -> GResult<PinIssuePayload> {
        let a = guard(ctx)?;
        let (issue, repo) = issue_by_node(ctx, &input.issue_id).await?;
        let (o, r) = owner_repo(&repo);
        into_json(bgh_issues::pins::pin(st(ctx), user(a), Path((o, r, issue.number))).await)
            .await?;
        Ok(PinIssuePayload {
            issue: as_issue(issue_like(ctx, issue.id).await?),
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Unpin a pinned issue from a repository.
    pub async fn unpin_issue(
        &self,
        ctx: &Context<'_>,
        input: UnpinIssueInput,
    ) -> GResult<UnpinIssuePayload> {
        let a = guard(ctx)?;
        let (issue, repo) = issue_by_node(ctx, &input.issue_id).await?;
        let (o, r) = owner_repo(&repo);
        into_json(bgh_issues::pins::unpin(st(ctx), user(a), Path((o, r, issue.number))).await)
            .await?;
        Ok(UnpinIssuePayload {
            issue: as_issue(issue_like(ctx, issue.id).await?),
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Transfer an issue to a different repository.
    pub async fn transfer_issue(
        &self,
        ctx: &Context<'_>,
        input: TransferIssueInput,
    ) -> GResult<TransferIssuePayload> {
        let a = guard(ctx)?;
        let (issue, repo) = issue_by_node(ctx, &input.issue_id).await?;
        let target = repo_by_node(ctx, &input.repository_id).await?;
        let (o, r) = owner_repo(&repo);
        let v = into_json(
            bgh_issues::transfer::transfer(
                st(ctx),
                user(a),
                BodyFormat::default(),
                Path((o, r, issue.number)),
                Json(body(
                    json!({"new_owner": target.owner.login, "new_name": target.repo.name}),
                )?),
            )
            .await,
        )
        .await?;
        let id = v["id"].as_i64().unwrap_or(issue.id);
        Ok(TransferIssuePayload {
            issue: as_issue(issue_like(ctx, id).await?),
            client_mutation_id: input.client_mutation_id,
        })
    }
}

async fn load_comment(ctx: &Context<'_>, id: i64) -> GResult<IssueComment> {
    let row: Option<db::Comment> = sqlx::query_as(&format!(
        "SELECT {} FROM comments WHERE id = $1",
        db::Comment::COLUMNS
    ))
    .bind(id)
    .fetch_optional(&gql(ctx).state.db)
    .await
    .map_err(|e| api_err(e.into()))?;
    let c = row.ok_or_else(|| not_found("Could not resolve to an IssueComment."))?;
    super::repo_by_id(ctx, c.repo_id).await?;
    Ok(IssueComment(Arc::new(c)))
}
