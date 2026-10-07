//! Live fan-out of sync actions to WebSocket connections.
//!
//! One [`Hub`] per process (per Redis prefix): a single Redis connection
//! `PSUBSCRIBE`s `{prefix}sync:*` and multiplexes deltas to every socket;
//! sockets never talk to Redis themselves.
//!
//! Ordering and completeness: sync ids may commit out of order (writers
//! take no lock), so the hub never delivers past the commit-order watermark
//! (`bgh_core::seqlog`): every id `<=` it has a committed row, so nothing
//! can appear below it later. Redis messages are buffered for ~10 ms; the
//! highest buffered id is `M`. When `(delivered, M]` is all in the buffer
//! (each message is published after its commit) it is delivered as is;
//! otherwise the hub delivers `(delivered, min(M, watermark)]` re-read from
//! `sync_actions` (out-of-order commits, lost messages, ids burned by
//! rollbacks, which `seqlog` fills) and keeps the rest buffered, retrying
//! every 20 ms. A 1-second poll of the watermark catches a lost tail, and a
//! Redis reconnect is followed by the same catch-up. `delivered` only moves forward, under the
//! same lock that registers subscriptions, so a socket that subscribes at
//! `delivered = L` gets every later action live and replays `(since, L]`
//! from the database without gaps or duplicates.
//!
//! Access: messages on `{prefix}sync:!access` (published for
//! `Event::AccessChanged` and repository updates/deletes) and deltas that
//! may change who can read a scope trigger a permission recheck of the
//! affected sockets; every 5 minutes all sockets are rechecked as a safety
//! net. Lost scopes are revoked. Rechecks are cheap and bounded:
//!
//! * `repo` deltas are mostly counter refreshes (stars, open issues, push
//!   time). The hub caches each locally subscribed repo's access key
//!   (visibility, owner) and rechecks only when it changes or the repo is
//!   deleted; an access message for the repo invalidates the entry.
//!   `viewerRepo` deltas recheck only that user's sockets for that repo;
//!   `membership` deltas only the member's sockets.
//! * A repo-level trigger checks only that scope of each socket, not all of
//!   its scopes.
//! * Triggers are coalesced into one pending set drained by a single worker
//!   task per hub. A pass checks signed-in state for all sockets in two
//!   queries, then checks scopes in chunks of users (`scopes::check_many`,
//!   `2 + users` queries per chunk) with at most `RECHECK_CONCURRENCY`
//!   chunks in flight, each holding one pool connection only for its own
//!   queries.
//!
//! Records whose scope has no local subscriber are not converted to items
//! (no user lookups for them).
//!
//! Backpressure: each socket has a bounded queue; a socket that can't keep
//! up is flagged and closed, and the client resumes from its `lastSyncId`.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use bgh_core::auth::AuthContext;
use bgh_core::state::AppState;
use bgh_core::sync::{SyncAction, SyncRecord};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::sync::{Notify, mpsc};
use tokio::time::{Instant, sleep_until};

use crate::delta::{self, Item};
use crate::scopes;

/// Coalescing window for live actions.
const WINDOW: Duration = Duration::from_millis(10);
/// Poll interval for lost messages.
const POLL: Duration = Duration::from_secs(1);
/// Re-flush delay while buffered records wait for an in-flight lower id.
const GAP_RETRY: Duration = Duration::from_millis(20);
/// Interval of the safety-net permission recheck of every socket.
const FULL_RECHECK: Duration = Duration::from_secs(300);
/// Queue length (hub messages) per socket before it counts as slow.
pub const QUEUE: usize = 256;
/// Page size when reading gaps from the database.
const FILL_PAGE: i64 = 2000;
/// Users per permission-recheck chunk (one pool connection each).
const RECHECK_USERS: usize = 64;
/// Recheck chunks in flight at once (pool connections used by rechecks).
const RECHECK_CONCURRENCY: usize = 2;
/// Delay before retrying the targets of a failed recheck pass.
const RECHECK_RETRY: Duration = Duration::from_secs(1);
/// Attempts at delivering only to locally subscribed scopes before
/// converting every record (subscriptions racing with a delivery).
const FILTER_ATTEMPTS: usize = 3;
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

