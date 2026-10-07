//! Merge queues (P39): `Repository.mergeQueue`, `MergeQueue`,
//! `MergeQueueEntry`, `MergeQueueConfiguration` and the merge queue fields
//! of `PullRequest`. Reads go through `bgh_pulls::merge_queue`.

use std::sync::Arc;

use async_graphql::{Context, Enum, ID, Object};
use bgh_core::node_id::{self, NodeType};
use bgh_pulls::merge_queue::{self as mq, Entry, GroupingStrategy, QueueConfig};

use super::Actor;
use super::actor;
use super::enums::PullRequestMergeMethod;
use super::git::{self, Commit};
use super::nid;
use super::pull::{PullRequest, from_issues};
use super::repo::{self, Repository};
use crate::conn::{ConnArgs, Page, connection};
use crate::ctx::{GResult, OrGql, gql};
use crate::loaders::{Loaders, one};
use crate::scalars::{DateTime, URI, dt};

/// The possible states for a merge queue entry.
#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug)]
pub enum MergeQueueEntryState {
    /// The entry is currently waiting for checks to pass.
    AwaitingChecks,
    /// The entry is currently locked.
    Locked,
    /// The entry is currently mergeable.
    Mergeable,
    /// The entry is currently queued.
    Queued,
    /// The entry is currently unmergeable.
    Unmergeable,
}

impl MergeQueueEntryState {
    fn from_db(s: &str) -> Self {
        match s {
            "awaiting_checks" => Self::AwaitingChecks,
            "mergeable" | "merged" => Self::Mergeable,
            "unmergeable" => Self::Unmergeable,
            _ => Self::Queued,
        }
    }
}

/// The possible merging strategies for a merge queue.
#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug)]
pub enum MergeQueueMergingStrategy {
    /// Entries only allowed to merge if they are passing.
    #[graphql(name = "ALLGREEN")]
    AllGreen,
    /// Failing Entires are allowed to merge if they are with a passing entry.
    #[graphql(name = "HEADGREEN")]
    HeadGreen,
}

/// The queue of one branch (`repo` is readable by the viewer).
#[derive(Clone)]
pub struct MergeQueue {
    repo: Repository,
    branch: String,
    config: Arc<QueueConfig>,
}

impl MergeQueue {
    /// `repo`'s `branch` queue; `None` when no merge queue rule targets it.
    pub async fn load(ctx: &Context<'_>, repo: Repository, branch: &str) -> GResult<Option<Self>> {
        let l = ctx.data_unchecked::<Loaders>();
        let config = one(&l.queue_configs, (repo.rid(), branch.to_string())).await?;
        Ok(config.map(|config| Self {
            repo,
            branch: branch.to_string(),
            config,
        }))
    }

    fn path(&self) -> String {
        format!("/{}/queue/{}", self.repo.row().full_name(), self.branch)
    }
}

#[Object]
impl MergeQueue {
    pub async fn id(&self) -> ID {
        ID(node_id::encode_str(
            NodeType::MergeQueue,
            &format!("{}:{}", self.repo.rid(), self.branch),
        ))
    }
    /// The configuration for this merge queue.
    pub async fn configuration(&self) -> Option<MergeQueueConfiguration> {
        Some(MergeQueueConfiguration(self.config.clone()))
    }
    /// The entries in the queue.
    pub async fn entries(
        &self,
        ctx: &Context<'_>,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
    ) -> GResult<MergeQueueEntryConnection> {
        let rows = mq::entries_for(&gql(ctx).state.db, self.repo.rid(), &self.branch)
            .await
            .gql()?;
        let items = rows
            .into_iter()
            .map(|e| MergeQueueEntry(Arc::new(e)))
            .collect();
        Ok(Page::from_vec(items, &ConnArgs::new(first, last, after, before))?.into())
    }
    /// The estimated time in seconds until a newly added entry would be
    /// merged (not estimated yet).
    pub async fn next_entry_estimated_time_to_merge(&self) -> Option<i32> {
        None
    }
    /// The repository this merge queue belongs to.
    pub async fn repository(&self) -> Option<Repository> {
        Some(self.repo.clone())
    }
    /// The HTTP path for this merge queue.
    pub async fn resource_path(&self) -> URI {
        URI(self.path())
    }
    /// The HTTP URL for this merge queue.
    pub async fn url(&self, ctx: &Context<'_>) -> URI {
        URI(gql(ctx).state.urls.html(&self.path()))
    }
}

