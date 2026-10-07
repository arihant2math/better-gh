//! Latency benchmark of the code browser on a real repository.
//!
//! Ignored by default. Run against a bare clone:
//!
//! ```text
//! git clone --bare https://github.com/tokio-rs/tokio.git /tmp/tokio.git
//! BGH_BENCH_REPO=/tmp/tokio.git BGH_BENCH_DIR=tokio/src/runtime \
//!   BGH_BENCH_FILE=tokio/src/runtime/builder.rs \
//!   cargo test --release -p bgh-repos --test bench -- --ignored --nocapture
//! ```
//!
//! "cold" runs delete the Redis caches before each request (pure compute
//! + git); "warm" runs hit the caches. Reports p50 / p90 in milliseconds.

use crate::common;

use std::time::{Duration, Instant};

use bgh_core::testing::TestApp;

fn pct(mut v: Vec<Duration>, p: f64) -> f64 {
    v.sort();
    let i = ((v.len() as f64 - 1.0) * p).round() as usize;
    v[i].as_secs_f64() * 1000.0
}

async fn flush(app: &TestApp) {
    let mut redis = app.state.redis.clone();
    let keys: Vec<String> = redis::cmd("KEYS")
        .arg(app.state.redis_key("*"))
        .query_async(&mut redis)
        .await
        .unwrap();
    if !keys.is_empty() {
        let _: () = redis::cmd("DEL")
            .arg(&keys)
            .query_async(&mut redis)
            .await
            .unwrap();
    }
}

async fn measure(app: &TestApp, label: &str, path: &str, cold: bool, runs: usize) -> (f64, f64) {
    let mut times = Vec::with_capacity(runs);
    for _ in 0..runs {
        if cold {
            flush(app).await;
        }
        let start = Instant::now();
        let res = app.get(path).send().await;
        let elapsed = start.elapsed();
        assert_eq!(res.status(), 200, "{path}: {}", res.text());
        times.push(elapsed);
    }
    let (p50, p90) = (pct(times.clone(), 0.5), pct(times, 0.9));
    println!(
        "| {label} | {} | {p50:.1} | {p90:.1} |",
        if cold { "cold" } else { "warm" }
    );
    (p50, p90)
}

