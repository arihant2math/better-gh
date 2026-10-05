//! Triggering, scheduling and the runner protocol, driven by a fake HTTP
//! runner (no job execution).

mod common;

use std::io::Write;

use common::*;
use serde_json::{Value, json};

const CI: &str = r#"
name: CI
run-name: "CI for ${{ github.ref_name }}"
on:
  push:
    branches: [main]
env:
  GLOBAL: hello
jobs:
  build:
    runs-on: ubuntu-latest
    strategy:
      matrix:
        os: [linux, mac]
    env:
      WHO: ${{ matrix.os }}-${{ env.GLOBAL }}
    outputs:
      out: ${{ steps.s.outputs.v }}
    steps:
      - id: s
        run: echo "v=1" >> $GITHUB_OUTPUT
  test:
    needs: build
    runs-on: [self-hosted, linux]
    steps:
      - run: echo ${{ needs.build.outputs.out }}
"#;

async fn setup() -> (
    bgh_core::testing::TestApp,
    bgh_core::testing::TestUser,
    WorkingCopy,
) {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    app.create_repo(&alice, "demo").await;
    let wc = WorkingCopy::new(&app, &alice, "alice", "demo").await;
    (app, alice, wc)
}

#[tokio::test]
async fn push_creates_run_with_matrix_jobs_and_checks() {
    let (app, alice, wc) = setup().await;
    let sha = wc
        .commit(&[(".github/workflows/ci.yml", CI)], "Add CI\n\nbody")
        .await;
    wc.push("main").await;
    settle(&app).await;

    let res = app
        .get("/api/v3/repos/alice/demo/actions/workflows")
        .auth(&alice)
        .send()
        .await;
    res.assert_status(200);
    let wfs = res.json();
    assert_eq!(wfs["total_count"], 1);
    let wf = &wfs["workflows"][0];
    assert_eq!(wf["name"], "CI");
    assert_eq!(wf["path"], ".github/workflows/ci.yml");
    assert_eq!(wf["state"], "active");
    assert!(wf["node_id"].is_string() && wf["badge_url"].is_string());
    let res = app
        .get("/api/v3/repos/alice/demo/actions/workflows/ci.yml")
        .auth(&alice)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["id"], wf["id"]);

    let runs = runs(&app, &alice, "alice/demo").await;
    assert_eq!(runs.len(), 1);
    let r = &runs[0];
    assert_eq!(r["event"], "push");
    assert_eq!(r["status"], "queued");
    assert_eq!(r["conclusion"], Value::Null);
    assert_eq!(r["head_sha"], sha);
    assert_eq!(r["head_branch"], "main");
    assert_eq!(r["display_title"], "CI for main");
    assert_eq!(r["run_number"], 1);
    assert_eq!(r["run_attempt"], 1);
    assert_eq!(r["path"], ".github/workflows/ci.yml");
    assert_eq!(r["actor"]["login"], "alice");
    assert_eq!(r["head_commit"]["id"], sha);
    assert_eq!(r["head_commit"]["message"], "Add CI\n\nbody");
    assert_eq!(r["repository"]["full_name"], "alice/demo");
    assert!(r["check_suite_id"].is_i64());
    let run_id = r["id"].as_i64().unwrap();
    assert_eq!(
        r["jobs_url"],
        app.url(&format!(
            "/api/v3/repos/alice/demo/actions/runs/{run_id}/jobs"
        ))
    );

    let jobs = common::jobs(&app, &alice, "alice/demo", run_id).await;
    let names: Vec<&str> = jobs.iter().map(|j| j["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["build (linux)", "build (mac)"]);
    let j = &jobs[0];
    assert_eq!(j["status"], "queued");
    assert_eq!(j["labels"], json!(["ubuntu-latest"]));
    assert_eq!(j["workflow_name"], "CI");
    assert_eq!(j["steps"][0]["name"], "Set up job");
    assert_eq!(j["steps"][1]["name"], "Run echo \"v=1\" >> $GITHUB_OUTPUT");
    assert!(j["check_run_url"].is_string());

    // Check suite + runs exist in the shared tables.
    let (status, n): (String, i64) = sqlx::query_as(
        "SELECT s.status, (SELECT count(*) FROM check_runs c WHERE c.check_suite_id = s.id)
           FROM check_suites s WHERE s.id = $1",
    )
    .bind(r["check_suite_id"].as_i64().unwrap())
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!((status.as_str(), n), ("queued", 2));

    // Pushing a non-matching branch does nothing.
    wc.checkout_new("feature").await;
    wc.commit(&[("x.txt", "x")], "feature work").await;
    wc.push("feature").await;
    settle(&app).await;
    assert_eq!(common::runs(&app, &alice, "alice/demo").await.len(), 1);
}

#[tokio::test]
async fn runner_protocol_drives_run_to_completion() {
    let (app, alice, wc) = setup().await;
    wc.commit(&[(".github/workflows/ci.yml", CI)], "ci").await;
    wc.push("main").await;
    settle(&app).await;
    let run_id = runs(&app, &alice, "alice/demo").await[0]["id"]
        .as_i64()
        .unwrap();

    let runner = FakeRunner::register(&app, &alice, "alice/demo", &["ubuntu-latest"]).await;
    let spec = runner.acquire(&app).await.expect("a job");
    let job_id = spec["job_id"].as_i64().unwrap();
    assert_eq!(spec["name"], "build (linux)");
    assert_eq!(spec["repository"], "alice/demo");
    assert_eq!(spec["env"]["GLOBAL"], "hello");
    assert_eq!(spec["env"]["WHO"], "linux-hello");
    assert_eq!(spec["matrix"]["os"], "linux");
    assert_eq!(spec["github"]["event_name"], "push");
    assert_eq!(spec["github"]["ref"], "refs/heads/main");
    assert_eq!(spec["github"]["job"], "build");
    let token = spec["token"].as_str().unwrap().to_string();
    assert!(token.starts_with("bghp_"));
    assert_eq!(spec["secrets"]["GITHUB_TOKEN"], token);

    // The job token works on this repo only.
    app.get("/api/v3/repos/alice/demo")
        .token(&token)
        .send()
        .await
        .assert_status(200);
    app.create_private_repo(&alice, "secret").await;
    app.get("/api/v3/repos/alice/secret")
        .token(&token)
        .send()
        .await
        .assert_status(404);

    let run = common::run(&app, &alice, "alice/demo", run_id).await;
    assert_eq!(run["status"], "in_progress");
    let j = app
        .get(&format!("/api/v3/repos/alice/demo/actions/jobs/{job_id}"))
        .auth(&alice)
        .send()
        .await
        .json();
    assert_eq!(j["status"], "in_progress");
    assert_eq!(j["runner_name"], "fake");
    assert_eq!(j["steps"][0]["status"], "in_progress");

    runner.log(&app, job_id, 1, "Setting up\n").await;
    runner.log(&app, job_id, 2, "v=1\nsecond line\n").await;
    let hb = runner
        .steps(
            &app,
            job_id,
            json!([
                {"number": 1, "name": "Set up job", "status": "completed", "conclusion": "success",
                 "started_at": "2024-01-01T00:00:00.5Z", "completed_at": "2024-01-01T00:00:01Z"},
                {"number": 2, "name": "Run step", "status": "in_progress", "conclusion": null,
                 "started_at": "2024-01-01T00:00:01Z", "completed_at": null},
            ]),
        )
        .await;
    assert_eq!(hb["cancel"], false);
    let j = app
        .get(&format!("/api/v3/repos/alice/demo/actions/jobs/{job_id}"))
        .auth(&alice)
        .send()
        .await
        .json();
    assert_eq!(j["steps"][0]["started_at"], "2024-01-01T00:00:00Z");
    assert_eq!(j["steps"][1]["status"], "in_progress");
    runner
        .complete(&app, job_id, "success", json!({"out": "1"}))
        .await;

    // Token revoked once the job completes.
    app.get("/api/v3/repos/alice/demo")
        .token(&token)
        .send()
        .await
        .assert_status(401);

    // Second matrix leg, then the dependent job sees needs outputs.
    let spec2 = runner.acquire(&app).await.expect("second build job");
    assert_eq!(spec2["name"], "build (mac)");
    assert!(runner.acquire(&app).await.is_none(), "test waits for build");
    runner
        .complete(
            &app,
            spec2["job_id"].as_i64().unwrap(),
            "success",
            json!({"out": "1"}),
        )
        .await;
    // `test` needs self-hosted + linux labels, which this runner has.
    let spec3 = runner.acquire(&app).await.expect("test job");
    assert_eq!(spec3["name"], "test");
    assert_eq!(spec3["needs"]["build"]["result"], "success");
    assert_eq!(spec3["needs"]["build"]["outputs"]["out"], "1");
    runner
        .complete(
            &app,
            spec3["job_id"].as_i64().unwrap(),
            "success",
            json!({}),
        )
        .await;

    let run = common::run(&app, &alice, "alice/demo", run_id).await;
    assert_eq!(run["status"], "completed");
    assert_eq!(run["conclusion"], "success");
    let (status, conclusion): (String, Option<String>) =
        sqlx::query_as("SELECT status, conclusion FROM check_suites WHERE id = $1")
            .bind(run["check_suite_id"].as_i64().unwrap())
            .fetch_one(&app.state.db)
            .await
            .unwrap();
    assert_eq!(
        (status.as_str(), conclusion.as_deref()),
        ("completed", Some("success"))
    );
    let ccount: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM check_runs WHERE check_suite_id = $1 AND conclusion = 'success'",
    )
    .bind(run["check_suite_id"].as_i64().unwrap())
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(ccount, 3);

    // Job log (302 → text) and run logs zip.
    let res = app
        .get(&format!(
            "/api/v3/repos/alice/demo/actions/jobs/{job_id}/logs"
        ))
        .auth(&alice)
        .send()
        .await;
    let log = follow(&app, &res).await;
    log.assert_status(200);
    let text = log.text();
    assert!(text.contains("Z Setting up\n"), "{text}");
    assert!(text.contains("Z second line\n"));
    let res = app
        .get(&format!(
            "/api/v3/repos/alice/demo/actions/runs/{run_id}/logs"
        ))
        .auth(&alice)
        .send()
        .await;
    let zip = follow(&app, &res).await;
    zip.assert_status(200);
    assert_eq!(zip.header("content-type"), Some("application/zip"));
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip.body.to_vec())).unwrap();
    let names: Vec<String> = archive.file_names().map(String::from).collect();
    assert!(
        names.contains(&"0_build (linux).txt".to_string()),
        "{names:?}"
    );
    assert!(
        names.contains(&"build (linux)/2_Run step.txt".to_string()),
        "{names:?}"
    );
    let mut f = archive.by_name("0_build (linux).txt").unwrap();
    let mut s = String::new();
    std::io::Read::read_to_string(&mut f, &mut s).unwrap();
    assert!(s.contains("second line"));

    // Anonymous log download needs auth.
    app.get(&format!(
        "/api/v3/repos/alice/demo/actions/jobs/{job_id}/logs"
    ))
    .send()
    .await
    .assert_status(401);
}

