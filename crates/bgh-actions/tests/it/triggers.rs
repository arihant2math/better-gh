//! P26 triggers: repository_dispatch, workflow_run, the remaining
//! pull_request / issues / issue_comment / release activity types, reviews,
//! create / delete, label, milestone, watch, fork, public, gollum,
//! check_run / check_suite; merge-ref PR runs; re-run from Checks; job
//! concurrency; badge.svg.

use crate::common;

use bgh_core::events::Event;
use bgh_core::testing::{TestApp, TestUser};
use common::*;
use serde_json::{Value, json};

async fn setup(files: &[(&str, &str)]) -> (TestApp, TestUser, WorkingCopy, i64) {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    app.create_repo(&alice, "demo").await;
    let wc = WorkingCopy::new(&app, &alice, "alice", "demo").await;
    let mut all: Vec<(&str, &str)> = vec![("README.md", "# demo\n")];
    all.extend_from_slice(files);
    wc.commit(&all, "workflows").await;
    wc.push("main").await;
    settle(&app).await;
    let repo_id: i64 = sqlx::query_scalar("SELECT id FROM repositories WHERE name = 'demo'")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    (app, alice, wc, repo_id)
}

/// Runs of `event` (newest first).
async fn runs_of(app: &TestApp, user: &TestUser, event: &str) -> Vec<Value> {
    runs(app, user, "alice/demo")
        .await
        .into_iter()
        .filter(|r| r["event"] == event)
        .collect()
}

async fn payload_of(app: &TestApp, run_id: i64) -> Value {
    sqlx::query_scalar("SELECT event_payload FROM actions_runs WHERE id = $1")
        .bind(run_id)
        .fetch_one(&app.state.db)
        .await
        .unwrap()
}

fn wf(on: &str) -> String {
    format!("on:\n{on}\njobs:\n  j:\n    runs-on: x\n    steps: [{{run: echo}}]\n")
}

#[tokio::test]
async fn repository_dispatch_endpoint_starts_matching_workflows() {
    let deploy = wf("  repository_dispatch:\n    types: [deploy]");
    let any = wf("  repository_dispatch:");
    let (app, alice, _wc, _) = setup(&[
        (".github/workflows/deploy.yml", &deploy),
        (".github/workflows/any.yml", &any),
    ])
    .await;

    let res = app
        .post("/api/v3/repos/alice/demo/dispatches")
        .auth(&alice)
        .json(&json!({"event_type": "deploy", "client_payload": {"env": "prod", "n": 1}}))
        .send()
        .await;
    res.assert_status(204);
    assert_eq!(res.text(), "");
    settle(&app).await;
    let rs = runs_of(&app, &alice, "repository_dispatch").await;
    assert_eq!(rs.len(), 2);
    assert!(rs.iter().all(|r| r["head_branch"] == "main"));
    let p = payload_of(&app, rs[0]["id"].as_i64().unwrap()).await;
    assert_eq!(p["action"], "deploy");
    assert_eq!(p["branch"], "main");
    assert_eq!(p["client_payload"], json!({"env": "prod", "n": 1}));
    assert_eq!(p["sender"]["login"], "alice");

    // types filter: only the catch-all workflow runs.
    app.post("/api/v3/repos/alice/demo/dispatches")
        .auth(&alice)
        .json(&json!({"event_type": "other"}))
        .send()
        .await
        .assert_status(204);
    settle(&app).await;
    let rs = runs_of(&app, &alice, "repository_dispatch").await;
    assert_eq!(rs.len(), 3);
    assert_eq!(rs[0]["path"], ".github/workflows/any.yml");
    assert_eq!(
        payload_of(&app, rs[0]["id"].as_i64().unwrap()).await["client_payload"],
        json!({})
    );

    // Validation (422, GitHub error shape).
    let res = app
        .post("/api/v3/repos/alice/demo/dispatches")
        .auth(&alice)
        .json(&json!({"client_payload": {}}))
        .send()
        .await;
    res.assert_status(422);
    assert!(
        res.json()["message"]
            .as_str()
            .unwrap()
            .contains("event_type")
    );
    let big: serde_json::Map<String, Value> =
        (0..11).map(|i| (format!("k{i}"), json!(i))).collect();
    app.post("/api/v3/repos/alice/demo/dispatches")
        .auth(&alice)
        .json(&json!({"event_type": "deploy", "client_payload": big}))
        .send()
        .await
        .assert_status(422);
    app.post("/api/v3/repos/alice/demo/dispatches")
        .auth(&alice)
        .json(&json!({"event_type": "x".repeat(101)}))
        .send()
        .await
        .assert_status(422);
    // Read access is not enough; anonymous needs auth.
    let bob = app.create_user("bob").await;
    app.post("/api/v3/repos/alice/demo/dispatches")
        .auth(&bob)
        .json(&json!({"event_type": "deploy"}))
        .send()
        .await
        .assert_status(403);
    app.post("/api/v3/repos/alice/demo/dispatches")
        .json(&json!({"event_type": "deploy"}))
        .send()
        .await
        .assert_status(401);
    settle(&app).await;
    assert_eq!(runs_of(&app, &alice, "repository_dispatch").await.len(), 3);
}

