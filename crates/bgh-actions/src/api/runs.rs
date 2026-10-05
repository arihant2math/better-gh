//! Workflow runs and jobs:
//! `/repos/{o}/{r}/actions/runs[...]`, `/actions/workflows/{id}/runs`,
//! `/actions/jobs/{job_id}[/logs|/rerun]`.

use std::collections::HashSet;
use std::io::Write;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use bgh_core::pagination::Pagination;
use bgh_core::prelude::*;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{Postgres, QueryBuilder};

use super::wrapped;
use crate::engine;
use crate::json::{job_json, runs_json};
use crate::models::{JobRow, RunRow};

#[derive(Debug, Default, Deserialize)]
pub struct RunFilters {
    pub actor: Option<String>,
    pub branch: Option<String>,
    pub event: Option<String>,
    pub status: Option<String>,
    pub created: Option<String>,
    pub head_sha: Option<String>,
    pub check_suite_id: Option<i64>,
    pub exclude_pull_requests: Option<bool>,
}

const STATUSES: &[&str] = &[
    "requested",
    "queued",
    "in_progress",
    "completed",
    "waiting",
    "pending",
];
const CONCLUSIONS: &[&str] = &[
    "success",
    "failure",
    "neutral",
    "cancelled",
    "skipped",
    "timed_out",
    "action_required",
    "startup_failure",
    "stale",
];

/// Parse GitHub's `created` filter: `>=D`, `>D`, `<=D`, `<D`, `D..D`, `D`.
fn push_created(qb: &mut QueryBuilder<'_, Postgres>, created: &str) -> ApiResult<()> {
    let parse = |s: &str| -> ApiResult<chrono::DateTime<chrono::Utc>> {
        let s = s.trim();
        if let Ok(d) = chrono::DateTime::parse_from_rfc3339(s) {
            return Ok(d.with_timezone(&chrono::Utc));
        }
        chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
            .map(|d| d.and_hms_opt(0, 0, 0).expect("midnight").and_utc())
            .map_err(|_| ApiError::invalid_field(FieldError::invalid("WorkflowRun", "created")))
    };
    let day = chrono::Duration::days(1);
    let is_date = |s: &str| s.trim().len() == 10;
    if let Some((a, b)) = created.split_once("..") {
        if a != "*" {
            qb.push(" AND created_at >= ").push_bind(parse(a)?);
        }
        if b != "*" {
            let end = parse(b)?;
            qb.push(" AND created_at < ").push_bind(if is_date(b) {
                end + day
            } else {
                end + chrono::Duration::seconds(1)
            });
        }
    } else if let Some(d) = created.strip_prefix(">=") {
        qb.push(" AND created_at >= ").push_bind(parse(d)?);
    } else if let Some(d) = created.strip_prefix('>') {
        let t = parse(d)?;
        qb.push(" AND created_at >= ").push_bind(if is_date(d) {
            t + day
        } else {
            t + chrono::Duration::seconds(1)
        });
    } else if let Some(d) = created.strip_prefix("<=") {
        let t = parse(d)?;
        qb.push(" AND created_at < ").push_bind(if is_date(d) {
            t + day
        } else {
            t + chrono::Duration::seconds(1)
        });
    } else if let Some(d) = created.strip_prefix('<') {
        qb.push(" AND created_at < ").push_bind(parse(d)?);
    } else {
        let t = parse(created)?;
        qb.push(" AND created_at >= ").push_bind(t);
        qb.push(" AND created_at < ").push_bind(t + day);
    }
    Ok(())
}

