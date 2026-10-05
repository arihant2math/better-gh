//! `GET /_bgh/sync/ws` (docs/SYNC_PROTOCOL.md §5).

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::{HeaderMap, header};
use axum::response::{IntoResponse, Response};
use bgh_core::ApiError;
use bgh_core::auth::{self, AuthContext, AuthOptions};
use bgh_core::state::AppState;
use bgh_core::sync::SCHEMA_VERSION;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::mpsc::error::TryRecvError;
use tokio::time::Instant;

use crate::delta::{self, Item};
use crate::hub::{Conn, Hub, HubMsg};
use crate::{compact, config, scopes};

/// Close code: not authenticated (client goes to login).
pub const CLOSE_UNAUTHENTICATED: u16 = 4001;
/// Close code: rebootstrap required.
pub const CLOSE_REBOOTSTRAP: u16 = 4009;
/// Close code: the client fell behind; reconnect and resume.
pub const CLOSE_SLOW: u16 = 1013;
/// Connections that sent nothing for this long are closed.
const IDLE_TIMEOUT: Duration = Duration::from_secs(90);
/// Protocol-level ping interval (keeps proxies from timing out).
const HEARTBEAT: Duration = Duration::from_secs(30);
/// A single frame write may take at most this long.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Deserialize)]
pub struct WsQuery {
    /// Client schema version; a mismatch answers `rebootstrap` (`schema`).
    #[serde(alias = "schemaVersion")]
    pub v: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "t", rename_all = "lowercase")]
enum ClientMsg {
    Sub {
        scopes: Vec<String>,
        #[serde(default)]
        since: i64,
    },
    Unsub {
        scopes: Vec<String>,
    },
    Ping,
}

pub async fn handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<WsQuery>,
    ws: WebSocketUpgrade,
) -> Response {
    // Bad credentials are treated like none: the socket closes with 4001.
    let auth: Option<AuthContext> = auth::authenticate(&state, &headers, AuthOptions::default())
        .await
        .unwrap_or_default();
    if let Some(a) = &auth
        && a.is_session()
        && !origin_allowed(&state, &headers)
    {
        return ApiError::forbidden("Cross-origin WebSocket not allowed.").into_response();
    }
    ws.on_upgrade(move |socket| async move {
        let mut s = Socket { ws: socket };
        match auth {
            None => s.close(CLOSE_UNAUTHENTICATED, "unauthenticated").await,
            Some(auth) => session(state, Arc::new(auth), q.v, s).await,
        }
    })
}

/// Same-origin check for cookie-authenticated sockets. A missing `Origin`
/// (non-browser client) is accepted.
fn origin_allowed(state: &AppState, headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return true;
    };
    let origin = origin.trim_end_matches('/');
    let base = state.config.base_url.trim_end_matches('/');
    if origin.eq_ignore_ascii_case(base)
        || config::get()
            .allowed_origins
            .iter()
            .any(|o| o.eq_ignore_ascii_case(origin))
    {
        return true;
    }
    // Same host as the request (reverse proxies that rewrite the base URL).
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok());
    let origin_host = origin.split_once("://").map(|(_, h)| h);
    matches!((host, origin_host), (Some(h), Some(o)) if h.eq_ignore_ascii_case(o))
}

struct Socket {
    ws: WebSocket,
}

/// Why the session loop ends.
struct Closed;

impl Socket {
    async fn send(&mut self, text: String) -> Result<(), Closed> {
        match tokio::time::timeout(WRITE_TIMEOUT, self.ws.send(Message::Text(text.into()))).await {
            Ok(Ok(())) => Ok(()),
            _ => Err(Closed),
        }
    }

    async fn send_json(&mut self, v: serde_json::Value) -> Result<(), Closed> {
        self.send(v.to_string()).await
    }

    async fn close(&mut self, code: u16, reason: &str) {
        let frame = CloseFrame {
            code,
            reason: reason.into(),
        };
        let _ =
            tokio::time::timeout(WRITE_TIMEOUT, self.ws.send(Message::Close(Some(frame)))).await;
    }
}

struct Session {
    state: AppState,
    auth: Arc<AuthContext>,
    conn: Conn,
    /// Subscribed scopes → skip live actions with id <= this (the `since`
    /// of a client that is ahead of the hub).
    scopes: HashMap<String, i64>,
}

