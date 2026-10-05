//! Postgres-backed background job queue.
//!
//! * Define a payload type implementing [`JobPayload`] (its `KIND` is the
//!   queue key, conventionally `"<crate>.<action>"`, e.g. `"repos.post_receive"`).
//! * Enqueue inside the transaction that makes the work necessary:
//!   [`crate::db::Tx::enqueue`] or [`enqueue_job`]. Jobs become visible to
//!   workers on commit (a `NOTIFY bgh_jobs` wakes them immediately).
//! * Register the handler in your crate's `register(&mut Registry)`.
//!
//! Workers claim jobs with `FOR UPDATE SKIP LOCKED`; failures are retried
//! with exponential backoff until `max_attempts`, then kept with `failed_at`.
//! Handlers must be idempotent (a crash mid-job causes a retry).

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use futures::FutureExt;
use futures::future::BoxFuture;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use sqlx::PgExecutor;
use sqlx::postgres::PgListener;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::state::AppState;

/// Postgres NOTIFY channel used to wake workers.
pub const NOTIFY_CHANNEL: &str = "bgh_jobs";
/// Jobs locked longer than this are considered abandoned and re-claimed.
const LOCK_TIMEOUT_SECS: i64 = 15 * 60;
/// Upper bound for a single job execution.
const JOB_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// Fallback poll interval (NOTIFY covers the common case).
const POLL_INTERVAL: Duration = Duration::from_secs(5);

/// A typed job payload.
pub trait JobPayload: Serialize + DeserializeOwned + Send + Sync + 'static {
    /// Unique job kind, e.g. `"repos.post_receive"`.
    const KIND: &'static str;
    /// Attempts before the job is marked failed.
    const MAX_ATTEMPTS: i32 = 10;
}

/// Enqueue a raw job. Runs as soon as a worker is free (after commit).
pub async fn enqueue(
    db: impl PgExecutor<'_>,
    kind: &str,
    payload: &Value,
) -> Result<i64, sqlx::Error> {
    enqueue_at(db, kind, payload, Utc::now(), 10).await
}

/// Enqueue a raw job to run at `run_at`.
pub async fn enqueue_at(
    db: impl PgExecutor<'_>,
    kind: &str,
    payload: &Value,
    run_at: DateTime<Utc>,
    max_attempts: i32,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "WITH j AS (
            INSERT INTO jobs (kind, payload, run_at, max_attempts)
            VALUES ($1, $2, $3, $4) RETURNING id
         )
         SELECT j.id FROM j, LATERAL (SELECT pg_notify($5, $1)) n",
    )
    .bind(kind)
    .bind(payload)
    .bind(run_at)
    .bind(max_attempts)
    .bind(NOTIFY_CHANNEL)
    .fetch_one(db)
    .await
}

/// Enqueue a typed job.
pub async fn enqueue_job<J: JobPayload>(
    db: impl PgExecutor<'_>,
    job: &J,
) -> Result<i64, sqlx::Error> {
    let payload = serde_json::to_value(job).map_err(|e| sqlx::Error::Encode(Box::new(e)))?;
    enqueue_at(db, J::KIND, &payload, Utc::now(), J::MAX_ATTEMPTS).await
}

type Handler = Arc<dyn Fn(AppState, Value) -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync>;

/// Map of job kind → handler. Built at startup from every crate's
/// `register` function.
#[derive(Clone, Default)]
pub struct JobRegistry {
    handlers: HashMap<String, Handler>,
}

impl JobRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a typed handler. Panics on duplicate kinds.
    pub fn register<J, F, Fut>(&mut self, handler: F)
    where
        J: JobPayload,
        F: Fn(AppState, J) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        let handler = Arc::new(handler);
        self.register_raw(J::KIND, move |state, value| {
            let handler = handler.clone();
            async move {
                let payload: J = serde_json::from_value(value)
                    .map_err(|e| anyhow::anyhow!("invalid {} payload: {e}", J::KIND))?;
                handler(state, payload).await
            }
        });
    }

    /// Register a handler taking the raw JSON payload. Panics on duplicates.
    pub fn register_raw<F, Fut>(&mut self, kind: &str, handler: F)
    where
        F: Fn(AppState, Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        let h: Handler = Arc::new(move |state, value| handler(state, value).boxed());
        if self.handlers.insert(kind.to_string(), h).is_some() {
            panic!("job kind {kind:?} registered twice");
        }
    }

    pub fn kinds(&self) -> impl Iterator<Item = &str> {
        self.handlers.keys().map(String::as_str)
    }

    fn get(&self, kind: &str) -> Option<Handler> {
        self.handlers.get(kind).cloned()
    }
}

#[derive(sqlx::FromRow)]
struct ClaimedJob {
    id: i64,
    kind: String,
    payload: Value,
    attempts: i32,
    max_attempts: i32,
}

