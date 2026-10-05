//! Server side of the runner protocol (see [`crate::protocol`]): runner
//! authentication, job claiming (with `GITHUB_TOKEN` minting and secret
//! resolution), logs, heartbeats, completion and artifact storage. Shared by
//! the HTTP endpoints in [`crate::web`] and the in-process [`LocalBackend`].

use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use bgh_core::models::db;
use bgh_core::perms::{
    JOB_TOKEN_ACTOR_SCOPE_PREFIX, JOB_TOKEN_READ_ONLY_SCOPE, JOB_TOKEN_SCOPE_PREFIX,
};
use bgh_core::prelude::{SyncAction, Tx};
use bgh_core::token_permissions::{Access, Category, TokenPermissions};
use bgh_core::{AppState, crypto as core_crypto};
use chrono::Utc;
use indexmap::IndexMap;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::checks;
use crate::engine::{self, StoredJob};
use crate::expr::{self, MapContext};
use crate::models::{ArtifactRow, JobRow, RunRow, RunnerRow, StepState};
use crate::protocol::{
    Annotation, ArtifactInfo, Backend, ContainerSpec, Heartbeat, JobCompletion, JobSpec,
};
use crate::workflow::Container;

pub const CONCLUSIONS: &[&str] = &[
    "success",
    "failure",
    "cancelled",
    "skipped",
    "neutral",
    "timed_out",
];

// ---------------------------------------------------------------------------
// Runners
// ---------------------------------------------------------------------------

pub async fn runner_by_token(state: &AppState, token: &str) -> anyhow::Result<Option<RunnerRow>> {
    Ok(sqlx::query_as(&format!(
        "UPDATE actions_runners SET last_seen_at = now() WHERE token_hash = $1 RETURNING {}",
        RunnerRow::COLUMNS
    ))
    .bind(core_crypto::sha256_hex(token))
    .fetch_optional(&state.db)
    .await?)
}

/// Create (or refresh) the site-wide runner row of this process's built-in
/// runner.
pub async fn ensure_builtin_runner(state: &AppState) -> anyhow::Result<RunnerRow> {
    let host = std::env::var("HOSTNAME").unwrap_or_else(|_| "bgh".into());
    let name = format!("bgh-builtin-{host}");
    let token = core_crypto::random_token(40);
    let labels = &state.config.actions.runner_labels;
    let row: RunnerRow = sqlx::query_as(&format!(
        "WITH existing AS (
             UPDATE actions_runners SET system_labels = $2, last_seen_at = now()
              WHERE builtin AND name = $1 RETURNING {cols}
         ), inserted AS (
             INSERT INTO actions_runners (name, system_labels, token_hash, builtin, last_seen_at)
             SELECT $1, $2, $3, true, now() WHERE NOT EXISTS (SELECT 1 FROM existing)
             RETURNING {cols}
         )
         SELECT {cols} FROM existing UNION ALL SELECT {cols} FROM inserted",
        cols = RunnerRow::COLUMNS
    ))
    .bind(&name)
    .bind(labels)
    .bind(core_crypto::sha256_hex(&token))
    .fetch_one(&state.db)
    .await?;
    Ok(row)
}

// ---------------------------------------------------------------------------
// Claiming jobs
// ---------------------------------------------------------------------------

