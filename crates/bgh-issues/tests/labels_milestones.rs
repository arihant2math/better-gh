//! Labels, milestones, assignees.

mod common;

use common::*;
use serde_json::json;

#[tokio::test]
async fn default_labels_and_crud() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let repo = repo(&app, &alice, "hello").await;

    let v = app
        .get("/api/v3/repos/alice/hello/labels")
        .send()
        .await
        .json();
    let names: Vec<&str> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        vec![
            "bug",
            "documentation",
            "duplicate",
            "enhancement",
            "good first issue",
            "help wanted",
            "invalid",
            "question",
            "wontfix"
        ]
    );
    let bug = &v[0];
    assert_eq!(bug["color"], "d73a4a");
    assert_eq!(bug["description"], "Something isn't working");
    assert_eq!(bug["default"], true);
    assert_eq!(bug["url"], app.url("/api/v3/repos/alice/hello/labels/bug"));
    assert_eq!(
        bgh_core::node_id::decode(bug["node_id"].as_str().unwrap())
            .unwrap()
            .0,
        bgh_core::node_id::NodeType::Label
    );

    // Create.
    let res = app
        .post("/api/v3/repos/alice/hello/labels")
        .auth(&alice)
        .json(&json!({"name": "needs review", "color": "#FBCA04", "description": "Please review"}))
        .send()
        .await;
    res.assert_status(201);
    let v = res.json();
    assert_eq!(v["color"], "fbca04");
    assert_eq!(v["default"], false);
    assert_eq!(
        v["url"],
        app.url("/api/v3/repos/alice/hello/labels/needs%20review")
    );
    // Duplicate (case-insensitive), bad color, missing name.
    let res = app
        .post("/api/v3/repos/alice/hello/labels")
        .auth(&alice)
        .json(&json!({"name": "BUG"}))
        .send()
        .await;
    res.assert_status(422);
    assert_eq!(res.json()["errors"][0]["code"], "already_exists");
    app.post("/api/v3/repos/alice/hello/labels")
        .auth(&alice)
        .json(&json!({"name": "x", "color": "zzz"}))
        .send()
        .await
        .assert_status(422);
    app.post("/api/v3/repos/alice/hello/labels")
        .auth(&alice)
        .json(&json!({"color": "ffffff"}))
        .send()
        .await
        .assert_status(422);
    // Readers can't create.
    app.post("/api/v3/repos/alice/hello/labels")
        .auth(&bob)
        .json(&json!({"name": "x"}))
        .send()
        .await
        .assert_status(403);

    // Get (case-insensitive, encoded).
    let v = app
        .get("/api/v3/repos/alice/hello/labels/Needs%20Review")
        .send()
        .await;
    v.assert_status(200);
    assert_eq!(v.json()["name"], "needs review");
    app.get("/api/v3/repos/alice/hello/labels/nope")
        .send()
        .await
        .assert_status(404);

    // Update (rename + description null).
    let res = app
        .patch("/api/v3/repos/alice/hello/labels/needs%20review")
        .auth(&alice)
        .json(&json!({"new_name": "reviewing", "description": null}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["name"], "reviewing");
    assert!(res.json()["description"].is_null());
    assert_eq!(res.json()["color"], "fbca04");

    // Delete removes it from issues.
    issue(
        &app,
        &alice,
        "alice",
        "hello",
        json!({"title": "t", "labels": ["reviewing"]}),
    )
    .await;
    app.delete("/api/v3/repos/alice/hello/labels/reviewing")
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    let i = app
        .get("/api/v3/repos/alice/hello/issues/1")
        .send()
        .await
        .json();
    assert_eq!(i["labels"], json!([]));
    let repo_id = repo["id"].as_i64().unwrap();
    let deletes: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM sync_actions WHERE scope = $1 AND model = 'label' AND action = 'D'",
    )
    .bind(format!("repo:{repo_id}"))
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(deletes, 1);
}

