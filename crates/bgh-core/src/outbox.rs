//! Durable event delivery: the transactional outbox and its consumers.
//!
//! * [`append`] writes events to `event_outbox` inside the emitting
//!   transaction ([`crate::db::Tx::commit`] calls it), without a lock (ids
//!   may commit out of order; consumers stop at the commit-order watermark,
//!   [`crate::seqlog`]), and `NOTIFY`s [`NOTIFY_CHANNEL`] so consumers in
//!   every process wake on commit.
//! * Each `reg.on_event` listener is a durable consumer
//!   ([`start_listeners`]): it reads batches past its cursor in
//!   `event_listener_cursors`, runs the handler for each event in order, and
//!   advances the cursor after the batch. Delivery is at-least-once (a
//!   crash mid-batch redelivers the batch); handlers use
//!   [`crate::events::effect_key`] for idempotent writes.
//! * A lease on the cursor row makes one process the consumer of a
//!   listener at a time; others poll and take over when the lease expires
//!   (or is released on shutdown).
//! * Failed handlers are retried a few times, then the event is skipped
//!   with an error log (a poison event must not stall the listener).
//! * On shutdown, consumers drain what is already committed (bounded by
//!   [`DRAIN_TIMEOUT`]), release their lease and exit.
//! * Rows every registered cursor has passed are pruned once older than
//!   `BGH_EVENT_RETENTION_DAYS` ([`prune`]).

use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::postgres::PgListener;
use sqlx::{FromRow, PgConnection, PgPool};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::events::{Event, with_listener_event};
use crate::registry::Listener;
use crate::state::AppState;

/// Postgres NOTIFY channel announcing committed outbox rows.
pub const NOTIFY_CHANNEL: &str = "bgh_events";
/// Events read per batch.
pub const BATCH_SIZE: i64 = 200;
/// How long a consumer's lease lasts without renewal.
pub const LEASE: Duration = Duration::from_secs(30);
/// Renew the lease when less than this much of it is left.
const RENEW_MARGIN: Duration = Duration::from_secs(20);
/// Fallback poll interval (wakeups cover the common case) and the interval
/// at which a process without the lease retries taking it.
pub const POLL_INTERVAL: Duration = Duration::from_secs(2);
/// Retry interval while committed events wait above the watermark.
const BLOCKED_RETRY: Duration = Duration::from_millis(20);
/// How long consumers keep draining committed events after shutdown.
pub const DRAIN_TIMEOUT: Duration = Duration::from_secs(15);
/// Attempts per event before it is skipped.
pub const MAX_ATTEMPTS: u32 = 3;
/// Interval between prune passes.
const PRUNE_INTERVAL: Duration = Duration::from_secs(600);

/// Append `events` to the outbox in the caller's transaction, in order.
/// No lock: ids may commit out of order and consumers stop at the
/// commit-order watermark ([`crate::seqlog`]); rows that collide with a gap
/// filler are re-inserted with fresh ids (see [`crate::sync::record_all`]).
pub async fn append(conn: &mut PgConnection, events: &[Event]) -> Result<(), sqlx::Error> {
    if events.is_empty() {
        return Ok(());
    }
    let mut kinds = Vec::with_capacity(events.len());
    let mut payloads = Vec::with_capacity(events.len());
    for e in events {
        kinds.push(e.name());
        payloads.push(serde_json::to_value(e).map_err(|e| sqlx::Error::Encode(Box::new(e)))?);
    }
    for attempt in 1.. {
        let ids: Vec<i64> = sqlx::query_scalar(
            "INSERT INTO event_outbox (kind, payload, created_at)
             SELECT u.k, u.p, clock_timestamp()
               FROM (SELECT pg_notify($3, '')) l,
                    unnest($1::text[], $2::jsonb[]) WITH ORDINALITY AS u(k, p, o)
              ORDER BY u.o
             ON CONFLICT (id) DO NOTHING
             RETURNING id",
        )
        .bind(&kinds)
        .bind(&payloads)
        .bind(NOTIFY_CHANNEL)
        .fetch_all(&mut *conn)
        .await?;
        if ids.len() == events.len() {
            break;
        }
        tracing::warn!(attempt, "outbox ids taken by gap fillers; retrying");
        sqlx::query("DELETE FROM event_outbox WHERE id = ANY($1)")
            .bind(&ids)
            .execute(&mut *conn)
            .await?;
        if attempt >= 5 {
            return Err(sqlx::Error::Protocol(
                "outbox ids repeatedly taken by gap fillers".into(),
            ));
        }
    }
    Ok(())
}

