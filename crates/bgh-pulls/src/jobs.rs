//! Background jobs: mergeability refresh, push synchronization, auto-merge.

use bgh_core::events::RefUpdate;
use bgh_core::jobs::JobPayload;
use bgh_core::prelude::*;
use serde::{Deserialize, Serialize};

/// Recompute `mergeable` / `rebaseable` / `mergeable_state` and the test
/// merge commit (`refs/pull/{n}/merge`); request CODEOWNERS reviews when
/// `codeowners`; then try auto-merge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Refresh {
    pub pull_id: i64,
    #[serde(default)]
    pub codeowners: bool,
}

impl JobPayload for Refresh {
    const KIND: &'static str = "pulls.refresh";
}

/// Re-read a PR's head and base branches from git and apply changes
/// (synchronize).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncPull {
    pub pull_id: i64,
}

impl JobPayload for SyncPull {
    const KIND: &'static str = "pulls.sync_pull";
}

/// A push landed in `repo_id`: synchronize PRs whose head or base moved.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushSync {
    pub repo_id: i64,
    pub pusher_id: Option<i64>,
    pub updates: Vec<RefUpdate>,
}

impl JobPayload for PushSync {
    const KIND: &'static str = "pulls.push";
}

/// Commit statuses / checks changed for `sha`: refresh the PRs whose head
/// it is (mergeable_state, auto-merge).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChecksChanged {
    pub repo_id: i64,
    pub sha: String,
}

impl JobPayload for ChecksChanged {
    const KIND: &'static str = "pulls.checks_changed";
}

pub async fn refresh(state: AppState, job: Refresh) -> anyhow::Result<()> {
    crate::mergeability::refresh(&state, job.pull_id, job.codeowners)
        .await
        .map_err(|e| anyhow::anyhow!("refresh pull {}: {e:?}", job.pull_id))
}

pub async fn sync_pull(state: AppState, job: SyncPull) -> anyhow::Result<()> {
    crate::synchronize::synchronize(&state, job.pull_id, None)
        .await
        .map_err(|e| anyhow::anyhow!("sync pull {}: {e:?}", job.pull_id))
}

pub async fn push(state: AppState, job: PushSync) -> anyhow::Result<()> {
    crate::synchronize::on_push(&state, &job)
        .await
        .map_err(|e| anyhow::anyhow!("push sync repo {}: {e:?}", job.repo_id))
}

pub async fn checks_changed(state: AppState, job: ChecksChanged) -> anyhow::Result<()> {
    let ids: Vec<i64> = sqlx::query_scalar(
        "SELECT p.issue_id FROM pull_requests p JOIN issues i ON i.id = p.issue_id
          WHERE (p.repo_id = $1 OR p.head_repo_id = $1) AND p.head_sha = $2 AND i.state = 'open'",
    )
    .bind(job.repo_id)
    .bind(&job.sha)
    .fetch_all(&state.db)
    .await?;
    // The PRs' `checks` (and possibly `mergeableState`) changed.
    let mut tx = Tx::begin(&state)
        .await
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;
    tx.sync_models(SyncModel::Issue, &ids, SyncAction::Update)
        .await
        .map_err(|e| anyhow::anyhow!("sync pulls: {e:?}"))?;
    tx.commit().await.map_err(|e| anyhow::anyhow!("{e:?}"))?;
    for id in ids {
        crate::mergeability::refresh(&state, id, false)
            .await
            .map_err(|e| anyhow::anyhow!("refresh pull {id}: {e:?}"))?;
    }
    Ok(())
}

/// Event listener: turn pushes into durable `pulls.push` jobs.
pub async fn on_event(state: AppState, event: std::sync::Arc<Event>) -> anyhow::Result<()> {
    if let Event::Push(p) = &*event {
        let updates: Vec<RefUpdate> = p
            .updates
            .iter()
            .filter(|u| u.branch().is_some())
            .cloned()
            .collect();
        // Only repositories involved in open pull requests need work.
        let involved: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM pull_requests p JOIN issues i ON i.id = p.issue_id
                             WHERE (p.repo_id = $1 OR p.head_repo_id = $1) AND i.state = 'open')",
        )
        .bind(p.repo_id)
        .fetch_one(&state.db)
        .await?;
        if involved && !updates.is_empty() {
            bgh_core::jobs::enqueue_job(
                &state.db,
                &PushSync {
                    repo_id: p.repo_id,
                    pusher_id: p.pusher_id,
                    updates,
                },
            )
            .await?;
        }
    }
    Ok(())
}