/// Claim one queued job matching `runner`, or `None`.
pub async fn try_acquire(state: &AppState, runner: &RunnerRow) -> anyhow::Result<Option<JobSpec>> {
    let labels: Vec<String> = runner
        .all_labels()
        .into_iter()
        .map(|l| l.to_ascii_lowercase())
        .collect();
    let mut tx = Tx::begin(state).await?;
    let job: Option<JobRow> = sqlx::query_as(&format!(
        "UPDATE actions_jobs SET status = 'in_progress', runner_id = $1, runner_name = $2,
                started_at = now(), updated_at = now()
          WHERE id = (
              SELECT j.id FROM actions_jobs j JOIN repositories r ON r.id = j.repo_id
               WHERE j.status = 'queued' AND j.labels <@ $3
                 AND ($4::bigint IS NULL OR j.repo_id = $4)
                 AND ($5::bigint IS NULL OR r.owner_id = $5)
               ORDER BY j.id
               FOR UPDATE OF j SKIP LOCKED
               LIMIT 1)
          RETURNING {}",
        JobRow::COLUMNS
    ))
    .bind(runner.id)
    .bind(&runner.name)
    .bind(&labels)
    .bind(runner.repo_id)
    .bind(runner.org_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(job) = job else {
        return Ok(None);
    };
    match prepare_spec(state, &mut tx, &job).await {
        Ok((spec, token_id, steps)) => {
            let job: JobRow = sqlx::query_as(&format!(
                "UPDATE actions_jobs SET token_id = $2, steps = $3 WHERE id = $1 RETURNING {}",
                JobRow::COLUMNS
            ))
            .bind(job.id)
            .bind(token_id)
            .bind(serde_json::to_value(&steps)?)
            .fetch_one(&mut *tx)
            .await?;
            sqlx::query("UPDATE actions_runners SET busy = true WHERE id = $1")
                .bind(runner.id)
                .execute(&mut *tx)
                .await?;
            checks::start_run(&mut tx, job.check_run_id).await?;
            crate::gates::job_started(&mut tx, &job).await?;
            engine::sync_job(&mut tx, &job, SyncAction::Update).await?;
            let ev = engine::job_event(&mut tx, &job, "in_progress").await?;
            tx.emit(ev);
            tx.commit().await?;
            engine::advance_run(state, job.run_id).await?;
            Ok(Some(spec))
        }
        Err(err) => {
            // Can't build the spec (deleted repo, bad definition): fail the job.
            drop(tx);
            tracing::warn!(job_id = job.id, ?err, "failing job that cannot start");
            let mut tx = Tx::begin(state).await?;
            let row = JobRow::find(&mut *tx, job.id).await?;
            if let Some(row) = row {
                finish_job_row(&mut tx, &row, "failure", None, &[], None).await?;
            }
            tx.commit().await?;
            crate::logs::append(state, job.id, 1, &format!("##[error]{err:#}"))
                .await
                .ok();
            engine::advance_run(state, job.run_id).await?;
            Ok(None)
        }
    }
}

/// Long-poll variant of [`try_acquire`].
pub async fn acquire(
    state: &AppState,
    runner: &RunnerRow,
    wait: Duration,
) -> anyhow::Result<Option<JobSpec>> {
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        if let Some(spec) = try_acquire(state, runner).await? {
            return Ok(Some(spec));
        }
        if tokio::time::Instant::now() >= deadline {
            return Ok(None);
        }
        tokio::time::sleep(Duration::from_millis(500).min(wait)).await;
    }
}

fn container_spec(c: &Container, ctx: &MapContext) -> ContainerSpec {
    let s = |v: &str| expr::interpolate(v, ctx).unwrap_or_else(|_| v.to_string());
    let cred = |k: &str| {
        c.credentials
            .as_ref()
            .and_then(|v| v.get(k))
            .and_then(|v| v.as_str())
            .map(s)
            .filter(|v| !v.is_empty())
    };
    ContainerSpec {
        image: s(&c.image),
        env: c.env.iter().map(|(k, v)| (k.clone(), s(v))).collect(),
        ports: c.ports.iter().map(|p| s(p)).collect(),
        volumes: c.volumes.iter().map(|p| s(p)).collect(),
        options: c.options.as_deref().map(s),
        username: cred("username"),
        password: cred("password"),
    }
}