#[tokio::test]
async fn failure_skips_dependents_and_failure_jobs_run() {
    let (app, alice, wc) = setup().await;
    let wf = r#"
on: push
jobs:
  a:
    runs-on: ubuntu-latest
    strategy:
      matrix:
        n: [1, 2, 3]
    steps: [{run: "true"}]
  b:
    needs: a
    runs-on: ubuntu-latest
    steps: [{run: "true"}]
  report:
    needs: a
    if: failure()
    runs-on: ubuntu-latest
    steps: [{run: "true"}]
  always:
    needs: b
    if: always()
    runs-on: ubuntu-latest
    steps: [{run: "true"}]
"#;
    wc.commit(&[(".github/workflows/w.yml", wf)], "w").await;
    wc.push("main").await;
    settle(&app).await;
    let run_id = runs(&app, &alice, "alice/demo").await[0]["id"]
        .as_i64()
        .unwrap();
    let runner = FakeRunner::register(&app, &alice, "alice/demo", &["ubuntu-latest"]).await;
    let first = runner.acquire(&app).await.unwrap();
    assert_eq!(first["name"], "a (1)");
    assert_eq!(first["strategy"]["job-total"], 3);
    let second = runner.acquire(&app).await.unwrap();
    runner
        .complete(
            &app,
            first["job_id"].as_i64().unwrap(),
            "failure",
            json!({}),
        )
        .await;
    // fail-fast: the running sibling is asked to cancel, the queued one is cancelled.
    let hb = runner
        .steps(&app, second["job_id"].as_i64().unwrap(), json!([]))
        .await;
    assert_eq!(hb["cancel"], true);
    runner
        .complete(
            &app,
            second["job_id"].as_i64().unwrap(),
            "cancelled",
            json!({}),
        )
        .await;

    let jobs = common::jobs(&app, &alice, "alice/demo", run_id).await;
    let by_name = |n: &str| jobs.iter().find(|j| j["name"] == n).cloned().unwrap();
    assert_eq!(by_name("a (3)")["conclusion"], "cancelled");
    assert_eq!(by_name("b")["conclusion"], "skipped");
    assert_eq!(by_name("report")["status"], "queued");
    assert_eq!(by_name("always")["status"], "queued");
    let x = runner.acquire(&app).await.unwrap();
    let y = runner.acquire(&app).await.unwrap();
    let (report, always) = if x["name"] == "report" {
        (x, y)
    } else {
        (y, x)
    };
    assert_eq!(report["needs"]["a"]["result"], "failure");
    assert_eq!(always["needs"]["b"]["result"], "skipped");
    for j in [&report, &always] {
        runner
            .complete(&app, j["job_id"].as_i64().unwrap(), "success", json!({}))
            .await;
    }
    let run = common::run(&app, &alice, "alice/demo", run_id).await;
    assert_eq!(run["conclusion"], "failure");

    // Re-run failed jobs: a (all legs) and b are re-run; report/always too
    // (they depend on a); everything gets a fresh attempt.
    let res = app
        .post(&format!(
            "/api/v3/repos/alice/demo/actions/runs/{run_id}/rerun-failed-jobs"
        ))
        .auth(&alice)
        .send()
        .await;
    res.assert_status(201);
    let run = common::run(&app, &alice, "alice/demo", run_id).await;
    assert_eq!(run["run_attempt"], 2);
    assert_eq!(run["status"], "queued");
    assert!(
        run["previous_attempt_url"]
            .as_str()
            .unwrap()
            .ends_with("/attempts/1")
    );
    let latest = common::jobs(&app, &alice, "alice/demo", run_id).await;
    assert_eq!(latest.len(), 3);
    assert!(latest.iter().all(|j| j["run_attempt"] == 2));
    let all = app
        .get(&format!(
            "/api/v3/repos/alice/demo/actions/runs/{run_id}/jobs?filter=all"
        ))
        .auth(&alice)
        .send()
        .await
        .json();
    assert_eq!(all["total_count"], 9);
    let att = app
        .get(&format!(
            "/api/v3/repos/alice/demo/actions/runs/{run_id}/attempts/1"
        ))
        .auth(&alice)
        .send()
        .await;
    att.assert_status(200);
    assert_eq!(att.json()["run_attempt"], 1);
    assert_eq!(att.json()["conclusion"], "failure");
    for _ in 0..3 {
        let s = runner.acquire(&app).await.unwrap();
        runner
            .complete(&app, s["job_id"].as_i64().unwrap(), "success", json!({}))
            .await;
    }
    let b = runner.acquire(&app).await.unwrap();
    assert_eq!(b["name"], "b");
    assert_eq!(b["run_attempt"], 2);
    runner
        .complete(&app, b["job_id"].as_i64().unwrap(), "success", json!({}))
        .await;
    let al = runner.acquire(&app).await.unwrap();
    runner
        .complete(&app, al["job_id"].as_i64().unwrap(), "success", json!({}))
        .await;
    let run = common::run(&app, &alice, "alice/demo", run_id).await;
    assert_eq!(run["conclusion"], "success");
    let latest = common::jobs(&app, &alice, "alice/demo", run_id).await;
    let report = latest.iter().find(|j| j["name"] == "report").unwrap();
    assert_eq!(report["conclusion"], "skipped");
}

