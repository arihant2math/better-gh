//! Workflow run orchestration (server side).
//!
//! * [`create_run`] inserts a run (+ check suite), applies `concurrency`,
//!   then [`advance_run`].
//! * [`advance_run`] is the scheduler: it materializes jobs whose `needs`
//!   completed (evaluating `if`, `strategy.matrix`, `runs-on`, ...) as
//!   queued job rows with check runs, applies fail-fast and max-parallel,
//!   and completes the run when every job completed. Idempotent; called
//!   after every job state change.
//! * Runners claim queued jobs through [`crate::server`]; the job's
//!   environment/containers are evaluated at claim time (secrets are only
//!   decrypted then).

use std::collections::{HashMap, HashSet};

use anyhow::Context as _;
use bgh_core::AppState;
use bgh_core::events::Event;
use bgh_core::jobs::JobPayload;
use bgh_core::models::db;
use bgh_core::prelude::{SyncAction, Tx};
use bgh_core::sync;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sqlx::PgConnection;

use crate::checks;
use crate::context::{self, RunInfo};
use crate::expr::{self, JobStatus, MapContext};
use crate::models::{JobRow, RunRow, StepState};
use crate::protocol::JobSpec;
use crate::workflow::{self, Container, Job, Workflow};

/// Default `timeout-minutes` of a job.
pub const DEFAULT_TIMEOUT_MINUTES: i64 = 360;

/// A job definition as stored in `actions_jobs.spec`: the runner spec plus
/// the parts evaluated when a runner claims the job (they may use secrets).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredJob {
    pub spec: JobSpec,
    pub workflow_env: IndexMap<String, String>,
    pub job_env: IndexMap<String, String>,
    pub container: Option<Container>,
    pub services: IndexMap<String, Container>,
}

/// Durable "re-evaluate this run" job (used when one run unblocks another).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdvanceRun {
    pub run_id: i64,
}

impl JobPayload for AdvanceRun {
    const KIND: &'static str = "actions.advance_run";
}

pub async fn advance_run_job(state: AppState, job: AdvanceRun) -> anyhow::Result<()> {
    advance_run(&state, job.run_id).await
}

// ---------------------------------------------------------------------------
// Client (sync) shapes
// ---------------------------------------------------------------------------

pub fn run_sync_json(r: &RunRow) -> Value {
    json!({
        "id": r.id, "repo_id": r.repo_id, "workflow_id": r.workflow_id,
        "run_number": r.run_number, "run_attempt": r.run_attempt, "name": r.name,
        "display_title": r.display_title, "event": r.event, "status": r.status,
        "conclusion": r.conclusion, "head_branch": r.head_branch, "head_sha": r.head_sha,
        "actor_id": r.actor_id, "created_at": r.created_at, "updated_at": r.updated_at,
    })
}

pub fn job_sync_json(j: &JobRow) -> Value {
    json!({
        "id": j.id, "run_id": j.run_id, "run_attempt": j.run_attempt, "name": j.name,
        "status": j.status, "conclusion": j.conclusion, "steps": j.steps,
        "started_at": j.started_at, "completed_at": j.completed_at,
    })
}

async fn sync_run(tx: &mut Tx, run: &RunRow) -> anyhow::Result<()> {
    tx.sync(
        &sync::repo_scope(run.repo_id),
        "workflow_run",
        run.id,
        SyncAction::Update,
        &run_sync_json(run),
    )
    .await
    .map_err(|e| anyhow::anyhow!("{e}"))
}

pub async fn sync_job(tx: &mut Tx, job: &JobRow, action: SyncAction) -> anyhow::Result<()> {
    tx.sync(
        &sync::repo_scope(job.repo_id),
        "workflow_job",
        job.id,
        action,
        &job_sync_json(job),
    )
    .await
    .map_err(|e| anyhow::anyhow!("{e}"))
}

// ---------------------------------------------------------------------------
// Run creation
// ---------------------------------------------------------------------------

/// A run to create.
pub struct NewRun {
    pub repo: db::Repository,
    pub owner: db::User,
    /// `.github/workflows/ci.yml`
    pub path: String,
    pub yaml: String,
    /// Parsed workflow, or the parse error (→ `startup_failure` run).
    pub def: Result<Workflow, String>,
    pub event: String,
    pub git_ref: String,
    pub head_branch: Option<String>,
    pub head_sha: String,
    pub head_repo_id: Option<i64>,
    pub actor_id: Option<i64>,
    pub payload: Value,
    pub inputs: Option<Value>,
    pub pull_request_ids: Vec<i64>,
}

/// Insert or refresh the `actions_workflows` row of a file; returns its id.
pub async fn ensure_workflow(
    conn: &mut PgConnection,
    repo_id: i64,
    path: &str,
    name: &str,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "INSERT INTO actions_workflows (repo_id, path, name) VALUES ($1, $2, $3)
         ON CONFLICT (repo_id, path) DO UPDATE SET
             state = CASE WHEN actions_workflows.state = 'deleted' THEN 'active'
                          ELSE actions_workflows.state END
         RETURNING id",
    )
    .bind(repo_id)
    .bind(path)
    .bind(name)
    .fetch_one(conn)
    .await
}

/// Workflow display name: `name:` or the file path.
pub fn workflow_name(def: Option<&Workflow>, path: &str) -> String {
    def.and_then(|d| d.name.clone())
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| path.to_string())
}

fn default_display_title(new: &NewRun, name: &str) -> String {
    let p = &new.payload;
    let title = match new.event.as_str() {
        "push" => p["head_commit"]["message"]
            .as_str()
            .and_then(|m| m.lines().next())
            .map(String::from),
        e if e.starts_with("pull_request") => p["pull_request"]["title"].as_str().map(String::from),
        "issues" => p["issue"]["title"].as_str().map(String::from),
        "release" => p["release"]["name"]
            .as_str()
            .or(p["release"]["tag_name"].as_str())
            .map(String::from),
        _ => None,
    };
    title
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| name.to_string())
}

