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
//! * Jobs calling reusable workflows become `call` rows whose called jobs
//!   are scheduled as a nested [`Scope`] (see [`crate::reusable`]).

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
use crate::protocol::{Annotation, JobSpec};
use crate::reusable::{self, FileCache, SecretsLayer, Source, StoredCall};
use crate::workflow::{self, Container, Job, Permissions, Workflow};

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
    /// Effective `permissions:` (job-level, else workflow-level); `None`
    /// takes the site default when the token is minted.
    #[serde(default)]
    pub permissions: Option<crate::workflow::Permissions>,
    /// Secret layers of the reusable workflow calls leading to this job
    /// (empty for jobs of the run's own workflow).
    #[serde(default)]
    pub secret_layers: Vec<SecretsLayer>,
    /// `permissions:` of the callers leading to this job (`None`: the
    /// default); the token gets at most their intersection.
    #[serde(default)]
    pub permission_caps: Vec<Option<Permissions>>,
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

/// `Event::WorkflowRunUpdated` carrying the run and its workflow as GitHub
/// REST JSON (the `workflow_run` webhook payload), rendered on `conn` so
/// changes of the surrounding transaction are visible.
pub async fn run_event(
    state: &AppState,
    conn: &mut PgConnection,
    run: &RunRow,
    action: &str,
) -> anyhow::Result<Event> {
    let mut workflow_run = Value::Null;
    let mut workflow = None;
    if let Some(repo) = db::Repository::find(&mut *conn, run.repo_id).await?
        && let Some(owner) = db::User::find(&mut *conn, repo.owner_id).await?
    {
        let access = bgh_core::perms::RepoAccess {
            repo,
            owner,
            permission: bgh_core::perms::Permission::Read,
            authenticated: false,
        };
        let rendered = crate::json::runs_json_conn(state, conn, &access, std::slice::from_ref(run))
            .await
            .map_err(|e| anyhow::anyhow!("rendering workflow run: {e}"))?;
        workflow_run = rendered.into_iter().next().unwrap_or(Value::Null);
        let wf: Option<crate::models::WorkflowRow> = sqlx::query_as(&format!(
            "SELECT {} FROM actions_workflows WHERE id = $1",
            crate::models::WorkflowRow::COLUMNS
        ))
        .bind(run.workflow_id)
        .fetch_optional(&mut *conn)
        .await?;
        workflow = wf.map(|w| crate::json::workflow_json(state, &access, &w));
    }
    Ok(Event::WorkflowRunUpdated {
        repo_id: run.repo_id,
        run_id: run.id,
        action: action.to_string(),
        actor_id: run.triggering_actor_id.or(run.actor_id),
        workflow_run,
        workflow,
    })
}

