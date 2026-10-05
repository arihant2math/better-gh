//! Issues CRUD, lists and filters, media types, permissions.

mod common;

use common::*;
use serde_json::json;

#[tokio::test]
async fn create_issue_shape() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let repo = repo(&app, &alice, "hello").await;
    let ms = app
        .post("/api/v3/repos/alice/hello/milestones")
        .auth(&alice)
        .json(&json!({"title": "v1"}))
        .send()
        .await;
    ms.assert_status(201);

    let res = app
        .post("/api/v3/repos/alice/hello/issues")
        .auth(&alice)
        .json(&json!({
            "title": "Found a bug",
            "body": "I'm having a problem with this.",
            "assignees": ["alice"],
            "milestone": 1,
            "labels": ["bug", {"name": "brand new"}]
        }))
        .send()
        .await;
    res.assert_status(201);
    let v = res.json();
    assert_eq!(
        res.header("location"),
        Some(app.url("/api/v3/repos/alice/hello/issues/1").as_str())
    );
    assert_eq!(v["number"], 1);
    assert_eq!(v["title"], "Found a bug");
    assert_eq!(v["body"], "I'm having a problem with this.");
    assert_eq!(v["state"], "open");
    assert!(v["state_reason"].is_null());
    assert_eq!(v["locked"], false);
    assert!(v["active_lock_reason"].is_null());
    assert_eq!(v["comments"], 0);
    assert_eq!(v["user"]["login"], "alice");
    assert_eq!(v["author_association"], "OWNER");
    assert_eq!(v["url"], app.url("/api/v3/repos/alice/hello/issues/1"));
    assert_eq!(v["html_url"], app.url("/alice/hello/issues/1"));
    assert_eq!(v["repository_url"], app.url("/api/v3/repos/alice/hello"));
    assert_eq!(
        v["labels_url"],
        app.url("/api/v3/repos/alice/hello/issues/1/labels{/name}")
    );
    assert_eq!(
        v["comments_url"],
        app.url("/api/v3/repos/alice/hello/issues/1/comments")
    );
    assert_eq!(
        v["events_url"],
        app.url("/api/v3/repos/alice/hello/issues/1/events")
    );
    assert_eq!(
        v["timeline_url"],
        app.url("/api/v3/repos/alice/hello/issues/1/timeline")
    );
    assert_eq!(
        bgh_core::node_id::decode(v["node_id"].as_str().unwrap()),
        Some((
            bgh_core::node_id::NodeType::Issue,
            v["id"].as_i64().unwrap()
        ))
    );
    let labels: Vec<&str> = v["labels"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["name"].as_str().unwrap())
        .collect();
    assert_eq!(labels, vec!["brand new", "bug"]);
    assert_eq!(v["labels"][1]["color"], "d73a4a");
    assert_eq!(v["labels"][1]["default"], true);
    assert_eq!(v["assignee"]["login"], "alice");
    assert_eq!(v["assignees"][0]["login"], "alice");
    assert_eq!(v["milestone"]["number"], 1);
    assert_eq!(v["milestone"]["open_issues"], 1);
    assert_eq!(v["reactions"]["total_count"], 0);
    assert_eq!(v["reactions"]["+1"], 0);
    assert_eq!(
        v["reactions"]["url"],
        app.url("/api/v3/repos/alice/hello/issues/1/reactions")
    );
    assert_eq!(
        v["sub_issues_summary"],
        json!({"total": 0, "completed": 0, "percent_completed": 0})
    );
    assert!(v["closed_by"].is_null());
    assert!(v["closed_at"].is_null());
    assert!(v["performed_via_github_app"].is_null());
    assert!(v.get("pull_request").is_none());
    assert!(v.get("draft").is_none());
    assert!(v.get("body_html").is_none());
    assert!(v.get("repository").is_none());
    assert!(v["created_at"].as_str().unwrap().ends_with('Z'));

    // Repo counter, sync log, events.
    let r = app
        .get("/api/v3/repos/alice/hello")
        .auth(&alice)
        .send()
        .await;
    assert_eq!(r.json()["open_issues_count"], 1);
    let repo_id = repo["id"].as_i64().unwrap();
    assert_eq!(sync_count(&app, repo_id, "issue").await, 1);
    assert!(sync_count(&app, repo_id, "issue_event").await >= 4);
    // The auto-created label was synced.
    assert!(sync_count(&app, repo_id, "label").await >= 10);
}

