//! Domain events → webhook deliveries.
//!
//! The `notify.webhooks` listener first asks the database whether any
//! active hook (repository, owning organization, or site-wide) subscribes
//! to one of the webhook events the domain event maps to; only then are
//! payloads built ([`crate::payloads::for_event`]). Each matching hook gets
//! a `webhook_deliveries` row (exact body stored) and a
//! `notify.deliver_webhook` job, all in one transaction.
//!
//! Domain events are delivered at least once, so deliveries created for an
//! outbox event carry `(event_id, event_seq)` under a unique index per hook:
//! a redelivered event creates no second delivery.

use std::sync::Arc;

use bgh_core::events::{self, Event};
use bgh_core::prelude::*;
use serde_json::Value;

use super::deliver::DeliverWebhook;
use super::{HookRow, Owner, hook_json};
use crate::payloads;

/// Insert a pending delivery for `hook` and enqueue its job.
pub async fn queue_delivery(
    tx: &mut Tx,
    hook: &HookRow,
    event: &str,
    action: Option<&str>,
    repo_id: Option<i64>,
    payload: &Value,
    redelivery: bool,
) -> ApiResult<i64> {
    let id = insert_delivery(tx, hook, event, action, repo_id, payload, redelivery, None).await?;
    Ok(id.expect("unkeyed deliveries always insert"))
}

/// [`queue_delivery`] keyed by the outbox event `(event_id, seq)` that
/// produced it; `None` (nothing queued) if that key was already delivered.
#[allow(clippy::too_many_arguments)]
async fn insert_delivery(
    tx: &mut Tx,
    hook: &HookRow,
    event: &str,
    action: Option<&str>,
    repo_id: Option<i64>,
    payload: &Value,
    redelivery: bool,
    key: Option<(i64, i32)>,
) -> ApiResult<Option<i64>> {
    let raw = serde_json::to_string(payload)?;
    let (event_id, event_seq) = key.unzip();
    let id: Option<i64> = sqlx::query_scalar(
        "INSERT INTO webhook_deliveries
                (hook_id, guid, event, action, repo_id, redelivery, status, url,
                 payload_raw, content_type, event_id, event_seq)
         VALUES ($1, $2, $3, $4, $5, $6, 'pending', $7, $8, $9, $10, $11)
         ON CONFLICT (hook_id, event_id, event_seq) WHERE event_id IS NOT NULL DO NOTHING
         RETURNING id",
    )
    .bind(hook.id)
    .bind(uuid::Uuid::new_v4())
    .bind(event)
    .bind(action)
    .bind(repo_id)
    .bind(redelivery)
    .bind(&hook.url)
    .bind(&raw)
    .bind(&hook.content_type)
    .bind(event_id)
    .bind(event_seq)
    .fetch_optional(&mut **tx)
    .await?;
    if let Some(id) = id {
        tx.enqueue(&DeliverWebhook { delivery_id: id }).await?;
    }
    Ok(id)
}

/// Queue a `ping` delivery for `hook`.
pub async fn queue_ping(
    state: &AppState,
    tx: &mut Tx,
    owner: &Owner,
    hook: &HookRow,
    sender_id: i64,
) -> ApiResult<i64> {
    let hook_value = serde_json::to_value(hook_json(state, owner, hook))?;
    let payload = payloads::ping(
        state,
        hook.id,
        hook_value,
        owner.repo_id(),
        owner.org_id(),
        Some(sender_id),
    )
    .await
    .map_err(ApiError::internal)?;
    queue_delivery(tx, hook, "ping", None, owner.repo_id(), &payload, false).await
}

/// Organization that owns `repo_id`, if the owner is an organization.
async fn owning_org(state: &AppState, repo_id: i64) -> ApiResult<Option<i64>> {
    Ok(sqlx::query_scalar(
        "SELECT u.id FROM repositories r JOIN users u ON u.id = r.owner_id
          WHERE r.id = $1 AND u.type = 'Organization'",
    )
    .bind(repo_id)
    .fetch_optional(&state.db)
    .await?)
}

/// Active hooks that may want one of `names` for these repo/org scopes.
async fn candidate_hooks(
    state: &AppState,
    repo_ids: &[i64],
    org_ids: &[i64],
    names: &[&str],
) -> ApiResult<Vec<HookRow>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {} FROM webhooks
          WHERE active
            AND (events && $1 OR '*' = ANY(events))
            AND ((repo_id IS NOT NULL AND repo_id = ANY($2))
                 OR (org_id IS NOT NULL AND org_id = ANY($3))
                 OR (repo_id IS NULL AND org_id IS NULL))
          ORDER BY id",
        HookRow::COLUMNS
    ))
    .bind(names)
    .bind(repo_ids)
    .bind(org_ids)
    .fetch_all(&state.db)
    .await?)
}