#[tokio::test]
async fn rerun_failed_copies_successful_jobs() {
    let (app, alice, wc) = setup().await;
    let wf = "on: push\njobs:\n  ok:\n    runs-on: x\n    steps: [{run: a}]\n  bad:\n    runs-on: x\n    steps: [{run: b}]\n";
    wc.commit(&[(".github/workflows/w.yml", wf)], "w").await;
    wc.push("main").await;
    settle(&app).await;
    let run_id = runs(&app, &alice, "alice/demo").await[0]["id"]
        .as_i64()
        .unwrap();
    let runner = FakeRunner::register(&app, &alice, "alice/demo", &["x"]).await;
    let a = runner.acquire(&app).await.unwrap();
    runner
        .log(&app, a["job_id"].as_i64().unwrap(), 2, "ok log\n")
        .await;
    runner
        .complete(&app, a["job_id"].as_i64().unwrap(), "success", json!({}))
        .await;
    let b = runner.acquire(&app).await.unwrap();
    runner
        .complete(&app, b["job_id"].as_i64().unwrap(), "failure", json!({}))
        .await;
    app.post(&format!(
        "/api/v3/repos/alice/demo/actions/runs/{run_id}/rerun-failed-jobs"
    ))
    .auth(&alice)
    .send()
    .await
    .assert_status(201);
    let latest = jobs(&app, &alice, "alice/demo", run_id).await;
    let ok = latest.iter().find(|j| j["name"] == "ok").unwrap();
    assert_eq!(ok["conclusion"], "success");
    assert_eq!(ok["run_attempt"], 2);
    // Copied job keeps its logs.
    let res = app
        .get(&format!(
            "/api/v3/repos/alice/demo/actions/jobs/{}/logs",
            ok["id"]
        ))
        .auth(&alice)
        .send()
        .await;
    assert!(follow(&app, &res).await.text().contains("ok log"));
    let again = runner.acquire(&app).await.unwrap();
    assert_eq!(again["name"], "bad");
    assert!(runner.acquire(&app).await.is_none());
    // Rerun while running is refused.
    app.post(&format!(
        "/api/v3/repos/alice/demo/actions/runs/{run_id}/rerun"
    ))
    .auth(&alice)
    .send()
    .await
    .assert_status(403);
}