/// Claim and run at most one ready job. Returns `false` if none was ready.
pub async fn run_one(
    state: &AppState,
    registry: &JobRegistry,
    worker_id: &str,
) -> anyhow::Result<bool> {
    let job: Option<ClaimedJob> = sqlx::query_as(
        "UPDATE jobs SET locked_at = now(), locked_by = $1, attempts = attempts + 1
          WHERE id = (
              SELECT id FROM jobs
               WHERE failed_at IS NULL AND run_at <= now()
                 AND (locked_at IS NULL OR locked_at < now() - make_interval(secs => $2))
               ORDER BY run_at, id
               FOR UPDATE SKIP LOCKED
               LIMIT 1)
          RETURNING id, kind, payload, attempts, max_attempts",
    )
    .bind(worker_id)
    .bind(LOCK_TIMEOUT_SECS as f64)
    .fetch_optional(&state.db)
    .await?;
    let Some(job) = job else {
        return Ok(false);
    };

    let result = match registry.get(&job.kind) {
        None => Err(anyhow::anyhow!(
            "no handler registered for job kind {:?}",
            job.kind
        )),
        Some(handler) => {
            let fut = handler(state.clone(), job.payload.clone());
            // Run in its own task so panics are contained.
            match tokio::time::timeout(JOB_TIMEOUT, tokio::spawn(fut)).await {
                Err(_) => Err(anyhow::anyhow!("job timed out after {JOB_TIMEOUT:?}")),
                Ok(Err(join)) => Err(anyhow::anyhow!("job panicked: {join}")),
                Ok(Ok(r)) => r,
            }
        }
    };

    match result {
        Ok(()) => {
            sqlx::query("DELETE FROM jobs WHERE id = $1")
                .bind(job.id)
                .execute(&state.db)
                .await?;
            tracing::debug!(job.id, kind = %job.kind, "job done");
        }
        Err(err) => {
            let msg = format!("{err:#}");
            if job.attempts >= job.max_attempts {
                tracing::error!(job.id, kind = %job.kind, error = %msg, "job failed permanently");
                sqlx::query(
                    "UPDATE jobs SET failed_at = now(), locked_at = NULL, locked_by = NULL,
                            last_error = $2 WHERE id = $1",
                )
                .bind(job.id)
                .bind(&msg)
                .execute(&state.db)
                .await?;
            } else {
                let delay = backoff(job.attempts);
                tracing::warn!(job.id, kind = %job.kind, error = %msg, ?delay, "job failed; retrying");
                sqlx::query(
                    "UPDATE jobs SET run_at = now() + make_interval(secs => $2),
                            locked_at = NULL, locked_by = NULL, last_error = $3
                      WHERE id = $1",
                )
                .bind(job.id)
                .bind(delay.as_secs_f64())
                .bind(&msg)
                .execute(&state.db)
                .await?;
            }
        }
    }
    Ok(true)
}

/// Exponential backoff with jitter: ~5s, 10s, 20s, ... capped at 1h.
pub fn backoff(attempts: i32) -> Duration {
    let exp = attempts.clamp(1, 20) as u32 - 1;
    let base = 5u64.saturating_mul(1u64 << exp.min(12)).min(3600);
    let jitter = rand::random_range(0..=base / 4 + 1);
    Duration::from_secs(base + jitter)
}

/// Run every job that is ready now (including jobs enqueued by jobs), until
/// the queue has nothing ready. Returns how many ran. Used by tests.
pub async fn drain(state: &AppState, registry: &JobRegistry) -> anyhow::Result<usize> {
    let mut n = 0;
    while run_one(state, registry, "drain").await? {
        n += 1;
    }
    Ok(n)
}

/// Run `concurrency` worker loops until `shutdown` is cancelled. In-flight
/// jobs finish before this returns.
pub async fn run_workers(
    state: AppState,
    registry: Arc<JobRegistry>,
    concurrency: usize,
    shutdown: CancellationToken,
) {
    if concurrency == 0 {
        return;
    }
    let wake = Arc::new(Notify::new());

    // LISTEN for new jobs; on failure we still poll.
    let listener_task = {
        let wake = wake.clone();
        let db = state.db.clone();
        let shutdown = shutdown.clone();
        tokio::spawn(async move {
            loop {
                let mut listener = match PgListener::connect_with(&db).await {
                    Ok(l) => l,
                    Err(err) => {
                        tracing::warn!(?err, "job listener connect failed");
                        tokio::select! {
                            _ = shutdown.cancelled() => return,
                            _ = tokio::time::sleep(POLL_INTERVAL) => continue,
                        }
                    }
                };
                if let Err(err) = listener.listen(NOTIFY_CHANNEL).await {
                    tracing::warn!(?err, "LISTEN failed");
                    continue;
                }
                loop {
                    tokio::select! {
                        _ = shutdown.cancelled() => return,
                        msg = listener.recv() => match msg {
                            Ok(_) => wake.notify_waiters(),
                            Err(err) => {
                                tracing::warn!(?err, "job listener error; reconnecting");
                                break;
                            }
                        }
                    }
                }
            }
        })
    };

    let host = std::env::var("HOSTNAME").unwrap_or_else(|_| "bgh".into());
    let mut workers = Vec::with_capacity(concurrency);
    for i in 0..concurrency {
        let state = state.clone();
        let registry = registry.clone();
        let wake = wake.clone();
        let shutdown = shutdown.clone();
        let worker_id = format!("{host}:{}:{i}", std::process::id());
        workers.push(tokio::spawn(async move {
            loop {
                if shutdown.is_cancelled() {
                    return;
                }
                let notified = wake.notified();
                match run_one(&state, &registry, &worker_id).await {
                    Ok(true) => continue,
                    Ok(false) => {}
                    Err(err) => tracing::error!(?err, "job worker error"),
                }
                tokio::select! {
                    _ = shutdown.cancelled() => return,
                    _ = notified => {}
                    _ = tokio::time::sleep(POLL_INTERVAL) => {}
                }
            }
        }));
    }
    for w in workers {
        let _ = w.await;
    }
    listener_task.abort();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_caps() {
        assert!(backoff(1) >= Duration::from_secs(5));
        assert!(backoff(1) < Duration::from_secs(8));
        assert!(backoff(3) >= Duration::from_secs(20));
        assert!(backoff(50) <= Duration::from_secs(3600 + 901));
    }
}
