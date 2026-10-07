//! Merge queue (P39.1): `merge_queue` rule enforcement on direct merges,
//! enqueue / dequeue through `/_bgh`, queue order, permissions and
//! automatic removal.

use bgh_core::testing::{TestApp, TestUser};
use serde_json::{Value, json};

use crate::common::*;

async fn ruleset(app: &TestApp, user: &TestUser, rules: Value) {
    app.post("/api/v3/repos/alice/demo/rulesets")
        .auth(user)
        .json(&json!({
            "name": "Queue main",
            "target": "branch",
            "enforcement": "active",
            "conditions": {"ref_name": {"include": ["~DEFAULT_BRANCH"], "exclude": []}},
            "rules": rules,
        }))
        .send()
        .await
        .assert_status(201);
}

async fn queue_rule(app: &TestApp, user: &TestUser) {
    ruleset(
        app,
        user,
        json!([{"type": "merge_queue", "parameters": {"merge_method": "SQUASH"}}]),
    )
    .await;
}

/// Open PR `n` from a new branch `name` (one commit on top of `main`).
async fn pr_from(app: &TestApp, f: &Fixture, name: &str) -> i64 {
    branch(app, f.repo_id, name, &f.main).await;
    commit(
        app,
        f.repo_id,
        name,
        Some(&f.main),
        &[(&format!("{name}.txt"), Some("x\n"))],
        name,
    )
    .await;
    open_pr(app, &f.alice, "alice/demo", name, "main").await["number"]
        .as_i64()
        .unwrap()
}

async fn enqueue(app: &TestApp, user: &TestUser, n: i64, body: Value) -> (u16, Value) {
    let res = app
        .put(&format!("/_bgh/repos/alice/demo/pulls/{n}/queue"))
        .auth(user)
        .json(&body)
        .send()
        .await;
    let status = res.status();
    (status, res.json())
}

async fn queue(app: &TestApp, user: Option<&TestUser>) -> (u16, Value) {
    let mut req = app.get("/_bgh/repos/alice/demo/queue/main");
    if let Some(u) = user {
        req = req.auth(u);
    }
    let res = req.send().await;
    let status = res.status();
    (
        status,
        if status == 200 {
            res.json()
        } else {
            json!(null)
        },
    )
}

async fn pull_id(app: &TestApp, repo_id: i64, n: i64) -> i64 {
    sqlx::query_scalar("SELECT id FROM issues WHERE repo_id = $1 AND number = $2")
        .bind(repo_id)
        .bind(n)
        .fetch_one(&app.state.db)
        .await
        .unwrap()
}

