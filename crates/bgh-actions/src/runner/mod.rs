//! Job runner: executes a [`JobSpec`] step by step on the host (shell
//! executor) or inside a docker job container, reporting step states and
//! logs through a [`Backend`].
//!
//! Used in-process by the built-in runner and by the external `bgh-runner`
//! binary (with [`http::HttpBackend`]).
//!
//! Layout of a job directory (`{work_dir}/{job_id}`, mounted at `/__w`
//! under docker):
//!
//! ```text
//! _temp/                      RUNNER_TEMP, step scripts, file commands
//! _temp/_github_workflow/     event.json (GITHUB_EVENT_PATH)
//! _actions/{owner}/{repo}/{ref}
//! _tool/                      RUNNER_TOOL_CACHE
//! {repo}/{repo}/              GITHUB_WORKSPACE
//! ```

mod actions;
mod artifacts;
mod cache;
mod checkout;
mod commands;
mod context;
mod executor;
pub mod http;
mod job;
mod logger;
mod mask;
mod process;
#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::protocol::{Backend, JobCompletion, JobSpec};

pub use commands::{WorkflowCommand, parse_command, parse_file_commands, split_words};
pub use context::hash_files;
pub use mask::Masker;

/// Where steps run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutorKind {
    /// Directly on the host.
    Shell,
    /// Inside a per-job docker container.
    Docker,
}

#[derive(Debug, Clone)]
pub struct RunnerConfig {
    /// Runner name (`RUNNER_NAME`).
    pub name: String,
    /// Per-job directories are created under it and removed after the job.
    pub work_dir: PathBuf,
    pub executor: ExecutorKind,
    pub docker_bin: String,
    /// Docker executor image for jobs without `container:`.
    pub default_image: String,
    pub git_bin: String,
    /// Allow fetching `owner/repo@ref` actions from `github_url`.
    pub remote_actions: bool,
    pub github_url: String,
    /// Overrides the job timeout (`spec.timeout_minutes`); for tests.
    pub job_timeout: Option<Duration>,
    /// Interval of the step-state heartbeat.
    pub heartbeat_interval: Duration,
}

impl Default for RunnerConfig {
    fn default() -> Self {
        RunnerConfig {
            name: "bgh-runner".to_string(),
            work_dir: PathBuf::from("_work"),
            executor: ExecutorKind::Shell,
            docker_bin: "docker".to_string(),
            default_image: "catthehacker/ubuntu:act-latest".to_string(),
            git_bin: "git".to_string(),
            remote_actions: true,
            github_url: "https://github.com".to_string(),
            job_timeout: None,
            heartbeat_interval: Duration::from_secs(3),
        }
    }
}

impl RunnerConfig {
    /// The executor for `setting`: `docker`, `shell` (explicit only), or
    /// `auto` → docker if `docker info` succeeds, else `None`. `auto` never
    /// falls back to the shell executor, which runs workflow code with the
    /// runner's own privileges.
    pub async fn detect_executor(setting: &str, docker_bin: &str) -> Option<ExecutorKind> {
        match setting.trim().to_ascii_lowercase().as_str() {
            "docker" => Some(ExecutorKind::Docker),
            "shell" | "host" => Some(ExecutorKind::Shell),
            _ => executor::docker_available(docker_bin)
                .await
                .then_some(ExecutorKind::Docker),
        }
    }
}

/// Execute one job: reports steps / logs through `backend` and returns the
/// completion (does not call [`Backend::complete`]).
pub async fn execute_job(
    backend: Arc<dyn Backend>,
    cfg: Arc<RunnerConfig>,
    spec: JobSpec,
) -> JobCompletion {
    execute_job_with_cancel(backend, cfg, spec, CancellationToken::new()).await
}

/// [`execute_job`] that is also cancelled when `cancel` fires.
pub async fn execute_job_with_cancel(
    backend: Arc<dyn Backend>,
    cfg: Arc<RunnerConfig>,
    spec: JobSpec,
    cancel: CancellationToken,
) -> JobCompletion {
    let job_id = spec.job_id;
    let handle = tokio::spawn(job::run(backend, cfg, spec, cancel));
    match handle.await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(job_id, "job execution panicked: {e}");
            JobCompletion {
                conclusion: "failure".to_string(),
                ..Default::default()
            }
        }
    }
}

/// [`execute_job`] followed by [`Backend::complete`] (retried on error).
pub async fn run_job(backend: Arc<dyn Backend>, cfg: Arc<RunnerConfig>, spec: JobSpec) {
    run_job_with_cancel(backend, cfg, spec, CancellationToken::new()).await
}

async fn run_job_with_cancel(
    backend: Arc<dyn Backend>,
    cfg: Arc<RunnerConfig>,
    spec: JobSpec,
    cancel: CancellationToken,
) {
    let job_id = spec.job_id;
    tracing::info!(job_id, name = %spec.name, "running job");
    let completion = execute_job_with_cancel(backend.clone(), cfg, spec, cancel).await;
    tracing::info!(job_id, conclusion = %completion.conclusion, "job finished");
    let mut delay = Duration::from_secs(1);
    for attempt in 1..=5 {
        match backend.complete(job_id, &completion).await {
            Ok(()) => return,
            Err(e) => {
                tracing::warn!(job_id, attempt, "failed to report job completion: {e:#}");
                tokio::time::sleep(delay).await;
                delay *= 2;
            }
        }
    }
    tracing::error!(job_id, "giving up reporting job completion");
}

/// Acquire (long-poll 30 s) and run up to `max_jobs` jobs concurrently
/// until `shutdown` is cancelled. On shutdown, in-flight jobs are cancelled
/// (their `always()` steps still run) and reported before this returns.
/// `once`: stop after one job.
pub async fn worker_loop(
    backend: Arc<dyn Backend>,
    cfg: Arc<RunnerConfig>,
    max_jobs: usize,
    shutdown: CancellationToken,
    once: bool,
) {
    let max_jobs = max_jobs.max(1);
    let slots = Arc::new(tokio::sync::Semaphore::new(max_jobs));
    let mut jobs = JoinSet::new();
    let mut backoff = Duration::from_secs(1);
    loop {
        // Reap finished jobs.
        while jobs.try_join_next().is_some() {}
        if shutdown.is_cancelled() {
            break;
        }
        let permit = tokio::select! {
            p = slots.clone().acquire_owned() => match p {
                Ok(p) => p,
                Err(_) => break,
            },
            _ = shutdown.cancelled() => break,
        };
        let acquired = tokio::select! {
            r = backend.acquire(Duration::from_secs(30)) => r,
            _ = shutdown.cancelled() => break,
        };
        match acquired {
            Ok(Some(spec)) => {
                backoff = Duration::from_secs(1);
                let backend = backend.clone();
                let cfg = cfg.clone();
                let cancel = shutdown.child_token();
                jobs.spawn(async move {
                    run_job_with_cancel(backend, cfg, spec, cancel).await;
                    drop(permit);
                });
                if once {
                    break;
                }
            }
            Ok(None) => {}
            Err(e) => {
                drop(permit);
                tracing::warn!("acquiring a job failed: {e:#}");
                tokio::select! {
                    _ = tokio::time::sleep(backoff) => {}
                    _ = shutdown.cancelled() => break,
                }
                backoff = (backoff * 2).min(Duration::from_secs(60));
            }
        }
    }
    while jobs.join_next().await.is_some() {}
}
