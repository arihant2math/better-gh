//! Commit-order watermarks (#241): writers take no lock, so ids commit out
//! of order; readers (the hub, catch-up, outbox consumers) must still see a
//! gap-free, strictly increasing sequence.

use crate::common;
use crate::ws::{connect, next, next_deltas, subscribe};

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use bgh_core::events::Event;
use bgh_core::seqlog::{self, GAP, Log};
use bgh_core::sync::{PendingSync, SyncAction, SyncRecord};
use bgh_core::testing::TestApp;
use bgh_sync::delta;
use common::*;
use serde_json::json;
use sqlx::PgPool;

fn pending(scope: &str, n: i64) -> PendingSync {
    PendingSync {
        scope: scope.to_string(),
        model: "label".into(),
        model_id: n,
        action: SyncAction::Update,
        data: json!({"id": n}),
        tx: None,
    }
}

async fn record(
    db: &PgPool,
    scope: &str,
    n: i64,
) -> (sqlx::Transaction<'static, sqlx::Postgres>, i64) {
    let mut tx = db.begin().await.unwrap();
    let recs = bgh_core::sync::record_all(&mut tx, vec![pending(scope, n)])
        .await
        .unwrap();
    (tx, recs[0].id)
}

/// Fail instead of hanging when a writer blocks on another one (the old
/// global lock).
async fn within<T>(f: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(5), f)
        .await
        .expect("writer blocked by a concurrent transaction")
}

#[tokio::test]
async fn out_of_order_commit_holds_the_watermark() {
    let app = bgh_server::test_app().await;
    let db = &app.state.db;
    let (a, id_a) = record(db, "repo:1", 1).await;
    let (b, id_b) = within(record(db, "repo:1", 2)).await;
    assert!(id_b > id_a);
    within(b.commit()).await.unwrap();
    // `b` is visible, but `a` is still in flight below it.
    let w = delta::head(db).await.unwrap();
    assert!(w < id_a, "watermark {w} passed in-flight {id_a}");
    a.commit().await.unwrap();
    assert!(delta::head(db).await.unwrap() >= id_b);
    // Watermarks are persisted (throttled to every 100 ms) and monotonic.
    tokio::time::sleep(Duration::from_millis(150)).await;
    delta::head(db).await.unwrap();
    assert!(seqlog::stored(db, Log::Sync).await.unwrap() >= id_b);
}

#[tokio::test]
async fn rolled_back_ids_are_filled_after_grace() {
    let app = bgh_server::test_app().await;
    let db = &app.state.db;
    let (a, id_a) = record(db, "repo:1", 1).await;
    let (b, id_b) = within(record(db, "repo:1", 2)).await;
    b.commit().await.unwrap();
    a.rollback().await.unwrap();
    // Too young to tell a burned id from a slow writer.
    assert!(delta::head(db).await.unwrap() < id_a);
    tokio::time::sleep(seqlog::GRACE + Duration::from_millis(100)).await;
    assert!(delta::head(db).await.unwrap() >= id_b);
    let scope: String = sqlx::query_scalar("SELECT scope FROM sync_actions WHERE id = $1")
        .bind(id_a)
        .fetch_one(db)
        .await
        .unwrap();
    assert_eq!(scope, GAP);
    // Fillers are never delivered.
    let rows = delta::fetch_range(db, id_a - 1, id_b, 100).await.unwrap();
    assert_eq!(rows.iter().map(|r| r.id).collect::<Vec<_>>(), vec![id_b]);
}