/// Create a workflow run and schedule its first jobs. Returns the run id.
pub async fn create_run(state: &AppState, new: NewRun) -> anyhow::Result<i64> {
    let name = workflow_name(new.def.as_ref().ok(), &new.path);
    let actor = match new.actor_id {
        Some(id) => db::User::find(&state.db, id).await?,
        None => None,
    };
    let vars = crate::scoped::vars_for(state, &new.repo, None).await?;

    let mut tx = Tx::begin(state).await?;
    let workflow_id = ensure_workflow(&mut tx, new.repo.id, &new.path, &name).await?;
    let run_number: i64 = sqlx::query_scalar(
        "UPDATE actions_workflows SET next_run_number = next_run_number + 1, updated_at = now()
          WHERE id = $1 RETURNING next_run_number - 1",
    )
    .bind(workflow_id)
    .fetch_one(&mut *tx)
    .await?;
    let suite_id = checks::create_suite(
        &mut tx,
        new.repo.id,
        &new.head_sha,
        new.head_branch.as_deref(),
    )
    .await?;
    let (def_json, startup_error) = match &new.def {
        Ok(d) => (serde_json::to_value(d)?, None),
        Err(e) => (json!({}), Some(e.clone())),
    };
    let mut run: RunRow = sqlx::query_as(&format!(
        "INSERT INTO actions_runs (repo_id, workflow_id, run_number, name, display_title, event,
                                   status, ref, head_branch, head_sha, head_repo_id, actor_id,
                                   triggering_actor_id, check_suite_id, pull_request_ids,
                                   event_payload, workflow_yaml, workflow_def, inputs)
         VALUES ($1, $2, $3, $4, $5, $6, 'queued', $7, $8, $9, $10, $11, $11, $12, $13, $14,
                 $15, $16, $17)
         RETURNING {}",
        RunRow::COLUMNS
    ))
    .bind(new.repo.id)
    .bind(workflow_id)
    .bind(run_number)
    .bind(&name)
    .bind(default_display_title(&new, &name))
    .bind(&new.event)
    .bind(&new.git_ref)
    .bind(&new.head_branch)
    .bind(&new.head_sha)
    .bind(new.head_repo_id)
    .bind(new.actor_id)
    .bind(suite_id)
    .bind(&new.pull_request_ids)
    .bind(&new.payload)
    .bind(&new.yaml)
    .bind(&def_json)
    .bind(&new.inputs)
    .fetch_one(&mut *tx)
    .await?;

    if let Some(err) = startup_error {
        run = sqlx::query_as(&format!(
            "UPDATE actions_runs SET status = 'completed', conclusion = 'startup_failure',
                    display_title = $2, updated_at = now() WHERE id = $1 RETURNING {}",
            RunRow::COLUMNS
        ))
        .bind(run.id)
        .bind(format!("{}: {err}", new.path))
        .fetch_one(&mut *tx)
        .await?;
        checks::set_suite_status(
            &mut tx,
            Some(suite_id),
            "completed",
            Some("startup_failure"),
        )
        .await?;
        sync_run(&mut tx, &run).await?;
        tx.emit(Event::WorkflowRunUpdated {
            repo_id: run.repo_id,
            run_id: run.id,
            action: "completed".into(),
            actor_id: run.actor_id,
            // TODO(actions): GitHub REST JSON of the run/workflow for the
            // `workflow_run` webhook (bgh-notify skips the delivery while null).
            workflow_run: serde_json::Value::Null,
            workflow: None,
        });
        tx.commit().await?;
        return Ok(run.id);
    }
    let def = new.def.as_ref().expect("checked");

    // run-name and concurrency use github, inputs and vars.
    let info = RunInfo {
        repo: &new.repo,
        owner: &new.owner,
        actor: actor.as_ref(),
        triggering_actor: actor.as_ref(),
        workflow_path: &new.path,
    };
    let ctx = MapContext::new()
        .with("github", context::github_context(state, &run, &info, None))
        .with("inputs", new.inputs.clone().unwrap_or_else(|| json!({})))
        .with("vars", Value::Object(vars));
    if let Some(run_name) = &def.run_name
        && let Ok(title) = expr::interpolate(run_name, &ctx)
        && !title.trim().is_empty()
    {
        run.display_title = title;
        sqlx::query("UPDATE actions_runs SET display_title = $2 WHERE id = $1")
            .bind(run.id)
            .bind(&run.display_title)
            .execute(&mut *tx)
            .await?;
    }
    let mut pending = false;
    if let Some(conc) = &def.concurrency {
        let group = expr::interpolate(&conc.group, &ctx).unwrap_or_else(|_| conc.group.clone());
        let cancel_in_progress = eval_bool(&conc.cancel_in_progress, &ctx, false);
        if !group.is_empty() {
            sqlx::query("UPDATE actions_runs SET concurrency_group = $2 WHERE id = $1")
                .bind(run.id)
                .bind(&group)
                .execute(&mut *tx)
                .await?;
            run.concurrency_group = Some(group.clone());
            let others: Vec<(i64, String)> = sqlx::query_as(
                "SELECT id, status FROM actions_runs
                  WHERE repo_id = $1 AND concurrency_group = $2 AND status <> 'completed'
                    AND id <> $3 ORDER BY id",
            )
            .bind(run.repo_id)
            .bind(&group)
            .bind(run.id)
            .fetch_all(&mut *tx)
            .await?;
            // GitHub keeps at most one pending run per group (the newest);
            // with cancel-in-progress, running ones are cancelled too.
            let to_cancel: Vec<i64> = others
                .iter()
                .filter(|(_, status)| cancel_in_progress || status == "pending")
                .map(|(id, _)| *id)
                .collect();
            pending = !cancel_in_progress && others.iter().any(|(_, s)| s != "pending");
            if pending {
                sqlx::query("UPDATE actions_runs SET status = 'pending' WHERE id = $1")
                    .bind(run.id)
                    .execute(&mut *tx)
                    .await?;
                run.status = "pending".into();
            }
            for id in to_cancel {
                tx.enqueue(&CancelRun { run_id: id }).await?;
            }
        }
    }
    sync_run(&mut tx, &run).await?;
    tx.emit(Event::WorkflowRunUpdated {
        repo_id: run.repo_id,
        run_id: run.id,
        action: "requested".into(),
        actor_id: run.actor_id,
        // TODO(actions): GitHub REST JSON of the run/workflow for the
        // `workflow_run` webhook (bgh-notify skips the delivery while null).
        workflow_run: serde_json::Value::Null,
        workflow: None,
    });
    tx.commit().await?;
    if !pending {
        advance_run(state, run.id).await?;
    }
    Ok(run.id)
}