#[tokio::test]
async fn create_validation_and_permissions() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    repo(&app, &alice, "hello").await;
    private_repo(&app, &alice, "secret").await;

    let res = app
        .post("/api/v3/repos/alice/hello/issues")
        .auth(&alice)
        .json(&json!({"body": "no title"}))
        .send()
        .await;
    res.assert_status(422);
    assert_eq!(res.json()["errors"][0]["code"], "missing_field");
    assert_eq!(res.json()["errors"][0]["field"], "title");

    // Unknown milestone / non-assignable user.
    let res = app
        .post("/api/v3/repos/alice/hello/issues")
        .auth(&alice)
        .json(&json!({"title": "x", "milestone": 99}))
        .send()
        .await;
    res.assert_status(422);
    assert_eq!(res.json()["errors"][0]["field"], "milestone");
    let res = app
        .post("/api/v3/repos/alice/hello/issues")
        .auth(&alice)
        .json(&json!({"title": "x", "assignees": ["bob"]}))
        .send()
        .await;
    res.assert_status(422);
    assert_eq!(res.json()["errors"][0]["field"], "assignees");

    // Anonymous: 401.
    app.post("/api/v3/repos/alice/hello/issues")
        .json(&json!({"title": "x"}))
        .send()
        .await
        .assert_status(401);
    // Private repo without access: 404.
    app.post("/api/v3/repos/alice/secret/issues")
        .auth(&bob)
        .json(&json!({"title": "x"}))
        .send()
        .await
        .assert_status(404);
    app.get("/api/v3/repos/alice/secret/issues")
        .auth(&bob)
        .send()
        .await
        .assert_status(404);

    // Readers can open issues; labels/assignees are silently dropped.
    let v = issue(
        &app,
        &bob,
        "alice",
        "hello",
        json!({"title": "from bob", "labels": ["bug"], "assignees": ["alice"]}),
    )
    .await;
    assert_eq!(v["labels"], json!([]));
    assert_eq!(v["assignees"], json!([]));
    assert_eq!(v["author_association"], "NONE");

    // Issues disabled → 410.
    sqlx::query("UPDATE repositories SET has_issues = false WHERE name = 'hello'")
        .execute(&app.state.db)
        .await
        .unwrap();
    app.get("/api/v3/repos/alice/hello/issues")
        .send()
        .await
        .assert_status(410);
    app.get("/api/v3/repos/alice/hello/issues/1")
        .send()
        .await
        .assert_status(410);
}

#[tokio::test]
async fn update_issue_permissions_and_state() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let carol = app.create_user("carol").await;
    let tri = app.create_user("tri").await;
    repo(&app, &alice, "hello").await;
    add_collaborator(&app, "alice", "hello", &tri, "triage").await;
    simple_issue(&app, &bob, "alice", "hello", "bob's issue").await;

    // Author can edit title/body and close.
    let res = app
        .patch("/api/v3/repos/alice/hello/issues/1")
        .auth(&bob)
        .json(&json!({"title": "renamed", "body": "new body"}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["title"], "renamed");
    assert_eq!(res.json()["body"], "new body");
    // A stranger can't.
    app.patch("/api/v3/repos/alice/hello/issues/1")
        .auth(&carol)
        .json(&json!({"state": "closed"}))
        .send()
        .await
        .assert_status(403);
    // Triage can close and label but not edit the title.
    app.patch("/api/v3/repos/alice/hello/issues/1")
        .auth(&tri)
        .json(&json!({"title": "nope"}))
        .send()
        .await
        .assert_status(403);
    let res = app
        .patch("/api/v3/repos/alice/hello/issues/1")
        .auth(&tri)
        .json(&json!({"state": "closed", "state_reason": "not_planned", "labels": ["bug"], "assignees": ["tri"]}))
        .send()
        .await;
    res.assert_status(200);
    let v = res.json();
    assert_eq!(v["state"], "closed");
    assert_eq!(v["state_reason"], "not_planned");
    assert_eq!(v["closed_by"]["login"], "tri");
    assert!(v["closed_at"].is_string());
    assert_eq!(v["labels"][0]["name"], "bug");
    assert_eq!(v["assignees"][0]["login"], "tri");
    let r = app.get("/api/v3/repos/alice/hello").send().await;
    assert_eq!(r.json()["open_issues_count"], 0);

    // Reopen.
    let res = app
        .patch("/api/v3/repos/alice/hello/issues/1")
        .auth(&bob)
        .json(&json!({"state": "open"}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["state_reason"], "reopened");
    assert!(res.json()["closed_by"].is_null());

    // Invalid state.
    app.patch("/api/v3/repos/alice/hello/issues/1")
        .auth(&alice)
        .json(&json!({"state": "weird"}))
        .send()
        .await
        .assert_status(422);
    // Clear body with null, milestone null.
    let res = app
        .patch("/api/v3/repos/alice/hello/issues/1")
        .auth(&alice)
        .json(&json!({"body": null, "milestone": null, "assignees": []}))
        .send()
        .await;
    res.assert_status(200);
    assert!(res.json()["body"].is_null());
    assert_eq!(res.json()["assignees"], json!([]));

    // Events reflect the changes.
    let ev = app
        .get("/api/v3/repos/alice/hello/issues/1/events")
        .send()
        .await
        .json();
    let kinds: Vec<&str> = ev
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["event"].as_str().unwrap())
        .collect();
    for k in [
        "renamed",
        "labeled",
        "assigned",
        "closed",
        "reopened",
        "unassigned",
    ] {
        assert!(kinds.contains(&k), "missing {k} in {kinds:?}");
    }
    let renamed = ev
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["event"] == "renamed")
        .unwrap();
    assert_eq!(
        renamed["rename"],
        json!({"from": "bob's issue", "to": "renamed"})
    );
    let closed = ev
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["event"] == "closed")
        .unwrap();
    assert_eq!(closed["state_reason"], "not_planned");
    assert_eq!(closed["actor"]["login"], "tri");
}

