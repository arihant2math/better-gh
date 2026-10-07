//! Pull request, review and auto-merge mutations (bgh-pulls).

use std::sync::Arc;

use async_graphql::{Context, ID, InputObject, Object, SimpleObject};
use axum::extract::State;
use bgh_core::auth::RequireUser;
use bgh_core::node_id::NodeType;
use bgh_core::prelude::*;
use serde_json::json;

use super::issues::patch_issue;
use super::{
    body, decode, guard, into_json, issue_by_node, label_names, logins, milestone_number,
    owner_repo, reload_issue, repo_by_id, repo_by_node,
};
use crate::ctx::{GResult, err, gql, not_found};
use crate::loaders::{Loaders, RepoRow, one};
use crate::model::PullRequest;
use crate::model::enums::{
    DiffSide, PullRequestMergeMethod, PullRequestReviewEvent, PullRequestUpdateState,
};
use crate::model::pull::{
    PullRequestReview, PullRequestReviewThread, decode_thread_id, from_issues, threads,
};
use crate::scalars::GitObjectID;

fn st(ctx: &Context<'_>) -> State<AppState> {
    State(gql(ctx).state.clone())
}

fn user(a: &bgh_core::auth::AuthContext) -> RequireUser {
    RequireUser(a.clone())
}

/// A pull request (by node id) plus its base repository.
async fn pull_by_node(ctx: &Context<'_>, id: &ID) -> GResult<(db::Issue, Arc<RepoRow>)> {
    let (issue, repo) = issue_by_node(ctx, id).await?;
    if !issue.is_pull_request {
        return Err(not_found(format!(
            "Could not resolve to a PullRequest with the global id of '{}'.",
            id.0
        )));
    }
    Ok((issue, repo))
}

async fn pr(ctx: &Context<'_>, id: i64) -> GResult<PullRequest> {
    let i = reload_issue(ctx, id).await?;
    from_issues(ctx, vec![i])
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| not_found("pull request not found"))
}

