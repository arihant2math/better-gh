//! Long-running services: the scheduler/maintenance loop and the built-in
//! runner.

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

/// Runner configuration of the built-in runner.
pub async fn builtin_config(state: &AppState, runner_name: &str) -> crate::runner::RunnerConfig {
    let a = &state.config.actions;
    crate::runner::RunnerConfig {
        name: runner_name.to_string(),
        work_dir: a
            .work_dir
            .clone()
            .unwrap_or_else(|| state.config.data_dir.join("actions").join("work")),
        executor: crate::runner::RunnerConfig::detect_executor(&a.executor, &a.docker_bin).await,
        docker_bin: a.docker_bin.clone(),
        default_image: a.default_image.clone(),
        git_bin: state.config.git_bin.clone(),
        remote_actions: a.remote_actions,
        github_url: a.github_url.clone(),
        ..Default::default()
    }
}

/// The built-in runner: claims jobs through [`crate::server::LocalBackend`]
/// and runs up to `BGH_ACTIONS_MAX_JOBS` of them concurrently.
pub async fn builtin_runner(state: AppState, shutdown: CancellationToken) {
    if !state.config.actions.builtin_runner || state.config.actions.max_jobs == 0 {
        return;
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
    let cfg = builtin_config(&state, &row.name).await;
    tracing::info!(runner = %row.name, executor = ?cfg.executor, "built-in actions runner started");
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