#[tokio::test]
async fn writer_retries_when_a_filler_took_its_id() {
    let app = bgh_server::test_app().await;
    let db = &app.state.db;
    // Fill the next two ids the sequences will hand out, as a reader would
    // for a gap it judged burned.
    for (table, fill) in [
        (
            "sync_actions",
            "INSERT INTO sync_actions (id, scope, model, model_id, action, created_at)
             OVERRIDING SYSTEM VALUE
             SELECT g, '!gap', '', 0, 'D', now() FROM generate_series($1, $1 + 1) g",
        ),
        (
            "event_outbox",
            "INSERT INTO event_outbox (id, kind, payload)
             SELECT g, '!gap', '{}' FROM generate_series($1, $1 + 1) g",
        ),
    ] {
        let next: i64 = sqlx::query_scalar(&format!(
            "SELECT CASE WHEN is_called THEN last_value + 1 ELSE last_value END
               FROM {}",
            sqlx::query_scalar::<_, String>("SELECT pg_get_serial_sequence($1, 'id')")
                .bind(table)
                .fetch_one(db)
                .await
                .unwrap()
        ))
        .fetch_one(db)
        .await
        .unwrap();
        sqlx::query(fill).bind(next).execute(db).await.unwrap();
    }
    let mut tx = db.begin().await.unwrap();
    let recs =
        bgh_core::sync::record_all(&mut tx, vec![pending("repo:1", 1), pending("repo:1", 2)])
            .await
            .unwrap();
    bgh_core::outbox::append(
        &mut tx,
        &[Event::AccessChanged {
            repo_id: Some(1),
            org_id: None,
            user_id: None,
        }],
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    // Fresh ids, in the transaction's own order (contiguous here: no
    // concurrent writer).
    assert_eq!(recs[1].id, recs[0].id + 1);
    assert_eq!(recs[0].model_id, 1);
    let real: i64 = scalar(
        &app,
        "SELECT count(*) FROM sync_actions WHERE scope = 'repo:1'",
    )
    .await;
    assert_eq!(real, 2);
    let events: i64 = scalar(
        &app,
        "SELECT count(*) FROM event_outbox WHERE kind = 'access_changed'",
    )
    .await;
    assert_eq!(events, 1);
    // The burned (deleted) first attempt is filled later.
    tokio::time::sleep(seqlog::GRACE + Duration::from_millis(100)).await;
    assert!(delta::head(db).await.unwrap() >= recs[1].id);
}

/// Tiny deterministic PRNG (xorshift).
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

const WRITERS: u64 = 24;
const TXS: u64 = 40;

/// Many tasks commit synced rows (1–3 per transaction, held open for a
/// random few ms so commits land out of id order, ~10% rolled back) and
/// publish them like `Tx::commit`. A log reader stopping at the watermark
/// and a live WebSocket client must each see every committed row exactly
/// once, in strictly increasing id order.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn hammered_commits_reach_clients_gap_free_and_in_order() {
    let app = TestApp::spawn_with_config(bgh_server::factory(), |c| {
        c.db_max_connections = 40;
    })
    .await;
    let ada = app.create_user("ada").await;
    let repo = repo_id(&app, &ada, "r", true).await;
    let scope = format!("repo:{repo}");
    let mut ws = connect(&app, Some(&ada), "").await;
    let _ = next(&mut ws).await;
    let start = delta::head(&app.state.db).await.unwrap();
    subscribe(&mut ws, std::slice::from_ref(&scope), start).await;

    let done = Arc::new(AtomicBool::new(false));
    let reader = {
        let (db, done, scope) = (app.state.db.clone(), done.clone(), scope.clone());
        tokio::spawn(async move {
            let (mut cursor, mut seen) = (start, Vec::new());
            loop {
                let finished = done.load(Ordering::SeqCst);
                let w = delta::head(&db).await.unwrap();
                assert!(w >= cursor, "watermark went backwards");
                let mut after = cursor;
                loop {
                    let page = delta::fetch_range(&db, after, w, 500).await.unwrap();
                    let Some(last) = page.last() else { break };
                    after = last.id;
                    seen.extend(page.iter().filter(|r| r.scope == scope).map(|r| r.id));
                }
                cursor = w;
                if finished {
                    // Let burned ids age past the grace period, then drain.
                    tokio::time::sleep(seqlog::GRACE + Duration::from_millis(100)).await;
                    let w = delta::head(&db).await.unwrap();
                    let mut after = cursor;
                    loop {
                        let page = delta::fetch_range(&db, after, w, 500).await.unwrap();
                        let Some(last) = page.last() else { break };
                        after = last.id;
                        seen.extend(page.iter().filter(|r| r.scope == scope).map(|r| r.id));
                    }
                    return seen;
                }
                tokio::time::sleep(Duration::from_millis(3)).await;
            }
        })
    };

    let mut writers = Vec::new();
    for w in 0..WRITERS {
        let (state, scope) = (app.state.clone(), scope.clone());
        writers.push(tokio::spawn(async move {
            let mut rng = Rng(0x9e37_79b9_7f4a_7c15 ^ (w + 1));
            for t in 0..TXS {
                let mut tx = state.db.begin().await.unwrap();
                let n = 1 + rng.next() % 3;
                let rows = (0..n).map(|i| pending(&scope, (w * TXS + t) as i64 * 4 + i as i64));
                let recs: Vec<SyncRecord> = bgh_core::sync::record_all(&mut tx, rows.collect())
                    .await
                    .unwrap();
                tokio::time::sleep(Duration::from_micros(rng.next() % 4000)).await;
                if rng.next().is_multiple_of(10) {
                    tx.rollback().await.unwrap();
                } else {
                    tx.commit().await.unwrap();
                    bgh_core::sync::notify(&state, &recs).await;
                }
            }
        }));
    }
    for w in writers {
        w.await.unwrap();
    }
    done.store(true, Ordering::SeqCst);
    let read = reader.await.unwrap();

    let committed: Vec<i64> =
        sqlx::query_scalar("SELECT id FROM sync_actions WHERE scope = $1 AND id > $2 ORDER BY id")
            .bind(&scope)
            .bind(start)
            .fetch_all(&app.state.db)
            .await
            .unwrap();
    assert!(committed.len() as u64 > WRITERS * TXS, "too few commits");

    assert!(
        read.windows(2).all(|p| p[0] < p[1]),
        "log reader saw ids out of order"
    );
    assert_eq!(read, committed, "log reader missed or duplicated rows");

    // The live client (hub → WebSocket).
    let mut live = Vec::new();
    while live.len() < committed.len() {
        for d in tokio::time::timeout(Duration::from_secs(20), next_deltas(&mut ws))
            .await
            .expect("live client stalled")
        {
            assert_eq!(d["scope"], scope);
            live.push(d["id"].as_i64().unwrap());
        }
    }
    assert!(
        live.windows(2).all(|p| p[0] < p[1]),
        "client saw ids out of order"
    );
    assert_eq!(live, committed, "client missed or duplicated deltas");
    let fillers: i64 = scalar(
        &app,
        "SELECT count(*) FROM sync_actions WHERE scope = '!gap'",
    )
    .await;
    assert!(fillers > 0, "no rolled-back ids were exercised");
}

