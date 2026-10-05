//! Live fan-out of sync actions to WebSocket connections.
//!
//! One [`Hub`] per process (per Redis prefix): a single Redis connection
//! `PSUBSCRIBE`s `{prefix}sync:*` and multiplexes deltas to every socket;
//! sockets never talk to Redis themselves.
//!
//! Ordering and completeness: sync ids commit in id order (advisory lock in
//! `bgh_core::sync::record`), so when the hub sees action `M`, every id
//! `<= M` is committed. Redis messages are buffered for ~10 ms, then the hub
//! delivers everything in `(delivered, M]` in ascending order — straight from
//! the buffer when it is contiguous, otherwise re-read from `sync_actions`
//! (out-of-order publishes, lost messages, ids burned by rollbacks). A
//! 1-second poll of `max(id)` catches a lost tail, and a Redis reconnect is
//! followed by the same catch-up. `delivered` only moves forward, under the
//! same lock that registers subscriptions, so a socket that subscribes at
//! `delivered = L` gets every later action live and replays `(since, L]`
//! from the database without gaps or duplicates.
//!
//! Access: deltas of `repo`/`org`/`membership`/`team`/`viewerRepo` and
//! messages on `{prefix}sync:!access` (published for
//! `Event::AccessChanged` and repository updates/deletes) trigger a
//! permission recheck of the affected sockets; every 5 minutes all sockets
//! are rechecked as a safety net. Lost scopes are revoked.
//!
//! Backpressure: each socket has a bounded queue; a socket that can't keep
//! up is flagged and closed, and the client resumes from its `lastSyncId`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use bgh_core::auth::AuthContext;
use bgh_core::state::AppState;
use bgh_core::sync::SyncRecord;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio::time::{Instant, sleep_until};

use crate::delta::{self, Item};
use crate::scopes;

/// Coalescing window for live actions.
const WINDOW: Duration = Duration::from_millis(10);
/// Poll interval for lost messages.
const POLL: Duration = Duration::from_secs(1);
/// Interval of the safety-net permission recheck of every socket.
const FULL_RECHECK: Duration = Duration::from_secs(300);
/// Queue length (hub messages) per socket before it counts as slow.
pub const QUEUE: usize = 256;
/// Page size when reading gaps from the database.
const FILL_PAGE: i64 = 2000;
pub use bgh_core::sync::ACCESS_CHANNEL;

/// What the hub sends a socket.
#[derive(Debug, Clone)]
pub enum HubMsg {
    /// Live actions in ascending id order (the socket filters by scope).
    Items(Arc<Vec<Item>>),
    /// The viewer lost access to this scope.
    Revoke(String),
    /// Close the socket (e.g. 4001 after sign-out).
    Close { code: u16, reason: &'static str },
}

/// Payload of `{prefix}sync:!access`. All ids `None` = recheck everyone.
/// With `sign_out`, the sockets of `user_id` (only those authenticated by
/// `session_id`, when given) are closed with 4001 instead
/// (`bgh_core::sync::signal_signed_out`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccessChange {
    #[serde(default)]
    pub repo_id: Option<i64>,
    #[serde(default)]
    pub org_id: Option<i64>,
    #[serde(default)]
    pub user_id: Option<i64>,
    #[serde(default)]
    pub session_id: Option<i64>,
    #[serde(default)]
    pub sign_out: bool,
}

