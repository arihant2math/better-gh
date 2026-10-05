//! Comments, reactions, locking, events and timeline, mentions and
//! cross-references.

mod common;

use bgh_core::events::Event;
use common::*;
use serde_json::json;

#[tokio::test]
async fn comments_crud() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let carol = app.create_user("carol").await;
    let repo = repo(&app, &alice, "hello").await;
    let repo_id = repo["id"].as_i64().unwrap();
    simple_issue(&app, &alice, "alice", "hello", "t").await;
    simple_issue(&app, &alice, "alice", "hello", "u").await;

    let res = app
        .post("/api/v3/repos/alice/hello/issues/1/comments")
        .auth(&bob)
        .json(&json!({"body": "Me too"}))
        .send()
        .await;
    res.assert_status(201);
    let c = res.json();
    let id = c["id"].as_i64().unwrap();
    let url = app.url(&format!("/api/v3/repos/alice/hello/issues/comments/{id}"));
    assert_eq!(res.header("location"), Some(url.as_str()));
    assert_eq!(c["url"], url);
    assert_eq!(
        c["html_url"],
        app.url(&format!("/alice/hello/issues/1#issuecomment-{id}"))
    );
    assert_eq!(
        c["issue_url"],
        app.url("/api/v3/repos/alice/hello/issues/1")
    );
    assert_eq!(c["body"], "Me too");
    assert_eq!(c["user"]["login"], "bob");
    assert_eq!(c["author_association"], "NONE");
    assert_eq!(c["reactions"]["total_count"], 0);
    assert_eq!(c["reactions"]["url"], format!("{url}/reactions"));
    assert!(c["performed_via_github_app"].is_null());
    assert_eq!(
        bgh_core::node_id::decode(c["node_id"].as_str().unwrap())
            .unwrap()
            .0,
        bgh_core::node_id::NodeType::IssueComment
    );
    // Counter.
    let i = app
        .get("/api/v3/repos/alice/hello/issues/1")
        .send()
        .await
        .json();
    assert_eq!(i["comments"], 1);

    // Validation and auth.
    app.post("/api/v3/repos/alice/hello/issues/1/comments")
        .auth(&bob)
        .json(&json!({"body": ""}))
        .send()
        .await
        .assert_status(422);
    app.post("/api/v3/repos/alice/hello/issues/1/comments")
        .json(&json!({"body": "x"}))
        .send()
        .await
        .assert_status(401);
    app.post("/api/v3/repos/alice/hello/issues/9/comments")
        .auth(&bob)
        .json(&json!({"body": "x"}))
        .send()
        .await
        .assert_status(404);

    app.post("/api/v3/repos/alice/hello/issues/2/comments")
        .auth(&alice)
        .json(&json!({"body": "second"}))
        .send()
        .await
        .assert_status(201);

    // Lists.
    let v = app
        .get("/api/v3/repos/alice/hello/issues/1/comments")
        .send()
        .await
        .json();
    assert_eq!(v.as_array().unwrap().len(), 1);
    let v = app
        .get("/api/v3/repos/alice/hello/issues/comments")
        .send()
        .await
        .json();
    assert_eq!(v.as_array().unwrap().len(), 2);
    assert_eq!(v[0]["body"], "Me too");
    let v = app
        .get("/api/v3/repos/alice/hello/issues/comments?sort=created&direction=desc")
        .send()
        .await
        .json();
    assert_eq!(v[0]["body"], "second");
    let v = app
        .get("/api/v3/repos/alice/hello/issues/comments?since=2999-01-01T00:00:00Z")
        .send()
        .await
        .json();
    assert_eq!(v, json!([]));
    let v = app
        .get(&format!("/api/v3/repos/alice/hello/issues/comments/{id}"))
        .header("accept", "application/vnd.github.html+json")
        .send()
        .await
        .json();
    assert_eq!(v["body_html"], "<p>Me too</p>\n");

    // Edit: author or write; others 403.
    app.patch(&format!("/api/v3/repos/alice/hello/issues/comments/{id}"))
        .auth(&carol)
        .json(&json!({"body": "hack"}))
        .send()
        .await
        .assert_status(403);
    let res = app
        .patch(&format!("/api/v3/repos/alice/hello/issues/comments/{id}"))
        .auth(&bob)
        .json(&json!({"body": "Me too!"}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["body"], "Me too!");
    app.patch(&format!("/api/v3/repos/alice/hello/issues/comments/{id}"))
        .auth(&alice)
        .json(&json!({"body": "moderated"}))
        .send()
        .await
        .assert_status(200);

    // Delete.
    app.delete(&format!("/api/v3/repos/alice/hello/issues/comments/{id}"))
        .auth(&carol)
        .send()
        .await
        .assert_status(403);
    app.delete(&format!("/api/v3/repos/alice/hello/issues/comments/{id}"))
        .auth(&bob)
        .send()
        .await
        .assert_status(204);
    app.get(&format!("/api/v3/repos/alice/hello/issues/comments/{id}"))
        .send()
        .await
        .assert_status(404);
    let i = app
        .get("/api/v3/repos/alice/hello/issues/1")
        .send()
        .await
        .json();
    assert_eq!(i["comments"], 0);
    assert_eq!(sync_count(&app, repo_id, "comment").await, 5);
}