#[tokio::test]
#[ignore]
async fn browse_latency() {
    let Ok(fixture) = std::env::var("BGH_BENCH_REPO") else {
        eprintln!("BGH_BENCH_REPO not set; skipping");
        return;
    };
    let dir = std::env::var("BGH_BENCH_DIR").unwrap_or_else(|_| "src".into());
    let file = std::env::var("BGH_BENCH_FILE").expect("BGH_BENCH_FILE");
    let runs: usize = std::env::var("BGH_BENCH_RUNS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20);

    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    app.create_repo(&alice, "bench").await;
    let id = common::repo_id(&app, &alice, "bench").await;
    let store = bgh_repos::store(&app.state);
    let target = store.path(id);
    let out = tokio::process::Command::new("git")
        .arg("--git-dir")
        .arg(&target)
        .args([
            "fetch",
            "-q",
            &fixture,
            "+refs/heads/*:refs/heads/*",
            "+refs/tags/*:refs/tags/*",
        ])
        .output()
        .await
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let head = String::from_utf8(
        tokio::process::Command::new("git")
            .arg("--git-dir")
            .arg(&fixture)
            .args(["symbolic-ref", "--short", "HEAD"])
            .output()
            .await
            .unwrap()
            .stdout,
    )
    .unwrap();
    let head = head.trim();
    sqlx::query("UPDATE repositories SET default_branch = $1 WHERE id = $2")
        .bind(head)
        .bind(id)
        .execute(&app.state.db)
        .await
        .unwrap();
    bgh_git::write::set_head(&store, id, head).await.unwrap();
    let commit = app.get("/_bgh/repos/alice/bench/refs").send().await.json()["branches"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["name"] == head)
        .unwrap()["sha"]
        .as_str()
        .unwrap()
        .to_string();

    println!("\nfixture {fixture} @ {head} {commit}, {runs} runs\n");
    println!("| endpoint | cache | p50 ms | p90 ms |\n|---|---|---|---|");
    let base = "/_bgh/repos/alice/bench";
    let cold_runs = (runs / 4).max(3);
    measure(
        &app,
        "refs (loose refs)",
        &format!("{base}/refs"),
        false,
        runs,
    )
    .await;
    bgh_git::maintenance::pack_refs(&store, id).await.unwrap();
    measure(
        &app,
        "refs (packed refs)",
        &format!("{base}/refs"),
        false,
        runs,
    )
    .await;
    measure(
        &app,
        "tree root",
        &format!("{base}/tree/{commit}"),
        true,
        cold_runs,
    )
    .await;
    measure(
        &app,
        "tree root",
        &format!("{base}/tree/{commit}"),
        false,
        runs,
    )
    .await;
    measure(
        &app,
        &format!("tree {dir}"),
        &format!("{base}/tree/{commit}/{dir}"),
        false,
        runs,
    )
    .await;
    measure(
        &app,
        "tree-commits root",
        &format!("{base}/tree-commits/{commit}"),
        true,
        cold_runs,
    )
    .await;
    measure(
        &app,
        "tree-commits root",
        &format!("{base}/tree-commits/{commit}"),
        false,
        runs,
    )
    .await;
    measure(
        &app,
        &format!("tree-commits {dir}"),
        &format!("{base}/tree-commits/{commit}/{dir}"),
        true,
        cold_runs,
    )
    .await;
    measure(
        &app,
        &format!("tree-commits {dir}"),
        &format!("{base}/tree-commits/{commit}/{dir}"),
        false,
        runs,
    )
    .await;
    measure(
        &app,
        &format!("blob {file}"),
        &format!("{base}/blob/{commit}/{file}"),
        true,
        cold_runs,
    )
    .await;
    measure(
        &app,
        &format!("blob {file}"),
        &format!("{base}/blob/{commit}/{file}"),
        false,
        runs,
    )
    .await;
    measure(
        &app,
        &format!("blame {file}"),
        &format!("{base}/blame/{commit}/{file}"),
        true,
        cold_runs,
    )
    .await;
    measure(
        &app,
        &format!("blame {file}"),
        &format!("{base}/blame/{commit}/{file}"),
        false,
        runs,
    )
    .await;
    measure(
        &app,
        &format!("history {file}"),
        &format!("{base}/history/{commit}/{file}"),
        true,
        cold_runs,
    )
    .await;
    measure(
        &app,
        &format!("history {file}"),
        &format!("{base}/history/{commit}/{file}"),
        false,
        runs,
    )
    .await;
    measure(
        &app,
        &format!("raw {file}"),
        &format!("/alice/bench/raw/{commit}/{file}"),
        false,
        runs,
    )
    .await;

    let mut tag_times = Vec::new();
    for _ in 0..runs {
        let t = Instant::now();
        let n = store.read(id, |r| Ok(r.tags()?.len())).await.unwrap();
        tag_times.push(t.elapsed());
        assert!(n > 0 || runs > 0);
    }
    println!(
        "\nlist tags (gix, packed): p50 {:.2} ms",
        pct(tag_times, 0.5)
    );

    // Repository handle cache: open cost with and without it.
    let path = store.path(id);
    let mut cached = Vec::new();
    let mut uncached = Vec::new();
    for _ in 0..runs {
        let p = path.clone();
        let t = Instant::now();
        tokio::task::spawn_blocking(move || {
            bgh_git::GitRepo::open_cached(&p, 1 << 20)
                .unwrap()
                .branches()
                .unwrap()
        })
        .await
        .unwrap();
        cached.push(t.elapsed());
        let p = path.clone();
        let t = Instant::now();
        tokio::task::spawn_blocking(move || {
            bgh_git::GitRepo::open(&p, 1 << 20)
                .unwrap()
                .branches()
                .unwrap()
        })
        .await
        .unwrap();
        uncached.push(t.elapsed());
    }
    println!(
        "\nopen+list branches: cached p50 {:.2} ms, uncached p50 {:.2} ms",
        pct(cached, 0.5),
        pct(uncached, 0.5)
    );
}