/// Publish an access change to every process's hub.
pub async fn publish_access_change(state: &AppState, change: &AccessChange) {
    let mut redis = state.redis.clone();
    let payload = serde_json::to_string(change).expect("serializable");
    let res: Result<(), _> = redis::cmd("PUBLISH")
        .arg(state.redis_key(ACCESS_CHANNEL))
        .arg(payload)
        .query_async(&mut redis)
        .await;
    if let Err(err) = res {
        tracing::warn!(?err, "publishing sync access change");
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Target {
    All,
    User(i64),
    Scope(String),
}

struct ConnEntry {
    tx: mpsc::Sender<HubMsg>,
    auth: Arc<AuthContext>,
    scopes: HashSet<String>,
    overflow: Arc<AtomicBool>,
}

#[derive(Default)]
struct Inner {
    delivered: i64,
    conns: HashMap<u64, ConnEntry>,
    by_scope: HashMap<String, HashSet<u64>>,
}

pub struct Hub {
    state: AppState,
    inner: Mutex<Inner>,
    next_id: AtomicU64,
}

/// A socket's registration; unregisters on drop.
pub struct Conn {
    pub id: u64,
    pub rx: mpsc::Receiver<HubMsg>,
    /// Set when the socket fell behind and must be closed.
    pub overflow: Arc<AtomicBool>,
    hub: Arc<Hub>,
}

impl Drop for Conn {
    fn drop(&mut self) {
        self.hub.remove(self.id);
    }
}

impl Conn {
    pub fn hub(&self) -> &Arc<Hub> {
        &self.hub
    }
}

type Registry = tokio::sync::Mutex<HashMap<String, Weak<Hub>>>;

fn registry() -> &'static Registry {
    static HUBS: std::sync::OnceLock<Registry> = std::sync::OnceLock::new();
    HUBS.get_or_init(Default::default)
}

/// Escape Redis glob characters.
fn glob_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '*' | '?' | '[' | ']' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

impl Hub {
    /// The process's hub for this state's Redis prefix, started on demand.
    /// It stops once its last connection is gone.
    pub async fn get(state: &AppState) -> anyhow::Result<Arc<Hub>> {
        let key = state.config.redis_prefix.clone();
        let mut hubs = registry().lock().await;
        if let Some(hub) = hubs.get(&key).and_then(Weak::upgrade) {
            return Ok(hub);
        }
        let stream = subscribe(state).await?;
        let delivered = delta::head(&state.db).await?;
        let hub = Arc::new(Hub {
            state: state.clone(),
            inner: Mutex::new(Inner {
                delivered,
                ..Default::default()
            }),
            next_id: AtomicU64::new(1),
        });
        hubs.insert(key, Arc::downgrade(&hub));
        tokio::spawn(run(Arc::downgrade(&hub), state.clone(), stream));
        Ok(hub)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Highest sync id delivered to sockets.
    pub fn delivered(&self) -> i64 {
        self.lock().delivered
    }

    /// Number of connected sockets.
    pub fn connections(&self) -> usize {
        self.lock().conns.len()
    }

    /// Register a socket (with no scopes yet).
    pub fn register(self: &Arc<Self>, auth: Arc<AuthContext>) -> Conn {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel(QUEUE);
        let overflow = Arc::new(AtomicBool::new(false));
        self.lock().conns.insert(
            id,
            ConnEntry {
                tx,
                auth,
                scopes: HashSet::new(),
                overflow: overflow.clone(),
            },
        );
        Conn {
            id,
            rx,
            overflow,
            hub: self.clone(),
        }
    }

    /// Start streaming `scopes` to the socket. Returns `L`: every action
    /// with id `> L` in these scopes will be delivered live; actions
    /// `<= L` must be replayed from the database.
    pub fn subscribe(&self, conn: u64, scopes: &[String]) -> i64 {
        let mut inner = self.lock();
        let inner = &mut *inner;
        if let Some(entry) = inner.conns.get_mut(&conn) {
            for s in scopes {
                entry.scopes.insert(s.clone());
                inner.by_scope.entry(s.clone()).or_default().insert(conn);
            }
        }
        inner.delivered
    }

    pub fn unsubscribe(&self, conn: u64, scopes: &[String]) {
        let mut inner = self.lock();
        let inner = &mut *inner;
        if let Some(entry) = inner.conns.get_mut(&conn) {
            for s in scopes {
                entry.scopes.remove(s);
                unindex(&mut inner.by_scope, s, conn);
            }
        }
    }

    fn remove(&self, conn: u64) {
        let mut inner = self.lock();
        let inner = &mut *inner;
        if let Some(entry) = inner.conns.remove(&conn) {
            for s in &entry.scopes {
                unindex(&mut inner.by_scope, s, conn);
            }
        }
    }

    /// Deliver `items` (ascending, all `<= upto`) and advance `delivered`.
    fn dispatch(&self, items: Vec<Item>, upto: i64) {
        let mut inner = self.lock();
        let inner = &mut *inner;
        inner.delivered = inner.delivered.max(upto);
        if items.is_empty() {
            return;
        }
        let mut targets: HashSet<u64> = HashSet::new();
        for item in &items {
            if let Some(conns) = inner.by_scope.get(&item.scope) {
                targets.extend(conns);
            }
        }
        let items = Arc::new(items);
        let mut dead = Vec::new();
        for id in targets {
            let Some(entry) = inner.conns.get(&id) else {
                continue;
            };
            if entry.tx.try_send(HubMsg::Items(items.clone())).is_err() {
                entry.overflow.store(true, Ordering::SeqCst);
                dead.push(id);
            }
        }
        for id in dead {
            if let Some(entry) = inner.conns.remove(&id) {
                for s in &entry.scopes {
                    unindex(&mut inner.by_scope, s, id);
                }
            }
        }
    }

    /// Unregister a socket and tell it to close.
    fn close(&self, conn: u64, code: u16, reason: &'static str) {
        let mut inner = self.lock();
        let inner = &mut *inner;
        if let Some(entry) = inner.conns.remove(&conn) {
            for s in &entry.scopes {
                unindex(&mut inner.by_scope, s, conn);
            }
            if entry.tx.try_send(HubMsg::Close { code, reason }).is_err() {
                entry.overflow.store(true, Ordering::SeqCst);
            }
        }
    }

    /// Close the sockets of a signed-out user (or of one of their sessions).
    fn sign_out(&self, user_id: i64, session_id: Option<i64>) {
        let ids: Vec<u64> = self
            .lock()
            .conns
            .iter()
            .filter(|(_, c)| {
                c.auth.user.id == user_id
                    && session_id.is_none_or(|sid| {
                        c.auth.method == bgh_core::auth::AuthMethod::Session { session_id: sid }
                    })
            })
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            self.close(id, crate::ws::CLOSE_UNAUTHENTICATED, "signed out");
        }
    }

    /// Remove `scope` from a socket and tell it.
    fn revoke(&self, conn: u64, scope: &str) {
        let mut inner = self.lock();
        let inner = &mut *inner;
        let Some(entry) = inner.conns.get_mut(&conn) else {
            return;
        };
        if !entry.scopes.remove(scope) {
            return;
        }
        if entry
            .tx
            .try_send(HubMsg::Revoke(scope.to_string()))
            .is_err()
        {
            entry.overflow.store(true, Ordering::SeqCst);
        }
        unindex(&mut inner.by_scope, scope, conn);
    }

    /// Flush buffered live records (see module docs).
    async fn flush(
        self: &Arc<Self>,
        pending: &mut BTreeMap<i64, SyncRecord>,
    ) -> Result<(), sqlx::Error> {
        let l = self.delivered();
        *pending = pending.split_off(&(l + 1));
        let Some(&m) = pending.keys().next_back() else {
            return Ok(());
        };
        let contiguous = pending.len() as i64 == m - l;
        let records: Vec<SyncRecord> = if contiguous {
            pending.values().cloned().collect()
        } else {
            self.read_range(l, m).await?
        };
        self.deliver(records, m).await?;
        pending.clear();
        Ok(())
    }

    /// Catch up with the database head (lost messages, reconnects).
    async fn catch_up(self: &Arc<Self>) -> Result<(), sqlx::Error> {
        let head = delta::head(&self.state.db).await?;
        let l = self.delivered();
        if head > l {
            let records = self.read_range(l, head).await?;
            self.deliver(records, head).await?;
        }
        Ok(())
    }

    async fn read_range(&self, after: i64, upto: i64) -> Result<Vec<SyncRecord>, sqlx::Error> {
        let mut out = Vec::new();
        let mut from = after;
        loop {
            let page = delta::fetch_range(&self.state.db, from, upto, FILL_PAGE).await?;
            let done = (page.len() as i64) < FILL_PAGE;
            if let Some(last) = page.last() {
                from = last.id;
            }
            out.extend(page);
            if done {
                return Ok(out);
            }
        }
    }

    async fn deliver(
        self: &Arc<Self>,
        records: Vec<SyncRecord>,
        upto: i64,
    ) -> Result<(), sqlx::Error> {
        let mut targets = HashSet::new();
        for r in &records {
            match r.model.as_str() {
                "repo" | "org" | "membership" | "team" | "viewerRepo" => {
                    targets.insert(Target::Scope(r.scope.clone()));
                }
                _ => {}
            }
        }
        let mut conn = self.state.db.acquire().await?;
        let items = delta::to_items(&mut conn, &records).await?;
        drop(conn);
        for chunk in items.chunks(delta::BATCH_SIZE as usize) {
            let last = chunk.last().map(|i| i.id).unwrap_or(upto);
            self.dispatch(chunk.to_vec(), last.min(upto));
        }
        self.dispatch(Vec::new(), upto);
        if !targets.is_empty() {
            self.spawn_recheck(targets.into_iter().collect());
        }
        Ok(())
    }

    fn spawn_recheck(self: &Arc<Self>, targets: Vec<Target>) {
        let hub = self.clone();
        tokio::spawn(async move {
            if let Err(err) = hub.recheck(&targets).await {
                tracing::warn!(?err, "sync permission recheck");
            }
        });
    }

    /// Recheck the scopes of every socket matching `targets`; revoke lost ones.
    async fn recheck(&self, targets: &[Target]) -> Result<(), sqlx::Error> {
        let conns: Vec<(u64, Arc<AuthContext>, Vec<String>)> = {
            let inner = self.lock();
            inner
                .conns
                .iter()
                .filter(|(_, c)| {
                    !c.scopes.is_empty()
                        && targets.iter().any(|t| match t {
                            Target::All => true,
                            Target::User(u) => c.auth.user.id == *u,
                            Target::Scope(s) => c.scopes.contains(s),
                        })
                })
                .map(|(id, c)| (*id, c.auth.clone(), c.scopes.iter().cloned().collect()))
                .collect()
        };
        if conns.is_empty() {
            return Ok(());
        }
        let mut db = self.state.db.acquire().await?;
        for (id, auth, scopes) in conns {
            // Deleted / suspended users and ended sessions are signed out.
            let session = match auth.method {
                bgh_core::auth::AuthMethod::Session { session_id } => Some(session_id),
                _ => None,
            };
            let signed_in: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM users WHERE id = $1 AND suspended_at IS NULL)
                    AND ($2::bigint IS NULL OR EXISTS (
                        SELECT 1 FROM sessions WHERE id = $2 AND user_id = $1 AND expires_at > now()))",
            )
            .bind(auth.user.id)
            .bind(session)
            .fetch_one(&mut *db)
            .await?;
            if !signed_in {
                self.close(id, crate::ws::CLOSE_UNAUTHENTICATED, "signed out");
                continue;
            }
            for scope in scopes::check(&mut db, &auth, &scopes).await?.denied {
                self.revoke(id, &scope);
            }
        }
        Ok(())
    }
}