/// Build the runner spec of a just-claimed job: mint `GITHUB_TOKEN`, load
/// secrets, evaluate env/container/services.
async fn prepare_spec(
    state: &AppState,
    tx: &mut Tx,
    job: &JobRow,
) -> anyhow::Result<(JobSpec, i64, Vec<StepState>)> {
    let stored: StoredJob = serde_json::from_value(
        job.spec
            .clone()
            .ok_or_else(|| anyhow::anyhow!("job has no definition"))?,
    )?;
    let run = RunRow::find(&mut **tx, job.run_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("run deleted"))?;
    let repo = db::Repository::find(&mut **tx, job.repo_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("repository deleted"))?;
    let mut spec = stored.spec;
    spec.job_id = job.id;
    spec.run_attempt = job.run_attempt;
    spec.github["run_attempt"] = json!(job.run_attempt.to_string());

    // GITHUB_TOKEN: a short-lived token of github-actions[bot] restricted
    // to this repository and to the workflow's `permissions:` (see
    // bgh_core::perms::effective and bgh_core::token_permissions). Writes
    // made with it are attributed to the bot; the triggering actor is kept
    // in a scope for the audit log.
    let triggering_actor = run
        .triggering_actor_id
        .or(run.actor_id)
        .unwrap_or(repo.owner_id);
    let expires = Utc::now() + chrono::Duration::minutes(spec.timeout_minutes as i64 + 60);
    // Pull requests from forks get no secrets and a read-only token.
    let from_fork = run.event == "pull_request" && run.head_repo_id.is_some_and(|h| h != repo.id);
    let settings = bgh_core::settings::load(state)
        .await
        .map_err(|e| anyhow::anyhow!("loading settings: {e}"))?;
    let mut permissions = match &stored.permissions {
        Some(p) => token_permissions(p),
        None => TokenPermissions::default_for(&settings.actions.default_workflow_permissions),
    };
    if from_fork {
        permissions = permissions.read_only();
    }
    let mut scopes = vec![
        "repo".to_string(),
        format!("{JOB_TOKEN_SCOPE_PREFIX}{}", repo.id),
        format!("{JOB_TOKEN_ACTOR_SCOPE_PREFIX}{triggering_actor}"),
    ];
    if !permissions.has_write() {
        scopes.push(JOB_TOKEN_READ_ONLY_SCOPE.to_string());
    }
    scopes.extend(permissions.to_scopes());
    let bot = bgh_core::bots::ensure_actions_bot(tx).await?;
    let (token_row, token) = bgh_core::auth::create_access_token(
        &mut **tx,
        bot,
        &format!("GITHUB_TOKEN (job {})", job.id),
        &scopes,
        Some(expires),
    )
    .await
    .map_err(|e| anyhow::anyhow!("creating job token: {e}"))?;
    sqlx::query(
        "UPDATE access_tokens SET kind = 'app', permissions = $2, created_by_id = $3 WHERE id = $1",
    )
    .bind(token_row.id)
    .bind(serde_json::to_value(&permissions)?)
    .bind(triggering_actor)
    .execute(&mut **tx)
    .await?;
    spec.token_permissions = permissions
        .iter()
        .map(|(c, a)| (c.as_str().to_string(), a.as_str().to_string()))
        .collect();

    let mut secrets = if from_fork {
        IndexMap::new()
    } else {
        crate::scoped::secrets_for(state, &repo, spec.environment.as_deref()).await?
    };
    secrets.insert("GITHUB_TOKEN".into(), token.clone());
    let vars = crate::scoped::vars_for(state, &repo, spec.environment.as_deref()).await?;
    spec.vars = Value::Object(vars);
    spec.token = token;
    let secrets_json: serde_json::Map<String, Value> = secrets
        .iter()
        .map(|(k, v)| (k.clone(), Value::String(v.clone())))
        .collect();
    spec.secrets = secrets;

    let mut ctx = MapContext::new()
        .with("github", spec.github.clone())
        .with("secrets", Value::Object(secrets_json))
        .with("vars", spec.vars.clone())
        .with("matrix", spec.matrix.clone())
        .with("needs", spec.needs.clone())
        .with("inputs", spec.inputs.clone())
        .with("strategy", spec.strategy.clone())
        .with("env", json!({}));
    let mut env: IndexMap<String, String> = IndexMap::new();
    for layer in [&stored.workflow_env, &stored.job_env] {
        let mut evaluated = Vec::new();
        for (k, v) in layer {
            evaluated.push((
                k.clone(),
                expr::interpolate(v, &ctx).unwrap_or_else(|_| v.clone()),
            ));
        }
        env.extend(evaluated);
        ctx = ctx.with("env", serde_json::to_value(&env)?);
    }
    spec.env = env;
    spec.container = stored
        .container
        .as_ref()
        .map(|c| container_spec(c, &ctx))
        .filter(|c| !c.image.is_empty());
    spec.services = stored
        .services
        .iter()
        .map(|(k, c)| (k.clone(), container_spec(c, &ctx)))
        .filter(|(_, c)| !c.image.is_empty())
        .collect();

    let mut steps: Vec<StepState> = serde_json::from_value(job.steps.clone()).unwrap_or_default();
    if let Some(first) = steps.first_mut() {
        first.status = "in_progress".into();
        first.started_at = Some(Utc::now());
    }
    Ok((spec, token_row.id, steps))
}