/// `Event::WorkflowJobUpdated` carrying the job as GitHub REST JSON (the
/// `workflow_job` webhook payload, same shape as `GET /actions/jobs/{id}`).
pub async fn job_event(tx: &mut Tx, job: &JobRow, action: &str) -> anyhow::Result<Event> {
    let state = tx.state().clone();
    let mut workflow_job = Value::Null;
    if let Some(repo) = db::Repository::find(&mut **tx, job.repo_id).await?
        && let Some(owner) = db::User::find(&mut **tx, repo.owner_id).await?
    {
        let access = bgh_core::perms::RepoAccess {
            repo,
            owner,
            permission: bgh_core::perms::Permission::Read,
            authenticated: false,
        };
        let name: Option<String> =
            sqlx::query_scalar("SELECT name FROM actions_runs WHERE id = $1")
                .bind(job.run_id)
                .fetch_optional(&mut **tx)
                .await?;
        workflow_job = crate::json::job_json(&state, &access, job, name.as_deref().unwrap_or(""));
    }
    Ok(Event::WorkflowJobUpdated {
        repo_id: job.repo_id,
        run_id: job.run_id,
        job_id: job.id,
        action: action.to_string(),
        workflow_job,
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
        if let Some(ev) =
            checks::check_suite_event(run.repo_id, Some(suite_id), "completed", run.actor_id)
        {
            tx.emit(ev);
        }
        sync_run(&mut tx, &run).await?;
        let ev = run_event(state, &mut tx, &run, "completed").await?;
        tx.emit(ev);
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
    let ev = run_event(state, &mut tx, &run, "requested").await?;
    tx.emit(ev);
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

/// Durable cancellation of one job or call row (job-level concurrency
/// `cancel-in-progress`, or a newer pending row replacing it).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelJob {
    pub job_id: i64,
}

impl JobPayload for CancelJob {
    const KIND: &'static str = "actions.cancel_job";
}

pub async fn cancel_job_job(state: AppState, job: CancelJob) -> anyhow::Result<()> {
    let mut tx = Tx::begin(&state).await?;
    let row: Option<JobRow> = sqlx::query_as(&format!(
        "SELECT {} FROM actions_jobs WHERE id = $1 AND status <> 'completed' FOR UPDATE",
        JobRow::COLUMNS
    ))
    .bind(job.job_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(row) = row else { return Ok(()) };
    cancel_job_row(&mut tx, &mut [], row.id).await?;
    tx.commit().await?;
    advance_run(&state, row.run_id).await
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

/// A workflow scheduled within a run: the run's own workflow, or a reusable
/// workflow called by an in-progress `call` row.
struct Scope {
    /// Job key prefix of the scope's jobs (`""` for the run's workflow).
    prefix: String,
    /// Name prefix of the scope's jobs (`"Build / "`).
    name_prefix: String,
    depth: u32,
    def: Workflow,
    /// File the workflow was read from (annotations of call errors).
    path: String,
    inputs: Value,
    /// Where `./` calls of this workflow resolve.
    source: Source,
    secret_layers: Vec<SecretsLayer>,
    permission_caps: Vec<Option<Permissions>>,
    /// The `call` row (`None` for the run's workflow).
    call_id: Option<i64>,
    /// The run or one of the calls leading here was cancelled.
    cancelled: bool,
    /// `github.job_workflow_sha` of called jobs.
    job_workflow_sha: Option<String>,
}

impl Scope {
    fn top(run: &RunRow, data: &RunData, def: Workflow) -> Scope {
        Scope {
            prefix: String::new(),
            name_prefix: String::new(),
            depth: 0,
            def,
            path: data.workflow_path.clone(),
            inputs: run.inputs.clone().unwrap_or_else(|| json!({})),
            source: Source {
                repo_id: data.repo.id,
                full_name: format!("{}/{}", data.owner.login, data.repo.name),
                git_ref: run.git_ref.clone(),
                sha: run.head_sha.clone(),
            },
            secret_layers: Vec::new(),
            permission_caps: Vec::new(),
            call_id: None,
            cancelled: run.cancel_requested,
            job_workflow_sha: None,
        }
    }

    fn of_call(run: &RunRow, row: &JobRow) -> Option<Scope> {
        let call: StoredCall = serde_json::from_value(row.spec.clone()?)
            .map_err(|err| tracing::error!(job_id = row.id, ?err, "unreadable call row"))
            .ok()?;
        Some(Scope {
            prefix: call.prefix,
            name_prefix: call.name_prefix,
            depth: call.depth,
            def: call.def,
            path: call.path,
            inputs: call.inputs,
            job_workflow_sha: Some(call.source.sha.clone()),
            source: call.source,
            secret_layers: call.secrets,
            permission_caps: call.permission_caps,
            call_id: Some(row.id),
            cancelled: run.cancel_requested || row.cancel_requested,
        })
    }

    fn key(&self, inner: &str) -> String {
        format!("{}{inner}", self.prefix)
    }

    /// Is `job_key` a direct job of this scope?
    fn owns(&self, job_key: &str) -> bool {
        job_key
            .strip_prefix(self.prefix.as_str())
            .is_some_and(|rest| !rest.contains('/'))
    }

    /// `needs`/`jobs`-style context: `{key: {result, outputs}}` of `keys`.
    fn results(&self, keys: &[String], rows: &[JobRow]) -> Value {
        let mut out = Map::new();
        for n in keys {
            let full = self.key(n);
            let mine: Vec<&JobRow> = rows.iter().filter(|r| r.job_key == full).collect();
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
            out.insert(
                n.clone(),
                json!({"result": aggregate_result(&mine), "outputs": outputs}),
            );
        }
        Value::Object(out)
    }
}

/// The run's workflow plus the scopes of its in-progress calls.
fn scopes_of(top: &Scope, run: &RunRow, rows: &[JobRow]) -> Vec<Scope> {
    let mut out = vec![Scope {
        prefix: top.prefix.clone(),
        name_prefix: top.name_prefix.clone(),
        depth: top.depth,
        def: top.def.clone(),
        path: top.path.clone(),
        inputs: top.inputs.clone(),
        source: top.source.clone(),
        secret_layers: Vec::new(),
        permission_caps: Vec::new(),
        call_id: None,
        cancelled: top.cancelled,
        job_workflow_sha: None,
    }];
    out.extend(
        rows.iter()
            .filter(|r| r.is_call() && r.status == "in_progress")
            .filter_map(|r| Scope::of_call(run, r)),
    );
    out
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
            return finish_run(state, &mut tx, &mut run, "startup_failure")
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
    let top = Scope::top(&run, &data, def);
    let mut cache = FileCache::new();
    loop {
        let mut progressed = false;
        let scopes = scopes_of(&top, &run, &rows);
        for scope in &scopes {
            for key in scope.def.job_order() {
                if rows.iter().any(|r| r.job_key == scope.key(&key)) {
                    continue;
                }
                let job = &scope.def.jobs[&key];
                let ready = job.needs.iter().all(|n| {
                    let full = scope.key(n);
                    let mine: Vec<&JobRow> = rows.iter().filter(|r| r.job_key == full).collect();
                    !mine.is_empty() && mine.iter().all(|r| r.status == "completed")
                });
                if !ready {
                    continue;
                }
                let new_rows = materialize(
                    state, &mut tx, &run, &data, scope, &key, job, &rows, &mut cache,
                )
                .await?;
                rows.extend(new_rows);
                progressed = true;
            }
        }
        // Calls whose jobs all completed complete themselves.
        for scope in &scopes {
            let Some(call_id) = scope.call_id else {
                continue;
            };
            let all_materialized = scope
                .def
                .jobs
                .keys()
                .all(|k| rows.iter().any(|r| r.job_key == scope.key(k)));
            if all_materialized
                && rows
                    .iter()
                    .filter(|r| scope.owns(&r.job_key))
                    .all(|r| r.status == "completed")
            {
                finish_call(state, &mut tx, &run, &data, scope, call_id, &mut rows).await?;
                progressed = true;
            }
        }
        for scope in &scopes {
            progressed |= apply_strategy(&mut tx, scope, &mut rows).await?;
        }
        if !progressed {
            break;
        }
    }

    // Jobs completed in this run free their concurrency groups.
    release_job_groups(state, &mut tx, &run).await?;

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
        finish_run(state, &mut tx, &mut run, conclusion).await?;
    } else {
        let started = rows.iter().any(|r| {
            !r.is_call()
                && (r.status == "in_progress"
                    || (r.status == "completed" && r.conclusion.as_deref() != Some("skipped")))
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
                let ev = run_event(state, &mut tx, &run, "in_progress").await?;
                tx.emit(ev);
            }
        }
    }
    tx.commit().await?;
    Ok(())
}

/// fail-fast and max-parallel (and job-level concurrency waits) for the
/// jobs of one scope. True when a row changed.
async fn apply_strategy(tx: &mut Tx, scope: &Scope, rows: &mut [JobRow]) -> anyhow::Result<bool> {
    let mut changed = false;
    for key in scope.def.job_order() {
        let full = scope.key(&key);
        let job = &scope.def.jobs[&key];
        let strategy = job.strategy.clone().unwrap_or_default();
        let ctx = MapContext::new();
        let has_matrix = strategy.matrix.is_some();
        let fail_fast = eval_bool(&strategy.fail_fast, &ctx, true);
        let failed = rows.iter().any(|r| {
            r.job_key == full && r.conclusion.as_deref() == Some("failure") && !r.continue_on_error
        });
        if has_matrix && fail_fast && failed {
            let ids: Vec<i64> = rows
                .iter()
                .filter(|r| r.job_key == full && r.status != "completed" && !r.cancel_requested)
                .map(|r| r.id)
                .collect();
            for id in ids {
                cancel_job_row(tx, rows, id).await?;
                changed = true;
            }
        }
        let max = eval_i64(&strategy.max_parallel, &ctx).filter(|m| *m > 0);
        let active = rows
            .iter()
            .filter(|r| r.job_key == full && matches!(r.status.as_str(), "queued" | "in_progress"))
            .count() as i64;
        let mut room = max.map(|m| (m - active).max(0));
        let pending: Vec<(i64, bool, Option<String>)> = rows
            .iter()
            .filter(|r| r.job_key == full && r.status == "pending")
            .map(|r| (r.id, r.is_call(), r.concurrency_group.clone()))
            .collect();
        for (id, is_call, group) in pending {
            if room == Some(0) {
                break;
            }
            if let Some(group) = &group
                && group_busy(tx, scope_repo(rows, id), group, id).await?
            {
                continue;
            }
            let row: JobRow = sqlx::query_as(&format!(
                "UPDATE actions_jobs SET status = $2, updated_at = now(),
                        started_at = CASE WHEN $3 THEN now() ELSE started_at END
                  WHERE id = $1 RETURNING {}",
                JobRow::COLUMNS
            ))
            .bind(id)
            .bind(if is_call { "in_progress" } else { "queued" })
            .bind(is_call)
            .fetch_one(&mut **tx)
            .await?;
            if !is_call {
                sync_job(tx, &row, SyncAction::Update).await?;
            }
            if let Some(r) = rows.iter_mut().find(|r| r.id == id) {
                *r = row;
            }
            room = room.map(|r| r - 1);
            changed = true;
        }
    }
    Ok(changed)
}

fn scope_repo(rows: &[JobRow], id: i64) -> i64 {
    rows.iter()
        .find(|r| r.id == id)
        .map(|r| r.repo_id)
        .unwrap_or_default()
}

/// Is a job-level concurrency group held by another active row?
async fn group_busy(
    conn: &mut PgConnection,
    repo_id: i64,
    group: &str,
    except: i64,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM actions_jobs
                         WHERE repo_id = $1 AND concurrency_group = $2 AND id <> $3
                           AND status IN ('queued', 'in_progress'))",
    )
    .bind(repo_id)
    .bind(group)
    .bind(except)
    .fetch_one(conn)
    .await
}

/// Complete a call row: result of its jobs and the called workflow's
/// outputs (its concurrency group is released by [`release_job_groups`]).
async fn finish_call(
    state: &AppState,
    tx: &mut Tx,
    run: &RunRow,
    data: &RunData,
    scope: &Scope,
    call_id: i64,
    rows: &mut [JobRow],
) -> anyhow::Result<()> {
    let mine: Vec<&JobRow> = rows.iter().filter(|r| scope.owns(&r.job_key)).collect();
    let conclusion = match aggregate_result(&mine) {
        "failure" => "failure",
        _ if scope.cancelled => "cancelled",
        other => other,
    };
    let keys: Vec<String> = scope.def.jobs.keys().cloned().collect();
    let info = RunInfo {
        repo: &data.repo,
        owner: &data.owner,
        actor: data.actor.as_ref(),
        triggering_actor: data.triggering.as_ref(),
        workflow_path: &data.workflow_path,
    };
    let ctx = MapContext::new()
        .with("github", context::github_context(state, run, &info, None))
        .with("inputs", scope.inputs.clone())
        .with("vars", Value::Object(data.vars.clone()))
        .with("jobs", scope.results(&keys, rows));
    let outputs = reusable::outputs(&scope.def, &ctx);
    let row: JobRow = sqlx::query_as(&format!(
        "UPDATE actions_jobs SET status = 'completed', conclusion = $2, outputs = $3,
                completed_at = now(), updated_at = now()
          WHERE id = $1 RETURNING {}",
        JobRow::COLUMNS
    ))
    .bind(call_id)
    .bind(conclusion)
    .bind(Value::Object(outputs))
    .fetch_one(&mut **tx)
    .await?;
    if let Some(r) = rows.iter_mut().find(|r| r.id == call_id) {
        *r = row;
    }
    Ok(())
}

async fn finish_run(
    state: &AppState,
    tx: &mut Tx,
    run: &mut RunRow,
    conclusion: &str,
) -> anyhow::Result<()> {
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
    if let Some(ev) = checks::check_suite_event(
        run.repo_id,
        run.check_suite_id,
        "completed",
        run.triggering_actor_id.or(run.actor_id),
    ) {
        tx.emit(ev);
    }
    sync_run(tx, run).await?;
    let ev = run_event(state, tx, run, "completed").await?;
    tx.emit(ev);
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

/// Cancel one not-yet-finished row: queued/pending ones complete as
/// cancelled at once, running ones are asked to stop.
async fn cancel_one(tx: &mut Tx, rows: &mut [JobRow], id: i64) -> anyhow::Result<JobRow> {
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
    if !row.is_call() {
        if row.status == "completed" && row.conclusion.as_deref() == Some("cancelled") {
            checks::complete_run(tx, row.check_run_id, "cancelled", None, &[]).await?;
            if let Some(ev) =
                checks::check_run_event(row.repo_id, row.check_run_id, "completed", None)
            {
                tx.emit(ev);
            }
            let ev = job_event(tx, &row, "completed").await?;
            tx.emit(ev);
        }
        sync_job(tx, &row, SyncAction::Update).await?;
    }
    if let Some(r) = rows.iter_mut().find(|r| r.id == id) {
        *r = row.clone();
    }
    Ok(row)
}

/// Cancel a not-yet-finished row; cancelling a call also cancels every job
/// of the called workflow (and the calls nested in it).
async fn cancel_job_row(tx: &mut Tx, rows: &mut [JobRow], id: i64) -> anyhow::Result<()> {
    let row = cancel_one(tx, rows, id).await?;
    if !row.is_call() {
        return Ok(());
    }
    let Some(prefix) = row
        .spec
        .as_ref()
        .and_then(|s| s.get("prefix"))
        .and_then(|p| p.as_str())
    else {
        return Ok(());
    };
    let nested: Vec<i64> = sqlx::query_scalar(
        "SELECT id FROM actions_jobs
          WHERE run_id = $1 AND run_attempt = $2 AND left(job_key, length($3)) = $3
            AND status <> 'completed' AND NOT cancel_requested
          ORDER BY id",
    )
    .bind(row.run_id)
    .bind(row.run_attempt)
    .bind(prefix)
    .fetch_all(&mut **tx)
    .await?;
    for id in nested {
        cancel_one(tx, rows, id).await?;
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

/// Materialize the rows of job `key` of `scope` (its `needs` completed).
#[allow(clippy::too_many_arguments)]
async fn materialize(
    state: &AppState,
    tx: &mut Tx,
    run: &RunRow,
    data: &RunData,
    scope: &Scope,
    key: &str,
    job: &Job,
    rows: &[JobRow],
    cache: &mut FileCache,
) -> anyhow::Result<Vec<JobRow>> {
    let info = RunInfo {
        repo: &data.repo,
        owner: &data.owner,
        actor: data.actor.as_ref(),
        triggering_actor: data.triggering.as_ref(),
        workflow_path: &data.workflow_path,
    };
    let mut github = context::github_context(state, run, &info, Some(key));
    if let Some(sha) = &scope.job_workflow_sha {
        github["job_workflow_sha"] = json!(sha);
    }
    let full_key = scope.key(key);
    let needs = scope.results(&job.needs, rows);
    let inputs = scope.inputs.clone();
    let any_failed = needs
        .as_object()
        .map(|o| o.values().any(|v| v["result"] == "failure"))
        .unwrap_or(false);
    let any_unsuccessful = needs
        .as_object()
        .map(|o| o.values().any(|v| v["result"] != "success"))
        .unwrap_or(false);
    let status = if scope.cancelled {
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
        let ctx = base_ctx.clone().with_status(if scope.cancelled {
            JobStatus::Cancelled
        } else {
            JobStatus::Failure
        });
        let mut ok = expr::evaluate_condition(&cond, &ctx).unwrap_or(false);
        if !scope.cancelled && cond.to_ascii_lowercase().contains("failure()") {
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
                tracing::warn!(run_id = run.id, job = %full_key, %err, "job if: evaluation failed");
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
            &full_key,
            &format!("{}{name}", scope.name_prefix),
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
                    let row = fail_job(
                        state,
                        tx,
                        run,
                        data,
                        &full_key,
                        &format!("{}{display_name}", scope.name_prefix),
                        None,
                        &format!("Invalid matrix: {err}"),
                        None,
                    )
                    .await?;
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
            &full_key,
            &format!("{}{display_name}", scope.name_prefix),
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
    let has_matrix = strategy.matrix.is_some();
    let max_parallel = eval_i64(&strategy.max_parallel, &base_ctx).filter(|m| *m > 0);
    let fail_fast = eval_bool(&strategy.fail_fast, &base_ctx, true);
    let mut out: Vec<JobRow> = Vec::with_capacity(total);
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
        let name = format!("{}{name}", scope.name_prefix);
        let active_here = out
            .iter()
            .filter(|r| matches!(r.status.as_str(), "queued" | "in_progress"))
            .count() as i64;
        let over_limit = max_parallel.is_some_and(|m| active_here >= m);

        if let Some(uses) = &job.uses {
            let call = CallSite {
                scope,
                key,
                job,
                uses,
                ctx: &ctx,
                name: &name,
                combo: combo.clone(),
                prefix: if has_matrix {
                    format!("{full_key}.{index}/")
                } else {
                    format!("{full_key}/")
                },
            };
            let row =
                materialize_call(state, tx, run, data, &call, over_limit, rows, cache).await?;
            out.push(row);
            continue;
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
        let def = &scope.def;
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
            job_key: full_key.clone(),
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
            token_permissions: IndexMap::new(),
        };
        let stored = StoredJob {
            spec,
            workflow_env: def.env.clone(),
            job_env: job.env.clone(),
            container: job.container.clone(),
            services: job.services.clone(),
            permissions: job.permissions.clone().or_else(|| def.permissions.clone()),
            secret_layers: scope.secret_layers.clone(),
            permission_caps: scope.permission_caps.clone(),
        };
        let status = if over_limit { "pending" } else { "queued" };
        let steps = initial_steps(&job.steps);
        let mut row = insert_job(
            tx,
            run,
            data,
            &full_key,
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
        if let Some(conc) = &job.concurrency {
            let group = expr::interpolate(&conc.group, &ctx).unwrap_or_else(|_| conc.group.clone());
            let cancel_in_progress = eval_bool(&conc.cancel_in_progress, &ctx, false);
            if !group.is_empty() {
                row = job_concurrency(tx, row, &group, cancel_in_progress).await?;
            }
        }
        out.push(row);
    }
    Ok(out)
}

/// One matrix instance of a job that calls a reusable workflow.
struct CallSite<'a> {
    scope: &'a Scope,
    key: &'a str,
    job: &'a Job,
    uses: &'a str,
    /// The caller's context (github, needs, inputs, vars, matrix, strategy).
    ctx: &'a MapContext,
    /// Full display name (`<scope name prefix><caller name>`).
    name: &'a str,
    combo: Option<Map<String, Value>>,
    /// Key prefix of the called jobs.
    prefix: String,
}

/// Resolve and validate a call; `Err(msg)` fails the calling job.
async fn prepare_call(
    state: &AppState,
    tx: &mut Tx,
    data: &RunData,
    call: &CallSite<'_>,
    rows: &[JobRow],
    cache: &mut FileCache,
) -> anyhow::Result<Result<StoredCall, String>> {
    let scope = call.scope;
    if scope.depth + 1 > reusable::MAX_DEPTH {
        return Ok(Err(format!(
            "error parsing called workflow \"{}\": job \"{}\" calls a reusable workflow nested deeper than the maximum of {} levels",
            call.uses,
            call.key,
            reusable::MAX_DEPTH
        )));
    }
    let resolved =
        match reusable::resolve(state, tx, cache, &data.repo, &scope.source, call.uses).await? {
            Ok(r) => r,
            Err(e) => return Ok(Err(e)),
        };
    let mut unique: HashSet<String> = rows
        .iter()
        .filter(|r| r.is_call())
        .filter_map(|r| {
            r.spec
                .as_ref()?
                .get("workflow_ref")?
                .as_str()
                .map(String::from)
        })
        .collect();
    unique.insert(resolved.workflow_ref.clone());
    if unique.len() > reusable::MAX_UNIQUE_WORKFLOWS {
        return Ok(Err(format!(
            "error parsing called workflow \"{}\": a run may call at most {} unique reusable workflows",
            call.uses,
            reusable::MAX_UNIQUE_WORKFLOWS
        )));
    }
    let mut with = IndexMap::new();
    for (k, v) in &call.job.with {
        match expr::evaluate_value(v, call.ctx) {
            Ok(v) => {
                with.insert(k.clone(), v);
            }
            Err(e) => return Ok(Err(format!("Invalid input, {k}: {e}"))),
        }
    }
    let inputs = match reusable::typed_inputs(&resolved.def, &with) {
        Ok(i) => i,
        Err(e) => return Ok(Err(e)),
    };
    let secrets_ctx: Map<String, Value> = ["matrix", "needs", "inputs", "strategy"]
        .into_iter()
        .filter_map(|k| Some((k.to_string(), call.ctx.contexts.get(k)?.clone())))
        .collect();
    let layer = match reusable::secrets_layer(&resolved.def, call.job.secrets.as_ref(), secrets_ctx)
    {
        Ok(l) => l,
        Err(e) => return Ok(Err(e)),
    };
    let mut secrets = scope.secret_layers.clone();
    secrets.push(layer);
    let mut permission_caps = scope.permission_caps.clone();
    permission_caps.push(
        call.job
            .permissions
            .clone()
            .or_else(|| scope.def.permissions.clone()),
    );
    Ok(Ok(StoredCall {
        uses: call.uses.to_string(),
        workflow_ref: resolved.workflow_ref,
        path: resolved.path,
        source: resolved.source,
        prefix: call.prefix.clone(),
        name_prefix: format!("{} / ", call.name),
        depth: scope.depth + 1,
        def: resolved.def,
        inputs,
        secrets,
        permission_caps,
    }))
}

/// Insert the `call` row of a calling job (or a failed job when the call
/// is invalid).
#[allow(clippy::too_many_arguments)]
async fn materialize_call(
    state: &AppState,
    tx: &mut Tx,
    run: &RunRow,
    data: &RunData,
    call: &CallSite<'_>,
    over_limit: bool,
    rows: &[JobRow],
    cache: &mut FileCache,
) -> anyhow::Result<JobRow> {
    let full_key = call.scope.key(call.key);
    let stored = match prepare_call(state, tx, data, call, rows, cache).await? {
        Ok(s) => s,
        Err(msg) => {
            return fail_job(
                state,
                tx,
                run,
                data,
                &full_key,
                call.name,
                call.combo.clone().map(Value::Object),
                &msg,
                Some(&call.scope.path),
            )
            .await;
        }
    };
    // Job-level concurrency of the calling job.
    let mut status = if over_limit { "pending" } else { "in_progress" };
    let mut group = None;
    if let Some(conc) = &call.job.concurrency {
        let g = expr::interpolate(&conc.group, call.ctx).unwrap_or_else(|_| conc.group.clone());
        if !g.is_empty() {
            lock_job_group(tx, run.repo_id, &g).await?;
            let cancel_in_progress = eval_bool(&conc.cancel_in_progress, call.ctx, false);
            let others: Vec<(i64, String)> = sqlx::query_as(
                "SELECT id, status FROM actions_jobs
                  WHERE repo_id = $1 AND concurrency_group = $2 AND status <> 'completed'
                  ORDER BY id",
            )
            .bind(run.repo_id)
            .bind(&g)
            .fetch_all(&mut **tx)
            .await?;
            // Like run concurrency: at most one pending row per group (the
            // newest); cancel-in-progress also cancels the active one.
            for (id, s) in &others {
                if cancel_in_progress || s == "pending" {
                    tx.enqueue(&CancelJob { job_id: *id }).await?;
                }
            }
            if !cancel_in_progress && others.iter().any(|(_, s)| s != "pending") {
                status = "pending";
            }
            group = Some(g);
        }
    }
    let row: JobRow = sqlx::query_as(&format!(
        "INSERT INTO actions_jobs (run_id, repo_id, run_attempt, job_key, name, matrix, status,
                                   head_sha, head_branch, spec, kind, concurrency_group,
                                   started_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 'call', $11,
                 CASE WHEN $7 = 'in_progress' THEN now() END)
         RETURNING {}",
        JobRow::COLUMNS
    ))
    .bind(run.id)
    .bind(run.repo_id)
    .bind(run.run_attempt)
    .bind(&full_key)
    .bind(call.name)
    .bind(call.combo.clone().map(Value::Object))
    .bind(status)
    .bind(&run.head_sha)
    .bind(&run.head_branch)
    .bind(serde_json::to_value(&stored)?)
    .bind(&group)
    .fetch_one(&mut **tx)
    .await?;
    Ok(row)
}

/// Insert a job that failed before it could run, with the error as its log
/// and as a check run annotation.
#[allow(clippy::too_many_arguments)]
async fn fail_job(
    state: &AppState,
    tx: &mut Tx,
    run: &RunRow,
    data: &RunData,
    key: &str,
    name: &str,
    matrix: Option<Value>,
    message: &str,
    path: Option<&str>,
) -> anyhow::Result<JobRow> {
    let row = insert_job(
        tx,
        run,
        data,
        key,
        name,
        matrix,
        "completed",
        Some("failure"),
        &[],
        None,
        false,
        DEFAULT_TIMEOUT_MINUTES,
        &[],
    )
    .await?;
    if let Some(path) = path {
        let annotation = Annotation {
            level: "failure".into(),
            message: message.to_string(),
            title: Some("Invalid workflow file".into()),
            path: Some(path.to_string()),
            ..Default::default()
        };
        checks::complete_run(
            tx,
            row.check_run_id,
            "failure",
            None,
            std::slice::from_ref(&annotation),
        )
        .await?;
    }
    crate::logs::append(state, row.id, 1, &format!("##[error]{message}"))
        .await
        .ok();
    Ok(row)
}

// ---------------------------------------------------------------------------
// Job-level concurrency
// ---------------------------------------------------------------------------

/// Serialize writers of one repository's concurrency group.
async fn lock_job_group(tx: &mut Tx, repo_id: i64, group: &str) -> anyhow::Result<()> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 26))")
        .bind(format!("actions-job-group:{repo_id}:{group}"))
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Apply `jobs.<id>.concurrency` to a just-inserted job: like GitHub, at
/// most one job of a group runs and one waits (`pending`, the newest);
/// older pending jobs are cancelled, and running ones too with
/// `cancel-in-progress`. The waiting job starts when the group frees up
/// ([`release_job_groups`]).
async fn job_concurrency(
    tx: &mut Tx,
    row: JobRow,
    group: &str,
    cancel_in_progress: bool,
) -> anyhow::Result<JobRow> {
    if row.status == "completed" {
        return Ok(row);
    }
    lock_job_group(tx, row.repo_id, group).await?;
    sqlx::query("UPDATE actions_jobs SET concurrency_group = $2 WHERE id = $1")
        .bind(row.id)
        .bind(group)
        .execute(&mut **tx)
        .await?;
    let others: Vec<(i64, String)> = sqlx::query_as(
        "SELECT id, status FROM actions_jobs
          WHERE repo_id = $1 AND concurrency_group = $2 AND status <> 'completed' AND id <> $3
          ORDER BY id",
    )
    .bind(row.repo_id)
    .bind(group)
    .bind(row.id)
    .fetch_all(&mut **tx)
    .await?;
    for (id, status) in &others {
        if cancel_in_progress || status == "pending" {
            tx.enqueue(&CancelJob { job_id: *id }).await?;
        }
    }
    let blocked = others.iter().any(|(_, s)| s != "pending");
    if !blocked || row.status != "queued" {
        return Ok(row);
    }
    let row: JobRow = sqlx::query_as(&format!(
        "UPDATE actions_jobs SET status = 'pending', updated_at = now() WHERE id = $1 RETURNING {}",
        JobRow::COLUMNS
    ))
    .bind(row.id)
    .fetch_one(&mut **tx)
    .await?;
    sync_job(tx, &row, SyncAction::Update).await?;
    let ev = job_event(tx, &row, "waiting").await?;
    tx.emit(ev);
    Ok(row)
}

/// Start the oldest waiting job of every concurrency group a completed job
/// of `run` belonged to, once nothing else of the group is queued or
/// running. Idempotent.
async fn release_job_groups(_state: &AppState, tx: &mut Tx, run: &RunRow) -> anyhow::Result<()> {
    let groups: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT concurrency_group FROM actions_jobs
          WHERE run_id = $1 AND concurrency_group IS NOT NULL AND status = 'completed'",
    )
    .bind(run.id)
    .fetch_all(&mut **tx)
    .await?;
    for group in groups {
        lock_job_group(tx, run.repo_id, &group).await?;
        let next: Option<JobRow> = sqlx::query_as(&format!(
            "UPDATE actions_jobs SET updated_at = now(),
                    status = CASE WHEN kind = 'call' THEN 'in_progress' ELSE 'queued' END,
                    started_at = CASE WHEN kind = 'call' THEN now() ELSE started_at END
              WHERE id = (SELECT id FROM actions_jobs
                           WHERE repo_id = $1 AND concurrency_group = $2 AND status = 'pending'
                           ORDER BY id LIMIT 1)
                AND NOT EXISTS (SELECT 1 FROM actions_jobs
                                 WHERE repo_id = $1 AND concurrency_group = $2
                                   AND status IN ('queued', 'in_progress'))
              RETURNING {}",
            JobRow::COLUMNS
        ))
        .bind(run.repo_id)
        .bind(&group)
        .fetch_optional(&mut **tx)
        .await?;
        match next {
            // A reusable workflow call: its run materializes the called jobs.
            Some(next) if next.is_call() => {
                tx.enqueue(&AdvanceRun {
                    run_id: next.run_id,
                })
                .await?;
            }
            Some(next) => {
                sync_job(tx, &next, SyncAction::Update).await?;
                let ev = job_event(tx, &next, "queued").await?;
                tx.emit(ev);
            }
            None => {}
        }
    }
    Ok(())
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
    // A check run of an earlier attempt reset by a rerequest is reused.
    let rerequested: Option<i64> = if run.run_attempt > 1 {
        sqlx::query_scalar(
            "SELECT j.check_run_id FROM actions_jobs j JOIN check_runs c ON c.id = j.check_run_id
              WHERE j.run_id = $1 AND j.run_attempt < $2 AND j.job_key = $3 AND j.name = $4
                AND c.status = 'queued'
                AND NOT EXISTS (SELECT 1 FROM actions_jobs k
                                 WHERE k.run_id = $1 AND k.run_attempt = $2
                                   AND k.check_run_id = j.check_run_id)
              ORDER BY j.run_attempt DESC LIMIT 1",
        )
        .bind(run.id)
        .bind(run.run_attempt)
        .bind(key)
        .bind(name)
        .fetch_optional(&mut **tx)
        .await?
    } else {
        None
    };
    let check_run_id = match rerequested {
        Some(id) => {
            checks::reuse_run(tx, id, name, check_status, conclusion).await?;
            Some(id)
        }
        None => {
            checks::create_run(
                tx,
                run.check_suite_id,
                run.repo_id,
                &run.head_sha,
                name,
                key,
                check_status,
                conclusion,
            )
            .await?
        }
    };
    if let Some(ev) = checks::check_run_event(
        run.repo_id,
        check_run_id,
        "created",
        run.triggering_actor_id.or(run.actor_id),
    ) {
        tx.emit(ev);
    }
    if status == "completed"
        && let Some(ev) = checks::check_run_event(run.repo_id, check_run_id, "completed", None)
    {
        tx.emit(ev);
    }
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
    let ev = job_event(tx, &row, if completed { "completed" } else { "queued" }).await?;
    tx.emit(ev);
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
        // Calls complete on their own once their jobs did.
        for r in rows
            .iter()
            .filter(|r| r.status != "completed" && !r.is_call())
        {
            crate::server::finish_job_row(&mut tx, r, "cancelled", None, &[], None).await?;
        }
    }
    if run.status == "pending" {
        finish_run(state, &mut tx, &mut run, "cancelled").await?;
    }
    tx.commit().await?;
    advance_run(state, run_id).await
}

