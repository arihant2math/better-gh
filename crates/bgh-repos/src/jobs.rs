//! Background jobs owned by bgh-repos.

use bgh_core::events::{Event, PushEvent, RefUpdate};
use bgh_core::jobs::JobPayload;
use bgh_core::prelude::*;
use bgh_git::write;
use serde::{Deserialize, Serialize};

/// Enqueued after a successful `git push`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostReceive {
    pub repo_id: i64,
    pub pusher_id: Option<i64>,
    pub updates: Vec<RefUpdate>,
}

impl JobPayload for PostReceive {
    const KIND: &'static str = "repos.post_receive";
}

/// Remove a deleted repository's storage. Forks borrowing its objects
/// (`objects/info/alternates`) are made self-contained first, so deleting a
/// fork network's source never breaks its forks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteStorage {
    pub repo_id: i64,
    /// Direct forks at deletion time (their `parent_id` is nulled by then).
    #[serde(default)]
    pub forks: Vec<i64>,
}

impl JobPayload for DeleteStorage {
    const KIND: &'static str = "repos.delete_storage";
}

/// Pick the default branch after a push into a repository whose configured
/// default branch doesn't exist (typically the first push).
fn choose_default(current: &str, branches: &[String], pushed: &[RefUpdate]) -> Option<String> {
    if branches.iter().any(|b| b == current) {
        return None;
    }
    let pushed: Vec<&str> = pushed
        .iter()
        .filter(|u| !u.is_delete())
        .filter_map(|u| u.branch())
        .collect();
    for preferred in ["main", "master"] {
        if pushed.contains(&preferred) && branches.iter().any(|b| b == preferred) {
            return Some(preferred.to_string());
        }
    }
    pushed
        .iter()
        .find(|b| branches.iter().any(|x| x == *b))
        .map(|b| b.to_string())
        .or_else(|| branches.first().cloned())
}

/// Post-receive: update `pushed_at`/size, initialize the default branch on
/// first push, record a sync action and emit [`Event::Push`].
pub async fn post_receive(state: AppState, job: PostReceive) -> anyhow::Result<()> {
    let Some(repo) = db::Repository::find(&state.db, job.repo_id).await? else {
        return Ok(()); // deleted meanwhile
    };
    let store = crate::store(&state);
    let branches: Vec<String> = store
        .read(repo.id, |r| {
            Ok(r.branches()?
                .iter()
                .map(|b| b.short_name().to_string())
                .collect())
        })
        .await?;
    let new_default = choose_default(&repo.default_branch, &branches, &job.updates);
    if let Some(branch) = &new_default {
        write::set_head(&store, repo.id, branch).await?;
    }
    let size = store.disk_size_kb(repo.id).await?;
    let default_moved = job
        .updates
        .iter()
        .any(|u| u.branch() == Some(new_default.as_deref().unwrap_or(&repo.default_branch)));

    let mut tx = Tx::begin(&state).await?;
    let repo: db::Repository = sqlx::query_as(&format!(
        "UPDATE repositories
            SET pushed_at = now(), updated_at = now(), size = $2,
                default_branch = coalesce($3, default_branch)
          WHERE id = $1 RETURNING {}",
        db::Repository::COLUMNS
    ))
    .bind(repo.id)
    .bind(size)
    .bind(new_default.as_deref())
    .fetch_one(&mut *tx)
    .await?;
    tx.sync_model(SyncModel::Repo, repo.id, SyncAction::Update)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    if default_moved {
        crate::stats::enqueue_languages(&mut tx, repo.id).await?;
    }
    tx.emit(Event::Push(PushEvent {
        repo_id: repo.id,
        pusher_id: job.pusher_id,
        updates: job.updates,
    }));
    tx.commit().await?;
    Ok(())
}

pub async fn delete_storage(state: AppState, job: DeleteStorage) -> anyhow::Result<()> {
    // Never delete storage of a repository that (still/again) exists.
    if db::Repository::find(&state.db, job.repo_id)
        .await?
        .is_some()
    {
        return Ok(());
    }
    let store = crate::store(&state);
    for fork in &job.forks {
        // Direct forks borrow from the deleted repository (`clone --shared`).
        if let Ok(git) = store.cli(*fork) {
            git.dissociate().await?;
        }
    }
    store.delete(job.repo_id).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn upd(branch: &str) -> RefUpdate {
        RefUpdate {
            old: bgh_core::events::ZERO_SHA.into(),
            new: "a".repeat(40),
            refname: format!("refs/heads/{branch}"),
        }
    }

    #[test]
    fn default_branch_selection() {
        let b = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(choose_default("main", &b(&["main"]), &[upd("main")]), None);
        assert_eq!(
            choose_default("main", &b(&["dev", "master"]), &[upd("dev"), upd("master")]),
            Some("master".into())
        );
        assert_eq!(
            choose_default("main", &b(&["feature"]), &[upd("feature")]),
            Some("feature".into())
        );
        assert_eq!(choose_default("main", &b(&[]), &[]), None);
    }
}
