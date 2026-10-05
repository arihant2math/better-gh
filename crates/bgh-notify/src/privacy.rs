//! Notification privacy: threads of repositories their holder can no longer
//! read are deleted (and synced as deletes), so private titles stop reaching
//! removed collaborators, ex-members and the like.
//!
//! The `notify.access` listener prunes on every event that can take read
//! access away (collaborator removal/downgrade, org or team membership
//! removal, team repository removal or team deletion, org base-permission
//! changes, a repository made private or transferred). The retention service
//! ([`sweep`]) is the backstop for anything an event missed. Readability is
//! the SQL function `bgh_can_read_repo` (migrations/3300), the same check
//! the `notification` sync shape and `fanout::retitle` use.

use std::sync::Arc;

use bgh_core::events::Event;
use bgh_core::prelude::*;
use bgh_core::sync;

/// Which threads to recheck: the conjunction of the set fields (all `None`
/// = every thread in a non-public repository).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Target {
    pub repo_id: Option<i64>,
    /// Repositories owned by this organization.
    pub org_id: Option<i64>,
    pub user_id: Option<i64>,
}

/// Rows deleted per transaction.
const BATCH: i64 = 1000;

/// Delete the threads selected by `target` whose holder can't read the
/// repository any more, recording a sync delete for each. Returns how many
/// were deleted.
pub async fn prune(state: &AppState, target: Target) -> ApiResult<u64> {
    let mut total = 0;
    loop {
        let mut tx = Tx::begin(state).await?;
        let gone: Vec<(i64, i64, bool)> = sqlx::query_as(
            "DELETE FROM notifications WHERE id IN (
                 SELECT n.id FROM notifications n JOIN repositories r ON r.id = n.repo_id
                  WHERE r.visibility <> 'public'
                    AND ($1::bigint IS NULL OR n.repo_id = $1)
                    AND ($2::bigint IS NULL OR r.owner_id = $2)
                    AND ($3::bigint IS NULL OR n.user_id = $3)
                    AND NOT bgh_can_read_repo(n.user_id, n.repo_id)
                  LIMIT $4)
             RETURNING id, user_id, done",
        )
        .bind(target.repo_id)
        .bind(target.org_id)
        .bind(target.user_id)
        .bind(BATCH)
        .fetch_all(&mut *tx)
        .await?;
        for (id, user_id, done) in &gone {
            // Done threads are already gone from the client's store.
            if !done {
                tx.sync_delete(&sync::user_scope(*user_id), SyncModel::Notification, *id)
                    .await?;
            }
        }
        tx.commit().await?;
        total += gone.len() as u64;
        if (gone.len() as i64) < BATCH {
            return Ok(total);
        }
    }
}

/// Recheck every thread in a non-public repository.
pub async fn sweep(state: &AppState) -> ApiResult<u64> {
    prune(state, Target::default()).await
}

/// The threads an event may have made unreadable.
pub fn target(event: &Event) -> Option<Target> {
    let t = |repo_id, org_id, user_id| {
        Some(Target {
            repo_id,
            org_id,
            user_id,
        })
    };
    match event {
        Event::AccessChanged {
            repo_id,
            org_id,
            user_id,
        } => t(*repo_id, *org_id, *user_id),
        Event::OrgMemberRemoved {
            org_id, user_id, ..
        }
        | Event::TeamMemberRemoved {
            org_id, user_id, ..
        } => t(None, Some(*org_id), Some(*user_id)),
        Event::TeamRepoRemoved { repo_id, .. }
        | Event::RepositoryPrivatized { repo_id, .. }
        | Event::RepositoryTransferred { repo_id, .. } => t(Some(*repo_id), None, None),
        Event::TeamDeleted { org_id, .. }
        | Event::TeamEdited { org_id, .. }
        | Event::OrganizationChanged { org_id, .. } => t(None, Some(*org_id), None),
        // e.g. site admin revoked
        Event::UserAccountChanged { user_id, .. } => t(None, None, Some(*user_id)),
        _ => None,
    }
}

/// Event listener (`notify.access`).
pub async fn on_event(state: AppState, event: Arc<Event>) -> anyhow::Result<()> {
    let Some(target) = target(&event) else {
        return Ok(());
    };
    let n = prune(&state, target)
        .await
        .map_err(|e| anyhow::anyhow!("pruning notifications for {}: {e:?}", event.name()))?;
    if n > 0 {
        tracing::info!(
            event = event.name(),
            deleted = n,
            "pruned unreadable notifications"
        );
    }
    Ok(())
}