async fn list_runs(
    state: &AppState,
    access: &RepoAccess,
    p: &Pagination,
    f: &RunFilters,
    workflow_id: Option<i64>,
) -> ApiResult<Response> {
    let actor_id = match &f.actor {
        Some(login) => match db::User::find_by_login(&state.db, login).await? {
            Some(u) => Some(u.id),
            None => return Ok(wrapped(p, 0, "workflow_runs", Vec::<Value>::new())),
        },
        None => None,
    };
    let build = |select: &str| -> ApiResult<QueryBuilder<'static, Postgres>> {
        let mut qb = QueryBuilder::new(format!(
            "SELECT {select} FROM actions_runs WHERE repo_id = "
        ));
        qb.push_bind(access.repo.id);
        if let Some(w) = workflow_id {
            qb.push(" AND workflow_id = ").push_bind(w);
        }
        if let Some(a) = actor_id {
            qb.push(" AND (actor_id = ").push_bind(a);
            qb.push(" OR triggering_actor_id = ").push_bind(a).push(")");
        }
        if let Some(b) = &f.branch {
            qb.push(" AND head_branch = ").push_bind(b.clone());
        }
        if let Some(e) = &f.event {
            qb.push(" AND event = ").push_bind(e.clone());
        }
        if let Some(s) = &f.status {
            if STATUSES.contains(&s.as_str()) {
                qb.push(" AND status = ").push_bind(s.clone());
            } else if CONCLUSIONS.contains(&s.as_str()) {
                qb.push(" AND conclusion = ").push_bind(s.clone());
            } else {
                return Err(ApiError::invalid_field(FieldError::invalid(
                    "WorkflowRun",
                    "status",
                )));
            }
        }
        if let Some(sha) = &f.head_sha {
            qb.push(" AND head_sha = ").push_bind(sha.clone());
        }
        if let Some(cs) = f.check_suite_id {
            qb.push(" AND check_suite_id = ").push_bind(cs);
        }
        if f.exclude_pull_requests == Some(true) {
            qb.push(" AND NOT event LIKE 'pull_request%'");
        }
        if let Some(c) = &f.created {
            push_created(&mut qb, c)?;
        }
        Ok(qb)
    };
    let total: i64 = build("count(*)")?
        .build_query_scalar()
        .fetch_one(&state.db)
        .await?;
    let mut qb = build(RunRow::COLUMNS)?;
    qb.push(" ORDER BY id DESC LIMIT ")
        .push_bind(p.limit())
        .push(" OFFSET ")
        .push_bind(p.offset());
    let rows: Vec<RunRow> = qb.build_query_as().fetch_all(&state.db).await?;
    let items = runs_json(state, access, &rows).await?;
    Ok(wrapped(p, total, "workflow_runs", items))
}

/// `GET /repos/{owner}/{repo}/actions/runs`
pub async fn list(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo)): Path<(String, String)>,
    Query(f): Query<RunFilters>,
) -> ApiResult<Response> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    list_runs(&state, &access, &p, &f, None).await
}

/// `GET /repos/{owner}/{repo}/actions/workflows/{workflow_id}/runs`
pub async fn list_for_workflow(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo, id)): Path<(String, String, String)>,
    Query(f): Query<RunFilters>,
) -> ApiResult<Response> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let wf = super::workflows::find_workflow(&state, &access, &id).await?;
    list_runs(&state, &access, &p, &f, Some(wf.id)).await
}

pub async fn load_run(state: &AppState, access: &RepoAccess, run_id: i64) -> ApiResult<RunRow> {
    RunRow::find(&state.db, run_id)
        .await?
        .filter(|r| r.repo_id == access.repo.id)
        .ok_or(ApiError::NotFound)
}

pub async fn load_job(state: &AppState, access: &RepoAccess, job_id: i64) -> ApiResult<JobRow> {
    JobRow::find(&state.db, job_id)
        .await?
        .filter(|j| j.repo_id == access.repo.id)
        .ok_or(ApiError::NotFound)
}

/// `GET /repos/{owner}/{repo}/actions/runs/{run_id}`
pub async fn get(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, run_id)): Path<(String, String, i64)>,
) -> ApiResult<Json<Value>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let run = load_run(&state, &access, run_id).await?;
    let mut v = runs_json(&state, &access, std::slice::from_ref(&run)).await?;
    Ok(Json(v.remove(0)))
}