/// What a permission recheck covers.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Target {
    /// Every scope of every socket.
    All,
    /// Every scope of the user's sockets.
    User(i64),
    /// Every scope of the sockets subscribed to this scope (org-level
    /// changes can affect the org's repositories too).
    Scope(String),
    /// Only this scope, of the sockets subscribed to it.
    Only(String),
    /// Only this scope, of the user's sockets subscribed to it.
    UserOnly(i64, String),
}

/// The part of a `repo` row that decides who can read it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RepoKey {
    visibility: Option<String>,
    owner_id: Option<i64>,
}

impl RepoKey {
    fn of(data: &serde_json::Value) -> Self {
        Self {
            visibility: data
                .get("visibility")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            owner_id: data.get("ownerId").and_then(|v| v.as_i64()),
        }
    }
}

/// Counters of permission-recheck work (tests and benchmarks).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RecheckStats {
    /// Recheck passes run by the worker.
    pub passes: u64,
    /// Socket checks (one socket in one pass).
    pub sockets: u64,
    /// Scope checks (one scope of one socket in one pass).
    pub scopes: u64,
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
    /// Last seen access key of locally subscribed `repo:{id}` scopes.
    repo_keys: HashMap<String, RepoKey>,
}

pub struct Hub {
    state: AppState,
    inner: Mutex<Inner>,
    next_id: AtomicU64,
    /// Pending recheck targets, drained by the recheck worker.
    rechecks: Mutex<HashSet<Target>>,
    recheck_wake: Arc<Notify>,
    passes: AtomicU64,
    sockets_checked: AtomicU64,
    scopes_checked: AtomicU64,
}

impl Drop for Hub {
    fn drop(&mut self) {
        // Let the worker see that the hub is gone.
        self.recheck_wake.notify_one();
    }
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
        let wake = Arc::new(Notify::new());
        let hub = Arc::new(Hub {
            state: state.clone(),
            inner: Mutex::new(Inner {
                delivered,
                ..Default::default()
            }),
            next_id: AtomicU64::new(1),
            rechecks: Mutex::default(),
            recheck_wake: wake.clone(),
            passes: AtomicU64::new(0),
            sockets_checked: AtomicU64::new(0),
            scopes_checked: AtomicU64::new(0),
        });
        hubs.insert(key, Arc::downgrade(&hub));
        tokio::spawn(run(Arc::downgrade(&hub), state.clone(), stream));
        tokio::spawn(recheck_worker(Arc::downgrade(&hub), wake));
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

    /// Permission-recheck work done so far.
    pub fn recheck_stats(&self) -> RecheckStats {
        RecheckStats {
            passes: self.passes.load(Ordering::SeqCst),
            sockets: self.sockets_checked.load(Ordering::SeqCst),
            scopes: self.scopes_checked.load(Ordering::SeqCst),
        }
    }

    /// Whether recheck targets are queued but not yet picked up.
    pub fn recheck_pending(&self) -> bool {
        !self
            .rechecks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty()
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
        let Some(entry) = inner.conns.get_mut(&conn) else {
            return;
        };
        let removed: Vec<&String> = scopes.iter().filter(|s| entry.scopes.remove(*s)).collect();
        for s in removed {
            unindex(inner, s, conn);
        }
    }

    fn remove(&self, conn: u64) {
        let mut inner = self.lock();
        let inner = &mut *inner;
        if let Some(entry) = inner.conns.remove(&conn) {
            for s in &entry.scopes {
                unindex(inner, s, conn);
            }
        }
    }

    /// Deliver `items` (ascending, all `<= upto`) and advance `delivered`.
    fn dispatch(inner: &mut Inner, items: Vec<Item>, upto: i64) {
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
                    unindex(inner, s, id);
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
                unindex(inner, s, conn);
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
        unindex(inner, scope, conn);
    }