#[tokio::test]
async fn cancel_run() {
    let (app, alice, wc) = setup().await;
    let wf = "on: push\njobs:\n  one:\n    runs-on: x\n    steps: [{run: a}]\n  two:\n    runs-on: x\n    steps: [{run: b}]\n  after:\n    needs: [one, two]\n    if: always()\n    runs-on: x\n    steps: [{run: c}]\n";
    wc.commit(&[(".github/workflows/w.yml", wf)], "w").await;
    wc.push("main").await;
    settle(&app).await;
    let run_id = runs(&app, &alice, "alice/demo").await[0]["id"]
        .as_i64()
        .unwrap();
    let runner = FakeRunner::register(&app, &alice, "alice/demo", &["x"]).await;
    let one = runner.acquire(&app).await.unwrap();
    let res = app
        .post(&format!(
            "/api/v3/repos/alice/demo/actions/runs/{run_id}/cancel"
        ))
        .auth(&alice)
        .send()
        .await;
    res.assert_status(202);
    let hb = runner
        .steps(&app, one["job_id"].as_i64().unwrap(), json!([]))
        .await;
    assert_eq!(hb["cancel"], true);
    let jobs = common::jobs(&app, &alice, "alice/demo", run_id).await;
    let two = jobs.iter().find(|j| j["name"] == "two").unwrap();
    assert_eq!(two["conclusion"], "cancelled");
    runner
        .complete(
            &app,
            one["job_id"].as_i64().unwrap(),
            "cancelled",
            json!({}),
        )
        .await;
    // `after` has always() and still runs.
    let after = runner.acquire(&app).await.unwrap();
    assert_eq!(after["name"], "after");
    runner
        .complete(
            &app,
            after["job_id"].as_i64().unwrap(),
            "success",
            json!({}),
        )
        .await;
    let run = common::run(&app, &alice, "alice/demo", run_id).await;
    assert_eq!(run["status"], "completed");
    assert_eq!(run["conclusion"], "cancelled");
    app.post(&format!(
        "/api/v3/repos/alice/demo/actions/runs/{run_id}/cancel"
    ))
    .auth(&alice)
    .send()
    .await
    .assert_status(409);

    // Delete run.
    app.delete(&format!("/api/v3/repos/alice/demo/actions/runs/{run_id}"))
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    app.get(&format!("/api/v3/repos/alice/demo/actions/runs/{run_id}"))
        .auth(&alice)
        .send()
        .await
        .assert_status(404);
}