#[tokio::test]
async fn locked_issues() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let tri = app.create_user("tri").await;
    repo(&app, &alice, "hello").await;
    add_collaborator(&app, "alice", "hello", &tri, "triage").await;
    simple_issue(&app, &bob, "alice", "hello", "t").await;
    let lock = "/api/v3/repos/alice/hello/issues/1/lock";

    app.put(lock)
        .auth(&bob)
        .json(&json!({}))
        .send()
        .await
        .assert_status(403);
    app.put(lock)
        .auth(&tri)
        .json(&json!({"lock_reason": "weird"}))
        .send()
        .await
        .assert_status(422);
    app.put(lock)
        .auth(&tri)
        .json(&json!({"lock_reason": "too heated"}))
        .send()
        .await
        .assert_status(204);
    let i = app
        .get("/api/v3/repos/alice/hello/issues/1")
        .send()
        .await
        .json();
    assert_eq!(i["locked"], true);
    assert_eq!(i["active_lock_reason"], "too heated");

    let res = app
        .post("/api/v3/repos/alice/hello/issues/1/comments")
        .auth(&bob)
        .json(&json!({"body": "let me in"}))
        .send()
        .await;
    res.assert_status(403);
    assert_eq!(
        res.json()["message"],
        "Unable to create comment because issue is locked."
    );
    app.post("/api/v3/repos/alice/hello/issues/1/reactions")
        .auth(&bob)
        .json(&json!({"content": "+1"}))
        .send()
        .await
        .assert_status(403);
    app.post("/api/v3/repos/alice/hello/issues/1/comments")
        .auth(&tri)
        .json(&json!({"body": "mods can"}))
        .send()
        .await
        .assert_status(201);

    app.delete(lock).auth(&tri).send().await.assert_status(204);
    let i = app
        .get("/api/v3/repos/alice/hello/issues/1")
        .send()
        .await
        .json();
    assert_eq!(i["locked"], false);
    assert!(i["active_lock_reason"].is_null());
    let ev = app
        .get("/api/v3/repos/alice/hello/issues/1/events")
        .send()
        .await
        .json();
    let locked = ev
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["event"] == "locked")
        .unwrap();
    assert_eq!(locked["lock_reason"], "too heated");
    assert!(
        ev.as_array()
            .unwrap()
            .iter()
            .any(|e| e["event"] == "unlocked")
    );
}