    /// Flush buffered live records (see module docs).
    /// Returns whether records stay buffered (above the watermark).
    async fn flush(
        self: &Arc<Self>,
        pending: &mut BTreeMap<i64, SyncRecord>,
    ) -> Result<bool, sqlx::Error> {
        let l = self.delivered();
        *pending = pending.split_off(&(l + 1));
        let Some(&m) = pending.keys().next_back() else {
            return Ok(false);
        };
        // Every buffered record was published after its commit, so a
        // contiguous `(l, m]` is committed. Otherwise a missing id may still
        // be in flight: deliver only up to the watermark.
        if pending.len() as i64 == m - l {
            let records = pending.values().cloned().collect();
            self.deliver(records, m).await?;
            pending.clear();
            return Ok(false);
        }
        let upto = m.min(delta::head(&self.state.db).await?);
        if upto > l {
            let records = self.read_range(l, upto).await?;
            self.deliver(records, upto).await?;
            *pending = pending.split_off(&(upto + 1));
        }
        Ok(!pending.is_empty())
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
        // Only records some local socket is subscribed to become items. A
        // socket subscribing meanwhile (its `L` = `delivered` < `upto`)
        // must still get every record of its scopes, so the check is
        // repeated under the lock that advances `delivered`.
        let mut attempt = 0;
        let targets = loop {
            let filter = attempt < FILTER_ATTEMPTS;
            let (wanted, skipped): (Vec<SyncRecord>, HashSet<String>) = {
                let inner = self.lock();
                let mut skipped = HashSet::new();
                let wanted = records
                    .iter()
                    .filter(|r| {
                        let keep = !filter || inner.by_scope.contains_key(&r.scope);
                        if !keep {
                            skipped.insert(r.scope.clone());
                        }
                        keep
                    })
                    .cloned()
                    .collect();
                (wanted, skipped)
            };
            let items = if wanted.is_empty() {
                Vec::new()
            } else {
                let mut conn = self.state.db.acquire().await?;
                delta::to_items(&mut conn, &wanted).await?
            };
            let mut inner = self.lock();
            let inner = &mut *inner;
            if skipped.iter().any(|s| inner.by_scope.contains_key(s)) {
                attempt += 1;
                continue;
            }
            // Under the same lock as `delivered`: a socket that subscribed
            // during the await above is matched by these targets too.
            let mut targets = HashSet::new();
            for r in &records {
                targets.extend(access_target(inner, r));
            }
            for chunk in items.chunks(delta::BATCH_SIZE as usize) {
                let last = chunk.last().map(|i| i.id).unwrap_or(upto);
                Self::dispatch(inner, chunk.to_vec(), last.min(upto));
            }
            Self::dispatch(inner, Vec::new(), upto);
            break targets;
        };
        if !targets.is_empty() {
            self.spawn_recheck(targets);
        }
        Ok(())
    }

    /// Put back the targets of a failed pass, dropping the cached access keys
    /// of the scopes involved.
    fn requeue(&self, targets: Vec<Target>) {
        {
            let mut inner = self.lock();
            for t in &targets {
                if let Target::Only(s) | Target::Scope(s) | Target::UserOnly(_, s) = t {
                    inner.repo_keys.remove(s);
                }
            }
        }
        self.rechecks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extend(targets);
    }

    /// Queue a permission recheck; the worker coalesces queued targets.
    fn spawn_recheck(&self, targets: impl IntoIterator<Item = Target>) {
        self.rechecks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extend(targets);
        self.recheck_wake.notify_one();
    }