#[tokio::test]
async fn concurrency_groups() {
    let (app, alice, wc) = setup().await;
    let wf = "on: push\nconcurrency: deploy-${{ github.ref }}\njobs:\n  d:\n    runs-on: x\n    steps: [{run: a}]\n";
    wc.commit(&[(".github/workflows/w.yml", wf)], "1").await;
    wc.push("main").await;
    settle(&app).await;
    wc.commit(&[("a", "1")], "2").await;
    wc.push("main").await;
    settle(&app).await;
    wc.commit(&[("a", "2")], "3").await;
    wc.push("main").await;
    settle(&app).await;
    let rs = runs(&app, &alice, "alice/demo").await;
    let st: Vec<(&str, Option<&str>)> = rs
        .iter()
        .map(|r| (r["status"].as_str().unwrap(), r["conclusion"].as_str()))
        .collect();
    // newest first: pending, cancelled (superseded pending), queued.
    assert_eq!(
        st,
        [
            ("pending", None),
            ("completed", Some("cancelled")),
            ("queued", None)
        ]
    );
    let runner = FakeRunner::register(&app, &alice, "alice/demo", &["x"]).await;
    let j = runner.acquire(&app).await.unwrap();
    assert!(runner.acquire(&app).await.is_none());
    runner
        .complete(&app, j["job_id"].as_i64().unwrap(), "success", json!({}))
        .await;
    settle(&app).await;
    let j3 = runner.acquire(&app).await.expect("pending run released");
    assert_eq!(j3["run_id"], rs[0]["id"]);

    // cancel-in-progress cancels the running one.
    let wf2 = "on: push\nconcurrency:\n  group: g\n  cancel-in-progress: true\njobs:\n  d:\n    runs-on: x\n    steps: [{run: a}]\n";
    wc.commit(&[(".github/workflows/w.yml", wf2)], "4").await;
    wc.push("main").await;
    settle(&app).await;
    let j4 = runner.acquire(&app).await.unwrap();
    wc.commit(&[("a", "3")], "5").await;
    wc.push("main").await;
    settle(&app).await;
    let hb = runner
        .steps(&app, j4["job_id"].as_i64().unwrap(), json!([]))
        .await;
    assert_eq!(hb["cancel"], true);
}

#[tokio::test]
async fn filters_tags_paths_and_startup_failure() {
    let (app, alice, wc) = setup().await;
    let wf = r#"
on:
  push:
    tags: ["v*"]
jobs:
  r:
    runs-on: x
    steps: [{run: a}]
"#;
    let docs = r#"
on:
  push:
    branches: [main]
    paths: ["docs/**"]
jobs:
  r:
    runs-on: x
    steps: [{run: a}]
"#;
    wc.commit(
        &[
            (".github/workflows/release.yml", wf),
            (".github/workflows/docs.yml", docs),
        ],
        "init",
    )
    .await;
    wc.push("main").await;
    settle(&app).await;
    // First push creates the branch: path filters can't be evaluated → docs runs.
    let rs = runs(&app, &alice, "alice/demo").await;
    assert_eq!(rs.len(), 1);
    assert_eq!(rs[0]["path"], ".github/workflows/docs.yml");
    wc.commit(&[("src/a.rs", "x")], "code").await;
    wc.push("main").await;
    settle(&app).await;
    assert_eq!(runs(&app, &alice, "alice/demo").await.len(), 1);
    wc.commit(&[("docs/a.md", "x")], "docs").await;
    wc.push("main").await;
    settle(&app).await;
    assert_eq!(runs(&app, &alice, "alice/demo").await.len(), 2);
    wc.tag("v1.0").await;
    wc.push("v1.0").await;
    settle(&app).await;
    let rs = runs(&app, &alice, "alice/demo").await;
    assert_eq!(rs.len(), 3);
    assert_eq!(rs[0]["path"], ".github/workflows/release.yml");
    assert_eq!(rs[0]["head_branch"], "v1.0");

    // Broken workflow file → startup_failure run.
    wc.commit(
        &[(".github/workflows/bad.yml", "on: push\njobs: {}\n")],
        "bad",
    )
    .await;
    wc.push("main").await;
    settle(&app).await;
    let rs = runs(&app, &alice, "alice/demo").await;
    let bad = rs
        .iter()
        .find(|r| r["path"] == ".github/workflows/bad.yml")
        .unwrap();
    assert_eq!(bad["status"], "completed");
    assert_eq!(bad["conclusion"], "startup_failure");

    // Filters on the runs list.
    let res = app
        .get("/api/v3/repos/alice/demo/actions/runs?status=startup_failure")
        .auth(&alice)
        .send()
        .await;
    assert_eq!(res.json()["total_count"], 1);
    let res = app
        .get("/api/v3/repos/alice/demo/actions/runs?branch=v1.0&event=push&actor=alice")
        .auth(&alice)
        .send()
        .await;
    assert_eq!(res.json()["total_count"], 1);
    let res = app
        .get("/api/v3/repos/alice/demo/actions/runs?per_page=1")
        .auth(&alice)
        .send()
        .await;
    assert!(res.header("link").unwrap().contains("rel=\"next\""));
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let res = app
        .get(&format!(
            "/api/v3/repos/alice/demo/actions/runs?created=%3E%3D{today}"
        ))
        .auth(&alice)
        .send()
        .await;
    assert_eq!(res.json()["total_count"], 4);
    app.get("/api/v3/repos/alice/demo/actions/runs?status=bogus")
        .auth(&alice)
        .send()
        .await
        .assert_status(422);
    let wf_runs = app
        .get("/api/v3/repos/alice/demo/actions/workflows/docs.yml/runs")
        .auth(&alice)
        .send()
        .await;
    assert_eq!(wf_runs.json()["total_count"], 2);
}

