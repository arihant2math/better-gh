//! Commit statuses, checks API, events, diff limits.

use crate::common;

use bgh_core::events::Event;
use common::*;
use serde_json::json;

#[tokio::test]
async fn commit_statuses_and_combined() {
    let f = fixture().await;
    let app = &f.app;
    let sha = &f.feature;
    // Empty combined status is pending.
    let res = app
        .get(&format!("/api/v3/repos/alice/demo/commits/{sha}/status"))
        .send()
        .await;
    res.assert_status(200);
    let c = res.json();
    assert_eq!(c["state"], "pending");
    assert_eq!(c["total_count"], 0);
    assert_eq!(c["sha"], *sha);
    assert_eq!(c["repository"]["full_name"], "alice/demo");

    let res = app
        .post(&format!("/api/v3/repos/alice/demo/statuses/{sha}"))
        .auth(&f.alice)
        .json(
            &json!({"state": "pending", "context": "ci/build", "description": "Building",
                      "target_url": "https://ci.example.com/1"}),
        )
        .send()
        .await;
    res.assert_status(201);
    let s = res.json();
    assert_eq!(s["state"], "pending");
    assert_eq!(s["context"], "ci/build");
    assert_eq!(s["description"], "Building");
    assert_eq!(s["target_url"], "https://ci.example.com/1");
    assert_eq!(s["creator"]["login"], "alice");
    assert_eq!(
        s["url"],
        app.url(&format!("/api/v3/repos/alice/demo/statuses/{sha}"))
    );
    assert!(s["node_id"].is_string());
    assert!(s["avatar_url"].is_string());
    app.post(&format!("/api/v3/repos/alice/demo/statuses/{sha}"))
        .auth(&f.alice)
        .json(&json!({"state": "success", "context": "ci/build"}))
        .send()
        .await
        .assert_status(201);
    app.post(&format!("/api/v3/repos/alice/demo/statuses/{sha}"))
        .auth(&f.alice)
        .json(&json!({"state": "success"}))
        .send()
        .await
        .assert_status(201);
    // Branch names resolve.
    let c = app
        .get("/api/v3/repos/alice/demo/commits/feature/status")
        .send()
        .await
        .json();
    assert_eq!(c["state"], "success");
    assert_eq!(c["total_count"], 2);
    assert!(c["statuses"][0].get("creator").is_none());
    let list = app
        .get(&format!("/api/v3/repos/alice/demo/commits/{sha}/statuses"))
        .send()
        .await
        .json();
    assert_eq!(list.as_array().unwrap().len(), 3);
    assert_eq!(list[0]["context"], "default", "newest first");
    let legacy = app
        .get(&format!("/api/v3/repos/alice/demo/statuses/{sha}"))
        .send()
        .await
        .json();
    assert_eq!(legacy.as_array().unwrap().len(), 3);
    app.post(&format!("/api/v3/repos/alice/demo/statuses/{sha}"))
        .auth(&f.alice)
        .json(&json!({"state": "error", "context": "lint"}))
        .send()
        .await
        .assert_status(201);
    let c = app
        .get(&format!("/api/v3/repos/alice/demo/commits/{sha}/status"))
        .send()
        .await
        .json();
    assert_eq!(c["state"], "failure");

    // Validation and permissions.
    app.post(&format!("/api/v3/repos/alice/demo/statuses/{sha}"))
        .auth(&f.alice)
        .json(&json!({"state": "great"}))
        .send()
        .await
        .assert_status(422);
    let res = app
        .post(&format!(
            "/api/v3/repos/alice/demo/statuses/{}",
            "1".repeat(40)
        ))
        .auth(&f.alice)
        .json(&json!({"state": "success"}))
        .send()
        .await;
    res.assert_status(422);
    let bob = app.create_user("bob").await;
    app.post(&format!("/api/v3/repos/alice/demo/statuses/{sha}"))
        .auth(&bob)
        .json(&json!({"state": "success"}))
        .send()
        .await
        .assert_status(403);
    app.get("/api/v3/repos/alice/demo/commits/nope/status")
        .send()
        .await
        .assert_status(404);
}

