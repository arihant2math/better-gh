//! Permission-recheck benchmark (ignored by default, issue #245):
//!
//! ```text
//! cargo test --release -p bgh-sync --test it recheck_bench -- --ignored --nocapture
//! ```
//!
//! `SOCKETS` users each hold one WebSocket subscribed to their user scope
//! and `REPOS` private repositories they collaborate on. A burst of
//! `BURST` `repo` counter deltas (separate commits on different repos, like
//! stars or issue closes) is committed twice: once with a cold hub cache,
//! once warm. While the hub works, the pool (20 connections, the default)
//! is sampled every millisecond and a probe task times `SELECT 1` through
//! it, as a request handler would.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use bgh_core::db::Tx;
use bgh_core::sync::SyncAction;
use bgh_core::sync::shapes::Model;
use bgh_core::testing::TestApp;
use futures::{SinkExt, StreamExt};
use serde_json::json;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

const SOCKETS: usize = 100;
const REPOS: i64 = 200;
const BURST: i64 = 50;

struct Report {
    peak_in_use: u32,
    conn_ms: f64,
    quiet_after: Duration,
    probe: Vec<Duration>,
}

fn pct(v: &[Duration], p: f64) -> Duration {
    v[((v.len() as f64 - 1.0) * p).round() as usize]
}

async fn burst(app: &TestApp, repos: &[i64]) -> Report {
    let pool = app.state.db.clone();
    let stop = Arc::new(AtomicBool::new(false));
    let probe = {
        let (pool, stop) = (pool.clone(), stop.clone());
        tokio::spawn(async move {
            let mut lat = Vec::new();
            while !stop.load(Ordering::Relaxed) {
                let t = Instant::now();
                let _: i32 = sqlx::query_scalar("SELECT 1")
                    .fetch_one(&pool)
                    .await
                    .unwrap();
                lat.push(t.elapsed());
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            lat
        })
    };
    let sampler = {
        let (pool, stop) = (pool.clone(), stop.clone());
        tokio::spawn(async move {
            let start = Instant::now();
            let (mut peak, mut conn_ms, mut last_busy) = (0u32, 0f64, Duration::ZERO);
            while !stop.load(Ordering::Relaxed) {
                // The probe holds at most one connection; don't count it.
                let in_use = (pool.size() - pool.num_idle() as u32).saturating_sub(1);
                peak = peak.max(in_use);
                conn_ms += in_use as f64;
                if in_use > 0 {
                    last_busy = start.elapsed();
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            (peak, conn_ms, last_busy)
        })
    };
    let start = Instant::now();
    for &repo in repos {
        let mut tx = Tx::begin(&app.state).await.unwrap();
        tx.sync_model(Model::Repo, repo, SyncAction::Update)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }
    // Run until the pool has been idle (besides the probe) for 500 ms.
    loop {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let idle_for = pool.num_idle() as u32 + 1 >= pool.size();
        if idle_for && start.elapsed() > Duration::from_millis(500) {
            let t = Instant::now();
            let mut quiet = true;
            while t.elapsed() < Duration::from_millis(500) {
                if (pool.size() - pool.num_idle() as u32) > 1 {
                    quiet = false;
                    break;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            if quiet {
                break;
            }
        }
        assert!(start.elapsed() < Duration::from_secs(300), "never settled");
    }
    stop.store(true, Ordering::Relaxed);
    let (peak_in_use, conn_ms, quiet_after) = sampler.await.unwrap();
    let mut probe = probe.await.unwrap();
    probe.sort();
    Report {
        peak_in_use,
        conn_ms,
        quiet_after,
        probe,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "benchmark; run with --ignored --nocapture (ideally --release)"]
async fn recheck_burst_pool_usage() {
    let app = TestApp::spawn_with_config(bgh_server::factory(), |c| {
        c.db_max_connections = 20;
    })
    .await;
    let owner = app.create_user("owner").await;
    let mut users = Vec::new();
    for i in 0..SOCKETS {
        users.push(app.create_user(&format!("u{i}")).await);
    }
    let repos: Vec<i64> = sqlx::query_scalar(&format!(
        "INSERT INTO repositories (owner_id, name, visibility)
              SELECT {}, 'r' || g, 'private' FROM generate_series(1, {REPOS}) g RETURNING id",
        owner.id
    ))
    .fetch_all(&app.state.db)
    .await
    .unwrap();
    sqlx::raw_sql(&format!(
        "INSERT INTO collaborators (repo_id, user_id, permission)
              SELECT r.id, u.id, 'write' FROM repositories r, users u
               WHERE r.owner_id = {} AND u.login LIKE 'u%'",
        owner.id
    ))
    .execute(&app.state.db)
    .await
    .unwrap();

    let mut readers = Vec::new();
    for u in &users {
        let mut req = format!("ws://{}/_bgh/sync/ws", app.addr)
            .into_client_request()
            .unwrap();
        req.headers_mut().insert(
            "authorization",
            format!("token {}", u.token).parse().unwrap(),
        );
        let mut ws = tokio_tungstenite::connect_async(req).await.unwrap().0;
        let mut scopes = vec![format!("user:{}", u.id)];
        scopes.extend(repos.iter().map(|r| format!("repo:{r}")));
        ws.send(Message::Text(
            json!({"t": "sub", "scopes": scopes, "since": 0})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
        loop {
            let Some(Ok(Message::Text(t))) = ws.next().await else {
                panic!("socket closed");
            };
            if t.contains("\"ready\"") {
                break;
            }
        }
        readers.push(tokio::spawn(
            async move { while ws.next().await.is_some() {} },
        ));
    }

    let targets: Vec<i64> = repos.iter().take(BURST as usize).copied().collect();
    for (label, r) in [
        ("cold", burst(&app, &targets).await),
        ("warm", burst(&app, &targets).await),
    ] {
        println!(
            "{label}: {SOCKETS} sockets x {} scopes, {BURST} repo deltas: \
             peak pool in use {}/20, {:.0} connection-ms, busy for {:?}, \
             probe SELECT 1 p50 {:?} p99 {:?} max {:?} (n={})",
            REPOS + 1,
            r.peak_in_use,
            r.conn_ms,
            r.quiet_after,
            pct(&r.probe, 0.5),
            pct(&r.probe, 0.99),
            r.probe.last().unwrap(),
            r.probe.len(),
        );
    }
    for r in readers {
        r.abort();
    }
}