/// Durable cancellation (concurrency `cancel-in-progress`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelRun {
    pub run_id: i64,
}

impl JobPayload for CancelRun {
    const KIND: &'static str = "actions.cancel_run";
}

pub async fn cancel_run_job(state: AppState, job: CancelRun) -> anyhow::Result<()> {
    cancel_run(&state, job.run_id, false).await
}

// ---------------------------------------------------------------------------
// Expression helpers
// ---------------------------------------------------------------------------

/// Evaluate a bool-or-expression field.
pub fn eval_bool(v: &Option<Value>, ctx: &dyn expr::Context, default: bool) -> bool {
    match v {
        None | Some(Value::Null) => default,
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => expr::evaluate_template(s, ctx)
            .map(|v| match v {
                Value::String(s) => s.trim() == "true",
                other => expr::truthy(&other),
            })
            .unwrap_or(default),
        Some(other) => expr::truthy(other),
    }
}

/// Evaluate a number-or-expression field.
pub fn eval_i64(v: &Option<Value>, ctx: &dyn expr::Context) -> Option<i64> {
    let value = match v {
        None | Some(Value::Null) => return None,
        Some(Value::String(s)) => expr::evaluate_template(s, ctx).ok()?,
        Some(other) => other.clone(),
    };
    match value {
        Value::Number(n) => n.as_f64().map(|f| f.round() as i64),
        Value::String(s) => s.trim().parse::<f64>().ok().map(|f| f.round() as i64),
        _ => None,
    }
}

fn labels_of(v: &Value) -> Vec<String> {
    let mut out: Vec<String> = match v {
        Value::String(s) => vec![s.clone()],
        Value::Array(a) => a.iter().map(expr::to_display_string).collect(),
        Value::Object(o) => match o.get("labels") {
            Some(Value::String(s)) => vec![s.clone()],
            Some(Value::Array(a)) => a.iter().map(expr::to_display_string).collect(),
            _ => vec![],
        },
        _ => vec![],
    };
    for l in &mut out {
        *l = l.trim().to_ascii_lowercase();
    }
    out.retain(|l| !l.is_empty());
    out.dedup();
    out
}

