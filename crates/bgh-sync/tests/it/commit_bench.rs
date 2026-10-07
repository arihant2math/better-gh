//! Synced-commit throughput benchmark (ignored by default, issue #241):
//!
//! ```text
//! cargo test --release -p bgh-sync --test it commit_bench -- --ignored --nocapture
//! ```
//!
//! `C` tasks commit as fast as they can for `SECS` seconds. Each
//! transaction does what `Tx::commit` does for a typical write: one domain
//! row, one `sync_actions` row (`sync::record_all`) and one outbox event
//! (`outbox::append`), then `COMMIT`. Reports commits/s and mean latency.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use bgh_core::events::Event;
use bgh_core::sync::{PendingSync, SyncAction};
use bgh_core::testing::TestApp;
use serde_json::json;

const SECS: u64 = 5;

async fn run(app: &TestApp, clients: usize) -> (f64, Duration) {
    let done = Arc::new(AtomicU64::new(0));
    let deadline = Instant::now() + Duration::from_secs(SECS);
    let mut tasks = Vec::new();
    for c in 0..clients {
        let (pool, done) = (app.state.db.clone(), done.clone());
        tasks.push(tokio::spawn(async move {
            let mut n = 0u64;
            while Instant::now() < deadline {
                let mut tx = pool.begin().await.unwrap();
                sqlx::query("INSERT INTO commit_bench (client) VALUES ($1)")
                    .bind(c as i64)
                    .execute(&mut *tx)
                    .await
                    .unwrap();
                bgh_core::sync::record_all(
                    &mut tx,
                    vec![PendingSync {
                        scope: format!("repo:{}", c + 1),
                        model: "label".into(),
                        model_id: n as i64,
                        action: SyncAction::Update,
                        data: json!({"id": n}),
                        tx: None,
                    }],
                )
                .await
                .unwrap();
                bgh_core::outbox::append(
                    &mut tx,
                    &[Event::AccessChanged {
                        repo_id: Some(c as i64 + 1),
                        org_id: None,
                        user_id: None,
                    }],
                )
                .await
                .unwrap();
                tx.commit().await.unwrap();
                n += 1;
            }
            done.fetch_add(n, Ordering::Relaxed);
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
    let n = done.load(Ordering::Relaxed) as f64;
    let tps = n / SECS as f64;
    let latency = Duration::from_secs_f64(clients as f64 / tps);
    (tps, latency)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "benchmark; run with --ignored --nocapture (ideally --release)"]
async fn synced_commit_throughput() {
    let app = TestApp::spawn_with_config(bgh_server::factory(), |c| {
        c.db_max_connections = 70;
    })
    .await;
    app.stop_listeners().await;
    sqlx::raw_sql("CREATE TABLE commit_bench (id BIGSERIAL PRIMARY KEY, client BIGINT NOT NULL)")
        .execute(&app.state.db)
        .await
        .unwrap();
    for clients in [1, 8, 32, 64] {
        let (tps, latency) = run(&app, clients).await;
        println!("clients {clients:>2}: {tps:>7.0} tps, mean latency {latency:.2?}");
    }
}