/// The outbox's commit-order watermark: every id `<=` it is committed
/// (consumers never read past it; [`crate::seqlog`]).
pub async fn head(db: &PgPool) -> Result<i64, sqlx::Error> {
    crate::seqlog::advance(db, crate::seqlog::Log::Outbox).await
}

/// Consumer state of one listener (for metrics and admin pages).
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct ConsumerLag {
    pub listener: String,
    /// Last event the listener fully processed.
    pub last_id: i64,
    /// The outbox watermark (every event `<=` it is committed).
    pub head_id: i64,
    /// Committed events not yet processed (`head_id - last_id`, ids may
    /// have gaps so this is an upper bound).
    pub lag: i64,
    /// Age in seconds of the oldest unprocessed event (0 when caught up).
    pub oldest_pending_secs: f64,
    pub lease_owner: Option<Uuid>,
    pub lease_until: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}

/// Lag of every listener cursor, ordered by name.
pub async fn consumer_lag(db: &PgPool) -> Result<Vec<ConsumerLag>, sqlx::Error> {
    let head = head(db).await?;
    sqlx::query_as(
        "WITH h AS (SELECT $1::bigint AS head)
         SELECT c.listener, c.last_id, h.head AS head_id,
                greatest(h.head - c.last_id, 0) AS lag,
                coalesce(extract(epoch FROM now() - (
                    SELECT o.created_at FROM event_outbox o WHERE o.id > c.last_id
                        AND o.kind <> '!gap'
                     ORDER BY o.id LIMIT 1))::float8, 0) AS oldest_pending_secs,
                c.lease_owner, c.lease_until, c.updated_at
           FROM event_listener_cursors c, h
          ORDER BY c.listener",
    )
    .bind(head)
    .fetch_all(db)
    .await
}

/// Delete outbox rows older than `retention` that every listener in
/// `listeners` has processed, and idempotency receipts of the same age.
/// Returns the number of outbox rows deleted.
pub async fn prune(
    db: &PgPool,
    listeners: &[&str],
    retention: Duration,
) -> Result<u64, sqlx::Error> {
    let secs = retention.as_secs_f64();
    let deleted = sqlx::query(
        "DELETE FROM event_outbox
          WHERE created_at < now() - make_interval(secs => $2)
            AND id <= coalesce((SELECT min(last_id) FROM event_listener_cursors
                                 WHERE listener = ANY($1)), 0)",
    )
    .bind(listeners)
    .bind(secs)
    .execute(db)
    .await?
    .rows_affected();
    sqlx::query(
        "DELETE FROM event_receipts
          WHERE created_at < now() - make_interval(secs => $1)",
    )
    .bind(secs)
    .execute(db)
    .await?;
    Ok(deleted)
}