#[tokio::test]
async fn list_filters_and_sorting() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    repo(&app, &alice, "hello").await;
    add_collaborator(&app, "alice", "hello", &bob, "write").await;
    app.post("/api/v3/repos/alice/hello/milestones")
        .auth(&alice)
        .json(&json!({"title": "v1"}))
        .send()
        .await
        .assert_status(201);
    issue(
        &app,
        &alice,
        "alice",
        "hello",
        json!({"title": "one", "labels": ["bug"], "milestone": 1}),
    )
    .await;
    issue(
        &app,
        &bob,
        "alice",
        "hello",
        json!({"title": "two", "assignees": ["alice"], "body": "cc @bob"}),
    )
    .await;
    issue(
        &app,
        &alice,
        "alice",
        "hello",
        json!({"title": "three", "labels": ["bug", "question"]}),
    )
    .await;
    app.patch("/api/v3/repos/alice/hello/issues/3")
        .auth(&alice)
        .json(&json!({"state": "closed"}))
        .send()
        .await
        .assert_status(200);

    let titles = |v: serde_json::Value| -> Vec<String> {
        v.as_array()
            .unwrap()
            .iter()
            .map(|i| i["title"].as_str().unwrap().to_string())
            .collect()
    };
    let get = |q: &str| {
        app.get(&format!("/api/v3/repos/alice/hello/issues{q}"))
            .send()
    };
    assert_eq!(titles(get("").await.json()), vec!["two", "one"]);
    assert_eq!(
        titles(get("?state=all").await.json()),
        vec!["three", "two", "one"]
    );
    assert_eq!(titles(get("?state=closed").await.json()), vec!["three"]);
    assert_eq!(
        titles(get("?state=all&direction=asc").await.json()),
        vec!["one", "two", "three"]
    );
    assert_eq!(
        titles(get("?labels=bug&state=all").await.json()),
        vec!["three", "one"]
    );
    assert_eq!(
        titles(get("?labels=bug,question&state=all").await.json()),
        vec!["three"]
    );
    assert_eq!(titles(get("?milestone=1").await.json()), vec!["one"]);
    assert_eq!(titles(get("?milestone=none").await.json()), vec!["two"]);
    assert_eq!(titles(get("?milestone=*").await.json()), vec!["one"]);
    assert_eq!(titles(get("?assignee=alice").await.json()), vec!["two"]);
    assert_eq!(titles(get("?assignee=none").await.json()), vec!["one"]);
    assert_eq!(titles(get("?assignee=*").await.json()), vec!["two"]);
    assert_eq!(titles(get("?creator=bob").await.json()), vec!["two"]);
    assert_eq!(
        titles(get("?creator=nobody").await.json()),
        Vec::<String>::new()
    );
    assert_eq!(
        titles(get("?mentioned=bob").await.json()),
        Vec::<String>::new()
    ); // self-mention ignored
    assert_eq!(
        titles(get("?since=2000-01-01T00:00:00Z").await.json()),
        vec!["two", "one"]
    );
    assert_eq!(
        titles(get("?since=2999-01-01T00:00:00Z").await.json()),
        Vec::<String>::new()
    );
    get("?since=garbage").await.assert_status(422);
    get("?sort=bogus").await.assert_status(422);

    // Pagination with Link header.
    let res = get("?state=all&per_page=2").await;
    assert_eq!(res.json().as_array().unwrap().len(), 2);
    assert!(res.header("link").unwrap().contains("rel=\"next\""));

    // Sort by comments.
    app.post("/api/v3/repos/alice/hello/issues/1/comments")
        .auth(&alice)
        .json(&json!({"body": "hi"}))
        .send()
        .await
        .assert_status(201);
    assert_eq!(
        titles(get("?sort=comments").await.json()),
        vec!["one", "two"]
    );
    // Updated sort: issue 1 was just commented.
    assert_eq!(
        titles(get("?sort=updated").await.json()),
        vec!["one", "two"]
    );
}

