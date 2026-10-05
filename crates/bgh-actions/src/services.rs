//! Long-running services: the scheduler/maintenance loop and the built-in
//! runner.

use std::path::{Path, PathBuf};
use std::time::Duration;

use bgh_core::AppState;
use tokio_util::sync::CancellationToken;

/// Every 30 s: fire due cron schedules, fail jobs of vanished runners and
/// expire artifacts.
pub async fn maintenance(state: AppState, shutdown: CancellationToken) {
    let mut tick = tokio::time::interval(Duration::from_secs(30));
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = tick.tick() => {}
        }
        if state.config.actions.enabled
            && let Err(err) = crate::trigger::schedule_tick(&state, chrono::Utc::now()).await
        {
            tracing::warn!(?err, "actions scheduler tick failed");
        }
        if let Err(err) = crate::server::reap_stale_jobs(&state).await {
            tracing::warn!(?err, "reaping stale jobs failed");
        }
        if let Err(err) = crate::server::expire_artifacts(&state).await {
            tracing::warn!(?err, "expiring artifacts failed");
        }
    }
}

/// Work directory of the built-in runner: `BGH_ACTIONS_WORK_DIR`, else
/// `{tmp}/bgh-actions-work`. Never inside `data_dir` (repositories, keys,
/// secrets): a configured directory there is replaced by the default.
pub fn builtin_work_dir(state: &AppState) -> PathBuf {
    let default = std::env::temp_dir().join("bgh-actions-work");
    match &state.config.actions.work_dir {
        Some(dir) if inside(dir, &state.config.data_dir) => {
            tracing::error!(
                work_dir = %dir.display(),
                data_dir = %state.config.data_dir.display(),
                "BGH_ACTIONS_WORK_DIR must not be inside BGH_DATA_DIR; using {}",
                default.display()
            );
            default
        }
        Some(dir) => dir.clone(),
        None => default,
    }
}

/// Whether `path` is `base` or below it (lexically, after resolving what
/// exists on disk).
fn inside(path: &Path, base: &Path) -> bool {
    let abs = |p: &Path| {
        std::fs::canonicalize(p)
            .unwrap_or_else(|_| std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf()))
    };
    abs(path).starts_with(abs(base))
}

/// Runner configuration of the built-in runner; `None` when no executor is
/// usable (`auto` without docker).
pub async fn builtin_config(
    state: &AppState,
    runner_name: &str,
) -> Option<crate::runner::RunnerConfig> {
    let a = &state.config.actions;
    let executor = crate::runner::RunnerConfig::detect_executor(&a.executor, &a.docker_bin).await?;
    Some(crate::runner::RunnerConfig {
        name: runner_name.to_string(),
        work_dir: builtin_work_dir(state),
        executor,
        docker_bin: a.docker_bin.clone(),
        default_image: a.default_image.clone(),
        git_bin: state.config.git_bin.clone(),
        remote_actions: a.remote_actions,
        github_url: a.github_url.clone(),
        ..Default::default()
    })
}

/// The built-in runner: claims jobs through [`crate::server::LocalBackend`]
/// and runs up to `BGH_ACTIONS_MAX_JOBS` of them concurrently.
///
/// With `BGH_ACTIONS_EXECUTOR=auto` and no usable docker it takes no jobs
/// (they stay queued for external runners) instead of running workflow
/// code on the server host; `shell` must be chosen explicitly.
pub async fn builtin_runner(state: AppState, shutdown: CancellationToken) {
    if !state.config.actions.builtin_runner || state.config.actions.max_jobs == 0 {
        return;
    }
    let host = std::env::var("HOSTNAME").unwrap_or_else(|_| "bgh".into());
    let Some(cfg) = builtin_config(&state, &format!("bgh-builtin-{host}")).await else {
        tracing::warn!(
            "!!! The built-in Actions runner is DISABLED: BGH_ACTIONS_EXECUTOR=auto needs a \
             working docker (`{} info` failed). Jobs stay queued until an external runner \
             (bgh-runner) picks them up. Set BGH_ACTIONS_EXECUTOR=docker once docker works, \
             or BGH_ACTIONS_EXECUTOR=shell on trusted single-tenant installs only.",
            state.config.actions.docker_bin
        );
        return;
    };
    if cfg.executor == crate::runner::ExecutorKind::Shell {
        tracing::warn!(
            "!!! The built-in Actions runner uses the SHELL executor: workflow steps run on the \
             server host as the server's user and can read its files (repositories, keys). \
             Only use this on trusted single-tenant installs where everyone who can push a \
             workflow is trusted; prefer BGH_ACTIONS_EXECUTOR=docker or external runners."
        );
    }
    let row = loop {
        match crate::server::ensure_builtin_runner(&state).await {
            Ok(r) => break r,
            Err(err) => {
                tracing::warn!(?err, "registering built-in runner failed; retrying");
                tokio::select! {
                    _ = shutdown.cancelled() => return,
                    _ = tokio::time::sleep(Duration::from_secs(10)) => {}
                }
            }
        }
    };
    let cfg = crate::runner::RunnerConfig {
        name: row.name.clone(),
        ..cfg
    };
    tracing::info!(runner = %row.name, executor = ?cfg.executor, work_dir = %cfg.work_dir.display(), "built-in actions runner started");
    let backend = std::sync::Arc::new(crate::server::LocalBackend {
        state: state.clone(),
        runner: row,
    });
    crate::runner::worker_loop(
        backend,
        std::sync::Arc::new(cfg),
        state.config.actions.max_jobs,
        shutdown,
        false,
    )
    .await;
}

/// Run every queued job on the built-in runner until none is left (tests
/// and CLI use; the service does this continuously). Returns jobs run.
pub async fn run_queued_jobs(
    state: &AppState,
    cfg: crate::runner::RunnerConfig,
) -> anyhow::Result<usize> {
    let row = crate::server::ensure_builtin_runner(state).await?;
    let cfg = std::sync::Arc::new(crate::runner::RunnerConfig {
        name: row.name.clone(),
        ..cfg
    });
    let backend: std::sync::Arc<dyn crate::protocol::Backend> =
        std::sync::Arc::new(crate::server::LocalBackend {
            state: state.clone(),
            runner: row.clone(),
        });
    let mut n = 0;
    while let Some(spec) = crate::server::try_acquire(state, &row).await? {
        crate::runner::run_job(backend.clone(), cfg.clone(), spec).await;
        n += 1;
    }
    Ok(n)
}
