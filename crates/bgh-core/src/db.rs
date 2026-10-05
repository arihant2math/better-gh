//! Database helpers: embedded migrations and the [`Tx`] unit of work.

use std::ops::{Deref, DerefMut};

use serde::Serialize;
use sqlx::migrate::Migrator;
use sqlx::{PgConnection, PgPool, Postgres, Transaction};

use crate::error::ApiResult;
use crate::events::Event;
use crate::jobs::{self, JobPayload};
use crate::state::AppState;
use crate::sync::{self, SyncAction, SyncRecord};

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
pub struct Tx {
    tx: Transaction<'static, Postgres>,
    state: AppState,
    sync: Vec<SyncRecord>,
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

    /// Record a sync action in this transaction; it's published to clients
    /// after commit. `data` is the client-shape JSON of the model (for
    /// deletes, typically `{}` or `{"id": ..}`).
    pub async fn sync(
        &mut self,
        scope: &str,
        model: &str,
        model_id: i64,
        action: SyncAction,
        data: &impl Serialize,
    ) -> ApiResult<()> {
        let data = serde_json::to_value(data)?;
        let rec = sync::record(&mut self.tx, scope, model, model_id, action, &data).await?;
        self.sync.push(rec);
        Ok(())
    }

    /// Queue a domain event, emitted after commit.
    pub fn emit(&mut self, event: Event) {
        self.events.push(event);
    }

    /// Enqueue a background job in this transaction.
    pub async fn enqueue<J: JobPayload>(&mut self, job: &J) -> Result<i64, sqlx::Error> {
        jobs::enqueue_job(&mut *self.tx, job).await
    }

    /// Commit, then publish sync records and emit events.
    pub async fn commit(self) -> Result<(), sqlx::Error> {
        let Self {
            tx,
            state,
            sync,
            events,
        } = self;
        tx.commit().await?;
        sync::notify(&state, &sync).await;
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