/// Re-run a completed run as a new attempt. `only`: job keys to re-run
/// (plus everything depending on them); other jobs are copied from the
/// previous attempt. `None` re-runs everything. A job of a called workflow
/// re-runs the whole calling job.
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
        // Calling jobs re-run as a whole; expand to dependents (transitively).
        let mut rerun_keys: HashSet<String> = only
            .iter()
            .map(|k| reusable::root_key(k).to_string())
            .collect();
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
            if rerun_keys.contains(reusable::root_key(key)) {
                continue;
            }
            for r in rows {
                sqlx::query(
                    "INSERT INTO actions_jobs (run_id, repo_id, run_attempt, job_key, name, matrix,
                         status, conclusion, head_sha, head_branch, labels, runner_id, runner_name,
                         check_run_id, spec, steps, outputs, continue_on_error, timeout_minutes,
                         logs_job_id, started_at, completed_at, kind)
                     SELECT run_id, repo_id, $2, job_key, name, matrix, status, conclusion,
                            head_sha, head_branch, labels, runner_id, runner_name, check_run_id,
                            spec, steps, outputs, continue_on_error, timeout_minutes,
                            coalesce(logs_job_id, id), started_at, completed_at, kind
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
    let ev = run_event(state, &mut tx, &run, "requested").await?;
    tx.emit(ev);
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