/// `GET /repos/{owner}/{repo}/actions/runs/{run_id}/attempts/{attempt}`
pub async fn get_attempt(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, run_id, attempt)): Path<(String, String, i64, i32)>,
) -> ApiResult<Json<Value>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let mut run = load_run(&state, &access, run_id).await?;
    if attempt < 1 || attempt > run.run_attempt {
        return Err(ApiError::NotFound);
    }
    if attempt < run.run_attempt {
        // Reconstruct the attempt's status from its jobs.
        let rows: Vec<(Option<String>,)> = sqlx::query_as(
            "SELECT conclusion FROM actions_jobs WHERE run_id = $1 AND run_attempt = $2",
        )
        .bind(run.id)
        .bind(attempt)
        .fetch_all(&state.db)
        .await?;
        let c = |x: &str| rows.iter().any(|(c,)| c.as_deref() == Some(x));
        run.status = "completed".into();
        run.conclusion = Some(
            if c("failure") {
                "failure"
            } else if c("cancelled") {
                "cancelled"
            } else {
                "success"
            }
            .into(),
        );
        run.run_attempt = attempt;
    }
    let mut v = runs_json(&state, &access, std::slice::from_ref(&run)).await?;
    Ok(Json(v.remove(0)))
}

/// `DELETE /repos/{owner}/{repo}/actions/runs/{run_id}`
pub async fn delete(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, run_id)): Path<(String, String, i64)>,
) -> ApiResult<StatusCode> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    access.require(Permission::Write)?;
    let run = load_run(&state, &access, run_id).await?;
    if run.status != "completed" {
        return Err(ApiError::forbidden(
            "Cannot delete a workflow run that is in progress.",
        ));
    }
    let job_ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM actions_jobs WHERE run_id = $1")
        .bind(run.id)
        .fetch_all(&state.db)
        .await?;
    let art_ids: Vec<i64> =
        sqlx::query_scalar("SELECT id FROM actions_artifacts WHERE run_id = $1")
            .bind(run.id)
            .fetch_all(&state.db)
            .await?;
    let mut tx = Tx::begin(&state).await?;
    sqlx::query("DELETE FROM actions_runs WHERE id = $1")
        .bind(run.id)
        .execute(&mut *tx)
        .await?;
    tx.sync(
        &access.scope(),
        "workflow_run",
        run.id,
        SyncAction::Delete,
        &json!({"id": run.id}),
    )
    .await?;
    tx.commit().await?;
    for id in job_ids {
        crate::logs::delete_job(&state, id).await;
    }
    for id in art_ids {
        let _ = tokio::fs::remove_file(crate::server::artifact_path(&state, id)).await;
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn writable_run(
    state: &AppState,
    auth: &AuthContext,
    owner: &str,
    repo: &str,
    run_id: i64,
) -> ApiResult<(RepoAccess, RunRow)> {
    let access = RepoAccess::load(state, Some(auth), owner, repo).await?;
    access.require(Permission::Write)?;
    let run = load_run(state, &access, run_id).await?;
    Ok((access, run))
}

/// `POST .../runs/{run_id}/cancel` → 202
pub async fn cancel(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, run_id)): Path<(String, String, i64)>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let (_, run) = writable_run(&state, &auth, &owner, &repo, run_id).await?;
    if run.status == "completed" {
        return Err(ApiError::conflict(
            "Cannot cancel a workflow run that is completed.",
        ));
    }
    engine::cancel_run(&state, run.id, false)
        .await
        .map_err(ApiError::internal)?;
    Ok((StatusCode::ACCEPTED, Json(json!({}))))
}

/// `POST .../runs/{run_id}/force-cancel` → 202
pub async fn force_cancel(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, run_id)): Path<(String, String, i64)>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let (_, run) = writable_run(&state, &auth, &owner, &repo, run_id).await?;
    if run.status == "completed" {
        return Err(ApiError::conflict(
            "Cannot cancel a workflow run that is completed.",
        ));
    }
    engine::cancel_run(&state, run.id, true)
        .await
        .map_err(ApiError::internal)?;
    Ok((StatusCode::ACCEPTED, Json(json!({}))))
}

async fn do_rerun(
    state: &AppState,
    auth: &AuthContext,
    run: &RunRow,
    only: Option<HashSet<String>>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    if run.status != "completed" {
        return Err(ApiError::forbidden("This workflow is already running"));
    }
    if run.conclusion.as_deref() == Some("startup_failure") {
        return Err(ApiError::forbidden("This workflow run cannot be retried"));
    }
    engine::rerun(state, run.id, auth.user.id, only)
        .await
        .map_err(ApiError::internal)?;
    Ok((StatusCode::CREATED, Json(json!({}))))
}

