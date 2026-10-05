//! Startup registration of job handlers and event listeners.
//!
//! Every domain crate exposes `pub fn register(reg: &mut Registry)`;
//! `bgh-server` calls them all once at startup (and the test harness does
//! the same), then runs workers and the event dispatcher.

use std::future::Future;
use std::sync::Arc;

use futures::FutureExt;
use futures::future::BoxFuture;
use tokio::sync::broadcast::error::RecvError;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::events::Event;
use crate::jobs::{JobPayload, JobRegistry};
use crate::state::AppState;

type ListenerFn =
    Arc<dyn Fn(AppState, Arc<Event>) -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync>;

/// A named event listener.
#[derive(Clone)]
pub struct Listener {
    pub name: &'static str,
    handler: ListenerFn,
}

/// How to assemble the application: the router builder and the
/// registration function. `bgh_server::factory()` returns the real one; the
/// test harness uses it to build a full app per test.
#[derive(Clone, Copy)]
pub struct AppFactory {
    pub router: fn(AppState) -> axum::Router,
    pub register: fn(&mut Registry),
}

#[derive(Clone, Default)]
pub struct Registry {
    pub jobs: JobRegistry,
    pub listeners: Vec<Listener>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a typed job handler (see [`crate::jobs`]).
    pub fn job<J, F, Fut>(&mut self, handler: F) -> &mut Self
    where
        J: JobPayload,
        F: Fn(AppState, J) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        self.jobs.register::<J, F, Fut>(handler);
        self
    }

    /// Register an event listener. It receives every event (match on the
    /// variants you care about) in emission order. `name` is used in logs.
    pub fn on_event<F, Fut>(&mut self, name: &'static str, handler: F) -> &mut Self
    where
        F: Fn(AppState, Arc<Event>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        self.listeners.push(Listener {
            name,
            handler: Arc::new(move |s, e| handler(s, e).boxed()),
        });
        self
    }
}

/// Spawn one task per listener, each consuming the event bus in order until
/// `shutdown` is cancelled. Subscriptions are taken before returning, so no
/// event emitted after this call is missed.
pub fn spawn_listeners(
    state: &AppState,
    listeners: &[Listener],
    shutdown: CancellationToken,
) -> Vec<JoinHandle<()>> {
    listeners
        .iter()
        .cloned()
        .map(|listener| {
            let mut rx = state.events.subscribe();
            let state = state.clone();
            let shutdown = shutdown.clone();
            tokio::spawn(async move {
                loop {
                    let event = tokio::select! {
                        _ = shutdown.cancelled() => return,
                        ev = rx.recv() => ev,
                    };
                    match event {
                        Ok(event) => {
                            let fut = (listener.handler)(state.clone(), event.clone());
                            match tokio::spawn(fut).await {
                                Ok(Ok(())) => {}
                                Ok(Err(err)) => tracing::error!(
                                    listener = listener.name,
                                    event = event.name(),
                                    ?err,
                                    "event listener failed"
                                ),
                                Err(err) => tracing::error!(
                                    listener = listener.name, event = event.name(), %err,
                                    "event listener panicked"
                                ),
                            }
                        }
                        Err(RecvError::Lagged(n)) => {
                            tracing::warn!(
                                listener = listener.name,
                                skipped = n,
                                "event listener lagged"
                            );
                        }
                        Err(RecvError::Closed) => return,
                    }
                }
            })
        })
        .collect()
}