/// Configuration for a MergeQueue.
#[derive(Clone)]
pub struct MergeQueueConfiguration(Arc<QueueConfig>);

fn int(v: i64) -> Option<i32> {
    i32::try_from(v).ok()
}

#[Object]
impl MergeQueueConfiguration {
    /// The amount of time in minutes to wait for a check response before
    /// considering it a failure.
    pub async fn check_response_timeout(&self) -> Option<i32> {
        int(self.0.check_response_timeout_minutes)
    }
    /// The maximum number of entries to build at once.
    pub async fn maximum_entries_to_build(&self) -> Option<i32> {
        int(self.0.max_entries_to_build)
    }
    /// The maximum number of entries to merge at once.
    pub async fn maximum_entries_to_merge(&self) -> Option<i32> {
        int(self.0.max_entries_to_merge)
    }
    /// The merge method to use for this queue.
    pub async fn merge_method(&self) -> Option<PullRequestMergeMethod> {
        Some(PullRequestMergeMethod::from_rest(
            self.0.merge_method.as_str(),
        ))
    }
    /// The strategy to use when merging entries.
    pub async fn merging_strategy(&self) -> Option<MergeQueueMergingStrategy> {
        Some(match self.0.grouping_strategy {
            GroupingStrategy::AllGreen => MergeQueueMergingStrategy::AllGreen,
            GroupingStrategy::HeadGreen => MergeQueueMergingStrategy::HeadGreen,
        })
    }
    /// The minimum number of entries required to merge at once.
    pub async fn minimum_entries_to_merge(&self) -> Option<i32> {
        int(self.0.min_entries_to_merge)
    }
    /// The amount of time in minutes to wait before ignoring the minumum
    /// number of entries in the queue requirement and merging a collection
    /// of entries.
    pub async fn minimum_entries_to_merge_wait_time(&self) -> Option<i32> {
        int(self.0.min_entries_to_merge_wait_minutes)
    }
}

/// Entries in a MergeQueue.
#[derive(Clone)]
pub struct MergeQueueEntry(pub Arc<Entry>);

connection!(
    MergeQueueEntryConnection,
    MergeQueueEntryEdge,
    MergeQueueEntry
);

impl MergeQueueEntry {
    async fn repo(&self, ctx: &Context<'_>) -> GResult<Repository> {
        repo::load_unchecked(ctx, self.0.repo_id).await
    }
}

#[Object]
impl MergeQueueEntry {
    pub async fn id(&self) -> ID {
        nid(NodeType::MergeQueueEntry, self.0.id)
    }
    /// The base commit for this entry (the tip of the queue's branch).
    pub async fn base_commit(&self, ctx: &Context<'_>) -> GResult<Option<Commit>> {
        let rev = format!("refs/heads/{}", self.0.base_ref);
        git::commit(ctx, self.repo(ctx).await?, &rev).await
    }
    /// The date and time this entry was added to the merge queue.
    pub async fn enqueued_at(&self) -> DateTime {
        dt(self.0.enqueued_at)
    }
    /// The actor that enqueued this entry (the repository owner when that
    /// account is gone).
    pub async fn enqueuer(&self, ctx: &Context<'_>) -> GResult<Actor> {
        if let Some(a) = actor::actor(ctx, self.0.enqueuer_id).await? {
            return Ok(a);
        }
        Ok(Actor::from_user(Arc::new(
            self.repo(ctx).await?.row().owner.clone(),
        )))
    }
    /// The estimated time in seconds until this entry will be merged (not
    /// estimated yet).
    pub async fn estimated_time_to_merge(&self) -> Option<i32> {
        None
    }
    /// The head commit for this entry (its merge group commit once built).
    pub async fn head_commit(&self, ctx: &Context<'_>) -> GResult<Option<Commit>> {
        let sha = self.0.group_head_sha.as_deref().unwrap_or(&self.0.head_sha);
        git::commit(ctx, self.repo(ctx).await?, sha).await
    }
    /// Whether this pull request should jump the queue.
    pub async fn jump(&self) -> bool {
        self.0.jump
    }
    /// The merge queue that this entry belongs to.
    pub async fn merge_queue(&self, ctx: &Context<'_>) -> GResult<Option<MergeQueue>> {
        MergeQueue::load(ctx, self.repo(ctx).await?, &self.0.base_ref).await
    }
    /// The position of this entry in the queue.
    pub async fn position(&self) -> i32 {
        int(self.0.position).unwrap_or(0)
    }
    /// The pull request that will be added to a merge group.
    pub async fn pull_request(&self, ctx: &Context<'_>) -> GResult<Option<PullRequest>> {
        let l = ctx.data_unchecked::<Loaders>();
        let Some(i) = one(&l.issues, self.0.pull_id).await? else {
            return Ok(None);
        };
        Ok(from_issues(ctx, vec![(*i).clone()])
            .await?
            .into_iter()
            .next())
    }
    /// Does this pull request need to be deployed on its own.
    pub async fn solo(&self) -> bool {
        false
    }
    /// The state of this entry in the queue.
    pub async fn state(&self) -> MergeQueueEntryState {
        MergeQueueEntryState::from_db(&self.0.state)
    }
}