async fn session(
    state: AppState,
    auth: Arc<AuthContext>,
    version: Option<i64>,
    mut socket: Socket,
) {
    if version.is_some_and(|v| v != SCHEMA_VERSION) {
        let _ = socket
            .send_json(json!({"t": "rebootstrap", "reason": "schema"}))
            .await;
        socket.close(CLOSE_REBOOTSTRAP, "rebootstrap").await;
        return;
    }
    let hub = match Hub::get(&state).await {
        Ok(h) => h,
        Err(err) => {
            tracing::error!(?err, "sync hub unavailable");
            socket.close(1011, "unavailable").await;
            return;
        }
    };
    compact::ensure_scheduled(&state).await;
    let conn = hub.register(auth.clone());
    let mut s = Session {
        state,
        auth,
        conn,
        scopes: HashMap::new(),
    };
    let head = match delta::head(&s.state.db).await {
        Ok(h) => h,
        Err(_) => {
            socket.close(1011, "unavailable").await;
            return;
        }
    };
    if socket
        .send_json(json!({"t": "hello", "userId": s.auth.user.id, "head": head}))
        .await
        .is_err()
    {
        return;
    }
    let _ = s.run(&mut socket).await;
}

impl Session {
    async fn run(&mut self, socket: &mut Socket) -> Result<(), Closed> {
        let mut last_seen = Instant::now();
        let mut heartbeat = tokio::time::interval_at(Instant::now() + HEARTBEAT, HEARTBEAT);
        loop {
            if self.conn.overflow.load(Ordering::SeqCst) {
                return self.slow(socket).await;
            }
            tokio::select! {
                frame = socket.ws.recv() => {
                    let Some(Ok(frame)) = frame else { return Err(Closed) };
                    last_seen = Instant::now();
                    match frame {
                        Message::Text(text) => self.on_text(socket, text.as_str()).await?,
                        Message::Binary(_) => {
                            socket.send_json(error("bad_request", "binary frames are not supported")).await?
                        }
                        Message::Close(_) => return Err(Closed),
                        Message::Ping(_) | Message::Pong(_) => {}
                    }
                }
                msg = self.conn.rx.recv() => match msg {
                    Some(msg) => self.on_hub(socket, msg).await?,
                    None => return self.slow(socket).await,
                },
                _ = heartbeat.tick() => {
                    if last_seen.elapsed() >= IDLE_TIMEOUT {
                        socket.close(1001, "idle").await;
                        return Err(Closed);
                    }
                    let ping = socket.ws.send(Message::Ping(Default::default()));
                    if !matches!(tokio::time::timeout(WRITE_TIMEOUT, ping).await, Ok(Ok(()))) {
                        return Err(Closed);
                    }
                }
            }
        }
    }

    async fn slow(&mut self, socket: &mut Socket) -> Result<(), Closed> {
        let _ = socket
            .send_json(error(
                "slow_consumer",
                "connection fell behind; reconnect and resume",
            ))
            .await;
        socket.close(CLOSE_SLOW, "slow consumer").await;
        Err(Closed)
    }

    async fn on_text(&mut self, socket: &mut Socket, text: &str) -> Result<(), Closed> {
        let msg: ClientMsg = match serde_json::from_str(text) {
            Ok(m) => m,
            Err(e) => {
                return socket
                    .send_json(error("bad_request", &format!("invalid message: {e}")))
                    .await;
            }
        };
        match msg {
            ClientMsg::Ping => socket.send_json(json!({"t": "pong"})).await,
            ClientMsg::Unsub { scopes } => {
                self.conn.hub().unsubscribe(self.conn.id, &scopes);
                for s in &scopes {
                    self.scopes.remove(s);
                }
                Ok(())
            }
            ClientMsg::Sub { scopes, since } => self.subscribe(socket, scopes, since).await,
        }
    }

    async fn on_hub(&mut self, socket: &mut Socket, msg: HubMsg) -> Result<(), Closed> {
        match msg {
            HubMsg::Items(items) => self.send_items(socket, items.iter()).await,
            HubMsg::Revoke(scope) => {
                self.scopes.remove(&scope);
                socket
                    .send_json(json!({"t": "revoke", "scope": scope, "reason": "forbidden"}))
                    .await
            }
            HubMsg::Close { code, reason } => {
                socket.close(code, reason).await;
                Err(Closed)
            }
        }
    }

