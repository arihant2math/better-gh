//! `GET /_bgh/admin/health`: database, Redis, storage, git, job queue,
//! uptime and version.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use axum::extract::State;
use bgh_core::prelude::*;
use serde_json::{Value, json};
use tokio::process::Command;

static STARTED: OnceLock<(Instant, chrono::DateTime<chrono::Utc>)> = OnceLock::new();

/// Record the process start (called from `register` at startup).
pub fn mark_started() {
    STARTED.get_or_init(|| (Instant::now(), chrono::Utc::now()));
}

const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

async fn timed<T, E: std::fmt::Display>(
    fut: impl std::future::Future<Output = Result<T, E>>,
) -> (Result<T, String>, f64) {
    let start = Instant::now();
    let res = match tokio::time::timeout(PROBE_TIMEOUT, fut).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err("timed out".to_string()),
    };
    (res, start.elapsed().as_secs_f64() * 1000.0)
}

async fn database(state: &AppState) -> Value {
    let (res, ms) =
        timed(sqlx::query_scalar::<_, String>("SELECT version()").fetch_one(&state.db)).await;
    let size: Option<i64> = sqlx::query_scalar("SELECT pg_database_size(current_database())")
        .fetch_one(&state.db)
        .await
        .ok();
    match res {
        Ok(version) => json!({
            "status": "ok",
            "latency_ms": ms,
            "version": version,
            "size_bytes": size,
            "pool": { "size": state.db.size(), "idle": state.db.num_idle() },
        }),
        Err(e) => json!({ "status": "error", "error": e, "latency_ms": ms }),
    }
}

async fn redis(state: &AppState) -> Value {
    let mut conn = state.redis.clone();
    let (res, ms) = timed(redis::cmd("PING").query_async::<String>(&mut conn)).await;
    match res {
        Ok(_) => {
            let info: Option<String> = redis::cmd("INFO")
                .arg("server")
                .query_async(&mut conn)
                .await
                .ok();
            let version = info.as_deref().and_then(|i| {
                i.lines()
                    .find_map(|l| l.strip_prefix("redis_version:"))
                    .map(|v| v.trim().to_string())
            });
            json!({ "status": "ok", "latency_ms": ms, "version": version })
        }
        Err(e) => json!({ "status": "error", "error": e, "latency_ms": ms }),
    }
}

/// Filesystem totals for `dir` via `df -Pk` (portable, no extra deps).
async fn filesystem(dir: &std::path::Path) -> Option<(i64, i64, i64)> {
    let out = tokio::time::timeout(
        PROBE_TIMEOUT,
        Command::new("df")
            .arg("-Pk")
            .arg(dir)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().nth(1)?;
    let cols: Vec<&str> = line.split_whitespace().collect();
    let total: i64 = cols.get(1)?.parse().ok()?;
    let used: i64 = cols.get(2)?.parse().ok()?;
    let avail: i64 = cols.get(3)?.parse().ok()?;
    Some((total * 1024, used * 1024, avail * 1024))
}

async fn storage(state: &AppState) -> Value {
    let dir = &state.config.data_dir;
    let repos_kb: i64 =
        sqlx::query_scalar("SELECT coalesce(sum(size), 0)::bigint FROM repositories")
            .fetch_one(&state.db)
            .await
            .unwrap_or(0);
    let fs = filesystem(dir).await;
    let status = match fs {
        Some((total, _, avail)) if total > 0 && (avail as f64) / (total as f64) < 0.05 => "warning",
        Some(_) => "ok",
        None => "unknown",
    };
    json!({
        "status": status,
        "data_dir": dir.display().to_string(),
        "exists": dir.exists(),
        "repositories_bytes": repos_kb * 1024,
        "filesystem": fs.map(|(total, used, avail)| json!({
            "total_bytes": total, "used_bytes": used, "available_bytes": avail,
        })),
    })
}

async fn git(state: &AppState) -> Value {
    let res = tokio::time::timeout(
        PROBE_TIMEOUT,
        Command::new(&state.config.git_bin)
            .arg("--version")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .kill_on_drop(true)
            .output(),
    )
    .await;
    match res {
        Ok(Ok(out)) if out.status.success() => {
            let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
            json!({
                "status": "ok",
                "version": v.strip_prefix("git version ").unwrap_or(&v),
                "binary": state.config.git_bin,
            })
        }
        Ok(Ok(out)) => json!({
            "status": "error",
            "error": String::from_utf8_lossy(&out.stderr).trim(),
        }),
        Ok(Err(e)) => json!({ "status": "error", "error": e.to_string() }),
        Err(_) => json!({ "status": "error", "error": "timed out" }),
    }
}

async fn queue(state: &AppState) -> Value {
    let row: Result<(i64, i64, i64, Option<f64>), _> = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE failed_at IS NULL AND run_at <= now() AND locked_at IS NULL),
                count(*) FILTER (WHERE failed_at IS NULL AND locked_at IS NOT NULL),
                count(*) FILTER (WHERE failed_at IS NOT NULL),
                extract(epoch FROM now() - min(run_at) FILTER (
                    WHERE failed_at IS NULL AND run_at <= now() AND locked_at IS NULL))::float8
           FROM jobs",
    )
    .fetch_one(&state.db)
    .await;
    match row {
        Ok((ready, running, failed, oldest)) => json!({
            "status": if oldest.unwrap_or(0.0) > 600.0 { "warning" } else { "ok" },
            "depth": ready,
            "running": running,
            "failed": failed,
            "oldest_ready_age_secs": oldest,
            "workers": state.config.job_workers,
        }),
        Err(e) => json!({ "status": "error", "error": e.to_string() }),
    }
}

/// `GET /_bgh/admin/health` → 200 with per-component status; the top-level
/// `status` is `ok`, `degraded` (warnings) or `error`.
pub async fn health(
    State(state): State<AppState>,
    _auth: RequireSiteAdmin,
) -> ApiResult<Json<Value>> {
    let (db, rd, st, g, q) = tokio::join!(
        database(&state),
        redis(&state),
        storage(&state),
        git(&state),
        queue(&state)
    );
    let statuses =
        [&db, &rd, &st, &g, &q].map(|v| v["status"].as_str().unwrap_or("error").to_string());
    let overall = if statuses.iter().any(|s| s == "error") {
        "error"
    } else if statuses.iter().any(|s| s == "warning") {
        "degraded"
    } else {
        "ok"
    };
    let (uptime, started_at) = match STARTED.get() {
        Some((i, t)) => (i.elapsed().as_secs(), Some(Timestamp::from(*t))),
        None => (0, None),
    };
    Ok(Json(json!({
        "status": overall,
        "version": env!("CARGO_PKG_VERSION"),
        "site_name": state.config.site_name,
        "uptime_secs": uptime,
        "started_at": started_at,
        "database": db,
        "redis": rd,
        "storage": st,
        "git": g,
        "jobs": q,
    })))
}