/// The token permission map of a workflow `permissions:` value. Listed
/// categories get their level, unlisted ones `none`; unknown category names
/// are ignored (the parser already validated the levels).
pub fn token_permissions(p: &crate::workflow::Permissions) -> TokenPermissions {
    use crate::workflow::Permissions;
    match p {
        Permissions::ReadAll => TokenPermissions::read_all(),
        Permissions::WriteAll => TokenPermissions::write_all(),
        Permissions::Map(m) => TokenPermissions::from_pairs(
            m.iter()
                .filter_map(|(k, v)| Some((Category::parse(k)?, Access::parse(v)?))),
        ),
    }
}

// ---------------------------------------------------------------------------
// Running jobs
// ---------------------------------------------------------------------------

/// A job claimed by `runner` and still running (else 404/409-style error).
pub async fn runner_job(
    state: &AppState,
    runner: &RunnerRow,
    job_id: i64,
) -> anyhow::Result<Option<JobRow>> {
    let job = JobRow::find(&state.db, job_id).await?;
    Ok(job.filter(|j| j.runner_id == Some(runner.id)))
}

pub async fn update_steps(
    state: &AppState,
    runner: &RunnerRow,
    job_id: i64,
    steps: &[StepState],
) -> anyhow::Result<Option<Heartbeat>> {
    let mut tx = Tx::begin(state).await?;
    let row: Option<JobRow> = sqlx::query_as(&format!(
        "UPDATE actions_jobs SET steps = $3, updated_at = now()
          WHERE id = $1 AND runner_id = $2 AND status = 'in_progress' RETURNING {}",
        JobRow::COLUMNS
    ))
    .bind(job_id)
    .bind(runner.id)
    .bind(serde_json::to_value(steps)?)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(row) = row else {
        // Completed meanwhile (e.g. force-cancelled): tell the runner to stop.
        return Ok(runner_job(state, runner, job_id)
            .await?
            .map(|_| Heartbeat { cancel: true }));
    };
    engine::sync_job(&mut tx, &row, SyncAction::Update).await?;
    tx.commit().await?;
    Ok(Some(Heartbeat {
        cancel: row.cancel_requested,
    }))
}

/// Complete a job row in `tx` (job token revoked, check run completed).
pub async fn finish_job_row(
    tx: &mut Tx,
    row: &JobRow,
    conclusion: &str,
    completion: Option<&JobCompletion>,
    annotations: &[Annotation],
    summary: Option<&str>,
) -> anyhow::Result<JobRow> {
    let outputs = completion
        .map(|c| serde_json::to_value(&c.outputs))
        .transpose()?
        .unwrap_or_else(|| json!({}));
    let mut steps: Vec<StepState> = match completion {
        Some(c) if !c.steps.is_empty() => c.steps.clone(),
        _ => serde_json::from_value(row.steps.clone()).unwrap_or_default(),
    };
    let now = Utc::now();
    for s in &mut steps {
        if s.status != "completed" {
            s.status = "completed".into();
            s.conclusion = Some(if s.started_at.is_some() {
                conclusion.to_string()
            } else {
                "skipped".to_string()
            });
            s.completed_at = Some(now);
        }
    }
    let job: JobRow = sqlx::query_as(&format!(
        "UPDATE actions_jobs SET status = 'completed', conclusion = $2, outputs = $3, steps = $4,
                started_at = coalesce(started_at, now()), completed_at = now(), token_id = NULL,
                updated_at = now()
          WHERE id = $1 RETURNING {}",
        JobRow::COLUMNS
    ))
    .bind(row.id)
    .bind(conclusion)
    .bind(outputs)
    .bind(serde_json::to_value(&steps)?)
    .fetch_one(&mut **tx)
    .await?;
    if let Some(token_id) = row.token_id {
        sqlx::query("DELETE FROM access_tokens WHERE id = $1")
            .bind(token_id)
            .execute(&mut **tx)
            .await?;
    }
    if let Some(runner_id) = row.runner_id {
        sqlx::query("UPDATE actions_runners SET busy = false WHERE id = $1")
            .bind(runner_id)
            .execute(&mut **tx)
            .await?;
    }
    checks::complete_run(tx, row.check_run_id, conclusion, summary, annotations).await?;
    crate::gates::job_finished(tx, &job, conclusion).await?;
    if let Some(ev) = checks::check_run_event(row.repo_id, row.check_run_id, "completed", None) {
        tx.emit(ev);
    }
    engine::sync_job(tx, &job, SyncAction::Update).await?;
    let ev = engine::job_event(tx, &job, "completed").await?;
    tx.emit(ev);
    Ok(job)
}

