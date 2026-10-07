//! Merge queue mutations (`bgh_pulls::merge_queue`): `enqueuePullRequest`,
//! `dequeuePullRequest`, and the enqueue path of
//! `enablePullRequestAutoMerge` ([`enqueue_if_ready`]).

use std::sync::Arc;

use async_graphql::{Context, ID, InputObject, Object, SimpleObject};
use axum::extract::State;
use bgh_core::auth::{AuthContext, RequireUser};
use bgh_core::node_id::NodeType;
use bgh_core::prelude::*;
use bgh_pulls::merge_queue::{self as mq, web};
use serde_json::json;

use super::{body, decode, guard, into_json, issue_by_node, owner_repo};
use crate::ctx::{GResult, OrGql, err, gql, not_found};
use crate::loaders::{Loaders, RepoRow, one};
use crate::model::merge_queue::MergeQueueEntry;
use crate::scalars::GitObjectID;

#[derive(InputObject)]
pub struct EnqueuePullRequestInput {
    /// The ID of the pull request to enqueue.
    pub pull_request_id: ID,
    /// Add the pull request to the front of the queue.
    pub jump: Option<bool>,
    /// The expected head OID of the pull request.
    pub expected_head_oid: Option<GitObjectID>,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
pub struct EnqueuePullRequestPayload {
    /// The merge queue entry for the enqueued pull request.
    pub merge_queue_entry: Option<MergeQueueEntry>,
    pub client_mutation_id: Option<String>,
}

#[derive(InputObject)]
pub struct DequeuePullRequestInput {
    /// The ID of the queue entry to remove.
    pub id: ID,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
pub struct DequeuePullRequestPayload {
    /// The merge queue entry of the dequeued pull request.
    pub merge_queue_entry: Option<MergeQueueEntry>,
    pub client_mutation_id: Option<String>,
}

/// Add the pull request `issue` (of `repo`) to its base branch's merge
/// queue as `a` (same path as `PUT /_bgh/.../queue`); returns its entry.
pub async fn enqueue(
    ctx: &Context<'_>,
    a: &AuthContext,
    issue: &db::Issue,
    repo: &RepoRow,
    jump: bool,
    expected_head_oid: Option<&GitObjectID>,
) -> GResult<MergeQueueEntry> {
    let g = gql(ctx);
    check_head(ctx, issue, expected_head_oid).await?;
    let (o, r) = owner_repo(repo);
    into_json(
        web::put(
            State(g.state.clone()),
            RequireUser(a.clone()),
            Path((o, r, issue.number)),
            Json(body(json!({ "jump": jump }))?),
        )
        .await,
    )
    .await?;
    mq::entry_for_pull(&g.state.db, issue.id)
        .await
        .gql()?
        .map(|e| MergeQueueEntry(Arc::new(e)))
        .ok_or_else(|| err("UNPROCESSABLE", "Pull request left the merge queue"))
}

/// 422 unless `issue`'s head is `expected` (when given).
async fn check_head(
    ctx: &Context<'_>,
    issue: &db::Issue,
    expected: Option<&GitObjectID>,
) -> GResult<()> {
    let Some(oid) = expected else {
        return Ok(());
    };
    let l = ctx.data_unchecked::<Loaders>();
    let head = one(&l.pulls, issue.id).await?.map(|p| p.head_sha.clone());
    if head.as_deref() != Some(oid.0.as_str()) {
        return Err(err(
            "UNPROCESSABLE",
            "Head branch was modified. Review and try again.",
        ));
    }
    Ok(())
}

/// `enablePullRequestAutoMerge` on a queue branch: add the PR to the queue
/// now when only status checks (which run on the merge group) are
/// missing; `false` when other requirements (e.g. reviews) still block it,
/// in which case the caller enables auto-merge and
/// `bgh_pulls::automerge::try_merge` enqueues it once they pass.
pub async fn enqueue_if_ready(
    ctx: &Context<'_>,
    a: &AuthContext,
    issue: &db::Issue,
    repo: &RepoRow,
    expected_head_oid: Option<&GitObjectID>,
) -> GResult<bool> {
    let g = gql(ctx);
    check_head(ctx, issue, expected_head_oid).await?;
    let (o, r) = owner_repo(repo);
    let (access, pull) = bgh_pulls::pulls::load_pull(&g.state, Some(a), &o, &r, issue.number)
        .await
        .gql()?;
    match mq::enqueue(&g.state, &access, &pull, &a.user, false).await {
        Ok(_) => Ok(true),
        // `merge_queue::enqueue`'s requirements message.
        Err(ApiError::Validation { message, .. })
            if message.starts_with("Pull request is not ready for the merge queue") =>
        {
            Ok(false)
        }
        Err(e) => Err(crate::ctx::api_err(e)),
    }
}

/// Whether `issue`'s base branch requires the merge queue.
pub async fn queue_required(ctx: &Context<'_>, issue: &db::Issue) -> GResult<bool> {
    let l = ctx.data_unchecked::<Loaders>();
    let Some(p) = one(&l.pulls, issue.id).await? else {
        return Ok(false);
    };
    Ok(
        mq::config_for(&gql(ctx).state.db, issue.repo_id, &p.base_ref)
            .await
            .gql()?
            .is_some(),
    )
}

#[derive(Default)]
pub struct MergeQueueMutations;

#[Object]
impl MergeQueueMutations {
    /// Add a pull request to the merge queue.
    pub async fn enqueue_pull_request(
        &self,
        ctx: &Context<'_>,
        input: EnqueuePullRequestInput,
    ) -> GResult<EnqueuePullRequestPayload> {
        let a = guard(ctx)?;
        let (issue, repo) = issue_by_node(ctx, &input.pull_request_id).await?;
        if !issue.is_pull_request {
            return Err(not_found(format!(
                "Could not resolve to a PullRequest with the global id of '{}'.",
                input.pull_request_id.0
            )));
        }
        let entry = enqueue(
            ctx,
            a,
            &issue,
            &repo,
            input.jump.unwrap_or(false),
            input.expected_head_oid.as_ref(),
        )
        .await?;
        Ok(EnqueuePullRequestPayload {
            merge_queue_entry: Some(entry),
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Remove a pull request from the merge queue.
    pub async fn dequeue_pull_request(
        &self,
        ctx: &Context<'_>,
        input: DequeuePullRequestInput,
    ) -> GResult<DequeuePullRequestPayload> {
        let a = guard(ctx)?;
        let missing = || {
            not_found(format!(
                "Could not resolve to a MergeQueueEntry with the global id of '{}'.",
                input.id.0
            ))
        };
        let eid = decode(&input.id, &[NodeType::MergeQueueEntry], "a MergeQueueEntry")?;
        let g = gql(ctx);
        let row: Option<(i64, i64)> = sqlx::query_as(
            "SELECT repo_id, pull_id FROM merge_queue_entries
              WHERE id = $1 AND state IN ('queued', 'awaiting_checks', 'mergeable')",
        )
        .bind(eid)
        .fetch_optional(&g.state.db)
        .await
        .gql()?;
        let (repo_id, pull_id) = row.ok_or_else(missing)?;
        // An entry of a repository the viewer can't read doesn't exist.
        let l = ctx.data_unchecked::<Loaders>();
        let repo = one(&l.repos, repo_id)
            .await?
            .filter(|r| r.readable())
            .ok_or_else(missing)?;
        let entry = mq::entry_for_pull(&g.state.db, pull_id)
            .await
            .gql()?
            .filter(|e| e.id == eid)
            .ok_or_else(missing)?;
        let (o, r) = owner_repo(&repo);
        into_json(
            web::delete(
                State(g.state.clone()),
                RequireUser(a.clone()),
                Path((o, r, entry.number)),
            )
            .await,
        )
        .await?;
        // The entry as it was when removed.
        Ok(DequeuePullRequestPayload {
            merge_queue_entry: Some(MergeQueueEntry(Arc::new(entry))),
            client_mutation_id: input.client_mutation_id,
        })
    }
}
