//! Background job inspector: `/_bgh/admin/jobs` (list / filter / retry /
//! cancel) and per-kind statistics.
//!
//! Job states are derived from the `jobs` row: `failed` (`failed_at`),
//! `running` (locked within the lock timeout), `scheduled` (`run_at` in the
//! future, e.g. waiting for a retry) and `pending` (ready to run).

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use bgh_core::audit::Target;
use bgh_core::prelude::*;
use bgh_core::time::ts;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::FromRow;

use crate::common::log;

/// Must match `LOCK_TIMEOUT_SECS` in `bgh_core::jobs`.
const LOCK_TIMEOUT: &str = "interval '15 minutes'";

fn state_expr() -> String {
    format!(
        "CASE WHEN failed_at IS NOT NULL THEN 'failed'
              WHEN locked_at IS NOT NULL AND locked_at > now() - {LOCK_TIMEOUT} THEN 'running'
              WHEN run_at > now() THEN 'scheduled'
              ELSE 'pending' END"
    )
}

#[derive(Debug, FromRow)]
struct JobRow {
    id: i64,
    kind: String,
    state: String,
    payload: Value,
    attempts: i32,
    max_attempts: i32,
    run_at: DateTime<Utc>,
    locked_at: Option<DateTime<Utc>>,
    locked_by: Option<String>,
    last_error: Option<String>,
    failed_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct JobJson {
    pub id: i64,
    pub kind: String,
    pub state: String,
    pub payload: Value,
    pub attempts: i32,
    pub max_attempts: i32,
    pub run_at: Timestamp,
    pub locked_at: Option<Timestamp>,
    pub locked_by: Option<String>,
    pub last_error: Option<String>,
    pub failed_at: Option<Timestamp>,
    pub created_at: Timestamp,
}

impl From<JobRow> for JobJson {
    fn from(j: JobRow) -> Self {
        Self {
            id: j.id,
            kind: j.kind,
            state: j.state,
            payload: j.payload,
            attempts: j.attempts,
            max_attempts: j.max_attempts,
            run_at: j.run_at.into(),
            locked_at: ts(j.locked_at),
            locked_by: j.locked_by,
            last_error: j.last_error,
            failed_at: ts(j.failed_at),
            created_at: j.created_at.into(),
        }
    }
}

fn select() -> String {
    format!(
        "SELECT id, kind, {} AS state, payload, attempts, max_attempts, run_at, locked_at,
                locked_by, last_error, failed_at, created_at FROM jobs",
        state_expr()
    )
}

#[derive(Debug, Default, Deserialize)]
pub struct ListParams {
    /// `pending` | `scheduled` | `running` | `failed` (default: all)
    pub state: Option<String>,
    pub kind: Option<String>,
}

/// `GET /_bgh/admin/jobs` → newest first.
pub async fn list(
    State(state): State<AppState>,
    _auth: RequireSiteAdmin,
    p: Pagination,
    Query(q): Query<ListParams>,
) -> ApiResult<Page<JobJson>> {
    if let Some(s) = &q.state
        && !matches!(s.as_str(), "pending" | "scheduled" | "running" | "failed")
    {
        return Err(ApiError::invalid_field(FieldError::invalid("Job", "state")));
    }
    let rows: Vec<JobRow> = sqlx::query_as(&format!(
        "SELECT * FROM ({}) j
          WHERE ($1::text IS NULL OR j.state = $1) AND ($2::text IS NULL OR j.kind = $2)
          ORDER BY j.id DESC LIMIT $3 OFFSET $4",
        select()
    ))
    .bind(&q.state)
    .bind(&q.kind)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    Ok(p.page(rows).map(JobJson::from))
}

async fn find(state: &AppState, id: i64) -> ApiResult<JobRow> {
    sqlx::query_as(&format!("{} WHERE id = $1", select()))
        .bind(id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::NotFound)
}

/// `GET /_bgh/admin/jobs/{id}`
pub async fn get(
    State(state): State<AppState>,
    _auth: RequireSiteAdmin,
    Path(id): Path<i64>,
) -> ApiResult<Json<JobJson>> {
    Ok(Json(find(&state, id).await?.into()))
}

#[derive(Debug, Serialize, FromRow)]
pub struct KindStats {
    pub kind: String,
    pub pending: i64,
    pub scheduled: i64,
    pub running: i64,
    pub failed: i64,
    pub oldest_pending_at: Option<DateTime<Utc>>,
}

/// `GET /_bgh/admin/jobs/stats` → totals and per-kind counts.
pub async fn stats(
    State(state): State<AppState>,
    _auth: RequireSiteAdmin,
) -> ApiResult<Json<Value>> {
    let rows: Vec<KindStats> = sqlx::query_as(&format!(
        "SELECT kind,
                count(*) FILTER (WHERE state = 'pending') AS pending,
                count(*) FILTER (WHERE state = 'scheduled') AS scheduled,
                count(*) FILTER (WHERE state = 'running') AS running,
                count(*) FILTER (WHERE state = 'failed') AS failed,
                min(run_at) FILTER (WHERE state = 'pending') AS oldest_pending_at
           FROM ({}) j GROUP BY kind ORDER BY kind",
        select()
    ))
    .fetch_all(&state.db)
    .await?;
    let sum = |f: fn(&KindStats) -> i64| rows.iter().map(f).sum::<i64>();
    let oldest = rows.iter().filter_map(|r| r.oldest_pending_at).min();
    Ok(Json(json!({
        "pending": sum(|r| r.pending),
        "scheduled": sum(|r| r.scheduled),
        "running": sum(|r| r.running),
        "failed": sum(|r| r.failed),
        "oldest_pending_at": oldest.map(Timestamp::from),
        "kinds": rows.iter().map(|r| json!({
            "kind": r.kind,
            "pending": r.pending,
            "scheduled": r.scheduled,
            "running": r.running,
            "failed": r.failed,
            "oldest_pending_at": r.oldest_pending_at.map(Timestamp::from),
        })).collect::<Vec<_>>(),
    })))
}

/// `POST /_bgh/admin/jobs/{id}/retry`: run a failed or scheduled job now
/// with a fresh attempt budget. 409 while it runs.
pub async fn retry(
    State(state): State<AppState>,
    auth: RequireSiteAdmin,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<JobJson>> {
    let job = find(&state, id).await?;
    if job.state == "running" {
        return Err(ApiError::conflict("The job is running."));
    }
    let mut tx = Tx::begin(&state).await?;
    sqlx::query(
        "UPDATE jobs SET failed_at = NULL, attempts = 0, run_at = now(),
                locked_at = NULL, locked_by = NULL WHERE id = $1",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("SELECT pg_notify($1, $2)")
        .bind(bgh_core::jobs::NOTIFY_CHANNEL)
        .bind(&job.kind)
        .execute(&mut *tx)
        .await?;
    log(
        &mut tx,
        &auth,
        &headers,
        "job.retry",
        Target::Site,
        json!({ "job_id": id, "kind": job.kind, "previous_state": job.state }),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(find(&state, id).await?.into()))
}

/// `POST /_bgh/admin/jobs/{id}/cancel` → 204: removes a job that isn't
/// running (pending, scheduled or failed). 409 while it runs.
pub async fn cancel(
    State(state): State<AppState>,
    auth: RequireSiteAdmin,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    let job = find(&state, id).await?;
    if job.state == "running" {
        return Err(ApiError::conflict("The job is running."));
    }
    let mut tx = Tx::begin(&state).await?;
    let deleted = sqlx::query(&format!(
        "DELETE FROM jobs WHERE id = $1
           AND NOT (failed_at IS NULL AND locked_at IS NOT NULL AND locked_at > now() - {LOCK_TIMEOUT})"
    ))
    .bind(id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if deleted == 0 {
        return Err(ApiError::conflict("The job is running."));
    }
    log(
        &mut tx,
        &auth,
        &headers,
        "job.cancel",
        Target::Site,
        json!({ "job_id": id, "kind": job.kind, "state": job.state, "payload": job.payload }),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Default, Deserialize)]
pub struct BulkParams {
    pub kind: Option<String>,
}

/// `POST /_bgh/admin/jobs/retry-failed[?kind=]` → `{"retried": n}`.
pub async fn retry_failed(
    State(state): State<AppState>,
    auth: RequireSiteAdmin,
    headers: HeaderMap,
    Query(q): Query<BulkParams>,
) -> ApiResult<Json<Value>> {
    let mut tx = Tx::begin(&state).await?;
    let n = sqlx::query(
        "UPDATE jobs SET failed_at = NULL, attempts = 0, run_at = now(),
                locked_at = NULL, locked_by = NULL
          WHERE failed_at IS NOT NULL AND ($1::text IS NULL OR kind = $1)",
    )
    .bind(&q.kind)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if n > 0 {
        sqlx::query("SELECT pg_notify($1, 'retry')")
            .bind(bgh_core::jobs::NOTIFY_CHANNEL)
            .execute(&mut *tx)
            .await?;
        log(
            &mut tx,
            &auth,
            &headers,
            "job.retry_failed",
            Target::Site,
            json!({ "kind": q.kind, "count": n }),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(Json(json!({ "retried": n })))
}