    /// Recheck the sockets matching `targets` (see [`Target`]); close the
    /// signed-out ones and revoke lost scopes.
    async fn recheck(&self, targets: &[Target]) -> Result<(), sqlx::Error> {
        let work: Vec<Work> = {
            let inner = self.lock();
            inner
                .conns
                .iter()
                .filter_map(|(id, c)| {
                    let scopes = scopes_to_check(c, targets);
                    (!scopes.is_empty()).then(|| (*id, c.auth.clone(), scopes))
                })
                .collect()
        };
        if work.is_empty() {
            return Ok(());
        }
        self.sockets_checked
            .fetch_add(work.len() as u64, Ordering::SeqCst);
        self.scopes_checked.fetch_add(
            work.iter().map(|w| w.2.len() as u64).sum(),
            Ordering::SeqCst,
        );

        // Deleted / suspended users and ended sessions are signed out.
        let users: Vec<i64> = work
            .iter()
            .map(|w| w.1.user.id)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let sessions: Vec<i64> = work
            .iter()
            .filter_map(|w| session_of(&w.1))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let (live_users, live_sessions) = {
            let mut db = self.state.db.acquire().await?;
            let live_users: HashSet<i64> = sqlx::query_scalar(
                "SELECT id FROM users WHERE id = ANY($1) AND suspended_at IS NULL",
            )
            .bind(&users)
            .fetch_all(&mut *db)
            .await?
            .into_iter()
            .collect();
            let live_sessions: HashSet<(i64, i64)> = if sessions.is_empty() {
                HashSet::new()
            } else {
                sqlx::query_as::<_, (i64, i64)>(
                    "SELECT id, user_id FROM sessions WHERE id = ANY($1) AND expires_at > now()",
                )
                .bind(&sessions)
                .fetch_all(&mut *db)
                .await?
                .into_iter()
                .collect()
            };
            (live_users, live_sessions)
        };
        let mut by_user: BTreeMap<i64, Vec<Work>> = BTreeMap::new();
        for (id, auth, scopes) in work {
            let uid = auth.user.id;
            let signed_in = live_users.contains(&uid)
                && session_of(&auth).is_none_or(|sid| live_sessions.contains(&(sid, uid)));
            if !signed_in {
                self.close(id, crate::ws::CLOSE_UNAUTHENTICATED, "signed out");
                continue;
            }
            by_user.entry(uid).or_default().push((id, auth, scopes));
        }

        let mut chunks: Vec<Vec<Work>> = Vec::new();
        for (i, group) in by_user.into_values().enumerate() {
            if i % RECHECK_USERS == 0 {
                chunks.push(Vec::new());
            }
            chunks.last_mut().expect("pushed").extend(group);
        }
        let mut chunks = futures::stream::iter(chunks)
            .map(|chunk| self.recheck_chunk(chunk))
            .buffer_unordered(RECHECK_CONCURRENCY);
        let mut result = Ok(());
        while let Some(res) = chunks.next().await {
            if let Err(err) = res {
                result = Err(err);
            }
        }
        result
    }

    /// Scope checks for one chunk of users, on one pool connection.
    async fn recheck_chunk(&self, chunk: Vec<Work>) -> Result<(), sqlx::Error> {
        let requests: Vec<(&AuthContext, &[String])> = chunk
            .iter()
            .map(|(_, auth, scopes)| (auth.as_ref(), scopes.as_slice()))
            .collect();
        let access = {
            let mut db = self.state.db.acquire().await?;
            scopes::check_many(&mut db, &requests).await?
        };
        for ((id, _, _), access) in chunk.iter().zip(access) {
            for scope in access.denied {
                self.revoke(*id, &scope);
            }
        }
        Ok(())
    }
}

/// A socket to recheck: id, auth and the scopes to check.
type Work = (u64, Arc<AuthContext>, Vec<String>);

fn session_of(auth: &AuthContext) -> Option<i64> {
    match auth.method {
        bgh_core::auth::AuthMethod::Session { session_id } => Some(session_id),
        _ => None,
    }
}

/// Which of a socket's scopes `targets` ask to recheck.
fn scopes_to_check(c: &ConnEntry, targets: &[Target]) -> Vec<String> {
    if c.scopes.is_empty() {
        return Vec::new();
    }
    let uid = c.auth.user.id;
    let mut out: BTreeSet<&String> = BTreeSet::new();
    for t in targets {
        match t {
            Target::All => return c.scopes.iter().cloned().collect(),
            Target::User(u) if *u == uid => return c.scopes.iter().cloned().collect(),
            Target::Scope(s) if c.scopes.contains(s) => {
                return c.scopes.iter().cloned().collect();
            }
            Target::Only(s) => {
                if let Some(s) = c.scopes.get(s) {
                    out.insert(s);
                }
            }
            Target::UserOnly(u, s) if *u == uid => {
                if let Some(s) = c.scopes.get(s) {
                    out.insert(s);
                }
            }
            _ => {}
        }
    }
    out.into_iter().cloned().collect()
}

