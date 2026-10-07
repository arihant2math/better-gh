//! Startup registration of job handlers and event listeners.
//!
//! Every domain crate exposes `pub fn register(reg: &mut Registry)`;
//! `bgh-server` calls them all once at startup (and the test harness does
//! the same), then runs workers and the durable event consumers
//! ([`crate::outbox`]).

use std::future::Future;
use std::sync::Arc;

use futures::FutureExt;
use futures::future::BoxFuture;
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

impl Listener {
    /// Invoke the handler for one event.
    pub fn call(
        &self,
        state: AppState,
        event: Arc<Event>,
    ) -> BoxFuture<'static, anyhow::Result<()>> {
        (self.handler)(state, event)
    }
}

/// How to assemble the application: the router builder and the
/// registration function. `bgh_server::factory()` returns the real one; the
/// test harness uses it to build a full app per test.
#[derive(Clone, Copy)]
pub struct AppFactory {
    pub router: fn(AppState) -> axum::Router,
    pub register: fn(&mut Registry),
}

type ServiceFn = Arc<
    dyn Fn(AppState, CancellationToken) -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync,
>;

/// A named long-running background task (e.g. the built-in CI runner, a
/// scheduler or the SSH server), started by the server binary next to the
/// job workers. The task must return once its `CancellationToken` is
/// cancelled; an error is logged. Not started by the test harness (tests
/// drive the work deterministically or start what they need explicitly).
#[derive(Clone)]
pub struct Service {
    pub name: &'static str,
    run: ServiceFn,
}

#[derive(Clone, Default)]
pub struct Registry {
    pub jobs: JobRegistry,
    pub listeners: Vec<Listener>,
    pub services: Vec<Service>,
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

    /// Register a durable event listener. It receives every event (match on
    /// the variants you care about) in commit order, at least once: the
    /// handler must be idempotent (see [`crate::events::effect_key`]).
    /// `name` keys the listener's cursor in `event_listener_cursors`, so
    /// keep it stable (renaming starts a new cursor at the current head).
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

    /// Register a bootstrap scope provider for synced models owned by this
    /// crate (see [`crate::sync::ScopeProvider`]).
    pub fn scope_provider(&mut self, provider: crate::sync::ScopeProvider) -> &mut Self {
        crate::sync::register_scope_provider(provider);
        self
    }
}

impl Registry {
    /// Register a long-running background service (see [`Service`]).
    pub fn service<F, Fut>(&mut self, name: &'static str, run: F) -> &mut Self
    where
        F: Fn(AppState, CancellationToken) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        self.services.push(Service {
            name,
            run: Arc::new(move |s, c| run(s, c).boxed()),
        });
        self
    }
}

/// Spawn every service; each runs until `shutdown` is cancelled.
pub fn spawn_services(
    state: &AppState,
    services: &[Service],
    shutdown: CancellationToken,
) -> Vec<JoinHandle<()>> {
    services
        .iter()
        .cloned()
        .map(|svc| {
            tracing::info!(service = svc.name, "starting service");
            let fut = (svc.run)(state.clone(), shutdown.clone());
            tokio::spawn(async move {
                if let Err(err) = fut.await {
                    tracing::error!(service = svc.name, ?err, "service failed");
                }
            })
        })
        .collect()
}

/// Start the durable consumer of every listener (see
/// [`crate::outbox::start_listeners`]); events committed after this returns
/// are delivered. The tasks return once `shutdown` is cancelled, after
/// draining committed events.
pub async fn start_listeners(
    state: &AppState,
    listeners: &[Listener],
    shutdown: CancellationToken,
) -> anyhow::Result<Vec<JoinHandle<()>>> {
    crate::outbox::start_listeners(state, listeners, shutdown).await
}

/// Synchronous variant of [`start_listeners`] (cursor setup runs in the
/// background, so events emitted right after this call may predate a new
/// listener's cursor). Prefer `start_listeners(..).await`.
pub fn spawn_listeners(
    state: &AppState,
    listeners: &[Listener],
    shutdown: CancellationToken,
) -> Vec<JoinHandle<()>> {
    let state = state.clone();
    let listeners = listeners.to_vec();
    vec![tokio::spawn(async move {
        match start_listeners(&state, &listeners, shutdown).await {
            Ok(handles) => {
                for h in handles {
                    let _ = h.await;
                }
            }
            Err(err) => tracing::error!(?err, "starting event listeners failed"),
        }
    })]
}