fn unindex(by_scope: &mut HashMap<String, HashSet<u64>>, scope: &str, conn: u64) {
    if let Some(set) = by_scope.get_mut(scope) {
        set.remove(&conn);
        if set.is_empty() {
            by_scope.remove(scope);
        }
    }
}

async fn subscribe(state: &AppState) -> anyhow::Result<redis::aio::PubSubStream> {
    let client = redis::Client::open(state.config.redis_url.as_str())?;
    let mut pubsub = client.get_async_pubsub().await?;
    let pattern = format!("{}sync:*", glob_escape(&state.config.redis_prefix));
    pubsub.psubscribe(pattern).await?;
    Ok(pubsub.into_on_message())
}

fn far_future() -> Instant {
    Instant::now() + Duration::from_secs(86_400)
}

async fn run(weak: Weak<Hub>, state: AppState, mut stream: redis::aio::PubSubStream) {
    let access_channel = state.redis_key(ACCESS_CHANNEL);
    let mut pending: BTreeMap<i64, SyncRecord> = BTreeMap::new();
    let mut access: HashSet<Target> = HashSet::new();
    let mut flush_at: Option<Instant> = None;
    let mut tick = tokio::time::interval(POLL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_full = Instant::now();
    loop {
        tokio::select! {
            msg = stream.next() => {
                let Some(msg) = msg else {
                    tracing::warn!("sync hub: redis pubsub disconnected; reconnecting");
                    stream = loop {
                        if weak.strong_count() == 0 {
                            return;
                        }
                        tokio::time::sleep(Duration::from_millis(500)).await;
                        match subscribe(&state).await {
                            Ok(s) => break s,
                            Err(err) => tracing::warn!(?err, "sync hub: resubscribe failed"),
                        }
                    };
                    // Anything published while disconnected comes from the log.
                    flush_at.get_or_insert_with(Instant::now);
                    continue;
                };
                let payload: String = msg.get_payload().unwrap_or_default();
                if msg.get_channel_name() == access_channel {
                    let change: AccessChange = serde_json::from_str(&payload).unwrap_or_default();
                    match (change.sign_out, change.user_id) {
                        (true, Some(user)) => {
                            if let Some(hub) = weak.upgrade() {
                                hub.sign_out(user, change.session_id);
                            }
                            continue;
                        }
                        _ => access.extend(targets_of(&change)),
                    }
                } else {
                    match serde_json::from_str::<SyncRecord>(&payload) {
                        Ok(rec) => {
                            pending.insert(rec.id, rec);
                        }
                        Err(err) => tracing::warn!(?err, "sync hub: bad message"),
                    }
                }
                flush_at.get_or_insert_with(|| Instant::now() + WINDOW);
            }
            _ = sleep_until(flush_at.unwrap_or_else(far_future)), if flush_at.is_some() => {
                flush_at = None;
                let Some(hub) = weak.upgrade() else { return };
                if let Err(err) = hub.flush(&mut pending).await {
                    tracing::warn!(?err, "sync hub: flush failed; retrying");
                    flush_at = Some(Instant::now() + POLL);
                }
                if !access.is_empty() {
                    hub.spawn_recheck(access.drain().collect());
                }
            }
            _ = tick.tick() => {
                let Some(hub) = weak.upgrade() else { return };
                if pending.is_empty()
                    && let Err(err) = hub.catch_up().await
                {
                    tracing::warn!(?err, "sync hub: catch-up failed");
                }
                if last_full.elapsed() >= FULL_RECHECK {
                    last_full = Instant::now();
                    hub.spawn_recheck(vec![Target::All]);
                }
            }
        }
    }
}

fn targets_of(change: &AccessChange) -> Vec<Target> {
    let mut out = Vec::new();
    if let Some(id) = change.repo_id {
        out.push(Target::Scope(format!("repo:{id}")));
    }
    if let Some(id) = change.org_id {
        out.push(Target::Scope(format!("org:{id}")));
    }
    if let Some(id) = change.user_id {
        out.push(Target::User(id));
    }
    if out.is_empty() {
        out.push(Target::All);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_globs() {
        assert_eq!(glob_escape("a*b?[c]\\"), "a\\*b\\?\\[c\\]\\\\");
        assert_eq!(glob_escape("test:bgh_1:"), "test:bgh_1:");
    }

    #[test]
    fn access_targets() {
        assert_eq!(targets_of(&AccessChange::default()), vec![Target::All]);
        assert_eq!(
            targets_of(&AccessChange {
                repo_id: Some(1),
                user_id: Some(2),
                ..Default::default()
            }),
            vec![Target::Scope("repo:1".into()), Target::User(2)]
        );
    }
}
