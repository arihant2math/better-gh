//! Merge queue mutations (`bgh_pulls::merge_queue`): `enqueuePullRequest`,
//! `dequeuePullRequest`, and the enqueue path of
//! `enablePullRequestAutoMerge` ([`enqueue`]).

use std::sync::Arc;

use async_graphql::{Context, ID, InputObject, Object, SimpleObject};
use axum::extract::State;
use bgh_core::auth::{AuthContext, RequireUser};
use bgh_core::node_id::NodeType;
use bgh_core::prelude::*;
use bgh_pulls::merge_queue::{self as mq, web};
use serde_json::json;

use super::{body, decode, guard, into_json, issue_by_node, owner_repo, repo_by_id};
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
    if let Some(oid) = expected_head_oid {
        let l = ctx.data_unchecked::<Loaders>();
        let head = one(&l.pulls, issue.id).await?.map(|p| p.head_sha.clone());
        if head.as_deref() != Some(oid.0.as_str()) {
            return Err(err(
                "UNPROCESSABLE",
                "Head branch was modified. Review and try again.",
            ));
        }
    }
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
        let pull_id: Option<i64> = sqlx::query_scalar(
            "SELECT pull_id FROM merge_queue_entries
              WHERE id = $1 AND state IN ('queued', 'awaiting_checks', 'mergeable')",
        )
        .bind(eid)
        .fetch_optional(&g.state.db)
        .await
        .gql()?;
        let pull_id = pull_id.ok_or_else(missing)?;
        let entry = mq::entry_for_pull(&g.state.db, pull_id)
            .await
            .gql()?
            .filter(|e| e.id == eid)
            .ok_or_else(missing)?;
        let repo = repo_by_id(ctx, entry.repo_id).await?;
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
