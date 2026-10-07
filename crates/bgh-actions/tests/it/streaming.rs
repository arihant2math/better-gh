//! Large-payload paths stream instead of buffering: artifact uploads,
//! ranged log downloads, chunked live-log replay and the shared live-log
//! Redis connection (#218, #223).

use crate::common;

use common::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const WF: &str = "on: push\njobs:\n  a:\n    runs-on: x\n    steps: [{run: a}, {run: b}]\n";

/// A repo with one acquired job; returns (app, alice, runner, job id).
async fn job() -> (
    bgh_core::testing::TestApp,
    bgh_core::testing::TestUser,
    FakeRunner,
    i64,
) {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    app.create_repo(&alice, "demo").await;
    let wc = WorkingCopy::new(&app, &alice, "alice", "demo").await;
    wc.commit(&[(".github/workflows/w.yml", WF)], "w").await;
    wc.push("main").await;
    settle(&app).await;
    let runner = FakeRunner::register(&app, &alice, "alice/demo", &["x"]).await;
    let spec = runner.acquire(&app).await.unwrap();
    let job = spec["job_id"].as_i64().unwrap();
    (app, alice, runner, job)
}

#[tokio::test]
async fn runner_artifact_upload_is_hashed_and_moved() {
    let (app, _alice, runner, job) = job().await;
    let body: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
    let info: Value = runner.upload(&app, job, "big", body.clone()).await;
    let id = info["id"].as_i64().unwrap();
    assert_eq!(info["size_in_bytes"], body.len() as i64);
    let digest: Option<String> =
        sqlx::query_scalar("SELECT digest FROM actions_artifacts WHERE id = $1")
            .bind(id)
            .fetch_one(&app.state.db)
            .await
            .unwrap();
    assert_eq!(
        digest.unwrap(),
        format!("sha256:{}", hex::encode(Sha256::digest(&body)))
    );
    let stored = std::fs::read(bgh_actions::server::artifact_path(&app.state, id)).unwrap();
    assert!(stored == body);
    // The upload temp file was moved into place: nothing is left behind.
    let tmp = app.state.config.data_dir.join("actions").join("tmp");
    let left: Vec<_> = std::fs::read_dir(&tmp)
        .map(|d| d.flatten().map(|e| e.file_name()).collect())
        .unwrap_or_default();
    assert!(left.is_empty(), "{left:?}");
    let parts = std::fs::read_dir(
        bgh_actions::server::artifact_path(&app.state, id)
            .parent()
            .unwrap(),
    )
    .unwrap()
    .flatten()
    .filter(|e| e.file_name().to_string_lossy().ends_with(".part"))
    .count();
    assert_eq!(parts, 0);
}

#[tokio::test]
async fn runner_artifact_upload_over_the_cap_is_413() {
    let (app, _alice, runner, job) = job().await;
    bgh_actions::server::set_max_artifact_size_for_tests(&app.state, 1000);
    let res = app
        .put(&format!("/_bgh/actions/runner/jobs/{job}/artifacts/big"))
        .header("authorization", &format!("RunnerToken {}", runner.token))
        .body(vec![0u8; 1001])
        .send()
        .await;
    res.assert_status(413);
    // Nothing stored, nothing left in the upload temp dir.
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM actions_artifacts")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(n, 0);
    let tmp = app.state.config.data_dir.join("actions").join("tmp");
    let left = std::fs::read_dir(&tmp).map(|d| d.count()).unwrap_or(0);
    assert_eq!(left, 0);
    // At the cap is fine.
    runner.upload(&app, job, "big", vec![0u8; 1000]).await;
}

