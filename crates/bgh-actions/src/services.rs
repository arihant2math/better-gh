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