/// Wait until every listener in `listeners` has processed all events
/// committed before the call (flushing direct emits first). Returns false
/// on timeout.
pub async fn wait_caught_up(state: &AppState, listeners: &[&str], timeout: Duration) -> bool {
    state.events.flush().await;
    let deadline = Instant::now() + timeout;
    // Every committed event, including ones above the watermark (the
    // consumers get there once lower in-flight ids settle).
    let Ok(target) = sqlx::query_scalar::<_, i64>("SELECT coalesce(max(id), 0) FROM event_outbox")
        .fetch_one(&state.db)
        .await
    else {
        return false;
    };
    loop {
        let behind: Result<i64, _> = sqlx::query_scalar(
            "SELECT count(*) FROM unnest($1::text[]) AS l(name)
              LEFT JOIN event_listener_cursors c ON c.listener = l.name
             WHERE coalesce(c.last_id, 0) < $2",
        )
        .bind(listeners)
        .bind(target)
        .fetch_one(&state.db)
        .await;
        if matches!(behind, Ok(0)) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        state.events.wake();
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Create missing cursors (at the current watermark: a newly added listener
/// starts with events committed from now on), then spawn one consumer per
/// listener, the cross-process wakeup task and the pruner. Every task
/// returns once `shutdown` is cancelled (consumers after draining).
pub async fn start_listeners(
    state: &AppState,
    listeners: &[Listener],
    shutdown: CancellationToken,
) -> anyhow::Result<Vec<JoinHandle<()>>> {
    let names: Vec<&str> = listeners.iter().map(|l| l.name).collect();
    let mut seen = std::collections::HashSet::new();
    for n in &names {
        anyhow::ensure!(seen.insert(*n), "duplicate event listener name {n:?}");
    }
    // At the watermark, not max(id): a lower id may still be in flight.
    let start = head(&state.db).await?;
    sqlx::query(
        "INSERT INTO event_listener_cursors (listener, last_id)
         SELECT name, $2 FROM unnest($1::text[]) AS t(name)
         ON CONFLICT (listener) DO NOTHING",
    )
    .bind(&names)
    .bind(start)
    .execute(&state.db)
    .await?;

    let mut handles = Vec::with_capacity(listeners.len() + 2);
    handles.push(tokio::spawn(notify_task(state.clone(), shutdown.clone())));
    let owned: Vec<String> = names.iter().map(|s| s.to_string()).collect();
    handles.push(tokio::spawn(prune_task(
        state.clone(),
        owned,
        shutdown.clone(),
    )));
    for listener in listeners.iter().cloned() {
        let consumer = Consumer {
            state: state.clone(),
            listener,
            owner: Uuid::new_v4(),
            shutdown: shutdown.clone(),
        };
        handles.push(tokio::spawn(consumer.run()));
    }
    Ok(handles)
}

/// LISTEN on [`NOTIFY_CHANNEL`] (own connection, outside the pool) and
/// wake local consumers when another process commits events.
async fn notify_task(state: AppState, shutdown: CancellationToken) {
    loop {
        let connect = async {
            let mut l = PgListener::connect(&state.config.database_url).await?;
            l.listen(NOTIFY_CHANNEL).await?;
            Ok::<_, sqlx::Error>(l)
        };
        let mut listener = tokio::select! {
            _ = shutdown.cancelled() => return,
            l = connect => match l {
                Ok(l) => l,
                Err(err) => {
                    tracing::warn!(?err, "event LISTEN failed; polling");
                    tokio::select! {
                        _ = shutdown.cancelled() => return,
                        _ = tokio::time::sleep(POLL_INTERVAL * 5) => continue,
                    }
                }
            },
        };
        // Anything committed while (re)connecting.
        state.events.wake();
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => return,
                msg = listener.recv() => match msg {
                    Ok(_) => state.events.wake(),
                    Err(err) => {
                        tracing::warn!(?err, "event LISTEN connection lost; reconnecting");
                        break;
                    }
                },
            }
        }
    }
}

async fn prune_task(state: AppState, listeners: Vec<String>, shutdown: CancellationToken) {
    let names: Vec<&str> = listeners.iter().map(String::as_str).collect();
    let retention = Duration::from_secs(state.config.event_retention_days.max(0) as u64 * 86_400);
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = tokio::time::sleep(PRUNE_INTERVAL) => {}
        }
        match prune(&state.db, &names, retention).await {
            Ok(0) => {}
            Ok(n) => tracing::info!(deleted = n, "pruned event outbox"),
            Err(err) => tracing::warn!(?err, "event outbox prune failed"),
        }
    }
}