#[tokio::test]
async fn issue_labels() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    repo(&app, &alice, "hello").await;
    simple_issue(&app, &alice, "alice", "hello", "t").await;
    let base = "/api/v3/repos/alice/hello/issues/1/labels";

    // Add: object form, array form, auto-creates missing labels.
    let res = app
        .post(base)
        .auth(&alice)
        .json(&json!({"labels": ["bug"]}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json().as_array().unwrap().len(), 1);
    let res = app
        .post(base)
        .auth(&alice)
        .json(&json!(["question", {"name": "fresh"}]))
        .send()
        .await;
    res.assert_status(200);
    let names: Vec<String> = res
        .json()
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, vec!["bug", "fresh", "question"]);
    app.get("/api/v3/repos/alice/hello/labels/fresh")
        .send()
        .await
        .assert_status(200);
    // Empty add → 422.
    app.post(base)
        .auth(&alice)
        .json(&json!({"labels": []}))
        .send()
        .await
        .assert_status(422);

    // List.
    let v = app.get(base).send().await.json();
    assert_eq!(v.as_array().unwrap().len(), 3);

    // Set (replace).
    let res = app
        .put(base)
        .auth(&alice)
        .json(&json!({"labels": ["enhancement"]}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()[0]["name"], "enhancement");
    assert_eq!(res.json().as_array().unwrap().len(), 1);

    // Remove one: 200 with remaining; missing → 404.
    app.post(base)
        .auth(&alice)
        .json(&json!(["bug"]))
        .send()
        .await
        .assert_status(200);
    let res = app.delete(&format!("{base}/bug")).auth(&alice).send().await;
    res.assert_status(200);
    assert_eq!(res.json(), json!([res.json()[0].clone()]));
    assert_eq!(res.json()[0]["name"], "enhancement");
    let res = app.delete(&format!("{base}/bug")).auth(&alice).send().await;
    res.assert_status(404);
    assert_eq!(res.json()["message"], "Label does not exist");

    // Readers: 403. Remove all: 204.
    app.post(base)
        .auth(&bob)
        .json(&json!(["bug"]))
        .send()
        .await
        .assert_status(403);
    app.delete(base)
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    assert_eq!(app.get(base).send().await.json(), json!([]));

    // Events: labeled / unlabeled with label shape.
    let ev = app
        .get("/api/v3/repos/alice/hello/issues/1/events")
        .send()
        .await
        .json();
    let labeled = ev
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["event"] == "labeled")
        .unwrap();
    assert_eq!(labeled["label"], json!({"name": "bug", "color": "d73a4a"}));
    assert!(
        ev.as_array()
            .unwrap()
            .iter()
            .any(|e| e["event"] == "unlabeled")
    );
}

#[tokio::test]
async fn milestones_crud_and_counts() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    repo(&app, &alice, "hello").await;
    let base = "/api/v3/repos/alice/hello/milestones";

    let res = app
        .post(base)
        .auth(&alice)
        .json(&json!({"title": "v1.0", "description": "First", "due_on": "2030-01-01T00:00:00Z"}))
        .send()
        .await;
    res.assert_status(201);
    let m = res.json();
    assert_eq!(m["number"], 1);
    assert_eq!(m["state"], "open");
    assert_eq!(m["title"], "v1.0");
    assert_eq!(m["due_on"], "2030-01-01T00:00:00Z");
    assert_eq!(m["creator"]["login"], "alice");
    assert_eq!(m["open_issues"], 0);
    assert_eq!(m["url"], app.url("/api/v3/repos/alice/hello/milestones/1"));
    assert_eq!(m["html_url"], app.url("/alice/hello/milestone/1"));
    assert_eq!(
        m["labels_url"],
        app.url("/api/v3/repos/alice/hello/milestones/1/labels")
    );
    // Duplicate title / missing title / reader.
    let res = app
        .post(base)
        .auth(&alice)
        .json(&json!({"title": "V1.0"}))
        .send()
        .await;
    res.assert_status(422);
    assert_eq!(res.json()["errors"][0]["code"], "already_exists");
    app.post(base)
        .auth(&alice)
        .json(&json!({}))
        .send()
        .await
        .assert_status(422);
    app.post(base)
        .auth(&bob)
        .json(&json!({"title": "x"}))
        .send()
        .await
        .assert_status(403);
    app.post(base)
        .auth(&alice)
        .json(&json!({"title": "v2.0"}))
        .send()
        .await
        .assert_status(201);

    // Counts follow issue state and milestone changes.
    issue(
        &app,
        &alice,
        "alice",
        "hello",
        json!({"title": "a", "milestone": 1, "labels": ["bug"]}),
    )
    .await;
    issue(
        &app,
        &alice,
        "alice",
        "hello",
        json!({"title": "b", "milestone": 1}),
    )
    .await;
    app.patch("/api/v3/repos/alice/hello/issues/2")
        .auth(&alice)
        .json(&json!({"state": "closed"}))
        .send()
        .await
        .assert_status(200);
    let m = app.get(&format!("{base}/1")).send().await.json();
    assert_eq!(
        (m["open_issues"].as_i64(), m["closed_issues"].as_i64()),
        (Some(1), Some(1))
    );
    app.patch("/api/v3/repos/alice/hello/issues/1")
        .auth(&alice)
        .json(&json!({"milestone": 2}))
        .send()
        .await
        .assert_status(200);
    let m1 = app.get(&format!("{base}/1")).send().await.json();
    let m2 = app.get(&format!("{base}/2")).send().await.json();
    assert_eq!(m1["open_issues"], 0);
    assert_eq!(m2["open_issues"], 1);

    // Milestone labels.
    let v = app.get(&format!("{base}/2/labels")).send().await.json();
    assert_eq!(v[0]["name"], "bug");

    // List: default open, sorted by due_on asc (nulls last); state filter.
    let v = app.get(base).send().await.json();
    let titles: Vec<&str> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles, vec!["v1.0", "v2.0"]);
    let v = app
        .get(&format!("{base}?sort=completeness&direction=desc"))
        .send()
        .await
        .json();
    assert_eq!(v[0]["title"], "v1.0");

    // Update / close.
    let res = app
        .patch(&format!("{base}/1"))
        .auth(&alice)
        .json(&json!({"state": "closed", "due_on": null}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["state"], "closed");
    assert!(res.json()["closed_at"].is_string());
    assert!(res.json()["due_on"].is_null());
    let v = app.get(&format!("{base}?state=closed")).send().await.json();
    assert_eq!(v.as_array().unwrap().len(), 1);
    app.get(&format!("{base}?state=bogus"))
        .send()
        .await
        .assert_status(422);

    // Delete clears issue milestones.
    app.delete(&format!("{base}/2"))
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    app.get(&format!("{base}/2"))
        .send()
        .await
        .assert_status(404);
    let i = app
        .get("/api/v3/repos/alice/hello/issues/1")
        .send()
        .await
        .json();
    assert!(i["milestone"].is_null());

    // Milestone events.
    let ev = app
        .get("/api/v3/repos/alice/hello/issues/1/events")
        .send()
        .await
        .json();
    let m = ev
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["event"] == "demilestoned")
        .unwrap();
    assert_eq!(m["milestone"], json!({"title": "v1.0"}));
}