#[tokio::test]
async fn job_log_download_supports_ranges() {
    let (app, alice, runner, job) = job().await;
    runner.log(&app, job, 1, "first step\n").await;
    runner.log(&app, job, 2, "second step\n").await;
    let full = bgh_actions::logs::read_job(&app.state, job).await;
    let res = app
        .get(&format!("/api/v3/repos/alice/demo/actions/jobs/{job}/logs"))
        .auth(&alice)
        .send()
        .await;
    res.assert_status(302);
    let loc = res.header("location").unwrap().to_string();
    let path = loc.strip_prefix(&app.base_url).unwrap().to_string();

    let whole = app.get(&path).send().await;
    whole.assert_status(200);
    assert_eq!(whole.text(), full);
    assert_eq!(whole.header("accept-ranges"), Some("bytes"));
    assert_eq!(
        whole.header("content-length"),
        Some(full.len().to_string().as_str())
    );

    // A range spanning both step files.
    let step1 = bgh_actions::logs::read_step(&app.state, job, 1).await.len();
    let (a, b) = (step1 - 5, step1 + 7);
    let part = app
        .get(&path)
        .header("range", &format!("bytes={a}-{b}"))
        .send()
        .await;
    part.assert_status(206);
    assert_eq!(part.text(), &full[a..=b]);
    assert_eq!(
        part.header("content-range"),
        Some(format!("bytes {a}-{b}/{}", full.len()).as_str())
    );
    // Suffix range.
    let tail = app.get(&path).header("range", "bytes=-6").send().await;
    tail.assert_status(206);
    assert_eq!(tail.text(), &full[full.len() - 6..]);
    // Past the end.
    let bad = app
        .get(&path)
        .header("range", &format!("bytes={}-", full.len()))
        .send()
        .await;
    bad.assert_status(416);
    assert_eq!(
        bad.header("content-range"),
        Some(format!("bytes */{}", full.len()).as_str())
    );
}

/// `(event, data)` pairs of an SSE body.
fn sse_events(body: &str) -> Vec<(String, String)> {
    body.split("\n\n")
        .filter_map(|block| {
            let mut event = None;
            let mut data = String::new();
            for line in block.lines() {
                if let Some(e) = line.strip_prefix("event: ") {
                    event = Some(e.to_string());
                } else if let Some(d) = line.strip_prefix("data: ") {
                    data.push_str(d);
                }
            }
            Some((event?, data))
        })
        .collect()
}

#[tokio::test]
async fn live_log_replay_is_chunked() {
    let (app, alice, runner, job) = job().await;
    // ~210 KiB in one step, with a multi-byte character in every line.
    let text: String = (0..3000)
        .map(|i| format!("line {i:05} é {}\n", "x".repeat(50)))
        .collect();
    runner.log(&app, job, 1, &text).await;
    runner.complete(&app, job, "success", json!({})).await;
    let file = bgh_actions::logs::read_step(&app.state, job, 1).await;
    assert!(file.len() > 3 * 64 * 1024);

    let url = app.url(&format!("/_bgh/actions/jobs/{job}/logs/stream"));
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let body = client
        .get(&url)
        .header("authorization", format!("token {}", alice.token))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let events = sse_events(&body);
    assert_eq!(events.last().map(|e| e.0.as_str()), Some("done"));
    let mut replay = String::new();
    let mut chunks = 0;
    for (event, data) in &events {
        if event != "log" {
            continue;
        }
        let v: Value = serde_json::from_str(data).unwrap();
        assert_eq!(v["step"], 1);
        assert_eq!(v["offset"], replay.len());
        let t = v["text"].as_str().unwrap();
        assert!(t.len() <= 64 * 1024, "chunk of {} bytes", t.len());
        assert!(t.ends_with('\n'));
        replay.push_str(t);
        chunks += 1;
    }
    assert!(chunks >= 4, "{chunks} chunks");
    assert!(replay == file, "replay differs from the step log");
}

#[tokio::test]
async fn live_log_viewers_share_one_redis_subscription() {
    let (app, alice, runner, job) = job().await;
    let url = app.url(&format!("/_bgh/actions/jobs/{job}/logs/stream"));
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let open = || {
        client
            .get(&url)
            .header("authorization", format!("token {}", alice.token))
            .send()
    };
    let mut a = open().await.unwrap();
    let mut b = open().await.unwrap();
    assert_eq!((a.status().as_u16(), b.status().as_u16()), (200, 200));

    let channel = bgh_actions::logs::channel(&app.state, job);
    let numsub = || async {
        let mut redis = app.state.redis.clone();
        let v: (String, i64) = redis::cmd("PUBSUB")
            .arg("NUMSUB")
            .arg(&channel)
            .query_async(&mut redis)
            .await
            .unwrap();
        v.1
    };
    assert_eq!(numsub().await, 1, "two viewers, one Redis subscription");

    // Both viewers get the live chunk.
    runner.log(&app, job, 2, "live line\n").await;
    for resp in [&mut a, &mut b] {
        let mut body = String::new();
        while !body.contains("live line") {
            let chunk = tokio::time::timeout(std::time::Duration::from_secs(10), resp.chunk())
                .await
                .expect("live chunk")
                .unwrap()
                .unwrap();
            body.push_str(&String::from_utf8_lossy(&chunk));
        }
    }

    // The last viewer leaving unsubscribes the channel.
    drop(a);
    drop(b);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while numsub().await != 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "channel still subscribed"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}