#[tokio::test]
async fn workflow_run_runs_after_ci_completes() {
    let ci = "name: CI\non: push\njobs:\n  t:\n    runs-on: x\n    steps: [{run: echo}]\n";
    let after =
        wf("  workflow_run:\n    workflows: [CI]\n    types: [completed]\n    branches: [main]");
    let requested = wf("  workflow_run:\n    workflows: [CI]\n    types: [requested]");
    let (app, alice, _wc, _) = setup(&[
        (".github/workflows/ci.yml", ci),
        (".github/workflows/after.yml", &after),
        (".github/workflows/requested.yml", &requested),
    ])
    .await;
    let ci_run = runs_of(&app, &alice, "push").await[0].clone();
    // `requested` fired at once; `completed` waits for CI.
    let wr = runs_of(&app, &alice, "workflow_run").await;
    assert_eq!(wr.len(), 1);
    assert_eq!(wr[0]["path"], ".github/workflows/requested.yml");

    let runner = FakeRunner::register(&app, &alice, "alice/demo", &["x"]).await;
    let job = runner.acquire(&app).await.unwrap();
    runner
        .complete(&app, job["job_id"].as_i64().unwrap(), "success", json!({}))
        .await;
    settle(&app).await;
    let wr = runs_of(&app, &alice, "workflow_run").await;
    assert_eq!(wr.len(), 2);
    let after_run = wr
        .iter()
        .find(|r| r["path"] == ".github/workflows/after.yml")
        .unwrap();
    assert_eq!(after_run["head_branch"], "main");
    let p = payload_of(&app, after_run["id"].as_i64().unwrap()).await;
    assert_eq!(p["action"], "completed");
    assert_eq!(p["workflow_run"]["id"], ci_run["id"]);
    assert_eq!(p["workflow_run"]["conclusion"], "success");
    assert_eq!(p["workflow"]["name"], "CI");
}

#[tokio::test]
async fn workflow_run_chains_stop_after_three_levels() {
    // A workflow listening to itself would loop forever without the limit.
    let a = "name: A\non: push\njobs:\n  t:\n    runs-on: x\n    steps: [{run: echo}]\n";
    let b = "name: B\non:\n  workflow_run:\n    workflows: [A, B]\n    types: [requested]\njobs:\n  t:\n    runs-on: x\n    steps: [{run: echo}]\n";
    let (app, alice, _wc, _) = setup(&[
        (".github/workflows/a.yml", a),
        (".github/workflows/b.yml", b),
    ])
    .await;
    // A (push) → B (level 2) → B (level 3), then the chain stops.
    assert_eq!(runs_of(&app, &alice, "workflow_run").await.len(), 2);
}

/// Open a PR from `feature` through the pulls API.
async fn open_pr(app: &TestApp, alice: &TestUser) -> Value {
    let res = app
        .post("/api/v3/repos/alice/demo/pulls")
        .auth(alice)
        .json(&json!({"title": "My PR", "head": "feature", "base": "main"}))
        .send()
        .await;
    res.assert_status(201);
    settle(app).await;
    res.json()
}