fn matrix_suffix(m: &Map<String, Value>) -> String {
    m.values()
        .map(|v| match v {
            Value::Object(_) | Value::Array(_) => v.to_string(),
            other => expr::to_display_string(other),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

// ---------------------------------------------------------------------------
// Scheduler
// ---------------------------------------------------------------------------

struct RunData {
    repo: db::Repository,
    owner: db::User,
    actor: Option<db::User>,
    triggering: Option<db::User>,
    workflow_path: String,
    vars: Map<String, Value>,
    repo_html: String,
}

async fn load_run_data(state: &AppState, run: &RunRow) -> anyhow::Result<Option<RunData>> {
    let Some(repo) = db::Repository::find(&state.db, run.repo_id).await? else {
        return Ok(None);
    };
    let Some(owner) = db::User::find(&state.db, repo.owner_id).await? else {
        return Ok(None);
    };
    let users = db::User::find_many(
        &state.db,
        &[run.actor_id, run.triggering_actor_id]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>(),
    )
    .await?;
    let find = |id: Option<i64>| id.and_then(|id| users.iter().find(|u| u.id == id).cloned());
    let workflow_path: String =
        sqlx::query_scalar("SELECT path FROM actions_workflows WHERE id = $1")
            .bind(run.workflow_id)
            .fetch_optional(&state.db)
            .await?
            .unwrap_or_default();
    let vars = crate::scoped::vars_for(state, &repo, None).await?;
    Ok(Some(RunData {
        repo_html: state.urls.repo_html(&owner.login, &repo.name),
        actor: find(run.actor_id),
        triggering: find(run.triggering_actor_id),
        repo,
        owner,
        workflow_path,
        vars,
    }))
}

/// Result of a set of job rows (one job key, maybe a matrix).
fn aggregate_result(rows: &[&JobRow]) -> &'static str {
    let concl = |c: &str| rows.iter().any(|r| r.conclusion.as_deref() == Some(c));
    if rows
        .iter()
        .any(|r| r.conclusion.as_deref() == Some("failure") && !r.continue_on_error)
        || concl("timed_out")
    {
        "failure"
    } else if concl("cancelled") {
        "cancelled"
    } else if !rows.is_empty()
        && rows
            .iter()
            .all(|r| r.conclusion.as_deref() == Some("skipped"))
    {
        "skipped"
    } else {
        "success"
    }
}

fn needs_context(job: &Job, rows: &[JobRow]) -> Value {
    let mut needs = Map::new();
    for n in &job.needs {
        let mine: Vec<&JobRow> = rows.iter().filter(|r| &r.job_key == n).collect();
        let mut outputs = Map::new();
        for r in &mine {
            if let Value::Object(o) = &r.outputs {
                for (k, v) in o {
                    if !v.as_str().is_some_and(str::is_empty) || !outputs.contains_key(k) {
                        outputs.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        needs.insert(
            n.clone(),
            json!({"result": aggregate_result(&mine), "outputs": outputs}),
        );
    }
    Value::Object(needs)
}

/// Re-evaluate a run: materialize ready jobs, enforce strategy, finish.
pub async fn advance_run(state: &AppState, run_id: i64) -> anyhow::Result<()> {
    let mut tx = Tx::begin(state).await?;
    let run: Option<RunRow> = sqlx::query_as(&format!(
        "SELECT {} FROM actions_runs WHERE id = $1 FOR UPDATE",
        RunRow::COLUMNS
    ))
    .bind(run_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(mut run) = run else { return Ok(()) };
    if run.status == "completed" || run.status == "pending" {
        return Ok(());
    }
    let def: Workflow = match serde_json::from_value(run.workflow_def.clone()) {
        Ok(d) => d,
        Err(err) => {
            tracing::error!(run_id, ?err, "stored workflow definition unreadable");
            return finish_run(&mut tx, &mut run, "startup_failure")
                .await
                .and(Ok(tx.commit().await?));
        }
    };
    let Some(data) = load_run_data(state, &run).await? else {
        return Ok(());
    };
    let mut rows: Vec<JobRow> = sqlx::query_as(&format!(
        "SELECT {} FROM actions_jobs WHERE run_id = $1 AND run_attempt = $2 ORDER BY id",
        JobRow::COLUMNS
    ))
    .bind(run.id)
    .bind(run.run_attempt)
    .fetch_all(&mut *tx)
    .await?;

    let order = def.job_order();
    loop {
        let mut progressed = false;
        for key in &order {
            if rows.iter().any(|r| &r.job_key == key) {
                continue;
            }
            let job = &def.jobs[key];
            let ready = job.needs.iter().all(|n| {
                let mine: Vec<&JobRow> = rows.iter().filter(|r| &r.job_key == n).collect();
                !mine.is_empty() && mine.iter().all(|r| r.status == "completed")
            });
            if !ready {
                continue;
            }
            let new_rows = materialize(state, &mut tx, &run, &data, &def, key, job, &rows).await?;
            rows.extend(new_rows);
            progressed = true;
        }
        if !progressed {
            break;
        }
    }

    // fail-fast and max-parallel, per job key.
    for key in &order {
        let job = &def.jobs[key];
        let strategy = job.strategy.clone().unwrap_or_default();
        let ctx = MapContext::new();
        let has_matrix = strategy.matrix.is_some();
        let fail_fast = eval_bool(&strategy.fail_fast, &ctx, true);
        let failed = rows.iter().any(|r| {
            &r.job_key == key && r.conclusion.as_deref() == Some("failure") && !r.continue_on_error
        });
        if has_matrix && fail_fast && failed {
            let ids: Vec<i64> = rows
                .iter()
                .filter(|r| &r.job_key == key && r.status != "completed")
                .map(|r| r.id)
                .collect();
            for id in ids {
                cancel_job_row(&mut tx, &mut rows, id).await?;
            }
        }
        if let Some(max) = eval_i64(&strategy.max_parallel, &ctx).filter(|m| *m > 0) {
            let active = rows
                .iter()
                .filter(|r| {
                    &r.job_key == key && matches!(r.status.as_str(), "queued" | "in_progress")
                })
                .count() as i64;
            let promote: Vec<i64> = rows
                .iter()
                .filter(|r| &r.job_key == key && r.status == "pending")
                .take((max - active).max(0) as usize)
                .map(|r| r.id)
                .collect();
            for id in promote {
                let row: JobRow = sqlx::query_as(&format!(
                    "UPDATE actions_jobs SET status = 'queued', updated_at = now()
                      WHERE id = $1 RETURNING {}",
                    JobRow::COLUMNS
                ))
                .bind(id)
                .fetch_one(&mut *tx)
                .await?;
                sync_job(&mut tx, &row, SyncAction::Update).await?;
                if let Some(r) = rows.iter_mut().find(|r| r.id == id) {
                    *r = row;
                }
            }
        }
    }

    let all_materialized = order.iter().all(|k| rows.iter().any(|r| &r.job_key == k));
    let all_done = rows.iter().all(|r| r.status == "completed");
    if all_materialized && all_done {
        let conclusion = if run.cancel_requested {
            "cancelled"
        } else {
            let all: Vec<&JobRow> = rows.iter().collect();
            match aggregate_result(&all) {
                "failure" => "failure",
                "cancelled" => "cancelled",
                "skipped" => "skipped",
                _ => "success",
            }
        };
        finish_run(&mut tx, &mut run, conclusion).await?;
    } else {
        let started = rows.iter().any(|r| {
            r.status == "in_progress"
                || (r.status == "completed" && r.conclusion.as_deref() != Some("skipped"))
        });
        let status = if started { "in_progress" } else { "queued" };
        if run.status != status {
            run = sqlx::query_as(&format!(
                "UPDATE actions_runs SET status = $2, updated_at = now(),
                        run_started_at = coalesce(run_started_at, CASE WHEN $2 = 'in_progress' THEN now() END)
                  WHERE id = $1 RETURNING {}",
                RunRow::COLUMNS
            ))
            .bind(run.id)
            .bind(status)
            .fetch_one(&mut *tx)
            .await?;
            checks::set_suite_status(&mut tx, run.check_suite_id, status, None).await?;
            sync_run(&mut tx, &run).await?;
            if status == "in_progress" {
                tx.emit(Event::WorkflowRunUpdated {
                    repo_id: run.repo_id,
                    run_id: run.id,
                    action: "in_progress".into(),
                    actor_id: run.actor_id,
                    // TODO(actions): GitHub REST JSON of the run/workflow for the
                    // `workflow_run` webhook (bgh-notify skips the delivery while null).
                    workflow_run: serde_json::Value::Null,
                    workflow: None,
                });
            }
        }
    }
    tx.commit().await?;
    Ok(())
}

async fn finish_run(tx: &mut Tx, run: &mut RunRow, conclusion: &str) -> anyhow::Result<()> {
    *run = sqlx::query_as(&format!(
        "UPDATE actions_runs SET status = 'completed', conclusion = $2, updated_at = now()
          WHERE id = $1 RETURNING {}",
        RunRow::COLUMNS
    ))
    .bind(run.id)
    .bind(conclusion)
    .fetch_one(&mut **tx)
    .await?;
    let suite_conclusion = if conclusion == "skipped" {
        "skipped"
    } else {
        conclusion
    };
    checks::set_suite_status(tx, run.check_suite_id, "completed", Some(suite_conclusion)).await?;
    sync_run(tx, run).await?;
    tx.emit(Event::WorkflowRunUpdated {
        repo_id: run.repo_id,
        run_id: run.id,
        action: "completed".into(),
        actor_id: run.actor_id,
        // TODO(actions): GitHub REST JSON of the run/workflow for the
        // `workflow_run` webhook (bgh-notify skips the delivery while null).
        workflow_run: serde_json::Value::Null,
        workflow: None,
    });
    // Release the concurrency group: start the next pending run.
    if let Some(group) = &run.concurrency_group {
        let next: Option<i64> = sqlx::query_scalar(
            "UPDATE actions_runs SET status = 'queued', updated_at = now()
              WHERE id = (SELECT id FROM actions_runs
                           WHERE repo_id = $1 AND concurrency_group = $2 AND status = 'pending'
                           ORDER BY id LIMIT 1)
                AND NOT EXISTS (SELECT 1 FROM actions_runs
                                 WHERE repo_id = $1 AND concurrency_group = $2
                                   AND status IN ('queued', 'in_progress') AND id <> $3)
              RETURNING id",
        )
        .bind(run.repo_id)
        .bind(group)
        .bind(run.id)
        .fetch_optional(&mut **tx)
        .await?;
        if let Some(next) = next {
            tx.enqueue(&AdvanceRun { run_id: next }).await?;
        }
    }
    Ok(())
}

/// Cancel a not-yet-finished job row: queued/pending ones complete as
/// cancelled at once, running ones are asked to stop.
async fn cancel_job_row(tx: &mut Tx, rows: &mut [JobRow], id: i64) -> anyhow::Result<()> {
    let row: JobRow = sqlx::query_as(&format!(
        "UPDATE actions_jobs SET
             cancel_requested = true,
             status = CASE WHEN status IN ('queued', 'pending', 'waiting') THEN 'completed' ELSE status END,
             conclusion = CASE WHEN status IN ('queued', 'pending', 'waiting') THEN 'cancelled' ELSE conclusion END,
             completed_at = CASE WHEN status IN ('queued', 'pending', 'waiting') THEN now() ELSE completed_at END,
             updated_at = now()
          WHERE id = $1 RETURNING {}",
        JobRow::COLUMNS
    ))
    .bind(id)
    .fetch_one(&mut **tx)
    .await?;
    if row.status == "completed" && row.conclusion.as_deref() == Some("cancelled") {
        checks::complete_run(tx, row.check_run_id, "cancelled", None, &[]).await?;
        tx.emit(Event::WorkflowJobUpdated {
            repo_id: row.repo_id,
            run_id: row.run_id,
            job_id: row.id,
            action: "completed".into(),
        });
    }
    sync_job(tx, &row, SyncAction::Update).await?;
    if let Some(r) = rows.iter_mut().find(|r| r.id == id) {
        *r = row;
    }
    Ok(())
}

/// Steps list shown before a runner picks the job up.
pub fn initial_steps(steps: &[workflow::Step]) -> Vec<StepState> {
    let mut out = vec![StepState {
        number: 1,
        name: "Set up job".into(),
        status: "queued".into(),
        ..Default::default()
    }];
    for (i, s) in steps.iter().enumerate() {
        let name = s.name.clone().unwrap_or_else(|| match (&s.run, &s.uses) {
            (Some(run), _) => format!("Run {}", run.lines().next().unwrap_or("").trim()),
            (_, Some(uses)) => format!("Run {uses}"),
            _ => "Run".into(),
        });
        out.push(StepState {
            number: i as i64 + 2,
            name,
            status: "queued".into(),
            ..Default::default()
        });
    }
    out.push(StepState {
        number: steps.len() as i64 + 2,
        name: "Complete job".into(),
        status: "queued".into(),
        ..Default::default()
    });
    out
}

#[allow(clippy::too_many_arguments)]
async fn materialize(
    state: &AppState,
    tx: &mut Tx,
    run: &RunRow,
    data: &RunData,
    def: &Workflow,
    key: &str,
    job: &Job,
    rows: &[JobRow],
) -> anyhow::Result<Vec<JobRow>> {
    let info = RunInfo {
        repo: &data.repo,
        owner: &data.owner,
        actor: data.actor.as_ref(),
        triggering_actor: data.triggering.as_ref(),
        workflow_path: &data.workflow_path,
    };
    let github = context::github_context(state, run, &info, Some(key));
    let needs = needs_context(job, rows);
    let inputs = run.inputs.clone().unwrap_or_else(|| json!({}));
    let any_failed = needs
        .as_object()
        .map(|o| o.values().any(|v| v["result"] == "failure"))
        .unwrap_or(false);
    let any_unsuccessful = needs
        .as_object()
        .map(|o| o.values().any(|v| v["result"] != "success"))
        .unwrap_or(false);
    let status = if run.cancel_requested {
        JobStatus::Cancelled
    } else if any_failed {
        JobStatus::Failure
    } else {
        JobStatus::Success
    };
    let base_ctx = MapContext::new()
        .with("github", github.clone())
        .with("needs", needs.clone())
        .with("inputs", inputs.clone())
        .with("vars", Value::Object(data.vars.clone()))
        .with_status(status);

    // Job-level `if`. A skipped/cancelled (non-failed) dependency makes
    // success() false without making failure() true.
    let cond = job.r#if.clone().unwrap_or_default();
    let explicit_status = has_status_function(&cond);
    let run_it = if any_unsuccessful && !any_failed && !explicit_status {
        false
    } else if any_unsuccessful && !any_failed && explicit_status {
        // Evaluate with success() forced false: treat as cancelled-ish only
        // when the run was cancelled; otherwise success() must be false.
        let ctx = base_ctx.clone().with_status(if run.cancel_requested {
            JobStatus::Cancelled
        } else {
            JobStatus::Failure
        });
        let mut ok = expr::evaluate_condition(&cond, &ctx).unwrap_or(false);
        if !run.cancel_requested && cond.to_ascii_lowercase().contains("failure()") {
            // failure() would be wrongly true: re-check with only always().
            ok = expr::evaluate_condition(
                &cond.to_ascii_lowercase().replace("failure()", "false"),
                &ctx,
            )
            .unwrap_or(false);
        }
        ok
    } else {
        match expr::evaluate_condition(&cond, &base_ctx) {
            Ok(b) => b,
            Err(err) => {
                tracing::warn!(run_id = run.id, job = key, %err, "job if: evaluation failed");
                false
            }
        }
    };
    let display_name = job.name.clone().unwrap_or_else(|| key.to_string());

    if !run_it {
        let name = expr::interpolate(&display_name, &base_ctx).unwrap_or(display_name);
        let row = insert_job(
            tx,
            run,
            data,
            key,
            &name,
            None,
            "completed",
            Some("skipped"),
            &[],
            None,
            false,
            DEFAULT_TIMEOUT_MINUTES,
            &[],
        )
        .await?;
        return Ok(vec![row]);
    }

    if job.is_reusable_call() {
        let row = insert_job(
            tx,
            run,
            data,
            key,
            &display_name,
            None,
            "completed",
            Some("failure"),
            &[],
            None,
            false,
            DEFAULT_TIMEOUT_MINUTES,
            &[],
        )
        .await?;
        crate::logs::append(
            state,
            row.id,
            1,
            &format!(
                "##[error]Reusable workflow calls (`uses: {}`) are not supported by this server.",
                job.uses.as_deref().unwrap_or_default()
            ),
        )
        .await
        .ok();
        return Ok(vec![row]);
    }

    // Matrix expansion.
    let strategy = job.strategy.clone().unwrap_or_default();
    let combos: Vec<Option<Map<String, Value>>> = match &strategy.matrix {
        None => vec![None],
        Some(m) => {
            let evaluated = expr::evaluate_value(m, &base_ctx);
            match evaluated
                .map_err(|e| e.to_string())
                .and_then(|v| workflow::expand_matrix(&v).map_err(|e| e.to_string()))
            {
                Ok(c) if c.is_empty() => vec![],
                Ok(c) => c.into_iter().map(Some).collect(),
                Err(err) => {
                    let row = insert_job(
                        tx,
                        run,
                        data,
                        key,
                        &display_name,
                        None,
                        "completed",
                        Some("failure"),
                        &[],
                        None,
                        false,
                        DEFAULT_TIMEOUT_MINUTES,
                        &[],
                    )
                    .await?;
                    crate::logs::append(
                        state,
                        row.id,
                        1,
                        &format!("##[error]Invalid matrix: {err}"),
                    )
                    .await
                    .ok();
                    return Ok(vec![row]);
                }
            }
        }
    };
    if combos.is_empty() {
        let row = insert_job(
            tx,
            run,
            data,
            key,
            &display_name,
            None,
            "completed",
            Some("skipped"),
            &[],
            None,
            false,
            DEFAULT_TIMEOUT_MINUTES,
            &[],
        )
        .await?;
        return Ok(vec![row]);
    }
    let total = combos.len();
    let max_parallel = eval_i64(&strategy.max_parallel, &base_ctx).filter(|m| *m > 0);
    let fail_fast = eval_bool(&strategy.fail_fast, &base_ctx, true);
    let mut out = Vec::with_capacity(total);
    for (index, combo) in combos.into_iter().enumerate() {
        let matrix = combo
            .clone()
            .map(Value::Object)
            .unwrap_or_else(|| json!({}));
        let strategy_ctx = json!({
            "fail-fast": fail_fast,
            "job-index": index,
            "job-total": total,
            "max-parallel": max_parallel.unwrap_or(total as i64),
        });
        let ctx = base_ctx
            .clone()
            .with("matrix", matrix.clone())
            .with("strategy", strategy_ctx.clone());
        let mut name = expr::interpolate(&display_name, &ctx).unwrap_or(display_name.clone());
        if let Some(m) = &combo
            && !display_name.contains("matrix.")
            && !m.is_empty()
        {
            name = format!("{name} ({})", matrix_suffix(m));
        }
        let labels = labels_of(&expr::evaluate_value(&job.runs_on, &ctx).unwrap_or(Value::Null));
        let timeout = eval_i64(&job.timeout_minutes, &ctx)
            .filter(|t| *t > 0)
            .unwrap_or(DEFAULT_TIMEOUT_MINUTES);
        let continue_on_error = eval_bool(&job.continue_on_error, &ctx, false);
        let environment = job.environment.as_ref().and_then(|e| {
            let v = expr::evaluate_value(e, &ctx).ok()?;
            match v {
                Value::String(s) => Some(s),
                Value::Object(o) => o.get("name").and_then(|n| n.as_str()).map(String::from),
                _ => None,
            }
        });
        let defaults = job
            .defaults
            .as_ref()
            .and_then(|d| d.run.clone())
            .or_else(|| def.defaults.as_ref().and_then(|d| d.run.clone()))
            .map(|mut d| {
                // Merge: job-level fields win, workflow-level fill gaps.
                if let Some(wf) = def.defaults.as_ref().and_then(|d| d.run.as_ref()) {
                    d.shell = d.shell.or_else(|| wf.shell.clone());
                    d.working_directory =
                        d.working_directory.or_else(|| wf.working_directory.clone());
                }
                d
            })
            .unwrap_or_default();
        let spec = JobSpec {
            job_id: 0,
            run_id: run.id,
            run_number: run.run_number,
            run_attempt: run.run_attempt,
            job_key: key.to_string(),
            name: name.clone(),
            workflow_name: run.name.clone(),
            workflow_path: data.workflow_path.clone(),
            repository: format!("{}/{}", data.owner.login, data.repo.name),
            repository_id: data.repo.id,
            repository_owner: data.owner.login.clone(),
            server_url: state.config.base_url.clone(),
            api_url: state.config.api_url(),
            token: String::new(),
            github: github.clone(),
            env: IndexMap::new(),
            vars: Value::Object(data.vars.clone()),
            secrets: IndexMap::new(),
            matrix,
            needs: needs.clone(),
            inputs: inputs.clone(),
            strategy: strategy_ctx,
            defaults,
            container: None,
            services: IndexMap::new(),
            steps: job.steps.clone(),
            outputs: job.outputs.clone(),
            timeout_minutes: timeout as u64,
            environment,
        };
        let stored = StoredJob {
            spec,
            workflow_env: def.env.clone(),
            job_env: job.env.clone(),
            container: job.container.clone(),
            services: job.services.clone(),
        };
        let queued_here = out
            .iter()
            .filter(|r: &&JobRow| r.status == "queued")
            .count() as i64;
        let status = match max_parallel {
            Some(m) if queued_here >= m => "pending",
            _ => "queued",
        };
        let steps = initial_steps(&job.steps);
        let row = insert_job(
            tx,
            run,
            data,
            key,
            &name,
            combo.map(Value::Object),
            status,
            None,
            &labels,
            Some(serde_json::to_value(&stored)?),
            continue_on_error,
            timeout,
            &steps,
        )
        .await?;
        out.push(row);
    }
    Ok(out)
}

fn has_status_function(cond: &str) -> bool {
    let c = cond.to_ascii_lowercase();
    ["success()", "failure()", "always()", "cancelled()"]
        .iter()
        .any(|f| c.contains(f))
}

#[allow(clippy::too_many_arguments)]
async fn insert_job(
    tx: &mut Tx,
    run: &RunRow,
    data: &RunData,
    key: &str,
    name: &str,
    matrix: Option<Value>,
    status: &str,
    conclusion: Option<&str>,
    labels: &[String],
    spec: Option<Value>,
    continue_on_error: bool,
    timeout: i64,
    steps: &[StepState],
) -> anyhow::Result<JobRow> {
    let check_status = if status == "completed" {
        "completed"
    } else {
        "queued"
    };
    let check_run_id = checks::create_run(
        tx,
        run.check_suite_id,
        run.repo_id,
        &run.head_sha,
        name,
        key,
        check_status,
        conclusion,
    )
    .await?;
    let completed = status == "completed";
    let row: JobRow = sqlx::query_as(&format!(
        "INSERT INTO actions_jobs (run_id, repo_id, run_attempt, job_key, name, matrix, status,
                                   conclusion, head_sha, head_branch, labels, check_run_id, spec,
                                   steps, continue_on_error, timeout_minutes, started_at,
                                   completed_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16,
                 CASE WHEN $17 THEN now() END, CASE WHEN $17 THEN now() END)
         RETURNING {}",
        JobRow::COLUMNS
    ))
    .bind(run.id)
    .bind(run.repo_id)
    .bind(run.run_attempt)
    .bind(key)
    .bind(name)
    .bind(matrix)
    .bind(status)
    .bind(conclusion)
    .bind(&run.head_sha)
    .bind(&run.head_branch)
    .bind(labels)
    .bind(check_run_id)
    .bind(spec)
    .bind(serde_json::to_value(steps)?)
    .bind(continue_on_error)
    .bind(timeout as i32)
    .bind(completed)
    .fetch_one(&mut **tx)
    .await?;
    let url = format!("{}/actions/runs/{}/job/{}", data.repo_html, run.id, row.id);
    checks::set_details_url(tx, check_run_id, &url).await?;
    sync_job(tx, &row, SyncAction::Insert).await?;
    tx.emit(Event::WorkflowJobUpdated {
        repo_id: row.repo_id,
        run_id: row.run_id,
        job_id: row.id,
        action: if completed { "completed" } else { "queued" }.into(),
    });
    Ok(row)
}

// ---------------------------------------------------------------------------
// Cancellation and re-runs
// ---------------------------------------------------------------------------

/// Cancel a run (`force`: also complete running jobs immediately).
pub async fn cancel_run(state: &AppState, run_id: i64, force: bool) -> anyhow::Result<()> {
    let mut tx = Tx::begin(state).await?;
    let run: Option<RunRow> = sqlx::query_as(&format!(
        "UPDATE actions_runs SET cancel_requested = true, updated_at = now()
          WHERE id = $1 AND status <> 'completed' RETURNING {}",
        RunRow::COLUMNS
    ))
    .bind(run_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(mut run) = run else { return Ok(()) };
    let mut rows: Vec<JobRow> = sqlx::query_as(&format!(
        "SELECT {} FROM actions_jobs WHERE run_id = $1 AND run_attempt = $2 AND status <> 'completed'",
        JobRow::COLUMNS
    ))
    .bind(run.id)
    .bind(run.run_attempt)
    .fetch_all(&mut *tx)
    .await?;
    let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
    for id in ids {
        cancel_job_row(&mut tx, &mut rows, id).await?;
    }
    if force {
        for r in rows.iter().filter(|r| r.status != "completed") {
            crate::server::finish_job_row(&mut tx, r, "cancelled", None, &[], None).await?;
        }
    }
    if run.status == "pending" {
        finish_run(&mut tx, &mut run, "cancelled").await?;
    }
    tx.commit().await?;
    advance_run(state, run_id).await
}

/// Re-run a completed run as a new attempt. `only`: job keys to re-run
/// (plus everything depending on them); other jobs are copied from the
/// previous attempt. `None` re-runs everything.
pub async fn rerun(
    state: &AppState,
    run_id: i64,
    actor_id: i64,
    only: Option<HashSet<String>>,
) -> anyhow::Result<()> {
    let mut tx = Tx::begin(state).await?;
    let run: RunRow = sqlx::query_as(&format!(
        "SELECT {} FROM actions_runs WHERE id = $1 FOR UPDATE",
        RunRow::COLUMNS
    ))
    .bind(run_id)
    .fetch_one(&mut *tx)
    .await?;
    anyhow::ensure!(run.status == "completed", "run is not completed");
    let def: Workflow =
        serde_json::from_value(run.workflow_def.clone()).context("stored workflow definition")?;
    let prev: Vec<JobRow> = sqlx::query_as(&format!(
        "SELECT {} FROM actions_jobs WHERE run_id = $1 AND run_attempt = $2 ORDER BY id",
        JobRow::COLUMNS
    ))
    .bind(run.id)
    .bind(run.run_attempt)
    .fetch_all(&mut *tx)
    .await?;

    let attempt = run.run_attempt + 1;
    let run: RunRow = sqlx::query_as(&format!(
        "UPDATE actions_runs SET run_attempt = $2, status = 'queued', conclusion = NULL,
                cancel_requested = false, triggering_actor_id = $3, run_started_at = now(),
                updated_at = now()
          WHERE id = $1 RETURNING {}",
        RunRow::COLUMNS
    ))
    .bind(run.id)
    .bind(attempt)
    .bind(actor_id)
    .fetch_one(&mut *tx)
    .await?;
    checks::set_suite_status(&mut tx, run.check_suite_id, "queued", None).await?;

    if let Some(only) = only {
        // Expand to dependents (transitively).
        let mut rerun_keys = only;
        loop {
            let before = rerun_keys.len();
            for (k, j) in &def.jobs {
                if j.needs.iter().any(|n| rerun_keys.contains(n)) {
                    rerun_keys.insert(k.clone());
                }
            }
            if rerun_keys.len() == before {
                break;
            }
        }
        let by_key: HashMap<&str, Vec<&JobRow>> = prev.iter().fold(HashMap::new(), |mut m, r| {
            m.entry(r.job_key.as_str()).or_default().push(r);
            m
        });
        for (key, rows) in by_key {
            if rerun_keys.contains(key) {
                continue;
            }
            for r in rows {
                sqlx::query(
                    "INSERT INTO actions_jobs (run_id, repo_id, run_attempt, job_key, name, matrix,
                         status, conclusion, head_sha, head_branch, labels, runner_id, runner_name,
                         check_run_id, spec, steps, outputs, continue_on_error, timeout_minutes,
                         logs_job_id, started_at, completed_at)
                     SELECT run_id, repo_id, $2, job_key, name, matrix, status, conclusion,
                            head_sha, head_branch, labels, runner_id, runner_name, check_run_id,
                            spec, steps, outputs, continue_on_error, timeout_minutes,
                            coalesce(logs_job_id, id), started_at, completed_at
                       FROM actions_jobs WHERE id = $1",
                )
                .bind(r.id)
                .bind(attempt)
                .execute(&mut *tx)
                .await?;
            }
        }
    }
    sync_run(&mut tx, &run).await?;
    tx.emit(Event::WorkflowRunUpdated {
        repo_id: run.repo_id,
        run_id: run.id,
        action: "requested".into(),
        actor_id: Some(actor_id),
        // TODO(actions): GitHub REST JSON of the run/workflow for the
        // `workflow_run` webhook (bgh-notify skips the delivery while null).
        workflow_run: serde_json::Value::Null,
        workflow: None,
    });
    tx.commit().await?;
    advance_run(state, run_id).await
}

/// Keys of jobs that failed or were cancelled in the run's latest attempt.
pub async fn failed_job_keys(state: &AppState, run: &RunRow) -> anyhow::Result<HashSet<String>> {
    let keys: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT job_key FROM actions_jobs
          WHERE run_id = $1 AND run_attempt = $2
            AND conclusion IN ('failure', 'cancelled', 'timed_out')",
    )
    .bind(run.id)
    .bind(run.run_attempt)
    .fetch_all(&state.db)
    .await?;
    Ok(keys.into_iter().collect())
}
