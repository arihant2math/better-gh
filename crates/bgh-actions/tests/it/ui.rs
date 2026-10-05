//! Private endpoints for the web UI: run graph and dispatch form.

use crate::common;

use common::*;

const WF: &str = r#"
name: Build
on:
  push:
  workflow_dispatch:
    inputs:
      level:
        description: Log level
        required: true
        default: warning
        type: choice
        options: [info, warning, debug]
      dry:
        type: boolean
      env:
        type: environment
jobs:
  lint:
    runs-on: x
    steps: [{run: a}]
  build:
    name: Build it
    needs: lint
    runs-on: x
    strategy:
      matrix:
        os: [a, b]
    steps: [{run: a}]
  deploy:
    needs: [lint, build]
    runs-on: x
    steps: [{run: a}]
"#;

#[tokio::test]
async fn run_graph_and_dispatch_form() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    app.create_repo_with(
        &alice,
        None,
        serde_json::json!({"name": "demo", "private": true}),
    )
    .await;
    let wc = WorkingCopy::new(&app, &alice, "alice", "demo").await;
    wc.commit(&[(".github/workflows/b.yml", WF)], "wf").await;
    wc.push("main").await;
    settle(&app).await;
    let run = &runs(&app, &alice, "alice/demo").await[0];
    let run_id = run["id"].as_i64().unwrap();

    let path = format!("/_bgh/actions/repos/alice/demo/runs/{run_id}/graph");
    let g = app.get(&path).auth(&alice).send().await;
    g.assert_status(200);
    let g = g.json();
    assert_eq!(g["workflow_name"], "Build");
    let jobs = g["jobs"].as_array().unwrap();
    assert_eq!(jobs.len(), 3);
    assert_eq!(jobs[0]["key"], "lint");
    assert_eq!(jobs[1]["name"], "Build it");
    assert_eq!(jobs[1]["needs"], serde_json::json!(["lint"]));
    assert_eq!(jobs[1]["matrix"], true);
    assert_eq!(jobs[2]["needs"], serde_json::json!(["lint", "build"]));
    // Only `lint` is materialized so far.
    let rest_jobs = jobs_of(&app, &alice, run_id).await;
    assert_eq!(rest_jobs.len(), 1);
    let id = rest_jobs[0]["id"].as_i64().unwrap().to_string();
    assert_eq!(g["job_keys"][&id], "lint");
    // Private repo: no existence leak.
    app.get(&path).auth(&bob).send().await.assert_status(404);
    app.get(&path).send().await.assert_status(404);

    let f = app
        .get("/_bgh/actions/repos/alice/demo/workflows/b.yml/dispatch")
        .auth(&alice)
        .send()
        .await;
    f.assert_status(200);
    let f = f.json();
    assert_eq!(f["ref"], "main");
    assert_eq!(f["dispatchable"], true);
    assert!(f["sha"].is_string());
    let inputs = f["inputs"].as_array().unwrap();
    assert_eq!(inputs.len(), 3);
    assert_eq!(inputs[0]["name"], "level");
    assert_eq!(inputs[0]["type"], "choice");
    assert_eq!(inputs[0]["required"], true);
    assert_eq!(inputs[0]["default"], "warning");
    assert_eq!(
        inputs[0]["options"],
        serde_json::json!(["info", "warning", "debug"])
    );
    assert_eq!(inputs[1]["type"], "boolean");
    assert_eq!(inputs[2]["type"], "environment");

    let missing = app
        .get("/_bgh/actions/repos/alice/demo/workflows/b.yml/dispatch?ref=nope")
        .auth(&alice)
        .send()
        .await
        .json();
    assert_eq!(missing["dispatchable"], false);
    assert!(missing["error"].as_str().unwrap().contains("nope"));
    app.get("/_bgh/actions/repos/alice/demo/workflows/zzz.yml/dispatch")
        .auth(&alice)
        .send()
        .await
        .assert_status(404);
}

async fn jobs_of(
    app: &bgh_core::testing::TestApp,
    user: &bgh_core::testing::TestUser,
    run_id: i64,
) -> Vec<serde_json::Value> {
    jobs(app, user, "alice/demo", run_id).await
}
