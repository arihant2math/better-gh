//! Database helpers: embedded migrations and the [`Tx`] unit of work.

use std::ops::{Deref, DerefMut};

use serde::Serialize;
use sqlx::migrate::Migrator;
use sqlx::{PgConnection, PgPool, Postgres, Transaction};

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
/// Sync actions are written to `sync_actions` at [`Tx::commit`] (one
/// statement, right before the commit), so the ordering lock
/// ([`sync::SYNC_LOCK`]) is held only for the commit itself and never while
/// the transaction still takes row locks.
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

    /// Queue a domain event, emitted after commit.
    /// The application state this transaction was started with.
    pub fn state(&self) -> &AppState {
        &self.state
    }

    pub fn emit(&mut self, event: Event) {
        self.events.push(event);
    }

    /// Enqueue a background job in this transaction.
    pub async fn enqueue<J: JobPayload>(&mut self, job: &J) -> Result<i64, sqlx::Error> {
        jobs::enqueue_job(&mut *self.tx, job).await
    }

    /// Write the pending sync actions, commit, then publish them and emit
    /// events.
    pub async fn commit(self) -> Result<(), sqlx::Error> {
        let Self {
            mut tx,
            state,
            sync,
            events,
        } = self;
        let records = sync::record_all(&mut tx, sync).await?;
        tx.commit().await?;
        sync::notify(&state, &records).await;
        for event in events {
            state.events.emit(event);
        }
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