#[tokio::test]
async fn pull_request_activity_types_reviews_and_merge_ref() {
    let pr_wf = wf(
        "  pull_request:\n    types: [labeled, ready_for_review, edited, review_requested, opened]",
    );
    let review_wf =
        wf("  pull_request_review:\n    types: [submitted]\n  pull_request_review_comment:");
    let target_wf = wf("  pull_request_target:\n    types: [labeled]");
    let issues_wf = wf("  issues:\n    types: [labeled]");
    let (app, alice, wc, repo_id) = setup(&[
        (".github/workflows/pr.yml", &pr_wf),
        (".github/workflows/review.yml", &review_wf),
        (".github/workflows/target.yml", &target_wf),
        (".github/workflows/issues.yml", &issues_wf),
    ])
    .await;
    let base = git(&wc.path, &["rev-parse", "HEAD"]).await;
    wc.checkout_new("feature").await;
    let head = wc.commit(&[("f.txt", "x")], "feature").await;
    wc.push("feature").await;
    settle(&app).await;
    let pr = open_pr(&app, &alice).await;
    let number = pr["number"].as_i64().unwrap();

    let opened = runs_of(&app, &alice, "pull_request").await;
    assert_eq!(opened.len(), 1);
    // The run stays on the PR head (its checks belong to the PR); GITHUB_SHA
    // is the test merge commit (parents base, head) at refs/pull/N/merge.
    assert_eq!(opened[0]["head_sha"], head);
    let runner = FakeRunner::register(&app, &alice, "alice/demo", &["x"]).await;
    let spec = runner.acquire(&app).await.unwrap();
    let merge_sha = spec["github"]["sha"].as_str().unwrap().to_string();
    assert_ne!(merge_sha, head);
    let m = merge_sha.clone();
    let commit = bgh_actions::trigger::store(&app.state)
        .read(repo_id, move |r| r.commit(&m))
        .await
        .unwrap();
    assert_eq!(commit.parents, [base.clone(), head.clone()]);
    assert_eq!(spec["github"]["ref"], format!("refs/pull/{number}/merge"));
    assert_eq!(
        spec["github"]["event"]["pull_request"]["merge_commit_sha"],
        merge_sha
    );
    let r = format!("refs/pull/{number}/merge");
    let merge_ref = bgh_actions::trigger::store(&app.state)
        .read(repo_id, move |g| g.resolve(&r))
        .await
        .unwrap();
    assert_eq!(merge_ref.as_deref(), Some(merge_sha.as_str()));
    // The run's check suite is on the PR head, so the PR's checks see it.
    let checks = app
        .get(&format!(
            "/api/v3/repos/alice/demo/commits/{head}/check-runs"
        ))
        .auth(&alice)
        .send()
        .await
        .json();
    assert_eq!(checks["total_count"], 1);
    runner
        .complete(&app, spec["job_id"].as_i64().unwrap(), "success", json!({}))
        .await;

    // labeled → pull_request + pull_request_target, never `issues`.
    app.post("/api/v3/repos/alice/demo/labels")
        .auth(&alice)
        .json(&json!({"name": "needs-ci", "color": "ff0000"}))
        .send()
        .await
        .assert_status(201);
    app.post(&format!("/api/v3/repos/alice/demo/issues/{number}/labels"))
        .auth(&alice)
        .json(&json!({"labels": ["needs-ci"]}))
        .send()
        .await
        .assert_status(200);
    settle(&app).await;
    let prs = runs_of(&app, &alice, "pull_request").await;
    assert_eq!(prs.len(), 2);
    let p = payload_of(&app, prs[0]["id"].as_i64().unwrap()).await;
    assert_eq!(p["action"], "labeled");
    assert_eq!(p["label"]["name"], "needs-ci");
    assert_eq!(p["pull_request"]["number"], number);
    assert_eq!(runs_of(&app, &alice, "pull_request_target").await.len(), 1);
    assert_eq!(runs_of(&app, &alice, "issues").await.len(), 0);

    // edited (title) → payload carries changes.
    app.patch(&format!("/api/v3/repos/alice/demo/pulls/{number}"))
        .auth(&alice)
        .json(&json!({"title": "Renamed"}))
        .send()
        .await
        .assert_status(200);
    settle(&app).await;
    let prs = runs_of(&app, &alice, "pull_request").await;
    assert_eq!(prs.len(), 3);
    let p = payload_of(&app, prs[0]["id"].as_i64().unwrap()).await;
    assert_eq!(p["action"], "edited");
    assert_eq!(p["changes"]["title"]["from"], "My PR");

    // review_requested.
    let bob = app.create_user("bob").await;
    app.put("/api/v3/repos/alice/demo/collaborators/bob")
        .auth(&alice)
        .json(&json!({"permission": "push"}))
        .send()
        .await;
    sqlx::query(
        "INSERT INTO collaborators (repo_id, user_id, permission)
         VALUES ($1, $2, 'write') ON CONFLICT DO NOTHING",
    )
    .bind(repo_id)
    .bind(bob.id)
    .execute(&app.state.db)
    .await
    .ok();
    let res = app
        .post(&format!(
            "/api/v3/repos/alice/demo/pulls/{number}/requested_reviewers"
        ))
        .auth(&alice)
        .json(&json!({"reviewers": ["bob"]}))
        .send()
        .await;
    assert!(res.status() == 201, "{}", res.text());
    settle(&app).await;
    let prs = runs_of(&app, &alice, "pull_request").await;
    let p = payload_of(&app, prs[0]["id"].as_i64().unwrap()).await;
    assert_eq!(p["action"], "review_requested");
    assert_eq!(p["requested_reviewer"]["login"], "bob");

    // pull_request_review submitted, on the merge ref.
    app.post(&format!("/api/v3/repos/alice/demo/pulls/{number}/reviews"))
        .auth(&bob)
        .json(&json!({"event": "COMMENT", "body": "looks fine"}))
        .send()
        .await
        .assert_status(200);
    settle(&app).await;
    let reviews = runs_of(&app, &alice, "pull_request_review").await;
    assert_eq!(reviews.len(), 1);
    assert_eq!(reviews[0]["head_sha"], head);
    let p = payload_of(&app, reviews[0]["id"].as_i64().unwrap()).await;
    assert_eq!(p["action"], "submitted");
    assert_eq!(p["review"]["body"], "looks fine");
    assert_eq!(p["review"]["user"]["login"], "bob");

    // pull_request_review_comment created.
    app.post(&format!("/api/v3/repos/alice/demo/pulls/{number}/comments"))
        .auth(&bob)
        .json(
            &json!({"body": "nit", "commit_id": head, "path": "f.txt", "line": 1, "side": "RIGHT"}),
        )
        .send()
        .await
        .assert_status(201);
    settle(&app).await;
    let rc = runs_of(&app, &alice, "pull_request_review_comment").await;
    assert_eq!(rc.len(), 1);
    let p = payload_of(&app, rc[0]["id"].as_i64().unwrap()).await;
    assert_eq!(p["comment"]["body"], "nit");
    assert_eq!(p["comment"]["path"], "f.txt");
}

#[tokio::test]
async fn conflicting_pull_request_does_not_run() {
    let pr_wf = wf("  pull_request:");
    let target_wf = wf("  pull_request_target:");
    let (app, alice, wc, repo_id) = setup(&[
        (".github/workflows/pr.yml", &pr_wf),
        (".github/workflows/target.yml", &target_wf),
        ("c.txt", "base\n"),
    ])
    .await;
    wc.checkout_new("feature").await;
    wc.commit(&[("c.txt", "feature\n")], "feature").await;
    wc.push("feature").await;
    git(&wc.path, &["checkout", "-q", "main"]).await;
    wc.commit(&[("c.txt", "main\n")], "main moves").await;
    wc.push("main").await;
    settle(&app).await;
    let pr = open_pr(&app, &alice).await;
    let number = pr["number"].as_i64().unwrap();
    // No test merge commit for a conflicting PR: no pull_request run, but
    // pull_request_target (base branch) still runs, as on GitHub.
    assert_eq!(runs_of(&app, &alice, "pull_request").await.len(), 0);
    assert_eq!(runs_of(&app, &alice, "pull_request_target").await.len(), 1);
    let r = format!("refs/pull/{number}/merge");
    let merge_ref = bgh_actions::trigger::store(&app.state)
        .read(repo_id, move |g| g.resolve(&r))
        .await
        .unwrap();
    assert_eq!(merge_ref, None);
}