/// The recheck a delivered record calls for, if any (see module docs).
/// Records of scopes without local subscribers need none: a socket
/// subscribing later is checked then.
fn access_target(inner: &mut Inner, r: &SyncRecord) -> Option<Target> {
    if !inner.by_scope.contains_key(&r.scope) {
        return None;
    }
    match r.model.as_str() {
        "repo" => {
            if r.action == SyncAction::Delete {
                inner.repo_keys.remove(&r.scope);
                return Some(Target::Only(r.scope.clone()));
            }
            let key = RepoKey::of(&r.data);
            if inner.repo_keys.get(&r.scope) == Some(&key) {
                return None;
            }
            inner.repo_keys.insert(r.scope.clone(), key);
            Some(Target::Only(r.scope.clone()))
        }
        "viewerRepo" => {
            let uid = r.scope.strip_prefix("user:")?.parse().ok()?;
            Some(Target::UserOnly(uid, format!("repo:{}", r.model_id)))
        }
        "membership" => match r.data.get("userId").and_then(|v| v.as_i64()) {
            Some(uid) => Some(Target::User(uid)),
            None => Some(Target::Scope(r.scope.clone())),
        },
        "org" | "team" => Some(Target::Scope(r.scope.clone())),
        _ => None,
    }
}

/// Drains the hub's pending recheck targets, one pass at a time.
async fn recheck_worker(weak: Weak<Hub>, wake: Arc<Notify>) {
    loop {
        wake.notified().await;
        let Some(hub) = weak.upgrade() else { return };
        let targets: Vec<Target> =
            std::mem::take(&mut *hub.rechecks.lock().unwrap_or_else(|e| e.into_inner()))
                .into_iter()
                .collect();
        if targets.is_empty() {
            continue;
        }
        let failed = hub.recheck(&targets).await.err();
        hub.passes.fetch_add(1, Ordering::SeqCst);
        if let Some(err) = failed {
            // Nothing may be lost: the access-key cache already recorded the
            // change, so a later counter delta won't trigger it again. Forget
            // those keys (the next delta rechecks too) and retry the targets.
            tracing::warn!(?err, "sync permission recheck failed; retrying");
            hub.requeue(targets);
            drop(hub);
            tokio::time::sleep(RECHECK_RETRY).await;
            wake.notify_one();
        }
    }
}

fn unindex(inner: &mut Inner, scope: &str, conn: u64) {
    if let Some(set) = inner.by_scope.get_mut(scope) {
        set.remove(&conn);
        if set.is_empty() {
            inner.by_scope.remove(scope);
            inner.repo_keys.remove(scope);
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
                        _ => {
                            if let (Some(repo), Some(hub)) = (change.repo_id, weak.upgrade()) {
                                hub.lock().repo_keys.remove(&format!("repo:{repo}"));
                            }
                            access.extend(targets_of(&change));
                        }
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
                match hub.flush(&mut pending).await {
                    Ok(false) => {}
                    // A lower id is still in flight; look again shortly.
                    Ok(true) => flush_at = Some(Instant::now() + GAP_RETRY),
                    Err(err) => {
                        tracing::warn!(?err, "sync hub: flush failed; retrying");
                        flush_at = Some(Instant::now() + POLL);
                    }
                }
                if !access.is_empty() {
                    hub.spawn_recheck(access.drain());
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
                    hub.spawn_recheck([Target::All]);
                }
            }
        }
    }
}

fn targets_of(change: &AccessChange) -> Vec<Target> {
    let mut out = Vec::new();
    if let Some(id) = change.repo_id {
        out.push(Target::Only(format!("repo:{id}")));
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
            vec![Target::Only("repo:1".into()), Target::User(2)]
        );
    }
}
