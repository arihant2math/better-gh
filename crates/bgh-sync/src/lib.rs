//! bgh-sync: the local-first sync engine (docs/SYNC_PROTOCOL.md).
//!
//! * `GET /_bgh/sync/bootstrap` — consistent snapshot of the viewer's scopes
//!   ([`bootstrap`]).
//! * `GET /_bgh/sync/partial` — lazy models / issue bodies.
//! * `GET /_bgh/sync/ws` — WebSocket: replay + live deltas ([`ws`], [`hub`]).
//! * [`http_middleware`] — `X-Client-Tx` capture, `X-Bgh-Sync-Id`,
//!   idempotency (mounted for every route by bgh-server).
//! * `sync.compact` job — log retention ([`compact`]).
//!
//! The compact model shapes live in `bgh_core::sync::shapes` so every
//! domain crate records deltas in exactly the bootstrap shape.

use std::sync::Arc;

use axum::Router;
use axum::routing::get;
use bgh_core::events::Event;
use bgh_core::{AppState, Registry};

pub mod bootstrap;
pub mod compact;
pub mod config;
pub mod delta;
pub mod hub;
pub mod middleware;
pub mod scopes;
pub mod ws;

pub use hub::{AccessChange, publish_access_change};
pub use middleware::http_middleware;

/// REST API routes (none: the sync endpoints are web-client private).
pub fn router() -> Router<AppState> {
    Router::new()
}

/// `/_bgh/sync/*`.
pub fn web_router() -> Router<AppState> {
    Router::new()
        .route("/_bgh/sync/bootstrap", get(bootstrap::bootstrap))
        .route("/_bgh/sync/partial", get(bootstrap::partial))
        .route("/_bgh/sync/ws", get(ws::handler))
}

/// Compaction job and the access-change listener.
pub fn register(reg: &mut Registry) {
    reg.job(compact::run_job);
    reg.on_event("sync.access", on_event);
}

/// Forward events that may change who can read what to every process's
/// hub, which rechecks the affected sockets.
async fn on_event(state: AppState, event: Arc<Event>) -> anyhow::Result<()> {
    let change = match &*event {
        Event::RepositoryUpdated { repo_id, .. } | Event::RepositoryDeleted { repo_id, .. } => {
            AccessChange {
                repo_id: Some(*repo_id),
                ..Default::default()
            }
        }
        Event::AccessChanged {
            repo_id,
            org_id,
            user_id,
        } => AccessChange {
            repo_id: *repo_id,
            org_id: *org_id,
            user_id: *user_id,
        },
        _ => return Ok(()),
    };
    publish_access_change(&state, &change).await;
    Ok(())
}