#[tokio::test]
async fn create_and_delete_events() {
    let create = wf("  create:");
    let delete = wf("  delete:");
    let (app, alice, wc, _) = setup(&[
        (".github/workflows/create.yml", &create),
        (".github/workflows/delete.yml", &delete),
    ])
    .await;
    // The first push created `main`.
    let c = runs_of(&app, &alice, "create").await;
    assert_eq!(c.len(), 1);
    assert_eq!(c[0]["head_branch"], "main");
    wc.checkout_new("topic").await;
    wc.push("topic").await;
    settle(&app).await;
    let c = runs_of(&app, &alice, "create").await;
    assert_eq!(c.len(), 2);
    assert_eq!(c[0]["head_branch"], "topic");
    let p = payload_of(&app, c[0]["id"].as_i64().unwrap()).await;
    assert_eq!(p["ref"], "topic");
    assert_eq!(p["ref_type"], "branch");
    assert_eq!(p["master_branch"], "main");

    wc.tag("v1").await;
    wc.push("v1").await;
    settle(&app).await;
    let c = runs_of(&app, &alice, "create").await;
    assert_eq!(c.len(), 3);
    let p = payload_of(&app, c[0]["id"].as_i64().unwrap()).await;
    assert_eq!(p["ref_type"], "tag");

    wc.push(":topic").await;
    settle(&app).await;
    let d = runs_of(&app, &alice, "delete").await;
    assert_eq!(d.len(), 1);
    // `delete` runs on the default branch.
    assert_eq!(d[0]["head_branch"], "main");
    let p = payload_of(&app, d[0]["id"].as_i64().unwrap()).await;
    assert_eq!(p["ref"], "topic");
    assert_eq!(p["ref_type"], "branch");
}

#[tokio::test]
async fn issues_and_issue_comment_activity_types() {
    let issues = wf("  issues:\n    types: [labeled, assigned, milestoned, pinned]");
    let comments = wf("  issue_comment:\n    types: [edited, deleted]");
    let (app, alice, _wc, repo_id) = setup(&[
        (".github/workflows/issues.yml", &issues),
        (".github/workflows/comments.yml", &comments),
    ])
    .await;
    let res = app
        .post("/api/v3/repos/alice/demo/issues")
        .auth(&alice)
        .json(&json!({"title": "Bug"}))
        .send()
        .await;
    res.assert_status(201);
    let number = res.json()["number"].as_i64().unwrap();
    settle(&app).await;
    assert_eq!(runs_of(&app, &alice, "issues").await.len(), 0);

    app.post(&format!("/api/v3/repos/alice/demo/issues/{number}/labels"))
        .auth(&alice)
        .json(&json!({"labels": ["bug"]}))
        .send()
        .await;
    settle(&app).await;
    let rs = runs_of(&app, &alice, "issues").await;
    assert_eq!(rs.len(), 1);
    let p = payload_of(&app, rs[0]["id"].as_i64().unwrap()).await;
    assert_eq!(p["action"], "labeled");
    assert_eq!(p["label"]["name"], "bug");
    assert_eq!(p["issue"]["number"], number);

    app.post(&format!(
        "/api/v3/repos/alice/demo/issues/{number}/assignees"
    ))
    .auth(&alice)
    .json(&json!({"assignees": ["alice"]}))
    .send()
    .await;
    settle(&app).await;
    let rs = runs_of(&app, &alice, "issues").await;
    assert_eq!(rs.len(), 2);
    let p = payload_of(&app, rs[0]["id"].as_i64().unwrap()).await;
    assert_eq!(p["action"], "assigned");
    assert_eq!(p["assignee"]["login"], "alice");

    let ms = app
        .post("/api/v3/repos/alice/demo/milestones")
        .auth(&alice)
        .json(&json!({"title": "v1"}))
        .send()
        .await;
    ms.assert_status(201);
    app.patch(&format!("/api/v3/repos/alice/demo/issues/{number}"))
        .auth(&alice)
        .json(&json!({"milestone": ms.json()["number"]}))
        .send()
        .await
        .assert_status(200);
    settle(&app).await;
    let rs = runs_of(&app, &alice, "issues").await;
    assert_eq!(rs.len(), 3);
    let p = payload_of(&app, rs[0]["id"].as_i64().unwrap()).await;
    assert_eq!(p["action"], "milestoned");
    assert_eq!(p["milestone"]["title"], "v1");

    // pinned (domain event).
    let issue_id: i64 =
        sqlx::query_scalar("SELECT id FROM issues WHERE repo_id = $1 AND number = $2")
            .bind(repo_id)
            .bind(number)
            .fetch_one(&app.state.db)
            .await
            .unwrap();
    app.state.events.emit(Event::IssuePinned {
        repo_id,
        issue_id,
        actor_id: alice.id,
    });
    settle(&app).await;
    assert_eq!(runs_of(&app, &alice, "issues").await.len(), 4);

    // issue_comment edited / deleted (created is not in types).
    let c = app
        .post(&format!(
            "/api/v3/repos/alice/demo/issues/{number}/comments"
        ))
        .auth(&alice)
        .json(&json!({"body": "first"}))
        .send()
        .await;
    c.assert_status(201);
    let cid = c.json()["id"].as_i64().unwrap();
    settle(&app).await;
    assert_eq!(runs_of(&app, &alice, "issue_comment").await.len(), 0);
    app.patch(&format!("/api/v3/repos/alice/demo/issues/comments/{cid}"))
        .auth(&alice)
        .json(&json!({"body": "second"}))
        .send()
        .await
        .assert_status(200);
    settle(&app).await;
    let rs = runs_of(&app, &alice, "issue_comment").await;
    assert_eq!(rs.len(), 1);
    let p = payload_of(&app, rs[0]["id"].as_i64().unwrap()).await;
    assert_eq!(p["action"], "edited");
    assert_eq!(p["comment"]["body"], "second");
    assert_eq!(p["changes"]["body"]["from"], "first");
    app.delete(&format!("/api/v3/repos/alice/demo/issues/comments/{cid}"))
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    settle(&app).await;
    let rs = runs_of(&app, &alice, "issue_comment").await;
    assert_eq!(rs.len(), 2);
    let p = payload_of(&app, rs[0]["id"].as_i64().unwrap()).await;
    assert_eq!(p["action"], "deleted");
    assert_eq!(p["comment"]["id"], cid);
}

