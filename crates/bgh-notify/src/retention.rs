//! Data retention (`notify.retention` service, hourly, one leader via a pg
//! advisory lock), with windows from the `retention` site settings:
//!
//! 1. notification threads not updated for `notifications_days` (synced as
//!    deletes), plus the [`privacy::sweep`] backstop for threads whose
//!    holder lost read access without an event reaching us;
//! 2. webhook deliveries: request/response bodies dropped after
//!    `webhook_payload_days`, the metadata deleted after
//!    `webhook_delivery_days`;
//! 3. Events API / activity rows older than `activity_days`;
//! 4. expired sessions.
//!
//! Every delete runs in batches of [`BATCH`] rows, each its own statement,
//! so a large backlog never holds long locks. `POST
//! /_bgh/admin/retention/run` runs a pass on demand.

use std::time::Duration;

use axum::extract::State;
use bgh_core::db::AdvisoryLock;
use bgh_core::prelude::*;
use bgh_core::settings::{self, RetentionSettings};
use bgh_core::sync;
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use crate::privacy;

/// Leader lock key ("ntfret").
const LEADER_KEY: i64 = 0x0000_6e74_6672_6574;

/// Rows per batch.
pub const BATCH: i64 = 1000;

/// What one pass removed.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct Report {
    /// Ran (false: another process holds the leader lock, or disabled).
    pub ran: bool,
    pub notifications: u64,
    pub unreadable_notifications: u64,
    pub webhook_payloads: u64,
    pub webhook_deliveries: u64,
    pub activity_events: u64,
    pub sessions: u64,
}

pub async fn service(state: AppState, shutdown: CancellationToken) -> anyhow::Result<()> {
    let mut tick = tokio::time::interval(Duration::from_secs(3600));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return Ok(()),
            _ = tick.tick() => {}
        }
        let cfg = match settings::load(&state).await {
            Ok(s) if s.retention.enabled => s.retention.clone(),
            Ok(_) => continue,
            Err(err) => {
                tracing::warn!(?err, "retention: loading settings");
                continue;
            }
        };
        match run(&state, &cfg).await {
            Ok(r) if r != Report::default() && r.ran => tracing::info!(?r, "retention pass"),
            Ok(_) => {}
            Err(err) => tracing::warn!(?err, "retention pass failed"),
        }
    }
}

/// One pass with `cfg` (empty report when another process leads).
pub async fn run(state: &AppState, cfg: &RetentionSettings) -> anyhow::Result<Report> {
    let Some(leader) = AdvisoryLock::try_acquire(&state.db, LEADER_KEY).await? else {
        return Ok(Report::default());
    };
    let res = pass(state, cfg).await;
    leader.release().await;
    res
}

async fn pass(state: &AppState, cfg: &RetentionSettings) -> anyhow::Result<Report> {
    let mut r = Report {
        ran: true,
        ..Default::default()
    };
    if cfg.notifications_days > 0 {
        r.notifications = old_notifications(state, cfg.notifications_days).await?;
    }
    r.unreadable_notifications = privacy::sweep(state).await?;
    // Delete before stripping, so expiring rows aren't rewritten first.
    if cfg.webhook_delivery_days > 0 {
        r.webhook_deliveries = batched(
            state,
            "DELETE FROM webhook_deliveries
              WHERE id IN (SELECT id FROM webhook_deliveries
                            WHERE created_at < now() - make_interval(days => $1) LIMIT $2)",
            cfg.webhook_delivery_days,
        )
        .await?;
    }
    if cfg.webhook_payload_days > 0 {
        r.webhook_payloads = batched(
            state,
            "UPDATE webhook_deliveries
                SET payload_raw = '', request_payload = '{}', response_body = NULL,
                    response_headers = NULL
              WHERE id IN (SELECT id FROM webhook_deliveries
                            WHERE payload_raw <> '' AND created_at < now() - make_interval(days => $1)
                            LIMIT $2)",
            cfg.webhook_payload_days,
        )
        .await?;
    }
    if cfg.activity_days > 0 {
        r.activity_events = batched(
            state,
            "DELETE FROM activity_events
              WHERE id IN (SELECT id FROM activity_events
                            WHERE created_at < now() - make_interval(days => $1) LIMIT $2)",
            cfg.activity_days,
        )
        .await?;
    }
    r.sessions = batched(
        state,
        "DELETE FROM sessions
          WHERE id IN (SELECT id FROM sessions
                        WHERE expires_at < now() - make_interval(days => $1) LIMIT $2)",
        0,
    )
    .await?;
    Ok(r)
}

/// Run `sql` (binds: `$1` days, `$2` batch size) until a batch comes back
/// short. Returns the total rows affected.
async fn batched(state: &AppState, sql: &str, days: u32) -> anyhow::Result<u64> {
    let mut total = 0;
    loop {
        let n = sqlx::query(sql)
            .bind(days as i32)
            .bind(BATCH)
            .execute(&state.db)
            .await?
            .rows_affected();
        total += n;
        if (n as i64) < BATCH {
            return Ok(total);
        }
    }
}

/// Delete threads not updated for `days`, syncing deletes for the ones
/// still in clients' stores.
async fn old_notifications(state: &AppState, days: u32) -> anyhow::Result<u64> {
    let mut total = 0;
    loop {
        let mut tx = Tx::begin(state).await?;
        let gone: Vec<(i64, i64, bool)> = sqlx::query_as(
            "DELETE FROM notifications
              WHERE id IN (SELECT id FROM notifications
                            WHERE updated_at < now() - make_interval(days => $1) LIMIT $2)
             RETURNING id, user_id, done",
        )
        .bind(days as i32)
        .bind(BATCH)
        .fetch_all(&mut *tx)
        .await?;
        for (id, user_id, done) in &gone {
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

/// `POST /_bgh/admin/retention/run`: one pass now with the current
/// settings (site admins).
pub async fn run_now(
    State(state): State<AppState>,
    _admin: RequireSiteAdmin,
) -> ApiResult<Json<Report>> {
    let cfg = settings::load(&state).await?.retention.clone();
    Ok(Json(run(&state, &cfg).await?))
}