#[tokio::test]
async fn direct_merge_refused_with_merge_queue_rule() {
    let f = fixture().await;
    let app = &f.app;
    open_pr(app, &f.alice, "alice/demo", "feature", "main").await;
    settle(app).await;

    // Without the rule the queue is off and enqueueing is refused.
    let (status, q) = queue(app, None).await;
    assert_eq!(status, 200);
    assert_eq!(q["enabled"], false);
    assert_eq!(q["config"], Value::Null);
    // Branch names may contain slashes.
    let q = app
        .get("/_bgh/repos/alice/demo/queue/release/v1.x")
        .send()
        .await
        .json();
    assert_eq!(q["branch"], "release/v1.x");
    assert_eq!(q["entries"], json!([]));
    let (status, body) = enqueue(app, &f.alice, 1, json!({})).await;
    assert_eq!(status, 422, "{body}");

    queue_rule(app, &f.alice).await;
    // `mergeable_state` stays `blocked` for direct merges.
    let two = pr_from(app, &f, "two").await;
    settle(app).await;
    let pr: Value = app
        .get(&format!("/api/v3/repos/alice/demo/pulls/{two}"))
        .send()
        .await
        .json();
    assert_eq!(pr["mergeable_state"], "blocked");
    let res = app
        .put("/api/v3/repos/alice/demo/pulls/1/merge")
        .auth(&f.alice)
        .send()
        .await;
    res.assert_status(405);
    assert_eq!(
        res.json()["message"],
        "Repository rule violations found\n\nChanges must be made through the merge queue\n\n"
    );

    let req = app
        .get("/_bgh/repos/alice/demo/pulls/1/requirements")
        .auth(&f.alice)
        .send()
        .await
        .json();
    assert_eq!(
        req["merge_queue"],
        json!({"required": true, "branch": "main", "entry": null})
    );
    // The rule itself is not listed as a blocker.
    assert_eq!(req["blockers"], json!([]));
    assert_eq!(req["requirements"], json!([]));

    let (_, q) = queue(app, None).await;
    assert_eq!(q["enabled"], true);
    assert_eq!(
        q["config"],
        json!({"merge_method": "SQUASH", "max_entries_to_build": 5,
               "min_entries_to_merge": 1, "max_entries_to_merge": 5,
               "grouping_strategy": "ALLGREEN", "check_response_timeout_minutes": 60,
               "min_entries_to_merge_wait_minutes": 5})
    );

    // Auto-merge does not merge around the queue.
    sqlx::query("UPDATE repositories SET allow_auto_merge = true WHERE id = $1")
        .bind(f.repo_id)
        .execute(&app.state.db)
        .await
        .unwrap();
    app.put("/_bgh/repos/alice/demo/pulls/1/auto_merge")
        .auth(&f.alice)
        .json(&json!({"merge_method": "merge"}))
        .send()
        .await
        .assert_status(200);
    settle(app).await;
    let pr: Value = app
        .get("/api/v3/repos/alice/demo/pulls/1")
        .send()
        .await
        .json();
    assert_eq!(pr["merged"], false);
}

#[tokio::test]
async fn direct_merge_allowed_without_rule() {
    let f = fixture().await;
    let app = &f.app;
    ruleset(app, &f.alice, json!([{"type": "required_linear_history"}])).await;
    open_pr(app, &f.alice, "alice/demo", "feature", "main").await;
    settle(app).await;
    app.put("/api/v3/repos/alice/demo/pulls/1/merge")
        .auth(&f.alice)
        .json(&json!({"merge_method": "squash"}))
        .send()
        .await
        .assert_status(200);
}