/// Regression for #343: a direct `EventBus::emit` reaches the outbox
/// through an asynchronous writer, so `settle` must wait for it (and for
/// the listeners) rather than for a quiet job queue. The outbox is locked
/// while settling so the writer cannot append before `settle` checks.
#[tokio::test]
async fn settle_waits_for_events_emitted_outside_a_tx() {
    let issues = wf("  issues:\n    types: [pinned]");
    let (app, alice, _wc, repo_id) = setup(&[(".github/workflows/issues.yml", &issues)]).await;
    let res = app
        .post("/api/v3/repos/alice/demo/issues")
        .auth(&alice)
        .json(&json!({"title": "Bug"}))
        .send()
        .await;
    res.assert_status(201);
    let number = res.json()["number"].as_i64().unwrap();
    settle(&app).await;
    assert_eq!(runs_of(&app, &alice, "issues").await.len(), 0);
    let issue_id: i64 =
        sqlx::query_scalar("SELECT id FROM issues WHERE repo_id = $1 AND number = $2")
            .bind(repo_id)
            .bind(number)
            .fetch_one(&app.state.db)
            .await
            .unwrap();

    // Hold off outbox inserts (reads still proceed) until well after a
    // settle that only watched the job queue would have returned.
    let mut lock = app.state.db.begin().await.unwrap();
    sqlx::query("LOCK TABLE event_outbox IN EXCLUSIVE MODE")
        .execute(&mut *lock)
        .await
        .unwrap();
    app.state.events.emit(Event::IssuePinned {
        repo_id,
        issue_id,
        actor_id: alice.id,
    });
    let release = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        lock.rollback().await.unwrap();
    });
    settle(&app).await;
    release.await.unwrap();
    let rs = runs_of(&app, &alice, "issues").await;
    assert_eq!(rs.len(), 1);
    let p = payload_of(&app, rs[0]["id"].as_i64().unwrap()).await;
    assert_eq!(p["action"], "pinned");
}

#[tokio::test]
async fn release_activity_types() {
    let release = wf("  release:\n    types: [created, published, edited, deleted, prereleased]");
    let (app, alice, wc, _) = setup(&[(".github/workflows/release.yml", &release)]).await;
    wc.tag("v1.0").await;
    wc.push("v1.0").await;
    settle(&app).await;
    let res = app
        .post("/api/v3/repos/alice/demo/releases")
        .auth(&alice)
        .json(&json!({"tag_name": "v1.0", "name": "One"}))
        .send()
        .await;
    res.assert_status(201);
    let id = res.json()["id"].as_i64().unwrap();
    settle(&app).await;
    let mut actions: Vec<String> = Vec::new();
    for r in runs_of(&app, &alice, "release").await {
        actions.push(
            payload_of(&app, r["id"].as_i64().unwrap()).await["action"]
                .as_str()
                .unwrap()
                .to_string(),
        );
    }
    actions.sort();
    assert_eq!(actions, ["created", "published"]);

    app.patch(&format!("/api/v3/repos/alice/demo/releases/{id}"))
        .auth(&alice)
        .json(&json!({"name": "Uno", "prerelease": true}))
        .send()
        .await
        .assert_status(200);
    settle(&app).await;
    assert_eq!(runs_of(&app, &alice, "release").await.len(), 4);

    app.delete(&format!("/api/v3/repos/alice/demo/releases/{id}"))
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    settle(&app).await;
    let rs = runs_of(&app, &alice, "release").await;
    assert_eq!(rs.len(), 5);
    let p = payload_of(&app, rs[0]["id"].as_i64().unwrap()).await;
    assert_eq!(p["action"], "deleted");
    assert_eq!(p["release"]["tag_name"], "v1.0");
    assert_eq!(rs[0]["head_branch"], "v1.0");

    // Drafts don't trigger created.
    app.post("/api/v3/repos/alice/demo/releases")
        .auth(&alice)
        .json(&json!({"tag_name": "v2.0", "draft": true}))
        .send()
        .await
        .assert_status(201);
    settle(&app).await;
    assert_eq!(runs_of(&app, &alice, "release").await.len(), 5);
}