pub async fn complete_job(
    state: &AppState,
    runner: &RunnerRow,
    job_id: i64,
    completion: &JobCompletion,
) -> anyhow::Result<bool> {
    let conclusion = if CONCLUSIONS.contains(&completion.conclusion.as_str()) {
        completion.conclusion.as_str()
    } else {
        "failure"
    };
    let mut tx = Tx::begin(state).await?;
    let row: Option<JobRow> = sqlx::query_as(&format!(
        "SELECT {} FROM actions_jobs WHERE id = $1 AND runner_id = $2 FOR UPDATE",
        JobRow::COLUMNS
    ))
    .bind(job_id)
    .bind(runner.id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(row) = row else { return Ok(false) };
    if row.status == "completed" {
        return Ok(true); // idempotent (retries, force-cancel)
    }
    finish_job_row(
        &mut tx,
        &row,
        conclusion,
        Some(completion),
        &completion.annotations,
        completion.summary.as_deref(),
    )
    .await?;
    tx.commit().await?;
    crate::logs::publish(
        state,
        job_id,
        &crate::logs::LogEvent {
            step: 0,
            offset: 0,
            text: String::new(),
            done: true,
        },
    )
    .await;
    engine::advance_run(state, row.run_id).await?;
    Ok(true)
}

/// Fail jobs whose runner vanished or that exceeded their timeout.
pub async fn reap_stale_jobs(state: &AppState) -> anyhow::Result<usize> {
    let stale: Vec<JobRow> = sqlx::query_as(&format!(
        "SELECT {} FROM actions_jobs j
          WHERE j.status = 'in_progress'
            AND (j.started_at < now() - make_interval(mins => j.timeout_minutes + 10)
                 OR j.updated_at < now() - interval '10 minutes'
                 OR j.runner_id IS NULL)",
        crate::models::JobRow::COLUMNS
            .split(", ")
            .map(|c| format!("j.{c}"))
            .collect::<Vec<_>>()
            .join(", ")
    ))
    .fetch_all(&state.db)
    .await?;
    let n = stale.len();
    for row in stale {
        let mut tx = Tx::begin(state).await?;
        finish_job_row(&mut tx, &row, "failure", None, &[], None).await?;
        tx.commit().await?;
        crate::logs::append(
            state,
            row.id,
            1,
            "##[error]The runner has stopped responding or the job exceeded its time limit.",
        )
        .await
        .ok();
        engine::advance_run(state, row.run_id).await?;
    }
    Ok(n)
}

// ---------------------------------------------------------------------------
// Artifacts
// ---------------------------------------------------------------------------

pub fn artifact_path(state: &AppState, id: i64) -> PathBuf {
    state
        .config
        .data_dir
        .join("actions")
        .join("artifacts")
        .join(format!("{id}.zip"))
}

/// Store an uploaded artifact zip (already written to `tmp`) for `job`.
pub async fn store_artifact(
    state: &AppState,
    job: &JobRow,
    name: &str,
    tmp: &Path,
    retention_days: Option<i64>,
) -> anyhow::Result<ArtifactRow> {
    let bytes = tokio::fs::read(tmp).await?;
    let digest = format!("sha256:{}", hex::encode(Sha256::digest(&bytes)));
    let max = state.config.actions.artifact_retention_days.max(1);
    let days = retention_days.filter(|d| *d > 0).unwrap_or(max).min(max);
    let row: ArtifactRow = sqlx::query_as(&format!(
        "INSERT INTO actions_artifacts (repo_id, run_id, job_id, name, size_in_bytes, digest, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6, now() + make_interval(days => $7))
         ON CONFLICT (run_id, name) DO UPDATE SET
             job_id = EXCLUDED.job_id, size_in_bytes = EXCLUDED.size_in_bytes,
             digest = EXCLUDED.digest, expires_at = EXCLUDED.expires_at, expired = false,
             updated_at = now()
         RETURNING {}",
        ArtifactRow::COLUMNS
    ))
    .bind(job.repo_id)
    .bind(job.run_id)
    .bind(job.id)
    .bind(name)
    .bind(bytes.len() as i64)
    .bind(&digest)
    .bind(days as i32)
    .fetch_one(&state.db)
    .await?;
    let path = artifact_path(state, row.id);
    tokio::fs::create_dir_all(path.parent().expect("parent")).await?;
    let part = path.with_extension("zip.part");
    let mut f = tokio::fs::File::create(&part).await?;
    f.write_all(&bytes).await?;
    f.flush().await?;
    tokio::fs::rename(&part, &path).await?;
    Ok(row)
}

pub async fn run_artifacts(state: &AppState, run_id: i64) -> anyhow::Result<Vec<ArtifactRow>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {} FROM actions_artifacts WHERE run_id = $1 AND NOT expired ORDER BY id",
        ArtifactRow::COLUMNS
    ))
    .bind(run_id)
    .fetch_all(&state.db)
    .await?)
}

