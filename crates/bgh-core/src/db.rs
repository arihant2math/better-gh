//! Database helpers: embedded migrations, the [`Tx`] unit of work and
//! [`AdvisoryLock`].

use std::ops::{Deref, DerefMut};

use serde::Serialize;
use sqlx::migrate::Migrator;
use sqlx::{Connection, PgConnection, PgPool, Postgres, Transaction};

use crate::error::ApiResult;
use crate::events::Event;
use crate::jobs::{self, JobPayload};
use crate::state::AppState;
use crate::sync::{self, PendingSync, SyncAction};

pub use crate::error::unique_violation;

/// All migrations in `/migrations`, embedded at compile time.
pub static MIGRATOR: Migrator = sqlx::migrate!("../../migrations");

/// Apply pending migrations.
pub async fn migrate(db: &PgPool) -> Result<(), sqlx::migrate::MigrateError> {
    MIGRATOR.run(db).await
}

/// A session-level pg advisory lock (leader election, per-resource
/// exclusion), held on a dedicated connection opened outside the pool.
///
/// Never hold one on a pooled connection: the holder then needs a second
/// pooled connection for its work, and a few holders at once (services
/// starting together on a small pool) deadlock until the acquire timeout
/// (#341). Released by [`AdvisoryLock::release`] or by dropping it (the
/// connection closes, which ends the session and its locks).
///
/// Opening the connection is bounded by the pool's `acquire_timeout`, as a
/// pooled acquire would be (#362): a hung connect fails with a
/// [`std::io::ErrorKind::TimedOut`] error, which callers log and retry on
/// their next tick.
pub struct AdvisoryLock {
    conn: PgConnection,
}

impl AdvisoryLock {
    /// Take `key` if it is free, else `None`. The connect and the
    /// `pg_try_advisory_lock` round-trip together are bounded by the pool's
    /// `acquire_timeout`.
    pub async fn try_acquire(db: &PgPool, key: i64) -> sqlx::Result<Option<Self>> {
        bounded(db, async {
            let mut conn = PgConnection::connect_with(&db.connect_options()).await?;
            let got: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
                .bind(key)
                .fetch_one(&mut conn)
                .await?;
            Ok(got.then_some(Self { conn }))
        })
        .await
    }

    /// Take `key`, waiting while another session holds it. Only the connect
    /// is bounded (by the pool's `acquire_timeout`); the wait for the lock
    /// itself is not, since waiting is the point. Callers that need a bound
    /// on the wait drop the future (e.g. `tokio::time::timeout`), which
    /// closes the connection.
    pub async fn acquire(db: &PgPool, key: i64) -> sqlx::Result<Self> {
        let mut conn = bounded(db, PgConnection::connect_with(&db.connect_options())).await?;
        sqlx::query("SELECT pg_advisory_lock($1)")
            .bind(key)
            .execute(&mut conn)
            .await?;
        Ok(Self { conn })
    }

    /// Release the lock (closes its connection).
    pub async fn release(self) {
        let _ = self.conn.close().await;
    }
}

/// Run `fut` within the pool's `acquire_timeout`.
async fn bounded<T>(
    db: &PgPool,
    fut: impl std::future::Future<Output = sqlx::Result<T>>,
) -> sqlx::Result<T> {
    let limit = db.options().get_acquire_timeout();
    tokio::time::timeout(limit, fut).await.unwrap_or_else(|_| {
        Err(sqlx::Error::Io(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!("advisory lock: no connection within {limit:?}"),
        )))
    })
}

/// A database transaction that also collects post-commit side effects.
///
/// ```ignore
/// let mut tx = Tx::begin(&state).await?;
/// let id: i64 = sqlx::query_scalar("INSERT ... RETURNING id").fetch_one(&mut *tx).await?;
/// tx.sync(&scope, "label", id, SyncAction::Insert, &label_json).await?;
/// tx.enqueue(&SomeJob { id }).await?;
/// tx.emit(Event::...);
/// tx.commit().await?; // commits, then publishes sync deltas and events
/// ```
///
/// Dropping a `Tx` without committing rolls back and discards side effects.
///
/// Sync actions and outbox events are written at [`Tx::commit`] (one
/// statement each, right before the commit). They take no lock: concurrent
/// commits may make ids visible out of order, and readers stop at the
/// commit-order watermark ([`crate::seqlog`]).
pub struct Tx {
    tx: Transaction<'static, Postgres>,
    state: AppState,
    sync: Vec<PendingSync>,
    events: Vec<Event>,
}

impl Tx {
    pub async fn begin(state: &AppState) -> Result<Self, sqlx::Error> {
        Ok(Self {
            tx: state.db.begin().await?,
            state: state.clone(),
            sync: Vec::new(),
            events: Vec::new(),
        })
    }

    /// Record a sync action in this transaction (written at commit, then
    /// published to clients). `data` is the client-shape JSON of the model
    /// (`null` for deletes). Domain crates use the shape helpers
    /// (`sync_model`, `sync_issue`, `sync_delete`, ...) instead of building
    /// `data` themselves (BACKEND_PATTERNS.md §8a).
    pub async fn sync(
        &mut self,
        scope: &str,
        model: &str,
        model_id: i64,
        action: SyncAction,
        data: &impl Serialize,
    ) -> ApiResult<()> {
        self.sync.push(PendingSync {
            scope: scope.to_string(),
            model: model.to_string(),
            model_id,
            action,
            data: serde_json::to_value(data)?,
            tx: sync::client_tx(),
        });
        Ok(())
    }

    /// The application state this transaction was started with.
    pub fn state(&self) -> &AppState {
        &self.state
    }

    /// Queue a domain event: written to the outbox at commit (atomically
    /// with the transaction), delivered to listeners after it.
    pub fn emit(&mut self, event: Event) {
        self.events.push(event);
    }

    /// Enqueue a background job in this transaction.
    pub async fn enqueue<J: JobPayload>(&mut self, job: &J) -> Result<i64, sqlx::Error> {
        jobs::enqueue_job(&mut *self.tx, job).await
    }

    /// Write the pending sync actions and outbox events, commit, then
    /// publish the sync actions and wake event consumers.
    pub async fn commit(self) -> Result<(), sqlx::Error> {
        let Self {
            mut tx,
            state,
            sync,
            events,
        } = self;
        let records = sync::record_all(&mut tx, sync).await?;
        crate::outbox::append(&mut tx, &events).await?;
        tx.commit().await?;
        sync::notify(&state, &records).await;
        state.events.committed(events);
        Ok(())
    }

    /// Explicit rollback (dropping also rolls back).
    pub async fn rollback(self) -> Result<(), sqlx::Error> {
        self.tx.rollback().await
    }
}

impl Deref for Tx {
    type Target = PgConnection;
    fn deref(&self) -> &PgConnection {
        &self.tx
    }
}

impl DerefMut for Tx {
    fn deref_mut(&mut self) -> &mut PgConnection {
        &mut self.tx
    }
}