#[tokio::test]
async fn repository_events_label_milestone_watch_fork_public_gollum() {
    let files = [
        ("label", wf("  label:\n    types: [created]")),
        ("milestone", wf("  milestone:\n    types: [closed]")),
        ("watch", wf("  watch:\n    types: [started]")),
        ("fork", wf("  fork:")),
        ("public", wf("  public:")),
        ("gollum", wf("  gollum:")),
    ];
    let paths: Vec<(String, String)> = files
        .iter()
        .map(|(n, w)| (format!(".github/workflows/{n}.yml"), w.clone()))
        .collect();
    let refs: Vec<(&str, &str)> = paths
        .iter()
        .map(|(p, w)| (p.as_str(), w.as_str()))
        .collect();
    let (app, alice, _wc, repo_id) = setup(&refs).await;

    app.post("/api/v3/repos/alice/demo/labels")
        .auth(&alice)
        .json(&json!({"name": "triage", "color": "00ff00"}))
        .send()
        .await
        .assert_status(201);
    settle(&app).await;
    let rs = runs_of(&app, &alice, "label").await;
    assert_eq!(rs.len(), 1);
    let p = payload_of(&app, rs[0]["id"].as_i64().unwrap()).await;
    assert_eq!(p["action"], "created");
    assert_eq!(p["label"]["name"], "triage");

    let ms = app
        .post("/api/v3/repos/alice/demo/milestones")
        .auth(&alice)
        .json(&json!({"title": "M1"}))
        .send()
        .await;
    let n = ms.json()["number"].as_i64().unwrap();
    settle(&app).await;
    assert_eq!(runs_of(&app, &alice, "milestone").await.len(), 0);
    app.patch(&format!("/api/v3/repos/alice/demo/milestones/{n}"))
        .auth(&alice)
        .json(&json!({"state": "closed"}))
        .send()
        .await
        .assert_status(200);
    settle(&app).await;
    let rs = runs_of(&app, &alice, "milestone").await;
    assert_eq!(rs.len(), 1);
    let p = payload_of(&app, rs[0]["id"].as_i64().unwrap()).await;
    assert_eq!(p["milestone"]["title"], "M1");

    let bob = app.create_user("bob").await;
    app.put("/api/v3/user/starred/alice/demo")
        .auth(&bob)
        .send()
        .await
        .assert_status(204);
    settle(&app).await;
    let rs = runs_of(&app, &alice, "watch").await;
    assert_eq!(rs.len(), 1);
    let p = payload_of(&app, rs[0]["id"].as_i64().unwrap()).await;
    assert_eq!(p["action"], "started");
    assert_eq!(p["sender"]["login"], "bob");

    let res = app
        .post("/api/v3/repos/alice/demo/forks")
        .auth(&bob)
        .json(&json!({}))
        .send()
        .await;
    res.assert_status(202);
    settle(&app).await;
    let rs = runs_of(&app, &alice, "fork").await;
    assert_eq!(rs.len(), 1);
    let p = payload_of(&app, rs[0]["id"].as_i64().unwrap()).await;
    assert_eq!(p["forkee"]["full_name"], "bob/demo");

    app.state.events.emit(Event::RepositoryPublicized {
        repo_id,
        actor_id: alice.id,
    });
    app.state.events.emit(Event::WikiPagesUpdated {
        repo_id,
        actor_id: alice.id,
        pages: json!([{"page_name": "Home", "action": "created"}]),
    });
    settle(&app).await;
    assert_eq!(runs_of(&app, &alice, "public").await.len(), 1);
    let rs = runs_of(&app, &alice, "gollum").await;
    assert_eq!(rs.len(), 1);
    let p = payload_of(&app, rs[0]["id"].as_i64().unwrap()).await;
    assert_eq!(p["pages"][0]["page_name"], "Home");
}

#[tokio::test]
async fn check_run_and_check_suite_from_other_integrations() {
    let checks = wf("  check_run:\n    types: [completed]\n  check_suite:\n    types: [completed]");
    let (app, alice, wc, repo_id) = setup(&[(".github/workflows/checks.yml", &checks)]).await;
    let sha = git(&wc.path, &["rev-parse", "HEAD"]).await;
    let suite: i64 = sqlx::query_scalar(
        "INSERT INTO check_suites (repo_id, head_sha, head_branch, app_slug, status, conclusion)
         VALUES ($1, $2, 'main', 'external-ci', 'completed', 'success') RETURNING id",
    )
    .bind(repo_id)
    .bind(&sha)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    let check: i64 = sqlx::query_scalar(
        "INSERT INTO check_runs (check_suite_id, repo_id, head_sha, name, status, conclusion)
         VALUES ($1, $2, $3, 'lint', 'completed', 'success') RETURNING id",
    )
    .bind(suite)
    .bind(repo_id)
    .bind(&sha)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    app.state.events.emit(Event::CheckRunCompleted {
        repo_id,
        check_run_id: check,
        actor_id: None,
    });
    app.state.events.emit(Event::CheckSuiteCompleted {
        repo_id,
        check_suite_id: suite,
    });
    settle(&app).await;
    let rs = runs_of(&app, &alice, "check_run").await;
    assert_eq!(rs.len(), 1);
    let p = payload_of(&app, rs[0]["id"].as_i64().unwrap()).await;
    assert_eq!(p["check_run"]["name"], "lint");
    assert_eq!(p["check_run"]["conclusion"], "success");
    let rs = runs_of(&app, &alice, "check_suite").await;
    assert_eq!(rs.len(), 1);

    // Actions' own checks never trigger check_run / check_suite workflows.
    let own: (i64, i64) = sqlx::query_as(
        "SELECT r.id, r.check_suite_id FROM check_runs r JOIN check_suites s ON s.id = r.check_suite_id
          WHERE s.app_slug = 'actions' LIMIT 1",
    )
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    app.state.events.emit(Event::CheckRunCompleted {
        repo_id,
        check_run_id: own.0,
        actor_id: None,
    });
    app.state.events.emit(Event::CheckSuiteCompleted {
        repo_id,
        check_suite_id: own.1,
    });
    settle(&app).await;
    assert_eq!(runs_of(&app, &alice, "check_run").await.len(), 1);
    assert_eq!(runs_of(&app, &alice, "check_suite").await.len(), 1);
}