/// `POST .../runs/{run_id}/rerun` → 201
pub async fn rerun(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, run_id)): Path<(String, String, i64)>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let (_, run) = writable_run(&state, &auth, &owner, &repo, run_id).await?;
    do_rerun(&state, &auth, &run, None).await
}

/// `POST .../runs/{run_id}/rerun-failed-jobs` → 201
pub async fn rerun_failed(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, run_id)): Path<(String, String, i64)>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let (_, run) = writable_run(&state, &auth, &owner, &repo, run_id).await?;
    let failed = engine::failed_job_keys(&state, &run)
        .await
        .map_err(ApiError::internal)?;
    if failed.is_empty() {
        return Err(ApiError::forbidden("There are no failed jobs to re-run"));
    }
    do_rerun(&state, &auth, &run, Some(failed)).await
}

/// `POST /repos/{owner}/{repo}/actions/jobs/{job_id}/rerun` → 201
pub async fn rerun_job(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, job_id)): Path<(String, String, i64)>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    access.require(Permission::Write)?;
    let job = load_job(&state, &access, job_id).await?;
    let run = load_run(&state, &access, job.run_id).await?;
    do_rerun(&state, &auth, &run, Some(HashSet::from([job.job_key]))).await
}

#[derive(Debug, Default, Deserialize)]
pub struct JobFilter {
    pub filter: Option<String>,
}

async fn list_jobs_inner(
    state: &AppState,
    access: &RepoAccess,
    p: &Pagination,
    run: &RunRow,
    attempt: Option<i32>,
) -> ApiResult<Response> {
    let attempt_filter = attempt;
    let total: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM actions_jobs WHERE run_id = $1 AND ($2::int IS NULL OR run_attempt = $2)",
    )
    .bind(run.id)
    .bind(attempt_filter)
    .fetch_one(&state.db)
    .await?;
    let rows: Vec<JobRow> = sqlx::query_as(&format!(
        "SELECT {} FROM actions_jobs WHERE run_id = $1 AND ($2::int IS NULL OR run_attempt = $2)
          ORDER BY run_attempt DESC, id LIMIT $3 OFFSET $4",
        JobRow::COLUMNS
    ))
    .bind(run.id)
    .bind(attempt_filter)
    .bind(p.limit())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let items: Vec<Value> = rows
        .iter()
        .map(|j| job_json(state, access, j, &run.name))
        .collect();
    Ok(wrapped(p, total, "jobs", items))
}

/// `GET .../runs/{run_id}/jobs?filter=latest|all`
pub async fn list_jobs(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo, run_id)): Path<(String, String, i64)>,
    Query(f): Query<JobFilter>,
) -> ApiResult<Response> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let run = load_run(&state, &access, run_id).await?;
    let attempt = match f.filter.as_deref() {
        None | Some("latest") => Some(run.run_attempt),
        Some("all") => None,
        Some(_) => {
            return Err(ApiError::invalid_field(FieldError::invalid(
                "WorkflowJob",
                "filter",
            )));
        }
    };
    list_jobs_inner(&state, &access, &p, &run, attempt).await
}

/// `GET .../runs/{run_id}/attempts/{attempt}/jobs`
pub async fn list_attempt_jobs(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo, run_id, attempt)): Path<(String, String, i64, i32)>,
) -> ApiResult<Response> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let run = load_run(&state, &access, run_id).await?;
    if attempt < 1 || attempt > run.run_attempt {
        return Err(ApiError::NotFound);
    }
    list_jobs_inner(&state, &access, &p, &run, Some(attempt)).await
}

/// `GET /repos/{owner}/{repo}/actions/jobs/{job_id}`
pub async fn get_job(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, job_id)): Path<(String, String, i64)>,
) -> ApiResult<Json<Value>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let job = load_job(&state, &access, job_id).await?;
    let name: String = sqlx::query_scalar("SELECT name FROM actions_runs WHERE id = $1")
        .bind(job.run_id)
        .fetch_one(&state.db)
        .await?;
    Ok(Json(job_json(&state, &access, &job, &name)))
}

// ---------------------------------------------------------------------------
// Logs
// ---------------------------------------------------------------------------

