//! Commit-order watermarks of the append-only id logs (`sync_actions`,
//! `event_outbox`): SYNC_PROTOCOL.md §2.
//!
//! Writers don't serialize: ids come from the table's sequence and
//! transactions commit in any order, so at a given moment the visible ids
//! can have gaps that are still in flight (and will commit later) or burned
//! (rolled back, never to commit). Readers therefore never consume past the
//! **watermark** `W`: the largest id such that every id `<= W` has a
//! committed row. Nothing can ever commit with an id `<= W` afterwards
//! (the primary key holds a committed row for each), so a reader that
//! consumes `(cursor, W]` in id order never misses or reorders a row.
//!
//! [`advance`] moves `W` forward over the visible rows. A gap is closed by
//! inserting a filler row ([`GAP`]) for each missing id with
//! `ON CONFLICT (id) DO NOTHING` and a short `lock_timeout`:
//!
//! * a writer that inserted the id and is still in flight makes the insert
//!   wait (unique index); it times out and the gap stays open, or the writer
//!   finishes meanwhile: committed means conflict (its row stays), rolled
//!   back means the filler is inserted;
//! * a burned id gets the filler.
//!
//! Gaps are only filled once the row after them is older than [`GRACE`], so
//! a filler practically never races a writer that has drawn the id but not
//! inserted its row yet; if one does, the writer's `ON CONFLICT DO NOTHING`
//! insert comes back short and it retries with fresh ids
//! ([`crate::sync::record_all`], [`crate::outbox::append`]).
//!
//! Fillers have scope / kind [`GAP`]: no client scope or event matches, so
//! readers skip them. `W` is persisted in `log_watermarks` (monotonic, at
//! most every 100 ms per process), and compaction / pruning never delete
//! rows above it.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use sqlx::PgPool;

/// Scope (`sync_actions`) / kind (`event_outbox`) of filler rows.
pub const GAP: &str = "!gap";
/// How old the row after a gap must be before the gap is filled.
pub const GRACE: Duration = Duration::from_millis(250);
/// How long filling waits for an in-flight writer of a missing id.
const FILL_LOCK_TIMEOUT: &str = "100ms";
/// Ids read per scan step.
const SCAN: i64 = 10_000;
/// Most ids filled in one statement.
const FILL_MAX: i64 = 10_000;
/// Least time between two writes of a log's persisted watermark from one
/// process (every reader advances it; the row would be a hot spot).
const PERSIST_EVERY: Duration = Duration::from_millis(100);

/// An append-only log with an id sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Log {
    /// `sync_actions`.
    Sync,
    /// `event_outbox`.
    Outbox,
}

impl Log {
    pub fn table(self) -> &'static str {
        match self {
            Self::Sync => "sync_actions",
            Self::Outbox => "event_outbox",
        }
    }

    fn fill_sql(self) -> &'static str {
        match self {
            Self::Sync => {
                "INSERT INTO sync_actions (id, scope, model, model_id, action, data, created_at)
                 OVERRIDING SYSTEM VALUE
                 SELECT g, '!gap', '', 0, 'D', '{}', clock_timestamp()
                   FROM generate_series($1::bigint, $2::bigint) g
                 ON CONFLICT (id) DO NOTHING"
            }
            Self::Outbox => {
                "INSERT INTO event_outbox (id, kind, payload, created_at)
                 SELECT g, '!gap', '{}', clock_timestamp()
                   FROM generate_series($1::bigint, $2::bigint) g
                 ON CONFLICT (id) DO NOTHING"
            }
        }
    }
}

/// The persisted watermark (may lag [`advance`]).
pub async fn stored(db: &PgPool, log: Log) -> Result<i64, sqlx::Error> {
    let w: Option<i64> = sqlx::query_scalar("SELECT id FROM log_watermarks WHERE log = $1")
        .bind(log.table())
        .fetch_optional(db)
        .await?;
    Ok(w.unwrap_or(0))
}

/// Advance the watermark of `log` as far as committed rows allow (filling
/// burned ids once they are older than [`GRACE`]) and return it. Every id
/// `<= W` has a committed row when this returns.
pub async fn advance(db: &PgPool, log: Log) -> Result<i64, sqlx::Error> {
    let start = stored(db, log).await?;
    let mut w = start;
    let scan = format!(
        "SELECT id, created_at < clock_timestamp() - make_interval(secs => $2)
           FROM {} WHERE id > $1 ORDER BY id LIMIT $3",
        log.table()
    );
    'scan: loop {
        let rows: Vec<(i64, bool)> = sqlx::query_as(&scan)
            .bind(w)
            .bind(GRACE.as_secs_f64())
            .bind(SCAN)
            .fetch_all(db)
            .await?;
        let full = rows.len() as i64 == SCAN;
        for (id, old) in rows {
            if id != w + 1 {
                // Missing: (w, id). Only the gap's age (>= the next row's)
                // decides whether to try closing it now.
                if !old {
                    break 'scan;
                }
                let end = (id - 1).min(w + FILL_MAX);
                if !fill(db, log, w + 1, end).await? {
                    break 'scan;
                }
                w = end;
                if end < id - 1 {
                    continue 'scan;
                }
            }
            w = id;
        }
        if !full {
            break;
        }
    }
    if w > start && persist_due(db, log) {
        sqlx::query("UPDATE log_watermarks SET id = greatest(id, $2) WHERE log = $1")
            .bind(log.table())
            .bind(w)
            .execute(db)
            .await?;
    }
    Ok(w)
}

/// Whether this process should write the persisted watermark now. The
/// stored value is only a lower bound (scan start, compaction limit), so
/// writing it less often is always safe.
fn persist_due(db: &PgPool, log: Log) -> bool {
    type Last = Mutex<HashMap<(String, Log), Instant>>;
    static LAST: OnceLock<Last> = OnceLock::new();
    let key = (
        db.connect_options()
            .get_database()
            .unwrap_or("")
            .to_string(),
        log,
    );
    let mut last = LAST
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let now = Instant::now();
    match last.get(&key) {
        Some(t) if now.duration_since(*t) < PERSIST_EVERY => false,
        _ => {
            last.insert(key, now);
            true
        }
    }
}

/// Give every id in `lo..=hi` a committed row; `false` when an in-flight
/// writer still holds one of them.
async fn fill(db: &PgPool, log: Log, lo: i64, hi: i64) -> Result<bool, sqlx::Error> {
    let mut tx = db.begin().await?;
    sqlx::query(&format!("SET LOCAL lock_timeout = '{FILL_LOCK_TIMEOUT}'"))
        .execute(&mut *tx)
        .await?;
    match sqlx::query(log.fill_sql())
        .bind(lo)
        .bind(hi)
        .execute(&mut *tx)
        .await
    {
        Ok(_) => {}
        Err(sqlx::Error::Database(e)) if e.code().as_deref() == Some("55P03") => {
            return Ok(false);
        }
        Err(e) => return Err(e),
    }
    tx.commit().await?;
    Ok(true)
}