#[tokio::test]
async fn reactions() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    repo(&app, &alice, "hello").await;
    simple_issue(&app, &alice, "alice", "hello", "t").await;
    let base = "/api/v3/repos/alice/hello/issues/1/reactions";

    let res = app
        .post(base)
        .auth(&bob)
        .json(&json!({"content": "heart"}))
        .send()
        .await;
    res.assert_status(201);
    let r = res.json();
    assert_eq!(r["content"], "heart");
    assert_eq!(r["user"]["login"], "bob");
    assert!(r["created_at"].is_string());
    assert_eq!(
        bgh_core::node_id::decode(r["node_id"].as_str().unwrap())
            .unwrap()
            .0,
        bgh_core::node_id::NodeType::Reaction
    );
    // Same reaction again → 200 with the existing one.
    let again = app
        .post(base)
        .auth(&bob)
        .json(&json!({"content": "heart"}))
        .send()
        .await;
    again.assert_status(200);
    assert_eq!(again.json()["id"], r["id"]);
    for c in ["+1", "-1", "laugh", "confused", "hooray", "rocket", "eyes"] {
        app.post(base)
            .auth(&alice)
            .json(&json!({"content": c}))
            .send()
            .await
            .assert_status(201);
    }
    app.post(base)
        .auth(&alice)
        .json(&json!({"content": "nope"}))
        .send()
        .await
        .assert_status(422);
    app.post(base)
        .json(&json!({"content": "+1"}))
        .send()
        .await
        .assert_status(401);

    let i = app
        .get("/api/v3/repos/alice/hello/issues/1")
        .send()
        .await
        .json();
    assert_eq!(i["reactions"]["total_count"], 8);
    assert_eq!(i["reactions"]["heart"], 1);
    assert_eq!(i["reactions"]["+1"], 1);
    let v = app.get(base).send().await.json();
    assert_eq!(v.as_array().unwrap().len(), 8);
    let v = app
        .get(&format!("{base}?content=heart"))
        .send()
        .await
        .json();
    assert_eq!(v.as_array().unwrap().len(), 1);

    // Only the reactor (or an admin) can delete.
    let rid = r["id"].as_i64().unwrap();
    let carol = app.create_user("carol").await;
    app.delete(&format!("{base}/{rid}"))
        .auth(&carol)
        .send()
        .await
        .assert_status(403);
    app.delete(&format!("{base}/{rid}"))
        .auth(&bob)
        .send()
        .await
        .assert_status(204);
    app.delete(&format!("{base}/{rid}"))
        .auth(&bob)
        .send()
        .await
        .assert_status(404);

    // Comment reactions.
    let c = app
        .post("/api/v3/repos/alice/hello/issues/1/comments")
        .auth(&alice)
        .json(&json!({"body": "hi"}))
        .send()
        .await
        .json();
    let cid = c["id"].as_i64().unwrap();
    let cbase = format!("/api/v3/repos/alice/hello/issues/comments/{cid}/reactions");
    let res = app
        .post(&cbase)
        .auth(&bob)
        .json(&json!({"content": "rocket"}))
        .send()
        .await;
    res.assert_status(201);
    let crid = res.json()["id"].as_i64().unwrap();
    let c = app
        .get(&format!("/api/v3/repos/alice/hello/issues/comments/{cid}"))
        .send()
        .await
        .json();
    assert_eq!(c["reactions"]["rocket"], 1);
    assert_eq!(c["reactions"]["total_count"], 1);
    assert_eq!(
        app.get(&cbase)
            .send()
            .await
            .json()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    app.delete(&format!("{cbase}/{crid}"))
        .auth(&bob)
        .send()
        .await
        .assert_status(204);
    app.get("/api/v3/repos/alice/hello/issues/comments/999/reactions")
        .send()
        .await
        .assert_status(404);
}

#[tokio::test]
async fn events_api() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    repo(&app, &alice, "hello").await;
    issue(
        &app,
        &alice,
        "alice",
        "hello",
        json!({"title": "t", "labels": ["bug"]}),
    )
    .await;
    app.patch("/api/v3/repos/alice/hello/issues/1")
        .auth(&alice)
        .json(&json!({"state": "closed"}))
        .send()
        .await
        .assert_status(200);

    let v = app
        .get("/api/v3/repos/alice/hello/issues/events")
        .send()
        .await
        .json();
    let arr = v.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    // Newest first, with the issue embedded.
    let closed = &arr[0];
    assert_eq!(closed["event"], "closed");
    assert_eq!(closed["actor"]["login"], "alice");
    assert!(closed["commit_id"].is_null());
    assert!(closed["commit_url"].is_null());
    assert!(closed["performed_via_github_app"].is_null());
    assert_eq!(closed["issue"]["number"], 1);
    assert_eq!(closed["issue"]["state"], "closed");
    let id = closed["id"].as_i64().unwrap();
    assert_eq!(
        closed["url"],
        app.url(&format!("/api/v3/repos/alice/hello/issues/events/{id}"))
    );
    assert_eq!(
        bgh_core::node_id::decode(closed["node_id"].as_str().unwrap())
            .unwrap()
            .0,
        bgh_core::node_id::NodeType::IssueEvent
    );

    let one = app
        .get(&format!("/api/v3/repos/alice/hello/issues/events/{id}"))
        .send()
        .await;
    one.assert_status(200);
    assert_eq!(one.json()["event"], "closed");
    assert_eq!(one.json()["issue"]["title"], "t");
    app.get("/api/v3/repos/alice/hello/issues/events/999999")
        .send()
        .await
        .assert_status(404);

    // Per-issue events: oldest first, no embedded issue.
    let v = app
        .get("/api/v3/repos/alice/hello/issues/1/events")
        .send()
        .await
        .json();
    assert_eq!(v[0]["event"], "labeled");
    assert!(v[0].get("issue").is_none());
}