async fn check_json(app: &TestApp, user: &TestUser, id: i64) -> Value {
    app.get(&format!("/api/v3/repos/alice/demo/check-runs/{id}"))
        .auth(user)
        .send()
        .await
        .json()
}

#[tokio::test]
async fn rerequesting_a_check_reruns_the_job() {
    let ci = "name: CI\non: push\njobs:\n  a:\n    runs-on: x\n    steps: [{run: echo}]\n  b:\n    runs-on: x\n    steps: [{run: echo}]\n";
    let (app, alice, _wc, _) = setup(&[(".github/workflows/ci.yml", ci)]).await;
    let run_id = runs_of(&app, &alice, "push").await[0]["id"]
        .as_i64()
        .unwrap();
    let runner = FakeRunner::register(&app, &alice, "alice/demo", &["x"]).await;
    while let Some(job) = runner.acquire(&app).await {
        let concl = if job["job_key"] == "a" {
            "failure"
        } else {
            "success"
        };
        runner
            .complete(&app, job["job_id"].as_i64().unwrap(), concl, json!({}))
            .await;
    }
    settle(&app).await;
    let jobs1 = jobs(&app, &alice, "alice/demo", run_id).await;
    let a = jobs1.iter().find(|j| j["name"] == "a").unwrap();
    let check_id = a["check_run_url"]
        .as_str()
        .unwrap()
        .rsplit('/')
        .next()
        .unwrap()
        .parse::<i64>()
        .unwrap();
    assert_eq!(
        check_json(&app, &alice, check_id).await["conclusion"],
        "failure"
    );

    app.post(&format!(
        "/api/v3/repos/alice/demo/check-runs/{check_id}/rerequest"
    ))
    .auth(&alice)
    .send()
    .await
    .assert_status(201);
    settle(&app).await;
    // Re-run as attempt 2: the same check run is queued again.
    let r = run(&app, &alice, "alice/demo", run_id).await;
    assert_eq!(r["run_attempt"], 2);
    assert_eq!(check_json(&app, &alice, check_id).await["status"], "queued");
    let job = runner.acquire(&app).await.expect("job a re-queued");
    assert_eq!(job["job_key"], "a");
    assert!(runner.acquire(&app).await.is_none(), "b is not re-run");
    runner
        .complete(&app, job["job_id"].as_i64().unwrap(), "success", json!({}))
        .await;
    settle(&app).await;
    let c = check_json(&app, &alice, check_id).await;
    assert_eq!(c["status"], "completed");
    assert_eq!(c["conclusion"], "success");
    let r = run(&app, &alice, "alice/demo", run_id).await;
    assert_eq!(r["status"], "completed");
    assert_eq!(r["conclusion"], "success");

    // Re-requesting the suite re-runs the whole run.
    let suite = r["check_suite_id"].as_i64().unwrap();
    app.post(&format!(
        "/api/v3/repos/alice/demo/check-suites/{suite}/rerequest"
    ))
    .auth(&alice)
    .send()
    .await
    .assert_status(201);
    settle(&app).await;
    let r = run(&app, &alice, "alice/demo", run_id).await;
    assert_eq!(r["run_attempt"], 3);
    let mut keys = vec![];
    while let Some(job) = runner.acquire(&app).await {
        keys.push(job["job_key"].as_str().unwrap().to_string());
        runner
            .complete(&app, job["job_id"].as_i64().unwrap(), "success", json!({}))
            .await;
    }
    keys.sort();
    assert_eq!(keys, ["a", "b"]);
    settle(&app).await;
    let suite_json = app
        .get(&format!("/api/v3/repos/alice/demo/check-suites/{suite}"))
        .auth(&alice)
        .send()
        .await
        .json();
    assert_eq!(suite_json["status"], "completed");
    assert_eq!(suite_json["conclusion"], "success");
}