#[tokio::test]
async fn mentioned_filter() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    repo(&app, &alice, "hello").await;
    issue(
        &app,
        &alice,
        "alice",
        "hello",
        json!({"title": "ping", "body": "Hey @bob, look"}),
    )
    .await;
    simple_issue(&app, &alice, "alice", "hello", "other").await;
    let v = app
        .get("/api/v3/repos/alice/hello/issues?mentioned=bob")
        .send()
        .await
        .json();
    assert_eq!(v.as_array().unwrap().len(), 1);
    assert_eq!(v[0]["title"], "ping");
    // /issues?filter=mentioned for bob includes the public repo issue.
    let v = app
        .get("/api/v3/issues?filter=mentioned")
        .auth(&bob)
        .send()
        .await
        .json();
    assert_eq!(v.as_array().unwrap().len(), 1);
    assert_eq!(v[0]["repository"]["full_name"], "alice/hello");
    // filter=subscribed: bob was subscribed by the mention.
    let v = app
        .get("/api/v3/issues?filter=subscribed")
        .auth(&bob)
        .send()
        .await
        .json();
    assert_eq!(v.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn body_media_types() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    repo(&app, &alice, "hello").await;
    issue(
        &app,
        &alice,
        "alice",
        "hello",
        json!({"title": "md", "body": "**bold** see #1"}),
    )
    .await;
    let get = |accept: &'static str| {
        app.get("/api/v3/repos/alice/hello/issues/1")
            .header("accept", accept)
            .send()
    };
    let v = get("application/vnd.github.html+json").await.json();
    assert!(v.get("body").is_none());
    let html = v["body_html"].as_str().unwrap();
    assert!(html.contains("<strong>bold</strong>"), "{html}");
    assert!(html.contains("/alice/hello/issues/1"), "{html}");
    let v = get("application/vnd.github.text+json").await.json();
    assert_eq!(v["body_text"], "bold see #1");
    assert!(v.get("body_html").is_none());
    let v = get("application/vnd.github.full+json").await.json();
    assert!(v["body"].is_string() && v["body_text"].is_string() && v["body_html"].is_string());
    let v = get("application/vnd.github.raw+json").await.json();
    assert_eq!(v["body"], "**bold** see #1");
    assert!(v.get("body_text").is_none());
}

#[tokio::test]
async fn pull_requests_in_issue_lists() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let repo = repo(&app, &alice, "hello").await;
    let repo_id = repo["id"].as_i64().unwrap();
    simple_issue(&app, &alice, "alice", "hello", "an issue").await;
    let n = insert_pull(&app, repo_id, alice.id, "a pull").await;
    assert_eq!(n, 2);
    let v = app
        .get("/api/v3/repos/alice/hello/issues")
        .send()
        .await
        .json();
    assert_eq!(v.as_array().unwrap().len(), 2);
    let pr = &v[0];
    assert_eq!(pr["title"], "a pull");
    assert_eq!(pr["draft"], true);
    assert_eq!(pr["html_url"], app.url("/alice/hello/pull/2"));
    assert_eq!(
        pr["pull_request"],
        json!({
            "url": app.url("/api/v3/repos/alice/hello/pulls/2"),
            "html_url": app.url("/alice/hello/pull/2"),
            "diff_url": app.url("/alice/hello/pull/2.diff"),
            "patch_url": app.url("/alice/hello/pull/2.patch"),
            "merged_at": null,
        })
    );
    assert_eq!(
        bgh_core::node_id::decode(pr["node_id"].as_str().unwrap())
            .unwrap()
            .0,
        bgh_core::node_id::NodeType::PullRequest
    );
    assert!(v[1].get("pull_request").is_none());
    // Comments on PRs via the issues API.
    let c = app
        .post("/api/v3/repos/alice/hello/issues/2/comments")
        .auth(&alice)
        .json(&json!({"body": "LGTM"}))
        .send()
        .await;
    c.assert_status(201);
    assert!(
        c.json()["html_url"]
            .as_str()
            .unwrap()
            .contains("/pull/2#issuecomment-")
    );
}