#[derive(FromRow)]
struct OutboxRow {
    id: i64,
    kind: String,
    payload: serde_json::Value,
}

struct Consumer {
    state: AppState,
    listener: Listener,
    owner: Uuid,
    shutdown: CancellationToken,
}

/// What a pass over the outbox found.
enum Pass {
    /// Processed a batch (there may be more).
    Progress,
    /// Caught up.
    Idle,
    /// Committed events wait above the watermark (a lower id is still in
    /// flight, or burned and not filled yet): look again shortly.
    Blocked,
    /// Another process holds the lease.
    NotLeader,
}

impl Consumer {
    async fn run(self) {
        let name = self.listener.name;
        let mut wake = self.state.events.wake_receiver();
        let mut lease: Option<(i64, Instant)> = None; // (cursor, lease expiry)
        let mut drain_deadline: Option<Instant> = None;
        loop {
            if drain_deadline.is_none() && self.shutdown.is_cancelled() {
                drain_deadline = Some(Instant::now() + DRAIN_TIMEOUT);
            }
            if drain_deadline.is_some_and(|d| Instant::now() >= d) {
                tracing::warn!(
                    listener = name,
                    "stopping before catching up (drain timeout)"
                );
                break;
            }
            wake.borrow_and_update();
            let pass = self.pass(&mut lease, drain_deadline.is_some()).await;
            let wait = match pass {
                Ok(Pass::Progress) => continue,
                Ok(Pass::Idle) if drain_deadline.is_some() => break,
                Ok(Pass::NotLeader) if drain_deadline.is_some() => break,
                Ok(Pass::Idle) => POLL_INTERVAL,
                // Also while draining: the events are committed.
                Ok(Pass::Blocked) => BLOCKED_RETRY,
                Ok(Pass::NotLeader) => POLL_INTERVAL,
                Err(err) => {
                    if drain_deadline.is_some() {
                        tracing::warn!(listener = name, ?err, "event consumer stopped");
                        break;
                    }
                    tracing::warn!(listener = name, ?err, "event consumer error");
                    lease = None;
                    Duration::from_secs(1)
                }
            };
            tokio::select! {
                _ = self.shutdown.cancelled(), if drain_deadline.is_none() => {}
                // Without the lease, only poll: don't contend on every commit.
                _ = wake.changed(), if lease.is_some() => {}
                _ = tokio::time::sleep(wait) => {}
            }
        }
        if lease.is_some() {
            let _ = sqlx::query(
                "UPDATE event_listener_cursors SET lease_owner = NULL, lease_until = NULL
                  WHERE listener = $1 AND lease_owner = $2",
            )
            .bind(name)
            .bind(self.owner)
            .execute(&self.state.db)
            .await;
        }
    }

    /// Take or renew the lease if needed; returns the cursor, or `None` when
    /// another process holds the lease.
    async fn ensure_lease(
        &self,
        lease: &mut Option<(i64, Instant)>,
    ) -> Result<Option<i64>, sqlx::Error> {
        if let Some((cursor, until)) = *lease
            && until.saturating_duration_since(Instant::now()) > RENEW_MARGIN
        {
            return Ok(Some(cursor));
        }
        let started = Instant::now();
        let cursor: Option<i64> = sqlx::query_scalar(
            "UPDATE event_listener_cursors
                SET lease_owner = $2, lease_until = now() + make_interval(secs => $3)
              WHERE listener = $1
                AND (lease_owner IS NULL OR lease_owner = $2 OR lease_until < now())
             RETURNING last_id",
        )
        .bind(self.listener.name)
        .bind(self.owner)
        .bind(LEASE.as_secs_f64())
        .fetch_optional(&self.state.db)
        .await?;
        *lease = cursor.map(|c| (c, started + LEASE));
        Ok(cursor)
    }

