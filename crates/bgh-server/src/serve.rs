//! Running the server with ordered graceful shutdown.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use axum::Router;
use bgh_core::registry::{Registry, spawn_services, start_listeners};
use bgh_core::state::AppState;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

/// Serve `router` on `listener` with the registry's job workers, durable
/// event consumers and services until `signal` resolves, then shut down
/// in order:
///
/// 1. stop accepting connections,
/// 2. wait for in-flight requests (up to `BGH_SHUTDOWN_TIMEOUT_SECS`),
/// 3. flush directly emitted events to the outbox,
/// 4. cancel the background token: job workers finish their current job,
///    event consumers drain committed events and release their leases,
///    services stop,
/// 5. return.
///
/// HTTP and background work have separate cancellation tokens, so a
/// request that commits an event during shutdown still gets it delivered.
pub async fn serve(
    state: AppState,
    registry: Registry,
    router: Router,
    listener: TcpListener,
    signal: impl Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    let background = CancellationToken::new();
    let mut tasks = start_listeners(&state, &registry.listeners, background.clone())
        .await
        .context("starting event listeners")?;
    tasks.extend(spawn_services(
        &state,
        &registry.services,
        background.clone(),
    ));
    tasks.push(tokio::spawn(bgh_core::jobs::run_workers(
        state.clone(),
        Arc::new(registry.jobs.clone()),
        state.config.job_workers,
        background.clone(),
    )));

    let http = CancellationToken::new();
    let mut server = tokio::spawn({
        let http = http.clone();
        async move {
            axum::serve(
                listener,
                router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .with_graceful_shutdown(http.cancelled_owned())
            .await
        }
    });

    let result = tokio::select! {
        res = &mut server => {
            // The server stopped on its own (accept error): still stop the
            // background work in order.
            Some(res)
        }
        _ = signal => None,
    };
    let result = match result {
        Some(res) => res.context("http server task")?.context("http server"),
        None => {
            tracing::info!("shutting down: draining in-flight requests");
            http.cancel();
            let grace = Duration::from_secs(state.config.shutdown_timeout_secs);
            match tokio::time::timeout(grace, &mut server).await {
                Ok(res) => res.context("http server task")?.context("http server"),
                Err(_) => {
                    tracing::warn!(?grace, "requests still running after the shutdown timeout");
                    server.abort();
                    Ok(())
                }
            }
        }
    };

    state.events.flush().await;
    tracing::info!("stopping background work");
    background.cancel();
    let join = futures::future::join_all(tasks);
    if tokio::time::timeout(bgh_core::outbox::DRAIN_TIMEOUT * 2, join)
        .await
        .is_err()
    {
        tracing::warn!("background tasks did not stop in time");
    }
    result
}
