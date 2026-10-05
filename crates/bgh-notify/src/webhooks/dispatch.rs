//! Domain events → webhook deliveries.
//!
//! The `notify.webhooks` listener first asks the database whether any
//! active hook (repository, owning organization, or site-wide) subscribes
//! to one of the webhook events the domain event maps to; only then are
//! payloads built ([`crate::payloads::for_event`]). Each matching hook gets
//! a `webhook_deliveries` row (exact body stored) and a
//! `notify.deliver_webhook` job, all in one transaction.

use std::sync::Arc;

use bgh_core::events::Event;
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
    let raw = serde_json::to_string(payload)?;
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO webhook_deliveries
                (hook_id, guid, event, action, repo_id, redelivery, status, url,
                 payload_raw, content_type)
         VALUES ($1, $2, $3, $4, $5, $6, 'pending', $7, $8, $9) RETURNING id",
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
    .fetch_one(&mut **tx)
    .await?;
    tx.enqueue(&DeliverWebhook { delivery_id: id }).await?;
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

/// Active hooks that may want one of `names` for this repo/org scope.
async fn candidate_hooks(
    state: &AppState,
    repo_id: Option<i64>,
    org_ids: &[i64],
    names: &[&str],
) -> ApiResult<Vec<HookRow>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {} FROM webhooks
          WHERE active
            AND (events && $1 OR '*' = ANY(events))
            AND ((repo_id IS NOT NULL AND repo_id = $2)
                 OR (org_id IS NOT NULL AND org_id = ANY($3))
                 OR (repo_id IS NULL AND org_id IS NULL))
          ORDER BY id",
        HookRow::COLUMNS
    ))
    .bind(names)
    .bind(repo_id)
    .bind(org_ids)
    .fetch_all(&state.db)
    .await?)
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
    let repo_id = event.repo_id();
    let mut org_ids = Vec::new();
    if let Some(r) = repo_id
        && let Some(org) = owning_org(state, r).await?
    {
        org_ids.push(org);
    }
    match event {
        Event::OrgMemberAdded { org_id, .. } => org_ids.push(*org_id),
        Event::RepositoryDeleted { owner_id, .. } => org_ids.push(*owner_id),
        _ => {}
    }
    let hooks = candidate_hooks(state, repo_id, &org_ids, &names).await?;
    if hooks.is_empty() {
        return Ok(0);
    }
    let deliveries = payloads::for_event(state, event)
        .await
        .map_err(ApiError::internal)?;
    let mut tx = Tx::begin(state).await?;
    let mut n = 0;
    for d in &deliveries {
        for h in &hooks {
            let in_scope = match (h.repo_id, h.org_id) {
                (Some(r), _) => d.repo_id == Some(r),
                (None, Some(o)) => d.org_id == Some(o),
                (None, None) => true,
            };
            if in_scope && h.wants(d.event) {
                queue_delivery(
                    &mut tx,
                    h,
                    d.event,
                    d.action.as_deref(),
                    d.repo_id,
                    &d.payload,
                    false,
                )
                .await?;
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
    let mut tx = Tx::begin(state).await?;
    queue_delivery(&mut tx, &hook, "ping", None, None, &payload, false).await?;
    tx.commit().await?;
    Ok(1)
}

/// Event listener (`notify.webhooks`).
pub async fn on_event(state: AppState, event: Arc<Event>) -> anyhow::Result<()> {
    dispatch(&state, &event)
        .await
        .map(|_| ())
        .map_err(|e| anyhow::anyhow!("webhook dispatch for {}: {e:?}", event.name()))
}