#[tokio::test]
async fn dispatch_inputs_enable_disable() {
    let (app, alice, wc) = setup().await;
    let wf = r#"
name: Deploy
on:
  workflow_dispatch:
    inputs:
      env:
        type: choice
        options: [staging, prod]
        required: true
      dry:
        type: boolean
        default: true
jobs:
  d:
    runs-on: x
    name: deploy ${{ inputs.env }}
    steps: [{run: "echo ${{ inputs.env }}"}]
"#;
    wc.commit(&[(".github/workflows/deploy.yml", wf)], "w")
        .await;
    wc.push("main").await;
    settle(&app).await;
    assert!(runs(&app, &alice, "alice/demo").await.is_empty());
    let url = "/api/v3/repos/alice/demo/actions/workflows/deploy.yml/dispatches";
    app.post(url)
        .auth(&alice)
        .json(&json!({"ref": "main", "inputs": {"env": "dev"}}))
        .send()
        .await
        .assert_status(422);
    app.post(url)
        .auth(&alice)
        .json(&json!({"ref": "main"}))
        .send()
        .await
        .assert_status(422);
    app.post(url)
        .auth(&alice)
        .json(&json!({"ref": "nope", "inputs": {"env": "prod"}}))
        .send()
        .await
        .assert_status(422);
    app.post(url)
        .auth(&alice)
        .json(&json!({"ref": "main", "inputs": {"env": "prod"}}))
        .send()
        .await
        .assert_status(204);
    let rs = runs(&app, &alice, "alice/demo").await;
    assert_eq!(rs[0]["event"], "workflow_dispatch");
    let jobs = common::jobs(&app, &alice, "alice/demo", rs[0]["id"].as_i64().unwrap()).await;
    assert_eq!(jobs[0]["name"], "deploy prod");
    let runner = FakeRunner::register(&app, &alice, "alice/demo", &["x"]).await;
    let spec = runner.acquire(&app).await.unwrap();
    assert_eq!(spec["inputs"], json!({"env": "prod", "dry": true}));
    assert_eq!(spec["github"]["event"]["inputs"]["env"], "prod");

    let res = app
        .post(url)
        .auth(&alice)
        .json(&json!({"ref": "main", "inputs": {"env": "staging"}, "return_run_details": true}))
        .send()
        .await;
    res.assert_status(200);
    assert!(res.json()["workflow_run_id"].is_i64());

    app.put("/api/v3/repos/alice/demo/actions/workflows/deploy.yml/disable")
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    let w = app
        .get("/api/v3/repos/alice/demo/actions/workflows/deploy.yml")
        .auth(&alice)
        .send()
        .await
        .json();
    assert_eq!(w["state"], "disabled_manually");
    app.post(url)
        .auth(&alice)
        .json(&json!({"ref": "main", "inputs": {"env": "prod"}}))
        .send()
        .await
        .assert_status(422);
    app.put("/api/v3/repos/alice/demo/actions/workflows/deploy.yml/enable")
        .auth(&alice)
        .send()
        .await
        .assert_status(204);

    // Non-writers can't dispatch.
    let bob = app.create_user("bob").await;
    app.post(url)
        .auth(&bob)
        .json(&json!({"ref": "main", "inputs": {"env": "prod"}}))
        .send()
        .await
        .assert_status(403);
}

#[tokio::test]
async fn schedule_tick_creates_runs() {
    let (app, alice, wc) = setup().await;
    let wf = "on:\n  schedule:\n    - cron: '*/5 * * * *'\njobs:\n  n:\n    runs-on: x\n    steps: [{run: a}]\n";
    wc.commit(&[(".github/workflows/nightly.yml", wf)], "w")
        .await;
    wc.push("main").await;
    settle(&app).await;
    assert!(runs(&app, &alice, "alice/demo").await.is_empty());
    let now = chrono::Utc::now();
    let n = bgh_actions::trigger::schedule_tick(&app.state, now + chrono::Duration::minutes(6))
        .await
        .unwrap();
    assert_eq!(n, 1);
    // Same window again: nothing.
    let n = bgh_actions::trigger::schedule_tick(&app.state, now + chrono::Duration::minutes(6))
        .await
        .unwrap();
    assert_eq!(n, 0);
    let rs = runs(&app, &alice, "alice/demo").await;
    assert_eq!(rs[0]["event"], "schedule");
    assert_eq!(rs[0]["head_branch"], "main");
}