/// The same for the event outbox: a consumer that stops at the outbox
/// watermark processes every committed event exactly once, in order.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn hammered_outbox_is_consumed_gap_free_and_in_order() {
    let app = TestApp::spawn_with_config(bgh_server::factory(), |c| {
        c.db_max_connections = 40;
    })
    .await;
    app.stop_listeners().await;
    let db = app.state.db.clone();
    let start = bgh_core::outbox::head(&db).await.unwrap();
    let done = Arc::new(AtomicBool::new(false));
    let consumer = {
        let (db, done) = (db.clone(), done.clone());
        tokio::spawn(async move {
            let (mut cursor, mut seen) = (start, Vec::new());
            let mut drained = false;
            loop {
                let finished = done.load(Ordering::SeqCst);
                if finished {
                    tokio::time::sleep(seqlog::GRACE + Duration::from_millis(100)).await;
                }
                let w = bgh_core::outbox::head(&db).await.unwrap();
                let rows: Vec<(i64, String)> = sqlx::query_as(
                    "SELECT id, kind FROM event_outbox WHERE id > $1 AND id <= $2 ORDER BY id",
                )
                .bind(cursor)
                .bind(w)
                .fetch_all(&db)
                .await
                .unwrap();
                for (id, kind) in rows {
                    assert!(id > cursor);
                    cursor = id;
                    if kind != GAP {
                        seen.push(id);
                    }
                }
                cursor = cursor.max(w);
                if drained {
                    return seen;
                }
                drained = finished;
                tokio::time::sleep(Duration::from_millis(3)).await;
            }
        })
    };
    let mut writers = Vec::new();
    for w in 0..WRITERS {
        let db = db.clone();
        writers.push(tokio::spawn(async move {
            let mut rng = Rng(0x2545_f491_4f6c_dd1d ^ (w + 1));
            for _ in 0..TXS {
                let mut tx = db.begin().await.unwrap();
                let n = 1 + rng.next() % 3;
                let events: Vec<Event> = (0..n)
                    .map(|i| Event::AccessChanged {
                        repo_id: Some(i as i64 + 1),
                        org_id: None,
                        user_id: None,
                    })
                    .collect();
                bgh_core::outbox::append(&mut tx, &events).await.unwrap();
                tokio::time::sleep(Duration::from_micros(rng.next() % 4000)).await;
                if rng.next().is_multiple_of(10) {
                    tx.rollback().await.unwrap();
                } else {
                    tx.commit().await.unwrap();
                }
            }
        }));
    }
    for w in writers {
        w.await.unwrap();
    }
    done.store(true, Ordering::SeqCst);
    let seen = consumer.await.unwrap();
    let committed: Vec<i64> = sqlx::query_scalar(
        "SELECT id FROM event_outbox WHERE id > $1 AND kind <> '!gap' ORDER BY id",
    )
    .bind(start)
    .fetch_all(&db)
    .await
    .unwrap();
    assert!(committed.len() as u64 > WRITERS * TXS);
    let unique: BTreeSet<i64> = seen.iter().copied().collect();
    assert_eq!(unique.len(), seen.len(), "consumer saw duplicates");
    assert_eq!(seen, committed, "consumer missed or reordered events");
}