#[tokio::test]
async fn enqueue_dequeue_positions_and_jump() {
    let f = fixture().await;
    let app = &f.app;
    queue_rule(app, &f.alice).await;
    let one = pr_from(app, &f, "one").await;
    let two = pr_from(app, &f, "two").await;
    let three = pr_from(app, &f, "three").await;
    settle(app).await;

    let (status, e1) = enqueue(app, &f.alice, one, json!({})).await;
    assert_eq!(status, 201, "{e1}");
    assert_eq!(e1["position"], 1);
    assert_eq!(e1["state"], "queued");
    assert_eq!(e1["base_ref"], "main");
    assert_eq!(e1["jump"], false);
    assert_eq!(e1["pull"]["number"], one);
    assert_eq!(e1["pull"]["title"], "PR one");
    assert_eq!(e1["pull"]["user"]["login"], "alice");
    assert_eq!(e1["enqueuer"]["login"], "alice");
    assert_eq!(e1["estimated_time_to_merge"], Value::Null);
    assert_eq!(e1["group_head_sha"], Value::Null);
    assert_eq!(e1["failure_reason"], Value::Null);
    assert!(e1["head_sha"].as_str().unwrap().len() == 40);
    assert!(e1["enqueued_at"].is_string());

    // Idempotent.
    let (status, again) = enqueue(app, &f.alice, one, json!({})).await;
    assert_eq!(status, 200);
    assert_eq!(again["id"], e1["id"]);

    let (_, e2) = enqueue(app, &f.alice, two, json!({})).await;
    assert_eq!(e2["position"], 2);
    // Jumping puts a PR in front.
    let (status, e3) = enqueue(app, &f.alice, three, json!({"jump": true})).await;
    assert_eq!(status, 201, "{e3}");
    assert_eq!(e3["position"], 1);
    assert_eq!(e3["jump"], true);

    let (_, q) = queue(app, None).await;
    let order: Vec<i64> = q["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["pull"]["number"].as_i64().unwrap())
        .collect();
    assert_eq!(order, vec![three, one, two]);
    let positions: Vec<i64> = q["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["position"].as_i64().unwrap())
        .collect();
    assert_eq!(positions, vec![1, 2, 3]);

    let req = app
        .get(&format!("/_bgh/repos/alice/demo/pulls/{two}/requirements"))
        .auth(&f.alice)
        .send()
        .await
        .json();
    assert_eq!(req["merge_queue"]["required"], true);
    assert_eq!(req["merge_queue"]["entry"]["position"], 3);

    // Dequeue: 204, then 404.
    app.delete(&format!("/_bgh/repos/alice/demo/pulls/{one}/queue"))
        .auth(&f.alice)
        .send()
        .await
        .assert_status(204);
    app.delete(&format!("/_bgh/repos/alice/demo/pulls/{one}/queue"))
        .auth(&f.alice)
        .send()
        .await
        .assert_status(404);
    let (_, q) = queue(app, None).await;
    let order: Vec<i64> = q["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["pull"]["number"].as_i64().unwrap())
        .collect();
    assert_eq!(order, vec![three, two]);
    assert_eq!(q["entries"][1]["position"], 2);

    let id = pull_id(app, f.repo_id, one).await;
    let ev = events(app, id).await;
    assert!(ev.contains(&"added_to_merge_queue".to_string()), "{ev:?}");
    assert!(
        ev.contains(&"removed_from_merge_queue".to_string()),
        "{ev:?}"
    );
    let reason: Value = sqlx::query_scalar(
        "SELECT data FROM issue_events WHERE issue_id = $1 AND event = 'removed_from_merge_queue'",
    )
    .bind(id)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(reason["reason"], "dequeued");

    // Re-enqueue after removal works and goes to the back.
    let (status, e) = enqueue(app, &f.alice, one, json!({})).await;
    assert_eq!(status, 201);
    assert_eq!(e["position"], 3);
}

#[tokio::test]
async fn enqueue_requires_other_requirements() {
    let f = fixture().await;
    let app = &f.app;
    ruleset(
        app,
        &f.alice,
        json!([
            {"type": "merge_queue"},
            {"type": "pull_request", "parameters": {"required_approving_review_count": 1}},
            {"type": "required_status_checks", "parameters": {
                "required_status_checks": [{"context": "ci"}],
                "strict_required_status_checks_policy": false}},
        ]),
    )
    .await;
    let bob = app.create_user("bob").await;
    add_collaborator(app, f.repo_id, &bob, "write").await;
    open_pr(app, &f.alice, "alice/demo", "feature", "main").await;
    settle(app).await;

    // Missing approval blocks; the expected "ci" check does not.
    let (status, body) = enqueue(app, &f.alice, 1, json!({})).await;
    assert_eq!(status, 422, "{body}");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("approving review"),
        "{body}"
    );
    app.post("/api/v3/repos/alice/demo/pulls/1/reviews")
        .auth(&bob)
        .json(&json!({"event": "APPROVE"}))
        .send()
        .await
        .assert_status(200);
    let (status, body) = enqueue(app, &f.alice, 1, json!({})).await;
    assert_eq!(status, 201, "{body}");

    // Drafts can't be queued.
    branch(app, f.repo_id, "draft", &f.feature).await;
    let res = app
        .post("/api/v3/repos/alice/demo/pulls")
        .auth(&f.alice)
        .json(&json!({"title": "Draft", "head": "draft", "base": "main", "draft": true}))
        .send()
        .await;
    res.assert_status(201);
    let (status, _) = enqueue(app, &f.alice, 2, json!({})).await;
    assert_eq!(status, 422);
}

#[tokio::test]
async fn permissions_and_private_repos() {
    let f = fixture().await;
    let app = &f.app;
    queue_rule(app, &f.alice).await;
    let reader = app.create_user("reader").await;
    let writer = app.create_user("writer").await;
    let stranger = app.create_user("stranger").await;
    add_collaborator(app, f.repo_id, &reader, "read").await;
    add_collaborator(app, f.repo_id, &writer, "write").await;
    open_pr(app, &f.alice, "alice/demo", "feature", "main").await;
    settle(app).await;

    let (status, _) = enqueue(app, &reader, 1, json!({})).await;
    assert_eq!(status, 403);
    // Jumping needs admin.
    let (status, _) = enqueue(app, &writer, 1, json!({"jump": true})).await;
    assert_eq!(status, 403);
    let (status, _) = enqueue(app, &writer, 1, json!({})).await;
    assert_eq!(status, 201);
    app.delete("/_bgh/repos/alice/demo/pulls/1/queue")
        .auth(&reader)
        .send()
        .await
        .assert_status(403);
    // Anonymous callers are rejected.
    app.put("/_bgh/repos/alice/demo/pulls/1/queue")
        .json(&json!({}))
        .send()
        .await
        .assert_status(401);

    // Private: outsiders see nothing.
    sqlx::query("UPDATE repositories SET visibility = 'private' WHERE id = $1")
        .bind(f.repo_id)
        .execute(&app.state.db)
        .await
        .unwrap();
    let (status, _) = queue(app, Some(&stranger)).await;
    assert_eq!(status, 404);
    let (status, _) = queue(app, None).await;
    assert_eq!(status, 404);
    let (status, _) = enqueue(app, &stranger, 1, json!({})).await;
    assert_eq!(status, 404);
    app.delete("/_bgh/repos/alice/demo/pulls/1/queue")
        .auth(&stranger)
        .send()
        .await
        .assert_status(404);
    let (status, q) = queue(app, Some(&reader)).await;
    assert_eq!(status, 200);
    assert_eq!(q["entries"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn push_and_close_remove_from_queue() {
    let f = fixture().await;
    let app = &f.app;
    // A required check keeps the entries in the queue (no merge).
    ruleset(
        app,
        &f.alice,
        json!([
            {"type": "merge_queue"},
            {"type": "required_status_checks", "parameters": {
                "required_status_checks": [{"context": "ci"}],
                "strict_required_status_checks_policy": false}},
        ]),
    )
    .await;
    let one = pr_from(app, &f, "one").await;
    let two = pr_from(app, &f, "two").await;
    settle(app).await;
    enqueue(app, &f.alice, one, json!({})).await;
    enqueue(app, &f.alice, two, json!({})).await;

    // A push to the head removes the PR from the queue.
    let old = tip(app, f.repo_id, "one").await.unwrap();
    let new = commit(
        app,
        f.repo_id,
        "one",
        Some(&old),
        &[("more.txt", Some("more\n"))],
        "more",
    )
    .await;
    pushed(app, f.repo_id, &f.alice, "one", &old, &new).await;
    let (_, q) = queue(app, None).await;
    let order: Vec<i64> = q["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["pull"]["number"].as_i64().unwrap())
        .collect();
    assert_eq!(order, vec![two]);
    assert_eq!(q["entries"][0]["position"], 1);
    let reason: Value = sqlx::query_scalar(
        "SELECT data FROM issue_events WHERE issue_id = $1 AND event = 'removed_from_merge_queue'",
    )
    .bind(pull_id(app, f.repo_id, one).await)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(reason["reason"], "head changed");

    // Closing removes it as well.
    app.patch(&format!("/api/v3/repos/alice/demo/pulls/{two}"))
        .auth(&f.alice)
        .json(&json!({"state": "closed"}))
        .send()
        .await
        .assert_status(200);
    settle(app).await;
    let (_, q) = queue(app, None).await;
    assert_eq!(q["entries"], json!([]));
    let ev = events(app, pull_id(app, f.repo_id, two).await).await;
    assert!(
        ev.contains(&"removed_from_merge_queue".to_string()),
        "{ev:?}"
    );
}