    async fn send_items<'a>(
        &self,
        socket: &mut Socket,
        items: impl Iterator<Item = &'a Item>,
    ) -> Result<(), Closed> {
        let selected: Vec<&Item> = items
            .filter(|i| self.scopes.get(&i.scope).is_some_and(|skip| i.id > *skip))
            .collect();
        for chunk in selected.chunks(delta::BATCH_SIZE as usize) {
            if let Some(text) = delta::message(chunk.iter().copied()) {
                socket.send(text).await?;
            }
        }
        Ok(())
    }

    async fn rebootstrap(&self, socket: &mut Socket) -> Result<(), Closed> {
        let _ = socket
            .send_json(json!({"t": "rebootstrap", "reason": "too_old"}))
            .await;
        socket.close(CLOSE_REBOOTSTRAP, "rebootstrap").await;
        Err(Closed)
    }

    async fn subscribe(
        &mut self,
        socket: &mut Socket,
        requested: Vec<String>,
        since: i64,
    ) -> Result<(), Closed> {
        let db = self.state.db.clone();
        let min = delta::min_retained_id(&db).await.map_err(|_| Closed)?;
        if !delta::can_resume(since, min) {
            return self.rebootstrap(socket).await;
        }
        let access = {
            let mut c = db.acquire().await.map_err(|_| Closed)?;
            scopes::check(&mut c, &self.auth, &requested)
                .await
                .map_err(|_| Closed)?
        };
        for scope in &access.denied {
            if self.scopes.remove(scope).is_some() {
                self.conn
                    .hub()
                    .unsubscribe(self.conn.id, std::slice::from_ref(scope));
            }
            socket
                .send_json(json!({"t": "revoke", "scope": scope, "reason": "forbidden"}))
                .await?;
        }
        let allowed: Vec<String> = access.allowed.iter().map(ToString::to_string).collect();
        let new: Vec<String> = allowed
            .iter()
            .filter(|s| !self.scopes.contains_key(*s))
            .cloned()
            .collect();
        let upto = self.conn.hub().subscribe(self.conn.id, &new);
        for s in &new {
            self.scopes.insert(s.clone(), since);
        }

        // Live actions already queued for the existing scopes: those <= upto
        // go out before `ready` (so `ready.id` never covers an unsent
        // action), later ones after the replay.
        let mut later: Vec<Item> = Vec::new();
        loop {
            match self.conn.rx.try_recv() {
                Ok(HubMsg::Items(items)) => {
                    let (now, after): (Vec<&Item>, Vec<&Item>) =
                        items.iter().partition(|i| i.id <= upto);
                    self.send_items(socket, now.into_iter()).await?;
                    later.extend(after.into_iter().cloned());
                }
                Ok(msg @ (HubMsg::Revoke(_) | HubMsg::Close { .. })) => {
                    self.on_hub(socket, msg).await?
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return self.slow(socket).await,
            }
        }

        // Replay (since, upto] of the new scopes in batches.
        let mut after = since;
        while !new.is_empty() {
            let page = delta::fetch_scoped(&db, &new, after, upto, delta::BATCH_SIZE)
                .await
                .map_err(|_| Closed)?;
            let Some(last) = page.last().map(|r| r.id) else {
                break;
            };
            let items = {
                let mut c = db.acquire().await.map_err(|_| Closed)?;
                delta::to_items(&mut c, &page).await.map_err(|_| Closed)?
            };
            if let Some(text) = delta::message(items.iter()) {
                socket.send(text).await?;
            }
            after = last;
            if (page.len() as i64) < delta::BATCH_SIZE {
                break;
            }
        }
        // Compaction may have raced with the replay.
        let min = delta::min_retained_id(&db).await.map_err(|_| Closed)?;
        if !new.is_empty() && !delta::can_resume(since, min) {
            return self.rebootstrap(socket).await;
        }
        socket
            .send_json(json!({"t": "ready", "scopes": allowed, "id": upto.max(since)}))
            .await?;
        self.send_items(socket, later.iter()).await
    }
}

fn error(code: &str, message: &str) -> serde_json::Value {
    json!({"t": "error", "code": code, "message": message})
}