/// Repositories and organizations whose hooks may receive `event`'s
/// deliveries: the event's repository (and its organization), plus the
/// other side of cross-repository events and the organization of
/// org-level events.
async fn scopes(state: &AppState, event: &Event) -> ApiResult<(Vec<i64>, Vec<i64>)> {
    let mut repo_ids: Vec<i64> = event.repo_id().into_iter().collect();
    let mut org_ids = Vec::new();
    match event {
        Event::OrgMemberAdded { org_id, .. }
        | Event::OrgMemberRemoved { org_id, .. }
        | Event::OrgMemberInvited { org_id, .. }
        | Event::TeamCreated { org_id, .. }
        | Event::TeamEdited { org_id, .. }
        | Event::TeamDeleted { org_id, .. }
        | Event::TeamMemberAdded { org_id, .. }
        | Event::TeamMemberRemoved { org_id, .. }
        | Event::TeamRepoAdded { org_id, .. }
        | Event::TeamRepoRemoved { org_id, .. } => org_ids.push(*org_id),
        Event::RepositoryDeleted { owner_id, .. } => org_ids.push(*owner_id),
        Event::RepositoryTransferred { old_owner_id, .. } => org_ids.push(*old_owner_id),
        Event::IssueTransferred { old_repo_id, .. } => repo_ids.push(*old_repo_id),
        Event::SubIssueAdded { sub_issue_id, .. } | Event::SubIssueRemoved { sub_issue_id, .. } => {
            let sub_repo: Option<i64> =
                sqlx::query_scalar("SELECT repo_id FROM issues WHERE id = $1")
                    .bind(sub_issue_id)
                    .fetch_optional(&state.db)
                    .await?;
            repo_ids.extend(sub_repo);
        }
        _ => {}
    }
    repo_ids.sort_unstable();
    repo_ids.dedup();
    for r in &repo_ids {
        if let Some(org) = owning_org(state, *r).await? {
            org_ids.push(org);
        }
    }
    org_ids.sort_unstable();
    org_ids.dedup();
    Ok((repo_ids, org_ids))
}

/// Create deliveries for one domain event. Returns how many were queued.
pub async fn dispatch(state: &AppState, event: &Event) -> ApiResult<usize> {
    if let Event::GlobalHookPing { hook_id, actor_id } = event {
        return ping_global(state, *hook_id, *actor_id).await;
    }
    let names = payloads::event_names(event);
    if names.is_empty() {
        return Ok(0);
    }
    let (repo_ids, org_ids) = scopes(state, event).await?;
    let hooks = candidate_hooks(state, &repo_ids, &org_ids, &names).await?;
    if hooks.is_empty() {
        return Ok(0);
    }
    let deliveries = payloads::for_event(state, event)
        .await
        .map_err(ApiError::internal)?;
    // A transferred repository's previous organization also hears about it.
    let previous_org = match event {
        Event::RepositoryTransferred { old_owner_id, .. } => Some(*old_owner_id),
        _ => None,
    };
    let event_id = events::current_event_id();
    let mut tx = Tx::begin(state).await?;
    let mut n = 0;
    for (seq, d) in deliveries.iter().enumerate() {
        let key = event_id.map(|id| (id, seq as i32));
        for h in &hooks {
            let in_scope = match (h.repo_id, h.org_id) {
                (Some(r), _) => d.repo_id == Some(r),
                (None, Some(o)) => d.org_id == Some(o) || previous_org == Some(o),
                (None, None) => true,
            };
            if in_scope
                && h.wants(d.event)
                && insert_delivery(
                    &mut tx,
                    h,
                    d.event,
                    d.action.as_deref(),
                    d.repo_id,
                    &d.payload,
                    false,
                    key,
                )
                .await?
                .is_some()
            {
                n += 1;
            }
        }
    }
    tx.commit().await?;
    Ok(n)
}

/// `ping` for a global (site admin) hook, requested by bgh-admin via
/// [`Event::GlobalHookPing`].
async fn ping_global(state: &AppState, hook_id: i64, actor_id: i64) -> ApiResult<usize> {
    let hook: Option<HookRow> = sqlx::query_as(&format!(
        "SELECT {} FROM webhooks WHERE id = $1 AND repo_id IS NULL AND org_id IS NULL",
        HookRow::COLUMNS
    ))
    .bind(hook_id)
    .fetch_optional(&state.db)
    .await?;
    let Some(hook) = hook else { return Ok(0) };
    let hook_value = serde_json::to_value(super::global_hook_json(state, &hook))?;
    let payload = payloads::ping(state, hook.id, hook_value, None, None, Some(actor_id))
        .await
        .map_err(ApiError::internal)?;
    let key = events::current_event_id().map(|id| (id, 0));
    let mut tx = Tx::begin(state).await?;
    let queued = insert_delivery(&mut tx, &hook, "ping", None, None, &payload, false, key).await?;
    tx.commit().await?;
    Ok(usize::from(queued.is_some()))
}

/// Event listener (`notify.webhooks`).
pub async fn on_event(state: AppState, event: Arc<Event>) -> anyhow::Result<()> {
    if event.is_quiet() {
        return Ok(());
    }
    dispatch(&state, &event)
        .await
        .map(|_| ())
        .map_err(|e| anyhow::anyhow!("webhook dispatch for {}: {e:?}", event.name()))
}