#[tokio::test]
async fn mentions_cross_references_and_timeline() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    repo(&app, &alice, "hello").await;
    repo(&app, &alice, "other").await;
    private_repo(&app, &alice, "secret").await;
    simple_issue(&app, &alice, "alice", "hello", "target").await;
    let mut events = app.state.events.subscribe();

    // Comment mentioning bob and referencing #1 from issue 2.
    simple_issue(&app, &alice, "alice", "hello", "source").await;
    app.post("/api/v3/repos/alice/hello/issues/2/comments")
        .auth(&alice)
        .json(&json!({"body": "@bob see #1 and `@nobody #2`"}))
        .send()
        .await
        .assert_status(201);
    // Cross-repo reference from alice/other, and from a private repo.
    issue(
        &app,
        &alice,
        "alice",
        "other",
        json!({"title": "elsewhere", "body": "Related: alice/hello#1"}),
    )
    .await;
    issue(
        &app,
        &alice,
        "alice",
        "secret",
        json!({"title": "hidden", "body": "alice/hello#1"}),
    )
    .await;

    let mut seen_mention = false;
    let mut xrefs = 0;
    while let Ok(Ok(e)) =
        tokio::time::timeout(std::time::Duration::from_millis(200), events.recv()).await
    {
        match &*e {
            Event::IssueMentioned {
                user_id,
                comment_id,
                ..
            } => {
                assert_eq!(*user_id, bob.id);
                assert!(comment_id.is_some());
                seen_mention = true;
            }
            Event::IssueCrossReferenced { .. } => xrefs += 1,
            _ => {}
        }
    }
    assert!(seen_mention);
    assert_eq!(xrefs, 3);

    // Issue 2 events: mentioned + subscribed with bob as actor.
    let ev = app
        .get("/api/v3/repos/alice/hello/issues/2/events")
        .send()
        .await
        .json();
    let kinds: Vec<&str> = ev
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["event"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, vec!["mentioned", "subscribed"]);
    assert_eq!(ev[0]["actor"]["login"], "bob");

    // Cross-references are timeline-only.
    let ev = app
        .get("/api/v3/repos/alice/hello/issues/1/events")
        .send()
        .await
        .json();
    assert_eq!(ev, json!([]));

    // Timeline of #1 for anonymous: two visible cross-references (not the
    // private one).
    let tl = app
        .get("/api/v3/repos/alice/hello/issues/1/timeline")
        .send()
        .await
        .json();
    let arr = tl.as_array().unwrap();
    assert_eq!(arr.len(), 2, "{tl}");
    assert_eq!(arr[0]["event"], "cross-referenced");
    assert_eq!(arr[0]["actor"]["login"], "alice");
    assert_eq!(arr[0]["source"]["type"], "issue");
    assert_eq!(arr[0]["source"]["issue"]["number"], 2);
    assert_eq!(
        arr[0]["source"]["issue"]["repository"]["full_name"],
        "alice/hello"
    );
    assert_eq!(
        arr[1]["source"]["issue"]["repository"]["full_name"],
        "alice/other"
    );
    assert!(arr[0]["created_at"].is_string() && arr[0]["updated_at"].is_string());
    // The owner sees the private one too.
    let tl = app
        .get("/api/v3/repos/alice/hello/issues/1/timeline")
        .auth(&alice)
        .send()
        .await
        .json();
    assert_eq!(tl.as_array().unwrap().len(), 3);

    // Timeline of #2: comment item + mention events, in order.
    let tl = app
        .get("/api/v3/repos/alice/hello/issues/2/timeline")
        .send()
        .await
        .json();
    let kinds: Vec<&str> = tl
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["event"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, vec!["commented", "mentioned", "subscribed"]);
    assert_eq!(tl[0]["actor"]["login"], "alice");
    assert_eq!(tl[0]["user"]["login"], "alice");
    assert!(tl[0]["body"].as_str().unwrap().starts_with("@bob"));
    assert!(tl[0]["id"].is_i64());

    // Editing the comment without new references emits nothing new; adding
    // the same reference again doesn't duplicate the cross-reference.
    let cid: i64 = sqlx::query_scalar("SELECT id FROM comments ORDER BY id LIMIT 1")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    app.patch(&format!("/api/v3/repos/alice/hello/issues/comments/{cid}"))
        .auth(&alice)
        .json(&json!({"body": "@bob see #1 again, #1"}))
        .send()
        .await
        .assert_status(200);
    let tl = app
        .get("/api/v3/repos/alice/hello/issues/1/timeline")
        .auth(&alice)
        .send()
        .await
        .json();
    assert_eq!(tl.as_array().unwrap().len(), 3);

    // bob's mention shows up in filters.
    let v = app
        .get("/api/v3/repos/alice/hello/issues?mentioned=bob")
        .send()
        .await
        .json();
    assert_eq!(v[0]["number"], 2);
}

#[tokio::test]
async fn private_repo_mentions_require_access() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    app.create_user("bob").await;
    private_repo(&app, &alice, "secret").await;
    issue(
        &app,
        &alice,
        "alice",
        "secret",
        json!({"title": "t", "body": "@bob hello"}),
    )
    .await;
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM issue_mentions")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(n, 0);
}