#[tokio::test]
async fn assignees() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let carol = app.create_user("carol").await;
    let dave = app.create_user("dave").await;
    repo(&app, &alice, "hello").await;
    add_collaborator(&app, "alice", "hello", &bob, "write").await;
    add_collaborator(&app, "alice", "hello", &dave, "read").await;
    simple_issue(&app, &carol, "alice", "hello", "t").await;

    let v = app
        .get("/api/v3/repos/alice/hello/assignees")
        .send()
        .await
        .json();
    let logins: Vec<&str> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u["login"].as_str().unwrap())
        .collect();
    assert_eq!(logins, vec!["alice", "bob"]);
    assert_eq!(v[0]["type"], "User");

    app.get("/api/v3/repos/alice/hello/assignees/bob")
        .send()
        .await
        .assert_status(204);
    app.get("/api/v3/repos/alice/hello/assignees/carol")
        .send()
        .await
        .assert_status(404);
    app.get("/api/v3/repos/alice/hello/assignees/dave")
        .send()
        .await
        .assert_status(404);
    app.get("/api/v3/repos/alice/hello/assignees/ghost-x")
        .send()
        .await
        .assert_status(404);
    app.get("/api/v3/repos/alice/hello/issues/1/assignees/bob")
        .send()
        .await
        .assert_status(204);

    // Add: non-assignable users are ignored.
    let res = app
        .post("/api/v3/repos/alice/hello/issues/1/assignees")
        .auth(&alice)
        .json(&json!({"assignees": ["bob", "carol", "alice"]}))
        .send()
        .await;
    res.assert_status(201);
    let logins: Vec<String> = res.json()["assignees"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u["login"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(logins, vec!["bob", "alice"]);
    assert_eq!(res.json()["assignee"]["login"], "bob");

    // Non-triagers: accepted but ignored.
    let res = app
        .delete("/api/v3/repos/alice/hello/issues/1/assignees")
        .auth(&carol)
        .json(&json!({"assignees": ["bob"]}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["assignees"].as_array().unwrap().len(), 2);

    let res = app
        .delete("/api/v3/repos/alice/hello/issues/1/assignees")
        .auth(&alice)
        .json(&json!({"assignees": ["bob"]}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["assignees"].as_array().unwrap().len(), 1);

    let ev = app
        .get("/api/v3/repos/alice/hello/issues/1/events")
        .send()
        .await
        .json();
    let a = ev
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["event"] == "assigned")
        .unwrap();
    assert_eq!(a["assignee"]["login"], "bob");
    assert_eq!(a["assigner"]["login"], "alice");
    assert_eq!(a["actor"]["login"], "alice");
    let u = ev
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["event"] == "unassigned")
        .unwrap();
    assert_eq!(u["assignee"]["login"], "bob");

    // /issues?filter=assigned for bob is now empty, for alice has one.
    let v = app.get("/api/v3/issues").auth(&alice).send().await.json();
    assert_eq!(v.as_array().unwrap().len(), 1);
    let v = app.get("/api/v3/issues").auth(&bob).send().await.json();
    assert_eq!(v.as_array().unwrap().len(), 0);
}