#[tokio::test]
async fn cross_repository_lists() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let carol = app.create_user("carol").await;
    let org = app.create_org("acme", &alice).await;
    app.add_org_member(&org, &bob, "member").await;
    sqlx::query(
        "UPDATE org_settings SET default_repository_permission = 'write' WHERE org_id = $1",
    )
    .bind(org.id)
    .execute(&app.state.db)
    .await
    .unwrap();
    app.create_repo_with(
        &alice,
        Some("acme"),
        json!({"name": "widgets", "private": true}),
    )
    .await;
    wait_default_labels(&app, "acme", "widgets").await;
    private_repo(&app, &alice, "mine").await;
    repo(&app, &carol, "pub").await;

    issue(
        &app,
        &alice,
        "acme",
        "widgets",
        json!({"title": "org issue", "assignees": ["bob"]}),
    )
    .await;
    issue(
        &app,
        &alice,
        "alice",
        "mine",
        json!({"title": "private mine", "assignees": ["alice"]}),
    )
    .await;
    simple_issue(&app, &bob, "carol", "pub", "bob in public").await;

    let titles = |v: serde_json::Value| -> Vec<String> {
        let mut t: Vec<String> = v
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["title"].as_str().unwrap().to_string())
            .collect();
        t.sort();
        t
    };
    // Default filter=assigned.
    let v = app.get("/api/v3/issues").auth(&bob).send().await.json();
    assert_eq!(titles(v.clone()), vec!["org issue"]);
    assert_eq!(v[0]["repository"]["full_name"], "acme/widgets");
    assert_eq!(v[0]["repository"]["private"], true);
    let v = app
        .get("/api/v3/issues?filter=created")
        .auth(&bob)
        .send()
        .await
        .json();
    assert_eq!(titles(v), vec!["bob in public"]);
    let v = app
        .get("/api/v3/issues?filter=all")
        .auth(&bob)
        .send()
        .await
        .json();
    assert_eq!(titles(v), vec!["org issue"]);
    let v = app
        .get("/api/v3/issues?filter=all")
        .auth(&alice)
        .send()
        .await
        .json();
    assert_eq!(titles(v), vec!["org issue", "private mine"]);
    // /user/issues excludes org repositories.
    let v = app
        .get("/api/v3/user/issues?filter=all")
        .auth(&alice)
        .send()
        .await
        .json();
    assert_eq!(titles(v), vec!["private mine"]);
    // /orgs/{org}/issues.
    let v = app
        .get("/api/v3/orgs/acme/issues?filter=all")
        .auth(&alice)
        .send()
        .await
        .json();
    assert_eq!(titles(v), vec!["org issue"]);
    // Carol sees nothing private.
    let v = app
        .get("/api/v3/issues?filter=all")
        .auth(&carol)
        .send()
        .await
        .json();
    assert_eq!(titles(v), vec!["bob in public"]);
    // Token without `repo` scope: public only.
    let t = app.create_token(&alice, &["public_repo"]).await;
    let v = app
        .get("/api/v3/issues?filter=all")
        .token(&t)
        .send()
        .await
        .json();
    assert_eq!(titles(v), Vec::<String>::new());
    app.get("/api/v3/issues").send().await.assert_status(401);
    app.get("/api/v3/issues?filter=bogus")
        .auth(&bob)
        .send()
        .await
        .assert_status(422);
}

#[tokio::test]
async fn get_issue_not_found_and_private() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    private_repo(&app, &alice, "secret").await;
    simple_issue(&app, &alice, "alice", "secret", "hidden").await;
    app.get("/api/v3/repos/alice/secret/issues/1")
        .auth(&bob)
        .send()
        .await
        .assert_status(404);
    app.get("/api/v3/repos/alice/secret/issues/1")
        .send()
        .await
        .assert_status(404);
    app.get("/api/v3/repos/alice/secret/issues/2")
        .auth(&alice)
        .send()
        .await
        .assert_status(404);
    let v = app
        .get("/api/v3/repos/alice/secret/issues/1")
        .auth(&alice)
        .send()
        .await;
    v.assert_status(200);
    assert_eq!(v.json()["title"], "hidden");
}