#[tokio::test]
async fn artifacts_upload_list_download_delete() {
    let (app, alice, wc) = setup().await;
    wc.commit(
        &[(
            ".github/workflows/w.yml",
            "on: push\njobs:\n  a:\n    runs-on: x\n    steps: [{run: a}]\n",
        )],
        "w",
    )
    .await;
    wc.push("main").await;
    settle(&app).await;
    let runner = FakeRunner::register(&app, &alice, "alice/demo", &["x"]).await;
    let spec = runner.acquire(&app).await.unwrap();
    let job = spec["job_id"].as_i64().unwrap();
    let run_id = spec["run_id"].as_i64().unwrap();
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut z = zip::ZipWriter::new(&mut buf);
        z.start_file("hello.txt", zip::write::SimpleFileOptions::default())
            .unwrap();
        z.write_all(b"hi there").unwrap();
        z.finish().unwrap();
    }
    let zip_bytes = buf.into_inner();
    let info = runner.upload(&app, job, "my-art", zip_bytes.clone()).await;
    assert_eq!(info["name"], "my-art");
    let art_id = info["id"].as_i64().unwrap();
    let list = app
        .get(&format!("/_bgh/actions/runner/jobs/{job}/artifacts"))
        .header("authorization", &format!("RunnerToken {}", runner.token))
        .send()
        .await
        .json();
    assert_eq!(list[0]["id"], art_id);
    runner.complete(&app, job, "success", json!({})).await;

    let res = app
        .get("/api/v3/repos/alice/demo/actions/artifacts")
        .auth(&alice)
        .send()
        .await;
    res.assert_status(200);
    let v = res.json();
    assert_eq!(v["total_count"], 1);
    let a = &v["artifacts"][0];
    assert_eq!(a["name"], "my-art");
    assert_eq!(a["size_in_bytes"], zip_bytes.len());
    assert_eq!(a["expired"], false);
    assert_eq!(a["workflow_run"]["id"], run_id);
    assert!(a["digest"].as_str().unwrap().starts_with("sha256:"));
    assert_eq!(
        a["archive_download_url"],
        app.url(&format!(
            "/api/v3/repos/alice/demo/actions/artifacts/{art_id}/zip"
        ))
    );
    let res = app
        .get(&format!(
            "/api/v3/repos/alice/demo/actions/runs/{run_id}/artifacts?name=my-art"
        ))
        .auth(&alice)
        .send()
        .await;
    assert_eq!(res.json()["total_count"], 1);
    let res = app
        .get(&format!(
            "/api/v3/repos/alice/demo/actions/artifacts/{art_id}/zip"
        ))
        .auth(&alice)
        .send()
        .await;
    let dl = follow(&app, &res).await;
    dl.assert_status(200);
    assert_eq!(dl.body.to_vec(), zip_bytes);
    app.get(&format!(
        "/api/v3/repos/alice/demo/actions/artifacts/{art_id}/tar"
    ))
    .auth(&alice)
    .send()
    .await
    .assert_status(404);
    app.delete(&format!(
        "/api/v3/repos/alice/demo/actions/artifacts/{art_id}"
    ))
    .auth(&alice)
    .send()
    .await
    .assert_status(204);
    app.get(&format!(
        "/api/v3/repos/alice/demo/actions/artifacts/{art_id}"
    ))
    .auth(&alice)
    .send()
    .await
    .assert_status(404);
}