/// Expire artifacts past retention and delete their files.
pub async fn expire_artifacts(state: &AppState) -> anyhow::Result<usize> {
    let ids: Vec<i64> = sqlx::query_scalar(
        "UPDATE actions_artifacts SET expired = true, updated_at = now()
          WHERE NOT expired AND expires_at <= now() RETURNING id",
    )
    .fetch_all(&state.db)
    .await?;
    for id in &ids {
        let _ = tokio::fs::remove_file(artifact_path(state, *id)).await;
    }
    Ok(ids.len())
}

// ---------------------------------------------------------------------------
// In-process backend
// ---------------------------------------------------------------------------

/// [`Backend`] for the built-in runner: direct calls, no HTTP.
#[derive(Clone)]
pub struct LocalBackend {
    pub state: AppState,
    pub runner: RunnerRow,
}

#[async_trait]
impl Backend for LocalBackend {
    async fn acquire(&self, wait: Duration) -> anyhow::Result<Option<JobSpec>> {
        acquire(&self.state, &self.runner, wait).await
    }

    async fn append_log(&self, job_id: i64, step: i64, text: &str) -> anyhow::Result<()> {
        crate::logs::append(&self.state, job_id, step, text).await
    }

    async fn update_steps(&self, job_id: i64, steps: &[StepState]) -> anyhow::Result<Heartbeat> {
        sqlx::query("UPDATE actions_runners SET last_seen_at = now() WHERE id = $1")
            .bind(self.runner.id)
            .execute(&self.state.db)
            .await?;
        Ok(update_steps(&self.state, &self.runner, job_id, steps)
            .await?
            .unwrap_or(Heartbeat { cancel: true }))
    }

    async fn complete(&self, job_id: i64, result: &JobCompletion) -> anyhow::Result<()> {
        complete_job(&self.state, &self.runner, job_id, result).await?;
        Ok(())
    }

    async fn upload_artifact(
        &self,
        job_id: i64,
        name: &str,
        zip: &Path,
        retention_days: Option<i64>,
    ) -> anyhow::Result<ArtifactInfo> {
        let job = runner_job(&self.state, &self.runner, job_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("job {job_id} is not assigned to this runner"))?;
        let row = store_artifact(&self.state, &job, name, zip, retention_days).await?;
        Ok(ArtifactInfo {
            id: row.id,
            name: row.name,
            size_in_bytes: row.size_in_bytes,
        })
    }

    async fn list_artifacts(&self, job_id: i64) -> anyhow::Result<Vec<ArtifactInfo>> {
        let job = runner_job(&self.state, &self.runner, job_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("job {job_id} is not assigned to this runner"))?;
        Ok(run_artifacts(&self.state, job.run_id)
            .await?
            .into_iter()
            .map(|a| ArtifactInfo {
                id: a.id,
                name: a.name,
                size_in_bytes: a.size_in_bytes,
            })
            .collect())
    }

    async fn download_artifact(
        &self,
        job_id: i64,
        artifact_id: i64,
        dest: &Path,
    ) -> anyhow::Result<()> {
        let job = runner_job(&self.state, &self.runner, job_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("job {job_id} is not assigned to this runner"))?;
        let ok: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM actions_artifacts WHERE id = $1 AND run_id = $2)",
        )
        .bind(artifact_id)
        .bind(job.run_id)
        .fetch_one(&self.state.db)
        .await?;
        anyhow::ensure!(ok, "artifact {artifact_id} not found");
        tokio::fs::copy(artifact_path(&self.state, artifact_id), dest).await?;
        Ok(())
    }
}
