//! Local-first sync log.
//!
//! Every mutation of a synced model appends a row to `sync_actions` in the
//! same transaction ([`record`]); after commit the rows are published on the
//! Redis channel `{prefix}sync:{scope}` ([`notify`]). Prefer
//! [`crate::db::Tx::sync`], which does both at the right time.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::state::AppState;

pub mod context;
pub mod shapes;

pub use context::{RequestSync, client_tx};

/// Key of the transaction-scoped advisory lock taken by [`record`]: writers
/// of synced data serialize on it, so sync ids commit (become visible) in
/// id order (docs/SYNC_PROTOCOL.md section 2).
pub const SYNC_LOCK: i64 = 0x6267_685f_7379_6e63; // "bgh_sync"

/// Version of the client model shapes ([`shapes`]); bump with
/// docs/SYNC_PROTOCOL.md.
pub const SCHEMA_VERSION: i64 = 1;

/// Insert / Update / Delete.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SyncAction {
    #[serde(rename = "I")]
    Insert,
    #[serde(rename = "U")]
    Update,
    #[serde(rename = "D")]
    Delete,
}

impl SyncAction {
    pub fn code(self) -> &'static str {
        match self {
            Self::Insert => "I",
            Self::Update => "U",
            Self::Delete => "D",
        }
    }
}

/// `repo:{id}` — issues, PR metadata, labels, milestones, comments, refs.
pub fn repo_scope(repo_id: i64) -> String {
    format!("repo:{repo_id}")
}

/// `user:{id}` — notifications, preferences.
pub fn user_scope(user_id: i64) -> String {
    format!("user:{user_id}")
}

/// `org:{id}` — members, teams.
pub fn org_scope(org_id: i64) -> String {
    format!("org:{org_id}")
}

/// A recorded sync action; also the wire format published to Redis
/// (`{"id","scope","model","mid","a","d","tx"}`). bgh-sync turns it into
/// the client `delta` message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SyncRecord {
    pub id: i64,
    pub scope: String,
    pub model: String,
    #[serde(rename = "mid")]
    pub model_id: i64,
    #[serde(rename = "a")]
    pub action: SyncAction,
    #[serde(rename = "d")]
    pub data: Value,
    /// `X-Client-Tx` of the request that caused the action.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tx: Option<Uuid>,
}

/// Append to `sync_actions`. Call inside the transaction that performs the
/// change (`&mut *tx`). The row carries the current request's
/// `X-Client-Tx` ([`client_tx`]).
///
/// Takes the [`SYNC_LOCK`] advisory lock (held until commit), so keep the
/// time between the first `record` and the commit short: record sync
/// actions at the end of the transaction.
pub async fn record(
    conn: &mut PgConnection,
    scope: &str,
    model: &str,
    model_id: i64,
    action: SyncAction,
    data: &Value,
) -> Result<SyncRecord, sqlx::Error> {
    record_with_tx(conn, scope, model, model_id, action, data, client_tx()).await
}

/// [`record`] with an explicit client transaction id.
pub async fn record_with_tx(
    conn: &mut PgConnection,
    scope: &str,
    model: &str,
    model_id: i64,
    action: SyncAction,
    data: &Value,
    tx: Option<Uuid>,
) -> Result<SyncRecord, sqlx::Error> {
    // The lock is taken before the identity default is evaluated, so ids are
    // allocated (and committed) in lock order.
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO sync_actions (scope, model, model_id, action, data, tx)
         SELECT $1, $2, $3, $4, $5, $6 FROM (SELECT pg_advisory_xact_lock($7)) l
         RETURNING id",
    )
    .bind(scope)
    .bind(model)
    .bind(model_id)
    .bind(action.code())
    .bind(data)
    .bind(tx)
    .bind(SYNC_LOCK)
    .fetch_one(conn)
    .await?;
    Ok(SyncRecord {
        id,
        scope: scope.to_string(),
        model: model.to_string(),
        model_id,
        action,
        data: data.clone(),
        tx,
    })
}

/// Redis channel for a scope.
pub fn channel(state: &AppState, scope: &str) -> String {
    state.redis_key(&format!("sync:{scope}"))
}

/// Publish committed records to Redis. Failures are logged, not returned:
/// clients recover from `sync_actions` on reconnect.
pub async fn notify(state: &AppState, records: &[SyncRecord]) {
    if records.is_empty() {
        return;
    }
    if let Some(max) = records.iter().map(|r| r.id).max() {
        context::note_committed(max);
    }
    let mut pipe = redis::pipe();
    for rec in records {
        match serde_json::to_string(rec) {
            Ok(msg) => {
                pipe.publish(channel(state, &rec.scope), msg).ignore();
            }
            Err(err) => tracing::error!(?err, "serializing sync record"),
        }
    }
    let mut conn = state.redis.clone();
    if let Err(err) = pipe.query_async::<()>(&mut conn).await {
        tracing::warn!(?err, "publishing sync records to redis");
    }
}

// ----- scope providers (bootstrap extension point) -------------------------

/// Rows a [`ScopeProvider`] contributes to a bootstrap snapshot of one scope.
#[derive(Debug, Default, Clone)]
pub struct ScopeRows {
    /// `(model name, compact rows)`; several entries per model are merged.
    pub models: Vec<(&'static str, Vec<Value>)>,
    /// Users referenced by the rows (the bootstrap includes them in `user`).
    pub user_ids: Vec<i64>,
}

/// Loads a provider's rows for `scope` (`org:{id}` / `user:{id}` /
/// `repo:{id}`) as seen by `viewer`, inside the bootstrap's
/// `REPEATABLE READ` transaction. Return empty rows for scopes it doesn't own.
pub type ScopeLoadFn = for<'c> fn(
    &'c mut PgConnection,
    &'c str,
    Option<i64>,
)
    -> futures::future::BoxFuture<'c, Result<ScopeRows, sqlx::Error>>;

/// A crate that owns synced models outside `bgh-sync` (e.g. bgh-projects)
/// registers one of these from its `register()` via
/// [`crate::Registry::scope_provider`]; `bgh-sync` calls [`load_provided`]
/// for every scope it bootstraps.
#[derive(Clone, Copy)]
pub struct ScopeProvider {
    pub name: &'static str,
    /// Model names this provider emits (for docs / schema checks).
    pub models: &'static [&'static str],
    pub load: ScopeLoadFn,
}

static PROVIDERS: std::sync::RwLock<Vec<ScopeProvider>> = std::sync::RwLock::new(Vec::new());

/// Register a provider (idempotent per `name`).
pub fn register_scope_provider(provider: ScopeProvider) {
    let mut list = PROVIDERS.write().unwrap_or_else(|e| e.into_inner());
    if !list.iter().any(|p| p.name == provider.name) {
        list.push(provider);
    }
}

/// All registered providers.
pub fn scope_providers() -> Vec<ScopeProvider> {
    PROVIDERS.read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Run every registered provider for `scope` and merge their rows.
pub async fn load_provided(
    conn: &mut PgConnection,
    scope: &str,
    viewer: Option<i64>,
) -> Result<ScopeRows, sqlx::Error> {
    let mut out = ScopeRows::default();
    for provider in scope_providers() {
        let rows = (provider.load)(&mut *conn, scope, viewer).await?;
        out.models.extend(rows.models);
        out.user_ids.extend(rows.user_ids);
    }
    Ok(out)
}