#[tokio::test]
async fn job_concurrency_waits_and_cancels() {
    let deploy = "on: [push, workflow_dispatch]\njobs:\n  deploy:\n    runs-on: x\n    concurrency: production\n    steps: [{run: echo}]\n";
    let urgent = "on: workflow_dispatch\njobs:\n  deploy:\n    runs-on: x\n    concurrency:\n      group: production\n      cancel-in-progress: true\n    steps: [{run: echo}]\n";
    let (app, alice, _wc, _) = setup(&[
        (".github/workflows/deploy.yml", deploy),
        (".github/workflows/urgent.yml", urgent),
    ])
    .await;
    let runner = FakeRunner::register(&app, &alice, "alice/demo", &["x"]).await;
    let first = runner.acquire(&app).await.unwrap();
    let dispatch = |file: &'static str| {
        let app = &app;
        let alice = &alice;
        async move {
            app.post(&format!(
                "/api/v3/repos/alice/demo/actions/workflows/{file}/dispatches"
            ))
            .auth(alice)
            .json(&json!({"ref": "main"}))
            .send()
            .await
            .assert_status(204);
            settle(app).await;
        }
    };
    // Second run's job waits for the group.
    dispatch("deploy.yml").await;
    let second_run = runs_of(&app, &alice, "workflow_dispatch").await[0]["id"]
        .as_i64()
        .unwrap();
    let j2 = jobs(&app, &alice, "alice/demo", second_run).await;
    assert_eq!(j2[0]["status"], "pending");
    assert!(runner.acquire(&app).await.is_none());

    // A third waiting job replaces (cancels) the second.
    dispatch("deploy.yml").await;
    let third_run = runs_of(&app, &alice, "workflow_dispatch").await[0]["id"]
        .as_i64()
        .unwrap();
    let r2 = run(&app, &alice, "alice/demo", second_run).await;
    assert_eq!(r2["conclusion"], "cancelled");
    assert_eq!(
        jobs(&app, &alice, "alice/demo", third_run).await[0]["status"],
        "pending"
    );

    // The first job finishing starts the waiting one.
    runner
        .complete(
            &app,
            first["job_id"].as_i64().unwrap(),
            "success",
            json!({}),
        )
        .await;
    settle(&app).await;
    let next = runner.acquire(&app).await.expect("waiting job starts");
    assert_eq!(next["run_id"], third_run);

    // cancel-in-progress cancels the running job; the new one then runs.
    dispatch("urgent.yml").await;
    let urgent_run = runs_of(&app, &alice, "workflow_dispatch").await[0]["id"]
        .as_i64()
        .unwrap();
    let hb = runner
        .steps(&app, next["job_id"].as_i64().unwrap(), json!([]))
        .await;
    assert_eq!(hb["cancel"], true, "{hb}");
    runner
        .complete(
            &app,
            next["job_id"].as_i64().unwrap(),
            "cancelled",
            json!({}),
        )
        .await;
    settle(&app).await;
    let last = runner.acquire(&app).await.expect("urgent job starts");
    assert_eq!(last["run_id"], urgent_run);
    assert_eq!(
        run(&app, &alice, "alice/demo", third_run).await["conclusion"],
        "cancelled"
    );
}

#[tokio::test]
async fn badge_svg_reflects_latest_run() {
    let ci = "name: CI\non: push\njobs:\n  t:\n    runs-on: x\n    steps: [{run: echo}]\n";
    let (app, alice, wc, _) = setup(&[(".github/workflows/ci.yml", ci)]).await;
    let badge = |q: &'static str| {
        let app = &app;
        async move {
            let res = app
                .get(&format!(
                    "/alice/demo/actions/workflows/ci.yml/badge.svg{q}"
                ))
                .send()
                .await;
            res.assert_status(200);
            assert!(
                res.header("content-type")
                    .unwrap()
                    .starts_with("image/svg+xml")
            );
            assert!(res.header("cache-control").unwrap().contains("no-cache"));
            res.text()
        }
    };
    // Queued only: no status yet.
    let svg = badge("").await;
    assert!(svg.starts_with("<svg"), "{svg}");
    assert!(svg.contains("CI: no status"), "{svg}");

    let runner = FakeRunner::register(&app, &alice, "alice/demo", &["x"]).await;
    let job = runner.acquire(&app).await.unwrap();
    runner
        .complete(&app, job["job_id"].as_i64().unwrap(), "success", json!({}))
        .await;
    settle(&app).await;
    assert!(badge("").await.contains("CI: passing"));
    assert!(
        badge("?branch=main&event=push")
            .await
            .contains("CI: passing")
    );
    assert!(badge("?event=schedule").await.contains("CI: no status"));
    assert!(badge("?branch=nope").await.contains("CI: no status"));

    wc.commit(&[("x", "1")], "again").await;
    wc.push("main").await;
    settle(&app).await;
    let job = runner.acquire(&app).await.unwrap();
    runner
        .complete(&app, job["job_id"].as_i64().unwrap(), "failure", json!({}))
        .await;
    settle(&app).await;
    assert!(badge("").await.contains("CI: failing"));
    // By workflow id too; unknown workflows are 404.
    let id = app
        .get("/api/v3/repos/alice/demo/actions/workflows/ci.yml")
        .auth(&alice)
        .send()
        .await
        .json()["id"]
        .as_i64()
        .unwrap();
    let res = app
        .get(&format!("/alice/demo/actions/workflows/{id}/badge.svg"))
        .send()
        .await;
    res.assert_status(200);
    assert!(res.text().contains("CI: failing"));
    app.get("/alice/demo/actions/workflows/nope.yml/badge.svg")
        .send()
        .await
        .assert_status(404);
}

#[tokio::test]
async fn private_repo_badge_needs_access() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    app.create_private_repo(&alice, "secret").await;
    let wc = WorkingCopy::new(&app, &alice, "alice", "secret").await;
    wc.commit(
        &[(
            ".github/workflows/ci.yml",
            "name: CI\non: push\njobs:\n  t:\n    runs-on: x\n    steps: [{run: echo}]\n",
        )],
        "ci",
    )
    .await;
    wc.push("main").await;
    settle(&app).await;
    app.get("/alice/secret/actions/workflows/ci.yml/badge.svg")
        .send()
        .await
        .assert_status(404);
    app.get("/alice/secret/actions/workflows/ci.yml/badge.svg")
        .auth(&alice)
        .send()
        .await
        .assert_status(200);
}