#[derive(InputObject)]
pub struct CreatePullRequestInput {
    pub repository_id: ID,
    pub base_ref_name: String,
    pub head_ref_name: String,
    pub head_repository_id: Option<ID>,
    pub title: String,
    pub body: Option<String>,
    pub maintainer_can_modify: Option<bool>,
    pub draft: Option<bool>,
    /// Projects to add the pull request to (bgh extension; GitHub only
    /// takes it on `createIssue`).
    #[graphql(name = "projectV2Ids")]
    pub project_v2_ids: Option<Vec<ID>>,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
pub struct CreatePullRequestPayload {
    pub pull_request: Option<PullRequest>,
    pub client_mutation_id: Option<String>,
}

#[derive(InputObject)]
pub struct UpdatePullRequestInput {
    pub pull_request_id: ID,
    pub base_ref_name: Option<String>,
    pub title: Option<String>,
    pub body: Option<String>,
    pub state: Option<PullRequestUpdateState>,
    pub maintainer_can_modify: Option<bool>,
    pub assignee_ids: Option<Vec<ID>>,
    pub label_ids: Option<Vec<ID>>,
    pub milestone_id: Option<ID>,
    pub project_ids: Option<Vec<ID>>,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
pub struct UpdatePullRequestPayload {
    pub pull_request: Option<PullRequest>,
    pub actor: Option<crate::model::Actor>,
    pub client_mutation_id: Option<String>,
}

#[derive(InputObject)]
pub struct ClosePullRequestInput {
    pub pull_request_id: ID,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
pub struct ClosePullRequestPayload {
    pub pull_request: Option<PullRequest>,
    pub client_mutation_id: Option<String>,
}

#[derive(InputObject)]
pub struct ReopenPullRequestInput {
    pub pull_request_id: ID,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
pub struct ReopenPullRequestPayload {
    pub pull_request: Option<PullRequest>,
    pub client_mutation_id: Option<String>,
}

#[derive(InputObject)]
pub struct MergePullRequestInput {
    pub pull_request_id: ID,
    pub commit_headline: Option<String>,
    pub commit_body: Option<String>,
    pub expected_head_oid: Option<GitObjectID>,
    pub merge_method: Option<PullRequestMergeMethod>,
    pub author_email: Option<String>,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
pub struct MergePullRequestPayload {
    pub pull_request: Option<PullRequest>,
    pub actor: Option<crate::model::Actor>,
    pub client_mutation_id: Option<String>,
}

#[derive(InputObject)]
pub struct MarkPullRequestReadyForReviewInput {
    pub pull_request_id: ID,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
pub struct MarkPullRequestReadyForReviewPayload {
    pub pull_request: Option<PullRequest>,
    pub client_mutation_id: Option<String>,
}

#[derive(InputObject)]
pub struct ConvertPullRequestToDraftInput {
    pub pull_request_id: ID,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
pub struct ConvertPullRequestToDraftPayload {
    pub pull_request: Option<PullRequest>,
    pub client_mutation_id: Option<String>,
}

#[derive(InputObject)]
pub struct DraftPullRequestReviewComment {
    pub path: String,
    pub position: i32,
    pub body: String,
}

#[derive(InputObject)]
pub struct DraftPullRequestReviewThread {
    pub path: Option<String>,
    pub line: Option<i32>,
    pub side: Option<DiffSide>,
    pub start_line: Option<i32>,
    pub start_side: Option<DiffSide>,
    pub body: String,
}

#[derive(InputObject)]
pub struct AddPullRequestReviewInput {
    pub pull_request_id: ID,
    #[graphql(name = "commitOID")]
    pub commit_oid: Option<GitObjectID>,
    pub body: Option<String>,
    pub event: Option<PullRequestReviewEvent>,
    pub comments: Option<Vec<Option<DraftPullRequestReviewComment>>>,
    pub threads: Option<Vec<Option<DraftPullRequestReviewThread>>>,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
pub struct AddPullRequestReviewPayload {
    pub pull_request_review: Option<PullRequestReview>,
    pub client_mutation_id: Option<String>,
}

#[derive(InputObject)]
pub struct SubmitPullRequestReviewInput {
    pub pull_request_id: Option<ID>,
    pub pull_request_review_id: Option<ID>,
    pub event: PullRequestReviewEvent,
    pub body: Option<String>,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
pub struct SubmitPullRequestReviewPayload {
    pub pull_request_review: Option<PullRequestReview>,
    pub client_mutation_id: Option<String>,
}

#[derive(InputObject)]
pub struct DismissPullRequestReviewInput {
    pub pull_request_review_id: ID,
    pub message: String,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
pub struct DismissPullRequestReviewPayload {
    pub pull_request_review: Option<PullRequestReview>,
    pub client_mutation_id: Option<String>,
}

#[derive(InputObject)]
pub struct RequestReviewsInput {
    pub pull_request_id: ID,
    pub user_ids: Option<Vec<ID>>,
    pub team_ids: Option<Vec<ID>>,
    pub union: Option<bool>,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
pub struct RequestReviewsPayload {
    pub pull_request: Option<PullRequest>,
    pub actor: Option<crate::model::Actor>,
    pub client_mutation_id: Option<String>,
}

#[derive(InputObject)]
pub struct RequestReviewsByLoginInput {
    pub pull_request_id: ID,
    pub user_logins: Option<Vec<String>>,
    pub bot_logins: Option<Vec<String>>,
    pub team_slugs: Option<Vec<String>>,
    pub union: Option<bool>,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
pub struct RequestReviewsByLoginPayload {
    pub pull_request: Option<PullRequest>,
    pub client_mutation_id: Option<String>,
}

#[derive(InputObject)]
pub struct EnablePullRequestAutoMergeInput {
    pub pull_request_id: ID,
    pub commit_headline: Option<String>,
    pub commit_body: Option<String>,
    pub merge_method: Option<PullRequestMergeMethod>,
    pub author_email: Option<String>,
    pub expected_head_oid: Option<GitObjectID>,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
pub struct EnablePullRequestAutoMergePayload {
    pub pull_request: Option<PullRequest>,
    pub client_mutation_id: Option<String>,
}

#[derive(InputObject)]
pub struct DisablePullRequestAutoMergeInput {
    pub pull_request_id: ID,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
pub struct DisablePullRequestAutoMergePayload {
    pub pull_request: Option<PullRequest>,
    pub client_mutation_id: Option<String>,
}

#[derive(InputObject)]
pub struct ResolveReviewThreadInput {
    pub thread_id: ID,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
pub struct ResolveReviewThreadPayload {
    pub thread: Option<PullRequestReviewThread>,
    pub client_mutation_id: Option<String>,
}

#[derive(InputObject)]
pub struct UnresolveReviewThreadInput {
    pub thread_id: ID,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
pub struct UnresolveReviewThreadPayload {
    pub thread: Option<PullRequestReviewThread>,
    pub client_mutation_id: Option<String>,
}

#[derive(InputObject)]
pub struct UpdatePullRequestBranchInput {
    pub pull_request_id: ID,
    pub expected_head_oid: Option<GitObjectID>,
    pub update_method: Option<String>,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
pub struct UpdatePullRequestBranchPayload {
    pub pull_request: Option<PullRequest>,
    pub client_mutation_id: Option<String>,
}

/// `PATCH /repos/{o}/{r}/pulls/{n}` through bgh-pulls.
async fn patch_pull(
    ctx: &Context<'_>,
    issue: &db::Issue,
    repo: &RepoRow,
    patch: serde_json::Value,
) -> GResult<()> {
    let a = guard(ctx)?;
    let (o, r) = owner_repo(repo);
    into_json(
        bgh_pulls::pulls::update(
            st(ctx),
            user(a),
            Path((o, r, issue.number)),
            Json(body(patch)?),
        )
        .await,
    )
    .await?;
    Ok(())
}

async fn review_by_node(ctx: &Context<'_>, id: &ID) -> GResult<crate::loaders::ReviewRow> {
    let rid = decode(id, &[NodeType::PullRequestReview], "a PullRequestReview")?;
    let row: Option<crate::loaders::ReviewRow> = sqlx::query_as(&format!(
        "SELECT {} FROM pr_reviews WHERE id = $1",
        crate::loaders::ReviewRow::COLUMNS
    ))
    .bind(rid)
    .fetch_optional(&gql(ctx).state.db)
    .await
    .map_err(|e| crate::ctx::api_err(e.into()))?;
    row.ok_or_else(|| not_found("Could not resolve to a PullRequestReview."))
}

async fn review_obj(ctx: &Context<'_>, id: i64) -> GResult<PullRequestReview> {
    let r = review_by_node(ctx, &crate::model::nid(NodeType::PullRequestReview, id)).await?;
    Ok(PullRequestReview(Arc::new(r)))
}

async fn request_reviewers(
    ctx: &Context<'_>,
    issue: &db::Issue,
    repo: &RepoRow,
    users: Vec<String>,
    teams: Vec<String>,
    union: bool,
) -> GResult<()> {
    let a = guard(ctx)?;
    let (o, r) = owner_repo(repo);
    if !union {
        // Replace: drop requests that aren't in the new set.
        let l = ctx.data_unchecked::<Loaders>();
        let current = one(&l.review_requests, issue.id).await?.unwrap_or_default();
        let mut drop_users = vec![];
        let mut drop_teams = vec![];
        for rr in current.iter() {
            if let Some(u) = rr.user_id
                && let Some(u) = one(&l.users, u).await?
                && !users.iter().any(|x| x.eq_ignore_ascii_case(&u.login))
            {
                drop_users.push(u.login.clone());
            }
            if let Some(t) = rr.team_id
                && let Some(t) = one(&l.teams, t).await?
                && !teams.iter().any(|x| x.eq_ignore_ascii_case(&t.team.slug))
            {
                drop_teams.push(t.team.slug.clone());
            }
        }
        if !drop_users.is_empty() || !drop_teams.is_empty() {
            into_json(
                bgh_pulls::reviewers::remove(
                    st(ctx),
                    user(a),
                    Path((o.clone(), r.clone(), issue.number)),
                    Json(body(
                        json!({"reviewers": drop_users, "team_reviewers": drop_teams}),
                    )?),
                )
                .await,
            )
            .await?;
        }
    }
    if users.is_empty() && teams.is_empty() {
        return Ok(());
    }
    into_json(
        bgh_pulls::reviewers::request(
            st(ctx),
            user(a),
            Path((o, r, issue.number)),
            Json(body(json!({"reviewers": users, "team_reviewers": teams}))?),
        )
        .await,
    )
    .await?;
    Ok(())
}

#[derive(Default)]
pub struct PullMutations;

#[Object]
impl PullMutations {
    /// Create a new pull request.
    pub async fn create_pull_request(
        &self,
        ctx: &Context<'_>,
        input: CreatePullRequestInput,
    ) -> GResult<CreatePullRequestPayload> {
        let a = guard(ctx)?;
        let repo = repo_by_node(ctx, &input.repository_id).await?;
        let mut head = input.head_ref_name.clone();
        if let Some(hid) = &input.head_repository_id {
            let head_repo = repo_by_node(ctx, hid).await?;
            if head_repo.repo.id != repo.repo.id && !head.contains(':') {
                head = format!("{}:{}", head_repo.owner.login, head);
            }
        }
        let (o, r) = owner_repo(&repo);
        let v = into_json(
            bgh_pulls::pulls::create(
                st(ctx),
                user(a),
                Path((o, r)),
                Json(body(json!({
                    "title": input.title,
                    "head": head,
                    "base": input.base_ref_name,
                    "body": input.body,
                    "maintainer_can_modify": input.maintainer_can_modify,
                    "draft": input.draft,
                }))?),
            )
            .await,
        )
        .await?;
        let id = v["id"]
            .as_i64()
            .ok_or_else(|| not_found("pull request not created"))?;
        if let Some(projects) = &input.project_v2_ids {
            super::projects::add_to_projects(ctx, id, projects).await?;
        }
        Ok(CreatePullRequestPayload {
            pull_request: Some(pr(ctx, id).await?),
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Update a pull request.
    pub async fn update_pull_request(
        &self,
        ctx: &Context<'_>,
        input: UpdatePullRequestInput,
    ) -> GResult<UpdatePullRequestPayload> {
        let a = guard(ctx)?;
        let (issue, repo) = pull_by_node(ctx, &input.pull_request_id).await?;
        let mut p = json!({});
        if let Some(t) = &input.title {
            p["title"] = json!(t);
        }
        if let Some(t) = &input.body {
            p["body"] = json!(t);
        }
        if let Some(b) = &input.base_ref_name {
            p["base"] = json!(b);
        }
        if let Some(s) = input.state {
            p["state"] = json!(match s {
                PullRequestUpdateState::Open => "open",
                PullRequestUpdateState::Closed => "closed",
            });
        }
        if let Some(m) = input.maintainer_can_modify {
            p["maintainer_can_modify"] = json!(m);
        }
        if p.as_object().is_some_and(|o| !o.is_empty()) {
            patch_pull(ctx, &issue, &repo, p).await?;
        }
        let mut meta = json!({});
        if let Some(ids) = &input.assignee_ids {
            meta["assignees"] = json!(logins(ctx, ids).await?);
        }
        if let Some(ids) = &input.label_ids {
            meta["labels"] = json!(label_names(ctx, repo.repo.id, ids).await?);
        }
        if let Some(m) = &input.milestone_id {
            meta["milestone"] = json!(milestone_number(ctx, repo.repo.id, m).await?);
        }
        if meta.as_object().is_some_and(|o| !o.is_empty()) {
            patch_issue(ctx, &issue, &repo, meta).await?;
        }
        Ok(UpdatePullRequestPayload {
            pull_request: Some(pr(ctx, issue.id).await?),
            actor: Some(crate::model::Actor::from_user(Arc::new(a.user.clone()))),
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Close a pull request.
    pub async fn close_pull_request(
        &self,
        ctx: &Context<'_>,
        input: ClosePullRequestInput,
    ) -> GResult<ClosePullRequestPayload> {
        let (issue, repo) = pull_by_node(ctx, &input.pull_request_id).await?;
        patch_pull(ctx, &issue, &repo, json!({"state": "closed"})).await?;
        Ok(ClosePullRequestPayload {
            pull_request: Some(pr(ctx, issue.id).await?),
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Reopen a pull request.
    pub async fn reopen_pull_request(
        &self,
        ctx: &Context<'_>,
        input: ReopenPullRequestInput,
    ) -> GResult<ReopenPullRequestPayload> {
        let (issue, repo) = pull_by_node(ctx, &input.pull_request_id).await?;
        patch_pull(ctx, &issue, &repo, json!({"state": "open"})).await?;
        Ok(ReopenPullRequestPayload {
            pull_request: Some(pr(ctx, issue.id).await?),
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Merge a pull request.
    pub async fn merge_pull_request(
        &self,
        ctx: &Context<'_>,
        input: MergePullRequestInput,
    ) -> GResult<MergePullRequestPayload> {
        let a = guard(ctx)?;
        let (issue, repo) = pull_by_node(ctx, &input.pull_request_id).await?;
        let (o, r) = owner_repo(&repo);
        into_json(
            bgh_pulls::merge::merge(
                st(ctx),
                user(a),
                Path((o, r, issue.number)),
                Json(body(json!({
                    "commit_title": input.commit_headline,
                    "commit_message": input.commit_body,
                    "sha": input.expected_head_oid.map(|o| o.0),
                    "merge_method": input.merge_method.map(PullRequestMergeMethod::rest),
                }))?),
            )
            .await,
        )
        .await?;
        Ok(MergePullRequestPayload {
            pull_request: Some(pr(ctx, issue.id).await?),
            actor: Some(crate::model::Actor::from_user(Arc::new(a.user.clone()))),
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Marks a pull request ready for review.
    pub async fn mark_pull_request_ready_for_review(
        &self,
        ctx: &Context<'_>,
        input: MarkPullRequestReadyForReviewInput,
    ) -> GResult<MarkPullRequestReadyForReviewPayload> {
        let a = guard(ctx)?;
        let (issue, repo) = pull_by_node(ctx, &input.pull_request_id).await?;
        let (o, r) = owner_repo(&repo);
        into_json(
            bgh_pulls::web::ready_for_review(st(ctx), user(a), Path((o, r, issue.number))).await,
        )
        .await?;
        Ok(MarkPullRequestReadyForReviewPayload {
            pull_request: Some(pr(ctx, issue.id).await?),
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Converts a pull request to draft.
    pub async fn convert_pull_request_to_draft(
        &self,
        ctx: &Context<'_>,
        input: ConvertPullRequestToDraftInput,
    ) -> GResult<ConvertPullRequestToDraftPayload> {
        let a = guard(ctx)?;
        let (issue, repo) = pull_by_node(ctx, &input.pull_request_id).await?;
        let (o, r) = owner_repo(&repo);
        into_json(
            bgh_pulls::web::convert_to_draft(st(ctx), user(a), Path((o, r, issue.number))).await,
        )
        .await?;
        Ok(ConvertPullRequestToDraftPayload {
            pull_request: Some(pr(ctx, issue.id).await?),
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Adds a review to a Pull Request (pending unless `event` is given).
    pub async fn add_pull_request_review(
        &self,
        ctx: &Context<'_>,
        input: AddPullRequestReviewInput,
    ) -> GResult<AddPullRequestReviewPayload> {
        let a = guard(ctx)?;
        let (issue, repo) = pull_by_node(ctx, &input.pull_request_id).await?;
        let mut comments = vec![];
        for c in input.comments.into_iter().flatten().flatten() {
            comments.push(json!({"path": c.path, "position": c.position, "body": c.body}));
        }
        for t in input.threads.into_iter().flatten().flatten() {
            let side =
                |s: Option<DiffSide>| s.map(|s| if s == DiffSide::Left { "LEFT" } else { "RIGHT" });
            comments.push(json!({
                "path": t.path, "line": t.line, "side": side(t.side),
                "start_line": t.start_line, "start_side": side(t.start_side), "body": t.body,
            }));
        }
        let (o, r) = owner_repo(&repo);
        let v = into_json(
            bgh_pulls::reviews::create(
                st(ctx),
                user(a),
                Path((o, r, issue.number)),
                Json(body(json!({
                    "commit_id": input.commit_oid.map(|c| c.0),
                    "body": input.body,
                    "event": input.event.map(PullRequestReviewEvent::rest),
                    "comments": comments,
                }))?),
            )
            .await,
        )
        .await?;
        let id = v["id"]
            .as_i64()
            .ok_or_else(|| not_found("review not created"))?;
        Ok(AddPullRequestReviewPayload {
            pull_request_review: Some(review_obj(ctx, id).await?),
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Submits a pending pull request review.
    pub async fn submit_pull_request_review(
        &self,
        ctx: &Context<'_>,
        input: SubmitPullRequestReviewInput,
    ) -> GResult<SubmitPullRequestReviewPayload> {
        let a = guard(ctx)?;
        let review = match (&input.pull_request_review_id, &input.pull_request_id) {
            (Some(rid), _) => review_by_node(ctx, rid).await?,
            (None, Some(pid)) => {
                let (issue, _) = pull_by_node(ctx, pid).await?;
                let row: Option<crate::loaders::ReviewRow> = sqlx::query_as(&format!(
                    "SELECT {} FROM pr_reviews WHERE pull_id = $1 AND user_id = $2 AND state = 'PENDING'
                      ORDER BY id DESC LIMIT 1",
                    crate::loaders::ReviewRow::COLUMNS
                ))
                .bind(issue.id)
                .bind(a.user.id)
                .fetch_optional(&gql(ctx).state.db)
                .await
                .map_err(|e| crate::ctx::api_err(e.into()))?;
                row.ok_or_else(|| err("UNPROCESSABLE", "No pending review to submit."))?
            }
            (None, None) => {
                return Err(err(
                    "UNPROCESSABLE",
                    "Either pullRequestId or pullRequestReviewId is required.",
                ));
            }
        };
        let repo = repo_by_id(ctx, review.repo_id).await?;
        let issue = reload_issue(ctx, review.pull_id).await?;
        let (o, r) = owner_repo(&repo);
        into_json(
            bgh_pulls::reviews::submit(
                st(ctx),
                user(a),
                Path((o, r, issue.number, review.id)),
                Json(body(
                    json!({"body": input.body, "event": input.event.rest()}),
                )?),
            )
            .await,
        )
        .await?;
        Ok(SubmitPullRequestReviewPayload {
            pull_request_review: Some(review_obj(ctx, review.id).await?),
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Dismisses an approved or rejected pull request review.
    pub async fn dismiss_pull_request_review(
        &self,
        ctx: &Context<'_>,
        input: DismissPullRequestReviewInput,
    ) -> GResult<DismissPullRequestReviewPayload> {
        let a = guard(ctx)?;
        let review = review_by_node(ctx, &input.pull_request_review_id).await?;
        let repo = repo_by_id(ctx, review.repo_id).await?;
        let issue = reload_issue(ctx, review.pull_id).await?;
        let (o, r) = owner_repo(&repo);
        into_json(
            bgh_pulls::reviews::dismiss(
                st(ctx),
                user(a),
                Path((o, r, issue.number, review.id)),
                Json(body(json!({"message": input.message}))?),
            )
            .await,
        )
        .await?;
        Ok(DismissPullRequestReviewPayload {
            pull_request_review: Some(review_obj(ctx, review.id).await?),
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Set review requests on a pull request.
    pub async fn request_reviews(
        &self,
        ctx: &Context<'_>,
        input: RequestReviewsInput,
    ) -> GResult<RequestReviewsPayload> {
        let a = guard(ctx)?;
        let (issue, repo) = pull_by_node(ctx, &input.pull_request_id).await?;
        let users = logins(ctx, &input.user_ids.clone().unwrap_or_default()).await?;
        let mut teams = vec![];
        let l = ctx.data_unchecked::<Loaders>();
        for t in input.team_ids.clone().unwrap_or_default() {
            let tid = decode(&t, &[NodeType::Team], "a Team")?;
            let team = one(&l.teams, tid)
                .await?
                .ok_or_else(|| not_found("Could not resolve to a Team."))?;
            teams.push(team.team.slug.clone());
        }
        request_reviewers(
            ctx,
            &issue,
            &repo,
            users,
            teams,
            input.union.unwrap_or(false),
        )
        .await?;
        Ok(RequestReviewsPayload {
            pull_request: Some(pr(ctx, issue.id).await?),
            actor: Some(crate::model::Actor::from_user(Arc::new(a.user.clone()))),
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Set review requests on a pull request using login strings.
    pub async fn request_reviews_by_login(
        &self,
        ctx: &Context<'_>,
        input: RequestReviewsByLoginInput,
    ) -> GResult<RequestReviewsByLoginPayload> {
        let (issue, repo) = pull_by_node(ctx, &input.pull_request_id).await?;
        let mut users = input.user_logins.clone().unwrap_or_default();
        users.extend(input.bot_logins.clone().unwrap_or_default());
        let teams: Vec<String> = input
            .team_slugs
            .clone()
            .unwrap_or_default()
            .into_iter()
            .map(|s| s.rsplit('/').next().unwrap_or(&s).to_string())
            .collect();
        request_reviewers(
            ctx,
            &issue,
            &repo,
            users,
            teams,
            input.union.unwrap_or(false),
        )
        .await?;
        Ok(RequestReviewsByLoginPayload {
            pull_request: Some(pr(ctx, issue.id).await?),
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Enable the default auto-merge on a pull request.
    pub async fn enable_pull_request_auto_merge(
        &self,
        ctx: &Context<'_>,
        input: EnablePullRequestAutoMergeInput,
    ) -> GResult<EnablePullRequestAutoMergePayload> {
        let a = guard(ctx)?;
        let (issue, repo) = pull_by_node(ctx, &input.pull_request_id).await?;
        // Like GitHub (and what `gh pr merge --auto` relies on): a base
        // branch with a merge queue adds the PR to the queue instead.
        if super::merge_queue::queue_required(ctx, &issue).await? {
            super::merge_queue::enqueue(
                ctx,
                a,
                &issue,
                &repo,
                false,
                input.expected_head_oid.as_ref(),
            )
            .await?;
            return Ok(EnablePullRequestAutoMergePayload {
                pull_request: Some(pr(ctx, issue.id).await?),
                client_mutation_id: input.client_mutation_id,
            });
        }
        let (o, r) = owner_repo(&repo);
        into_json(
            bgh_pulls::automerge::put(
                st(ctx),
                user(a),
                Path((o, r, issue.number)),
                Json(body(json!({
                    "merge_method": input.merge_method.map(PullRequestMergeMethod::rest),
                    "commit_title": input.commit_headline,
                    "commit_message": input.commit_body,
                }))?),
            )
            .await,
        )
        .await?;
        Ok(EnablePullRequestAutoMergePayload {
            pull_request: Some(pr(ctx, issue.id).await?),
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Disable auto merge on the given pull request.
    pub async fn disable_pull_request_auto_merge(
        &self,
        ctx: &Context<'_>,
        input: DisablePullRequestAutoMergeInput,
    ) -> GResult<DisablePullRequestAutoMergePayload> {
        let a = guard(ctx)?;
        let (issue, repo) = pull_by_node(ctx, &input.pull_request_id).await?;
        let (o, r) = owner_repo(&repo);
        into_json(bgh_pulls::automerge::delete(st(ctx), user(a), Path((o, r, issue.number))).await)
            .await?;
        Ok(DisablePullRequestAutoMergePayload {
            pull_request: Some(pr(ctx, issue.id).await?),
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Marks a review thread as resolved.
    pub async fn resolve_review_thread(
        &self,
        ctx: &Context<'_>,
        input: ResolveReviewThreadInput,
    ) -> GResult<ResolveReviewThreadPayload> {
        let thread = set_thread(ctx, &input.thread_id, true).await?;
        Ok(ResolveReviewThreadPayload {
            thread: Some(thread),
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Marks a review thread as unresolved.
    pub async fn unresolve_review_thread(
        &self,
        ctx: &Context<'_>,
        input: UnresolveReviewThreadInput,
    ) -> GResult<UnresolveReviewThreadPayload> {
        let thread = set_thread(ctx, &input.thread_id, false).await?;
        Ok(UnresolveReviewThreadPayload {
            thread: Some(thread),
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Merge or rebase HEAD from upstream branch into pull request branch.
    pub async fn update_pull_request_branch(
        &self,
        ctx: &Context<'_>,
        input: UpdatePullRequestBranchInput,
    ) -> GResult<UpdatePullRequestBranchPayload> {
        let a = guard(ctx)?;
        let (issue, repo) = pull_by_node(ctx, &input.pull_request_id).await?;
        let (o, r) = owner_repo(&repo);
        into_json(
            bgh_pulls::merge::update_branch(
                st(ctx),
                user(a),
                Path((o, r, issue.number)),
                Json(body(
                    json!({"expected_head_sha": input.expected_head_oid.map(|o| o.0)}),
                )?),
            )
            .await,
        )
        .await?;
        Ok(UpdatePullRequestBranchPayload {
            pull_request: Some(pr(ctx, issue.id).await?),
            client_mutation_id: input.client_mutation_id,
        })
    }
}

async fn set_thread(
    ctx: &Context<'_>,
    id: &ID,
    resolved: bool,
) -> GResult<PullRequestReviewThread> {
    let a = guard(ctx)?;
    let root = decode_thread_id(&id.0).ok_or_else(|| {
        not_found(format!(
            "Could not resolve to a PullRequestReviewThread with the global id of '{}'.",
            id.0
        ))
    })?;
    let (pull_id, repo_id): (i64, i64) =
        sqlx::query_as("SELECT pull_id, repo_id FROM pr_review_comments WHERE id = $1")
            .bind(root)
            .fetch_optional(&gql(ctx).state.db)
            .await
            .map_err(|e| crate::ctx::api_err(e.into()))?
            .ok_or_else(|| not_found("Could not resolve to a PullRequestReviewThread."))?;
    let repo = repo_by_id(ctx, repo_id).await?;
    let issue = reload_issue(ctx, pull_id).await?;
    let (o, r) = owner_repo(&repo);
    let path = Path((o, r, issue.number, root));
    if resolved {
        into_json(bgh_pulls::web::resolve_thread(st(ctx), user(a), path).await).await?;
    } else {
        into_json(bgh_pulls::web::unresolve_thread(st(ctx), user(a), path).await).await?;
    }
    let rows: Vec<crate::loaders::ReviewCommentRow> = sqlx::query_as(&format!(
        "SELECT {} FROM pr_review_comments WHERE pull_id = $1 ORDER BY id",
        crate::loaders::ReviewCommentRow::COLUMNS
    ))
    .bind(pull_id)
    .fetch_all(&gql(ctx).state.db)
    .await
    .map_err(|e| crate::ctx::api_err(e.into()))?;
    threads(&rows)
        .into_iter()
        .find(|t| t.root.id == root)
        .ok_or_else(|| not_found("thread not found"))
}