/// `GET .../actions/jobs/{job_id}/logs` → 302 to a short-lived download URL.
pub async fn job_logs(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, job_id)): Path<(String, String, i64)>,
) -> ApiResult<Response> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    if auth.0.is_none() {
        return Err(ApiError::requires_auth());
    }
    let job = load_job(&state, &access, job_id).await?;
    crate::web::redirect_download(&state, crate::web::Download::JobLog(job.log_owner())).await
}

/// `GET .../actions/runs/{run_id}/logs` → 302 to a zip of every job log.
pub async fn run_logs(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, run_id)): Path<(String, String, i64)>,
) -> ApiResult<Response> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    if auth.0.is_none() {
        return Err(ApiError::requires_auth());
    }
    let run = load_run(&state, &access, run_id).await?;
    crate::web::redirect_download(
        &state,
        crate::web::Download::RunLogs(run.id, run.run_attempt),
    )
    .await
}

/// `GET .../actions/runs/{run_id}/attempts/{attempt}/logs`
pub async fn attempt_logs(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, run_id, attempt)): Path<(String, String, i64, i32)>,
) -> ApiResult<Response> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    if auth.0.is_none() {
        return Err(ApiError::requires_auth());
    }
    let run = load_run(&state, &access, run_id).await?;
    if attempt < 1 || attempt > run.run_attempt {
        return Err(ApiError::NotFound);
    }
    crate::web::redirect_download(&state, crate::web::Download::RunLogs(run.id, attempt)).await
}

/// `DELETE .../actions/runs/{run_id}/logs` → 204
pub async fn delete_run_logs(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, run_id)): Path<(String, String, i64)>,
) -> ApiResult<StatusCode> {
    let (_, run) = writable_run(&state, &auth, &owner, &repo, run_id).await?;
    if run.status != "completed" {
        return Err(ApiError::forbidden(
            "Cannot delete logs of a run in progress.",
        ));
    }
    let ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM actions_jobs WHERE run_id = $1")
        .bind(run.id)
        .fetch_all(&state.db)
        .await?;
    for id in ids {
        crate::logs::delete_job(&state, id).await;
    }
    Ok(StatusCode::NO_CONTENT)
}

fn zip_name(s: &str) -> String {
    s.chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':' | '\0') {
                '_'
            } else {
                c
            }
        })
        .collect()
}

/// Build the run-logs zip: `{n}_{job}.txt` plus `{job}/{step}_{name}.txt`.
pub async fn build_run_logs_zip(
    state: &AppState,
    run_id: i64,
    attempt: i32,
) -> anyhow::Result<Vec<u8>> {
    let jobs: Vec<JobRow> = sqlx::query_as(&format!(
        "SELECT {} FROM actions_jobs WHERE run_id = $1 AND run_attempt = $2 ORDER BY id",
        JobRow::COLUMNS
    ))
    .bind(run_id)
    .bind(attempt)
    .fetch_all(&state.db)
    .await?;
    let mut entries: Vec<(String, String)> = Vec::new();
    for (i, job) in jobs.iter().enumerate() {
        let owner_id = job.log_owner();
        let name = zip_name(&job.name);
        entries.push((
            format!("{i}_{name}.txt"),
            crate::logs::read_job(state, owner_id).await,
        ));
        let steps: Vec<crate::models::StepState> =
            serde_json::from_value(job.steps.clone()).unwrap_or_default();
        for n in crate::logs::steps(state, owner_id).await {
            let step_name = steps
                .iter()
                .find(|s| s.number == n)
                .map(|s| zip_name(&s.name))
                .unwrap_or_else(|| "step".into());
            entries.push((
                format!("{name}/{n}_{step_name}.txt"),
                crate::logs::read_step(state, owner_id, n).await,
            ));
        }
    }
    tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<u8>> {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut buf);
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            for (name, content) in entries {
                zip.start_file(name, opts)?;
                zip.write_all(content.as_bytes())?;
            }
            zip.finish()?;
        }
        Ok(buf.into_inner())
    })
    .await?
}

/// Not supported: deployment approvals.
pub async fn pending_deployments(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, run_id)): Path<(String, String, i64)>,
) -> ApiResult<Response> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    load_run(&state, &access, run_id).await?;
    Ok(Json(json!([])).into_response())
}
