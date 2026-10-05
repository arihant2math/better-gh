//! Request-scoped sync context.
//!
//! The HTTP middleware (`bgh_sync::http_middleware`, mounted by bgh-server)
//! runs every request inside [`RequestSync::scope`]. While it runs:
//! * [`client_tx`] returns the request's `X-Client-Tx` header, so every
//!   [`super::record`] (and `Tx::sync`) stores it without the handler
//!   passing it around;
//! * committed sync ids are noted ([`note_committed`]) so the middleware can
//!   answer with `X-Bgh-Sync-Id`.
//!
//! Outside a request (jobs, listeners, spawned tasks) there is no context:
//! `client_tx()` is `None` and nothing is noted.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use uuid::Uuid;

/// Request header carrying the client transaction id.
pub const CLIENT_TX_HEADER: &str = "x-client-tx";
/// Response header with the highest sync id written by the request.
pub const SYNC_ID_HEADER: &str = "x-bgh-sync-id";

tokio::task_local! {
    static REQUEST: Arc<RequestSync>;
}

/// Per-request sync state.
#[derive(Debug, Default)]
pub struct RequestSync {
    client_tx: Option<Uuid>,
    max_sync_id: AtomicI64,
}

impl RequestSync {
    pub fn new(client_tx: Option<Uuid>) -> Arc<Self> {
        Arc::new(Self {
            client_tx,
            max_sync_id: AtomicI64::new(0),
        })
    }

    pub fn client_tx(&self) -> Option<Uuid> {
        self.client_tx
    }

    /// Highest sync id committed while handling the request (0 = none).
    pub fn max_sync_id(&self) -> i64 {
        self.max_sync_id.load(Ordering::SeqCst)
    }

    /// Run `fut` with this context installed.
    pub async fn scope<F: Future>(self: Arc<Self>, fut: F) -> F::Output {
        REQUEST.scope(self, fut).await
    }
}

/// The current request's `X-Client-Tx`, if any.
pub fn client_tx() -> Option<Uuid> {
    REQUEST.try_with(|r| r.client_tx).ok().flatten()
}

/// Record that sync action `id` was committed by the current request.
pub fn note_committed(id: i64) {
    let _ = REQUEST.try_with(|r| r.max_sync_id.fetch_max(id, Ordering::SeqCst));
}

/// Parse an `X-Client-Tx` header value (a UUID).
pub fn parse_client_tx(value: &str) -> Option<Uuid> {
    Uuid::parse_str(value.trim()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn scoped_values() {
        assert_eq!(client_tx(), None);
        note_committed(5); // no context: ignored
        let id = Uuid::new_v4();
        let ctx = RequestSync::new(Some(id));
        let seen = ctx
            .clone()
            .scope(async {
                note_committed(3);
                note_committed(9);
                note_committed(4);
                client_tx()
            })
            .await;
        assert_eq!(seen, Some(id));
        assert_eq!(ctx.max_sync_id(), 9);
        assert_eq!(parse_client_tx(&format!(" {id} ")), Some(id));
        assert_eq!(parse_client_tx("nope"), None);
    }
}