#[tokio::test]
async fn pull_request_events_trigger_runs() {
    let (app, alice, wc) = setup().await;
    let wf = r#"
on:
  pull_request:
    branches: [main]
  pull_request_target:
    types: [opened]
jobs:
  t:
    runs-on: x
    steps: [{run: "echo ${{ github.event.pull_request.title }}"}]
"#;
    let base = wc.commit(&[(".github/workflows/pr.yml", wf)], "w").await;
    wc.push("main").await;
    wc.checkout_new("feature").await;
    let head = wc.commit(&[("f.txt", "x")], "feature").await;
    wc.push("feature").await;
    settle(&app).await;
    let before = runs(&app, &alice, "alice/demo").await.len();
    let repo_id: i64 = sqlx::query_scalar("SELECT id FROM repositories WHERE name = 'demo'")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    // Minimal PR rows (bgh-pulls owns the API).
    let issue_id: i64 = sqlx::query_scalar(
        "INSERT INTO issues (repo_id, number, title, author_id, is_pull_request)
         VALUES ($1, 1, 'My PR', $2, true) RETURNING id",
    )
    .bind(repo_id)
    .bind(alice.id)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO pull_requests (issue_id, repo_id, head_repo_id, head_ref, head_sha, base_ref, base_sha)
         VALUES ($1, $2, $2, 'feature', $3, 'main', $4)",
    )
    .bind(issue_id)
    .bind(repo_id)
    .bind(&head)
    .bind(&base)
    .execute(&app.state.db)
    .await
    .unwrap();
    app.state
        .events
        .emit(bgh_core::events::Event::PullRequestOpened {
            repo_id,
            pull_id: issue_id,
            actor_id: alice.id,
        });
    settle(&app).await;
    let rs = runs(&app, &alice, "alice/demo").await;
    assert_eq!(rs.len(), before + 2);
    let pr_run = rs.iter().find(|r| r["event"] == "pull_request").unwrap();
    assert_eq!(pr_run["head_sha"], head);
    assert_eq!(pr_run["head_branch"], "feature");
    assert_eq!(pr_run["display_title"], "My PR");
    assert_eq!(pr_run["pull_requests"][0]["number"], 1);
    assert_eq!(pr_run["pull_requests"][0]["base"]["ref"], "main");
    let target = rs
        .iter()
        .find(|r| r["event"] == "pull_request_target")
        .unwrap();
    assert_eq!(target["head_sha"], base);
    let runner = FakeRunner::register(&app, &alice, "alice/demo", &["x"]).await;
    let mut specs = vec![];
    while let Some(s) = runner.acquire(&app).await {
        specs.push(s);
    }
    let s = specs
        .iter()
        .find(|s| s["github"]["event_name"] == "pull_request")
        .unwrap();
    assert_eq!(s["github"]["ref"], "refs/pull/1/merge");
    assert_eq!(s["github"]["head_ref"], "feature");
    assert_eq!(s["github"]["base_ref"], "main");
    assert_eq!(s["github"]["event"]["pull_request"]["title"], "My PR");

    // synchronize is not in pull_request_target types: only one more run.
    app.state
        .events
        .emit(bgh_core::events::Event::PullRequestSynchronized {
            repo_id,
            pull_id: issue_id,
            actor_id: Some(alice.id),
            before: base.clone(),
            after: head.clone(),
        });
    settle(&app).await;
    assert_eq!(runs(&app, &alice, "alice/demo").await.len(), before + 3);
}

#[tokio::test]
async fn events_carry_webhook_payloads() {
    let (app, alice, wc) = setup().await;
    let mut rx = app.state.events.subscribe();
    wc.commit(
        &[(
            ".github/workflows/w.yml",
            "name: W\non: push\njobs:\n  a:\n    runs-on: x\n    steps: [{run: a}]\n",
        )],
        "w",
    )
    .await;
    wc.push("main").await;
    settle(&app).await;
    let runner = FakeRunner::register(&app, &alice, "alice/demo", &["x"]).await;
    let spec = runner.acquire(&app).await.unwrap();
    runner
        .complete(&app, spec["job_id"].as_i64().unwrap(), "success", json!({}))
        .await;

    let mut run_actions = vec![];
    let mut check_runs = vec![];
    let mut suites = vec![];
    let mut job_actions = vec![];
    while let Ok(ev) = rx.try_recv() {
        match &*ev {
            bgh_core::events::Event::WorkflowJobUpdated {
                action,
                job_id,
                workflow_job,
                ..
            } => {
                // Same shape as GET /actions/jobs/{id}.
                assert_eq!(workflow_job["id"], *job_id);
                assert_eq!(workflow_job["run_id"], spec["run_id"]);
                assert_eq!(workflow_job["workflow_name"], "W");
                assert_eq!(workflow_job["name"], "a");
                assert!(workflow_job["html_url"].as_str().unwrap().contains("/job/"));
                assert!(workflow_job["steps"].is_array());
                assert!(workflow_job["labels"].is_array());
                job_actions.push((action.clone(), workflow_job["status"].clone()));
            }
            bgh_core::events::Event::WorkflowRunUpdated {
                action,
                workflow_run,
                workflow,
                ..
            } => {
                assert_eq!(workflow_run["id"], spec["run_id"]);
                assert_eq!(workflow_run["path"], ".github/workflows/w.yml");
                assert_eq!(workflow_run["repository"]["full_name"], "alice/demo");
                assert_eq!(workflow.as_ref().unwrap()["name"], "W");
                run_actions.push((action.clone(), workflow_run["status"].clone()));
            }
            bgh_core::events::Event::CheckRunUpdated { action, .. } => {
                check_runs.push(action.clone())
            }
            bgh_core::events::Event::CheckSuiteUpdated {
                action, actor_id, ..
            } => {
                assert_eq!(*actor_id, Some(alice.id));
                suites.push(action.clone());
            }
            _ => {}
        }
    }
    assert_eq!(
        run_actions,
        [
            ("requested".to_string(), json!("queued")),
            ("in_progress".to_string(), json!("in_progress")),
            ("completed".to_string(), json!("completed")),
        ]
    );
    assert_eq!(check_runs, ["created", "completed"]);
    assert_eq!(suites, ["completed"]);
    assert_eq!(
        job_actions,
        [
            ("queued".to_string(), json!("queued")),
            ("in_progress".to_string(), json!("in_progress")),
            ("completed".to_string(), json!("completed")),
        ]
    );
    // The REST endpoint renders the same object.
    let job_id = spec["job_id"].as_i64().unwrap();
    let rest = app
        .get(&format!("/api/v3/repos/alice/demo/actions/jobs/{job_id}"))
        .auth(&alice)
        .send()
        .await
        .json();
    assert_eq!(rest["conclusion"], "success");
}