    async fn idle_or_blocked(&self, watermark: i64) -> anyhow::Result<Pass> {
        let above: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM event_outbox WHERE id > $1)")
                .bind(watermark)
                .fetch_one(&self.state.db)
                .await?;
        Ok(if above { Pass::Blocked } else { Pass::Idle })
    }

    async fn pass(
        &self,
        lease: &mut Option<(i64, Instant)>,
        draining: bool,
    ) -> anyhow::Result<Pass> {
        let Some(cursor) = self.ensure_lease(lease).await? else {
            return Ok(Pass::NotLeader);
        };
        // Only up to the commit-order watermark: a lower id may still be in
        // flight past the highest visible one (`seqlog`).
        let watermark = head(&self.state.db).await?;
        if watermark <= cursor {
            return self.idle_or_blocked(watermark).await;
        }
        let rows: Vec<OutboxRow> = sqlx::query_as(
            "SELECT id, kind, payload FROM event_outbox
              WHERE id > $1 AND id <= $3 ORDER BY id LIMIT $2",
        )
        .bind(cursor)
        .bind(BATCH_SIZE)
        .bind(watermark)
        .fetch_all(&self.state.db)
        .await?;
        let Some(last) = rows.last().map(|r| r.id) else {
            return Ok(Pass::Idle);
        };
        for row in rows {
            if row.kind == crate::seqlog::GAP {
                continue;
            }
            self.handle(row, draining).await;
            // Keep the lease through a slow batch.
            if lease.is_some_and(|(_, until)| {
                until.saturating_duration_since(Instant::now()) <= RENEW_MARGIN
            }) && self.ensure_lease(lease).await?.is_none()
            {
                tracing::warn!(listener = self.listener.name, "event lease lost mid-batch");
                return Ok(Pass::NotLeader);
            }
        }
        let advanced = sqlx::query(
            "UPDATE event_listener_cursors
                SET last_id = greatest(last_id, $3), updated_at = now(),
                    lease_until = now() + make_interval(secs => $4)
              WHERE listener = $1 AND lease_owner = $2",
        )
        .bind(self.listener.name)
        .bind(self.owner)
        .bind(last)
        .bind(LEASE.as_secs_f64())
        .execute(&self.state.db)
        .await?
        .rows_affected();
        if advanced == 0 {
            *lease = None;
            tracing::warn!(
                listener = self.listener.name,
                "event lease lost; batch not committed"
            );
            return Ok(Pass::NotLeader);
        }
        *lease = Some((last, Instant::now() + LEASE));
        Ok(Pass::Progress)
    }

    /// Run the handler for one event (retrying failures).
    async fn handle(&self, row: OutboxRow, draining: bool) {
        let name = self.listener.name;
        let event: Event = match serde_json::from_value(row.payload) {
            Ok(e) => e,
            Err(err) => {
                // Written by a newer (or older) binary with a different
                // variant set: nothing this process can do with it.
                tracing::warn!(listener = name, id = row.id, kind = row.kind, %err,
                    "skipping undecodable event");
                return;
            }
        };
        let event = Arc::new(event);
        for attempt in 1..=MAX_ATTEMPTS {
            let fut = self.listener.call(self.state.clone(), event.clone());
            let outcome = tokio::spawn(with_listener_event(name, row.id, fut)).await;
            let err = match outcome {
                Ok(Ok(())) => return,
                Ok(Err(err)) => format!("{err:?}"),
                Err(join) => format!("panicked: {join}"),
            };
            if attempt == MAX_ATTEMPTS {
                tracing::error!(
                    listener = name,
                    id = row.id,
                    event = event.name(),
                    attempt,
                    err,
                    "event listener failed; skipping event"
                );
            } else {
                tracing::warn!(
                    listener = name,
                    id = row.id,
                    event = event.name(),
                    attempt,
                    err,
                    "event listener failed; retrying"
                );
                if !draining {
                    tokio::time::sleep(Duration::from_millis(50 * u64::from(attempt))).await;
                }
            }
        }
    }
}