#[tokio::test]
async fn check_runs_and_suites() {
    let f = fixture().await;
    let app = &f.app;
    open_pr(app, &f.alice, "alice/demo", "feature", "main").await;
    let sha = f.feature.clone();
    let res = app
        .post("/api/v3/repos/alice/demo/check-runs")
        .auth(&f.alice)
        .json(&json!({
            "name": "lint", "head_sha": sha, "status": "in_progress",
            "external_id": "ext-1", "details_url": "https://ci.example.com/lint",
            "output": {"title": "Linting", "summary": "Running", "annotations": [
                {"path": "README.md", "start_line": 1, "end_line": 1, "annotation_level": "warning",
                 "message": "Heading style", "title": "MD001"}
            ]}
        }))
        .send()
        .await;
    res.assert_status(201);
    let run = res.json();
    let run_id = run["id"].as_i64().unwrap();
    assert_eq!(run["name"], "lint");
    assert_eq!(run["head_sha"], sha);
    assert_eq!(run["status"], "in_progress");
    assert!(run["conclusion"].is_null());
    assert_eq!(run["external_id"], "ext-1");
    assert_eq!(run["details_url"], "https://ci.example.com/lint");
    assert!(run["started_at"].is_string());
    assert!(run["completed_at"].is_null());
    assert_eq!(run["output"]["title"], "Linting");
    assert_eq!(run["output"]["annotations_count"], 1);
    assert_eq!(
        run["output"]["annotations_url"],
        app.url(&format!(
            "/api/v3/repos/alice/demo/check-runs/{run_id}/annotations"
        ))
    );
    assert_eq!(
        run["url"],
        app.url(&format!("/api/v3/repos/alice/demo/check-runs/{run_id}"))
    );
    assert_eq!(run["app"]["slug"], "api");
    assert_eq!(run["pull_requests"][0]["number"], 1);
    assert_eq!(run["pull_requests"][0]["head"]["ref"], "feature");
    assert_eq!(run["pull_requests"][0]["base"]["repo"]["name"], "demo");
    let suite_id = run["check_suite"]["id"].as_i64().unwrap();

    let ann = app
        .get(&format!(
            "/api/v3/repos/alice/demo/check-runs/{run_id}/annotations"
        ))
        .send()
        .await
        .json();
    assert_eq!(ann[0]["path"], "README.md");
    assert_eq!(ann[0]["annotation_level"], "warning");
    assert_eq!(ann[0]["message"], "Heading style");
    assert_eq!(ann[0]["title"], "MD001");
    assert_eq!(
        ann[0]["blob_href"],
        app.url(&format!("/alice/demo/blob/{sha}/README.md"))
    );

    // Suite in progress.
    let suite = app
        .get(&format!("/api/v3/repos/alice/demo/check-suites/{suite_id}"))
        .send()
        .await
        .json();
    assert_eq!(suite["status"], "in_progress");
    assert_eq!(suite["head_sha"], sha);
    assert_eq!(suite["head_branch"], "feature");
    assert_eq!(suite["latest_check_runs_count"], 1);
    assert_eq!(suite["head_commit"]["message"], "Improve readme");
    assert_eq!(suite["repository"]["full_name"], "alice/demo");
    assert_eq!(suite["pull_requests"][0]["number"], 1);

    // Complete it.
    let res = app
        .patch(&format!("/api/v3/repos/alice/demo/check-runs/{run_id}"))
        .auth(&f.alice)
        .json(&json!({"conclusion": "failure", "output": {"title": "Lint failed", "summary": "1 issue"}}))
        .send()
        .await;
    res.assert_status(200);
    let run = res.json();
    assert_eq!(run["status"], "completed");
    assert_eq!(run["conclusion"], "failure");
    assert!(run["completed_at"].is_string());
    assert_eq!(run["output"]["title"], "Lint failed");
    let suite = app
        .get(&format!("/api/v3/repos/alice/demo/check-suites/{suite_id}"))
        .send()
        .await
        .json();
    assert_eq!(suite["status"], "completed");
    assert_eq!(suite["conclusion"], "failure");

    // Second run in the same suite; list endpoints.
    app.post("/api/v3/repos/alice/demo/check-runs")
        .auth(&f.alice)
        .json(&json!({"name": "test", "head_sha": sha, "conclusion": "success"}))
        .send()
        .await
        .assert_status(201);
    let list = app
        .get(&format!(
            "/api/v3/repos/alice/demo/commits/{sha}/check-runs"
        ))
        .send()
        .await
        .json();
    assert_eq!(list["total_count"], 2);
    assert_eq!(list["check_runs"].as_array().unwrap().len(), 2);
    let list = app
        .get("/api/v3/repos/alice/demo/commits/feature/check-runs?check_name=test")
        .send()
        .await
        .json();
    assert_eq!(list["total_count"], 1);
    let list = app
        .get(&format!(
            "/api/v3/repos/alice/demo/check-suites/{suite_id}/check-runs?status=completed"
        ))
        .send()
        .await
        .json();
    assert_eq!(list["total_count"], 2);
    let suites = app
        .get(&format!(
            "/api/v3/repos/alice/demo/commits/{sha}/check-suites"
        ))
        .send()
        .await
        .json();
    assert_eq!(suites["total_count"], 1);
    assert_eq!(suites["check_suites"][0]["latest_check_runs_count"], 2);

    // Required check failing → blocked; mergeable_state reflects it.
    protect(
        app,
        f.repo_id,
        "main",
        &[(
            "required_status_checks",
            json!({"strict": false, "checks": [{"context": "lint", "app_id": null}]}),
        )],
    )
    .await;
    // Rerequest resets the run (and re-triggers mergeability).
    let res = app
        .post(&format!(
            "/api/v3/repos/alice/demo/check-runs/{run_id}/rerequest"
        ))
        .auth(&f.alice)
        .send()
        .await;
    res.assert_status(201);
    settle(app).await;
    let run = app
        .get(&format!("/api/v3/repos/alice/demo/check-runs/{run_id}"))
        .send()
        .await
        .json();
    assert_eq!(run["status"], "queued");
    assert_eq!(run["output"]["annotations_count"], 0);
    let pr = app
        .get("/api/v3/repos/alice/demo/pulls/1")
        .send()
        .await
        .json();
    assert_eq!(pr["mergeable_state"], "blocked");
    app.post(&format!(
        "/api/v3/repos/alice/demo/check-suites/{suite_id}/rerequest"
    ))
    .auth(&f.alice)
    .send()
    .await
    .assert_status(201);

    // Suites via POST (existing → 200).
    let res = app
        .post("/api/v3/repos/alice/demo/check-suites")
        .auth(&f.alice)
        .json(&json!({"head_sha": f.main}))
        .send()
        .await;
    res.assert_status(201);
    assert_eq!(res.json()["head_branch"], "main");
    app.post("/api/v3/repos/alice/demo/check-suites")
        .auth(&f.alice)
        .json(&json!({"head_sha": f.main}))
        .send()
        .await
        .assert_status(200);
    let res = app
        .patch("/api/v3/repos/alice/demo/check-suites/preferences")
        .auth(&f.alice)
        .json(&json!({"auto_trigger_checks": [{"app_id": 2, "setting": false}]}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(
        res.json()["preferences"]["auto_trigger_checks"][0]["app_id"],
        2
    );

    // Validation.
    app.post("/api/v3/repos/alice/demo/check-runs")
        .auth(&f.alice)
        .json(&json!({"head_sha": sha}))
        .send()
        .await
        .assert_status(422);
    app.post("/api/v3/repos/alice/demo/check-runs")
        .auth(&f.alice)
        .json(&json!({"name": "x", "head_sha": sha, "status": "completed"}))
        .send()
        .await
        .assert_status(422);
    app.post("/api/v3/repos/alice/demo/check-runs")
        .auth(&f.alice)
        .json(&json!({"name": "x", "head_sha": sha, "output": {"annotations": [{"path": "a"}]}}))
        .send()
        .await
        .assert_status(422);
    app.get("/api/v3/repos/alice/demo/check-runs/999999")
        .send()
        .await
        .assert_status(404);
}

#[tokio::test]
async fn events_are_emitted() {
    let f = fixture().await;
    let app = &f.app;
    let mut rx = app.state.events.subscribe();
    open_pr(app, &f.alice, "alice/demo", "feature", "main").await;
    app.post(&format!("/api/v3/repos/alice/demo/statuses/{}", f.feature))
        .auth(&f.alice)
        .json(&json!({"state": "success"}))
        .send()
        .await
        .assert_status(201);
    settle(app).await;
    app.put("/api/v3/repos/alice/demo/pulls/1/merge")
        .auth(&f.alice)
        .send()
        .await
        .assert_status(200);
    settle(app).await;
    let mut names = Vec::new();
    while let Ok(e) = rx.try_recv() {
        names.push(e.name());
        if let Event::PullRequestMerged {
            merge_commit_sha, ..
        } = &*e
        {
            assert_eq!(merge_commit_sha.len(), 40);
        }
    }
    for expected in [
        "pull_request_opened",
        "commit_status_created",
        "pull_request_merged",
        "push",
    ] {
        assert!(names.contains(&expected), "missing {expected} in {names:?}");
    }
}

#[tokio::test]
async fn huge_diff_is_rejected_for_diff_media_type() {
    let f = fixture().await;
    let app = &f.app;
    branch(app, f.repo_id, "big", &f.main).await;
    let names: Vec<String> = (0..301).map(|i| format!("gen/f{i}.txt")).collect();
    let files: Vec<(&str, Option<&str>)> =
        names.iter().map(|n| (n.as_str(), Some("x\n"))).collect();
    commit(app, f.repo_id, "big", Some(&f.main), &files, "many files").await;
    open_pr(app, &f.alice, "alice/demo", "big", "main").await;
    let res = app
        .get("/api/v3/repos/alice/demo/pulls/1")
        .header("accept", "application/vnd.github.diff")
        .send()
        .await;
    res.assert_status(406);
    assert!(
        res.json()["message"]
            .as_str()
            .unwrap()
            .contains("maximum number of files")
    );
    // The files API still pages through everything.
    let res = app
        .get("/api/v3/repos/alice/demo/pulls/1/files?per_page=100&page=4")
        .send()
        .await;
    assert_eq!(res.json().as_array().unwrap().len(), 1);
    let pr = app
        .get("/api/v3/repos/alice/demo/pulls/1")
        .send()
        .await
        .json();
    assert_eq!(pr["changed_files"], 301);
}