/// The active entry with database id `id`, if the viewer can read its
/// repository.
pub async fn entry_by_id(ctx: &Context<'_>, id: i64) -> GResult<Option<MergeQueueEntry>> {
    let g = gql(ctx);
    let pull: Option<(i64, i64)> = sqlx::query_as(
        "SELECT repo_id, pull_id FROM merge_queue_entries
          WHERE id = $1 AND state IN ('queued', 'awaiting_checks', 'mergeable')",
    )
    .bind(id)
    .fetch_optional(&g.state.db)
    .await
    .gql()?;
    let Some((repo_id, pull_id)) = pull else {
        return Ok(None);
    };
    if repo::load(ctx, repo_id).await?.is_none() {
        return Ok(None);
    }
    let l = ctx.data_unchecked::<Loaders>();
    Ok(one(&l.queue_entries, pull_id)
        .await?
        .filter(|e| e.id == id)
        .map(MergeQueueEntry))
}

/// The queue for a `MergeQueue` node key (`"{repo_id}:{branch}"`).
pub async fn queue_by_key(ctx: &Context<'_>, key: &str) -> GResult<Option<MergeQueue>> {
    let Some((repo_id, branch)) = key.split_once(':') else {
        return Ok(None);
    };
    let Some(repo) = repo::load(ctx, repo_id.parse().unwrap_or(0)).await? else {
        return Ok(None);
    };
    MergeQueue::load(ctx, repo, branch).await
}

/// `PullRequest` merge queue fields.
#[derive(Clone)]
pub struct PullMergeQueue {
    pub pull_id: i64,
    pub repo_id: i64,
    pub base_ref: String,
}

impl PullMergeQueue {
    async fn entry(&self, ctx: &Context<'_>) -> GResult<Option<Arc<Entry>>> {
        let l = ctx.data_unchecked::<Loaders>();
        one(&l.queue_entries, self.pull_id).await
    }
}

#[Object]
impl PullMergeQueue {
    /// This pull request has been added to the merge queue, but the merge
    /// queue has not yet merged it.
    pub async fn is_in_merge_queue(&self, ctx: &Context<'_>) -> GResult<bool> {
        Ok(self.entry(ctx).await?.is_some())
    }
    /// Whether the pull request's base ref has a merge queue enabled.
    pub async fn is_merge_queue_enabled(&self, ctx: &Context<'_>) -> GResult<bool> {
        let l = ctx.data_unchecked::<Loaders>();
        Ok(one(&l.queue_configs, (self.repo_id, self.base_ref.clone()))
            .await?
            .is_some())
    }
    /// The merge queue for the pull request's base branch.
    pub async fn merge_queue(&self, ctx: &Context<'_>) -> GResult<Option<MergeQueue>> {
        let repo = repo::load_unchecked(ctx, self.repo_id).await?;
        MergeQueue::load(ctx, repo, &self.base_ref).await
    }
    /// The merge queue entry of the pull request in the base branch's merge
    /// queue.
    pub async fn merge_queue_entry(&self, ctx: &Context<'_>) -> GResult<Option<MergeQueueEntry>> {
        Ok(self.entry(ctx).await?.map(MergeQueueEntry))
    }
}
