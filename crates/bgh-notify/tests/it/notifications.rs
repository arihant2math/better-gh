//! Notifications API, fan-out reasons, subscriptions and watching.

use crate::support;

use bgh_core::events::Event;
use bgh_core::testing::{TestApp, TestUser};
use serde_json::{Value, json};
use support::*;

struct World {
    app: TestApp,
    probe: Probe,
    alice: TestUser,
    bob: TestUser,
    carol: TestUser,
    repo_id: i64,
}

/// alice owns public `alice/hello` (and watches it); bob watches it too.
async fn world() -> World {
    let app = bgh_server::test_app().await;
    let probe = Probe::new(&app).await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let carol = app.create_user("carol").await;
    app.create_repo(&alice, "hello").await;
    let repo_id = repo_id(&app, "alice", "hello").await;
    app.put("/api/v3/repos/alice/hello/subscription")
        .auth(&bob)
        .json(&json!({"subscribed": true}))
        .send()
        .await
        .assert_status(200);
    World {
        app,
        probe,
        alice,
        bob,
        carol,
        repo_id,
    }
}

async fn open_issue(w: &World, author: &TestUser, title: &str, body: &str) -> (i64, i64) {
    let (id, number) = insert_issue(&w.app, w.repo_id, author, title, body, false).await;
    w.app.state.events.emit(Event::IssueOpened {
        repo_id: w.repo_id,
        issue_id: id,
        actor_id: author.id,
    });
    w.probe.settle(&w.app).await;
    (id, number)
}

async fn list(w: &World, user: &TestUser, query: &str) -> Vec<Value> {
    let res = w
        .app
        .get(&format!("/api/v3/notifications{query}"))
        .auth(user)
        .send()
        .await;
    res.assert_status(200);
    res.json().as_array().unwrap().clone()
}

#[tokio::test]
async fn watchers_get_threads_in_github_shape() {
    let w = world().await;
    let (_, number) = open_issue(&w, &w.alice, "Crash on start", "It crashes").await;

    // The author is not notified about their own issue.
    assert!(list(&w, &w.alice, "").await.is_empty());

    let threads = list(&w, &w.bob, "").await;
    assert_eq!(threads.len(), 1);
    let t = &threads[0];
    assert!(t["id"].is_string(), "thread id is a string: {t}");
    assert_eq!(t["reason"], "subscribed");
    assert_eq!(t["unread"], true);
    assert!(t["last_read_at"].is_null());
    assert_eq!(t["subject"]["type"], "Issue");
    assert_eq!(t["subject"]["title"], "Crash on start");
    assert_eq!(
        t["subject"]["url"],
        w.app
            .url(&format!("/api/v3/repos/alice/hello/issues/{number}"))
    );
    assert_eq!(t["subject"]["latest_comment_url"], t["subject"]["url"]);
    assert_eq!(t["repository"]["full_name"], "alice/hello");
    assert_eq!(t["repository"]["owner"]["login"], "alice");
    let id = t["id"].as_str().unwrap();
    assert_eq!(
        t["url"],
        w.app.url(&format!("/api/v3/notifications/threads/{id}"))
    );
    assert_eq!(
        t["subscription_url"],
        w.app
            .url(&format!("/api/v3/notifications/threads/{id}/subscription"))
    );
    assert!(t["updated_at"].as_str().unwrap().ends_with('Z'));

    // Watching is not participating.
    assert!(list(&w, &w.bob, "?participating=true").await.is_empty());
    // Repo-scoped listing.
    let res = w
        .app
        .get("/api/v3/repos/alice/hello/notifications")
        .auth(&w.bob)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json().as_array().unwrap().len(), 1);

    // Recorded as a sync action in bob's user scope.
    let synced: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM sync_actions WHERE scope = $1 AND model = 'notification'",
    )
    .bind(format!("user:{}", w.bob.id))
    .fetch_one(&w.app.state.db)
    .await
    .unwrap();
    assert_eq!(synced, 1);
}

#[tokio::test]
async fn mentions_comments_and_participation() {
    let w = world().await;
    // carol isn't watching; a mention reaches her (public repo).
    let (issue_id, _) = open_issue(&w, &w.alice, "Design", "cc @carol and @nobody, `@bob`").await;
    let carol = list(&w, &w.carol, "").await;
    assert_eq!(carol.len(), 1);
    assert_eq!(carol[0]["reason"], "mention");
    assert_eq!(list(&w, &w.carol, "?participating=true").await.len(), 1);

    // carol was subscribed by the mention: bob's comment notifies her and
    // alice (author), with latest_comment_url pointing at the comment.
    let comment_id = insert_comment(&w.app, w.repo_id, issue_id, &w.bob, "Looks good").await;
    w.app.state.events.emit(Event::IssueCommentCreated {
        repo_id: w.repo_id,
        issue_id,
        comment_id,
        actor_id: w.bob.id,
    });
    w.probe.settle(&w.app).await;
    let alice = list(&w, &w.alice, "").await;
    assert_eq!(alice.len(), 1);
    assert_eq!(alice[0]["reason"], "author");
    assert_eq!(
        alice[0]["subject"]["latest_comment_url"],
        w.app.url(&format!(
            "/api/v3/repos/alice/hello/issues/comments/{comment_id}"
        ))
    );
    let carol = list(&w, &w.carol, "").await;
    assert_eq!(carol[0]["reason"], "mention");
    // The commenter is not notified about their own comment.
    let bob_threads = list(&w, &w.bob, "?all=true").await;
    assert_eq!(bob_threads.len(), 1, "bob saw the opening only");

    // bob is now a participant (reason comment): alice's reply notifies him
    // with the participating reason, not `subscribed`.
    sqlx::query("UPDATE notifications SET unread = false WHERE user_id = $1")
        .bind(w.bob.id)
        .execute(&w.app.state.db)
        .await
        .unwrap();
    let c2 = insert_comment(&w.app, w.repo_id, issue_id, &w.alice, "Thanks").await;
    w.app.state.events.emit(Event::IssueCommentCreated {
        repo_id: w.repo_id,
        issue_id,
        comment_id: c2,
        actor_id: w.alice.id,
    });
    w.probe.settle(&w.app).await;
    let bob_threads = list(&w, &w.bob, "").await;
    assert_eq!(bob_threads.len(), 1);
    assert_eq!(bob_threads[0]["reason"], "comment");
    assert_eq!(bob_threads[0]["unread"], true);
}

#[tokio::test]
async fn private_repos_only_notify_readers() {
    let w = world().await;
    w.app.create_private_repo(&w.alice, "secret").await;
    let rid = repo_id(&w.app, "alice", "secret").await;
    let (id, _) = insert_issue(&w.app, rid, &w.alice, "Hidden", "ping @bob @carol", false).await;
    sqlx::query("INSERT INTO collaborators (repo_id, user_id, permission) VALUES ($1, $2, 'read')")
        .bind(rid)
        .bind(w.bob.id)
        .execute(&w.app.state.db)
        .await
        .unwrap();
    w.app.state.events.emit(Event::IssueOpened {
        repo_id: rid,
        issue_id: id,
        actor_id: w.alice.id,
    });
    w.probe.settle(&w.app).await;
    let bob = list(&w, &w.bob, "").await;
    assert_eq!(bob.len(), 1);
    assert_eq!(bob[0]["reason"], "mention");
    assert!(
        list(&w, &w.carol, "").await.is_empty(),
        "no access, no notification"
    );
    // carol wasn't subscribed either.
    let subs: i64 =
        sqlx::query_scalar("SELECT count(*) FROM thread_subscriptions WHERE user_id = $1")
            .bind(w.carol.id)
            .fetch_one(&w.app.state.db)
            .await
            .unwrap();
    assert_eq!(subs, 0);
}

#[tokio::test]
async fn mark_read_and_done() {
    let w = world().await;
    open_issue(&w, &w.alice, "One", "").await;
    open_issue(&w, &w.alice, "Two", "").await;
    let threads = list(&w, &w.bob, "").await;
    assert_eq!(threads.len(), 2);
    // Newest first.
    assert_eq!(threads[0]["subject"]["title"], "Two");

    // Pagination with Link header.
    let res = w
        .app
        .get("/api/v3/notifications?per_page=1")
        .auth(&w.bob)
        .send()
        .await;
    assert_eq!(res.json().as_array().unwrap().len(), 1);
    assert!(res.header("link").unwrap().contains("rel=\"next\""));

    // Mark one thread read.
    let id = threads[0]["id"].as_str().unwrap().to_string();
    w.app
        .patch(&format!("/api/v3/notifications/threads/{id}"))
        .auth(&w.bob)
        .send()
        .await
        .assert_status(205);
    let t = w
        .app
        .get(&format!("/api/v3/notifications/threads/{id}"))
        .auth(&w.bob)
        .send()
        .await;
    t.assert_status(200);
    assert_eq!(t.json()["unread"], false);
    assert!(t.json()["last_read_at"].is_string());
    assert_eq!(list(&w, &w.bob, "").await.len(), 1);
    assert_eq!(list(&w, &w.bob, "?all=true").await.len(), 2);

    // Web client: mark unread again, synced with the exact client shape.
    w.app
        .delete(&format!("/_bgh/notifications/threads/{id}/read"))
        .auth(&w.bob)
        .send()
        .await
        .assert_status(204);
    assert_eq!(list(&w, &w.bob, "").await.len(), 2);
    let d: Value = sqlx::query_scalar(
        "SELECT data FROM sync_actions WHERE scope = $1 AND model = 'notification' ORDER BY id DESC LIMIT 1",
    )
    .bind(format!("user:{}", w.bob.id))
    .fetch_one(&w.app.state.db)
    .await
    .unwrap();
    let mut keys: Vec<&str> = d.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "id",
            "lastReadAt",
            "reason",
            "repoId",
            "subjectId",
            "subjectType",
            "title",
            "unread",
            "updatedAt"
        ]
    );
    assert_eq!(d["unread"], true);
    assert!(d["updatedAt"].as_str().unwrap().ends_with('Z'));
    w.app
        .delete(&format!("/_bgh/notifications/threads/{id}/read"))
        .auth(&w.carol)
        .send()
        .await
        .assert_status(404);
    w.app
        .patch(&format!("/api/v3/notifications/threads/{id}"))
        .auth(&w.bob)
        .send()
        .await
        .assert_status(205);

    // Other users can't see the thread.
    w.app
        .get(&format!("/api/v3/notifications/threads/{id}"))
        .auth(&w.carol)
        .send()
        .await
        .assert_status(404);

    // Mark everything read.
    w.app
        .put("/api/v3/notifications")
        .auth(&w.bob)
        .json(&json!({}))
        .send()
        .await
        .assert_status(205);
    assert!(list(&w, &w.bob, "").await.is_empty());
    // Repo-level mark unread with read=false.
    w.app
        .put("/api/v3/repos/alice/hello/notifications")
        .auth(&w.bob)
        .json(&json!({"read": false}))
        .send()
        .await
        .assert_status(205);
    assert_eq!(list(&w, &w.bob, "").await.len(), 2);

    // Done removes it from every listing and syncs a delete.
    w.app
        .delete(&format!("/api/v3/notifications/threads/{id}"))
        .auth(&w.bob)
        .send()
        .await
        .assert_status(204);
    assert_eq!(list(&w, &w.bob, "?all=true").await.len(), 1);
    let deleted: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM sync_actions WHERE scope = $1 AND model = 'notification' AND action = 'D'",
    )
    .bind(format!("user:{}", w.bob.id))
    .fetch_one(&w.app.state.db)
    .await
    .unwrap();
    assert_eq!(deleted, 1);

    // since/before filters and validation.
    assert!(
        list(&w, &w.bob, "?all=true&since=2999-01-01T00:00:00Z")
            .await
            .is_empty()
    );
    assert_eq!(
        list(&w, &w.bob, "?all=true&before=2999-01-01T00:00:00Z")
            .await
            .len(),
        1
    );
    w.app
        .get("/api/v3/notifications?since=yesterday")
        .auth(&w.bob)
        .send()
        .await
        .assert_status(422);

    // Requires authentication and the notifications scope.
    w.app
        .get("/api/v3/notifications")
        .send()
        .await
        .assert_status(401);
    let token = w.app.create_token(&w.bob, &["user"]).await;
    w.app
        .get("/api/v3/notifications")
        .token(&token)
        .send()
        .await
        .assert_status(403);
}

#[tokio::test]
async fn thread_subscriptions_mute_threads() {
    let w = world().await;
    let (issue_id, _) = open_issue(&w, &w.alice, "Noisy", "").await;
    let id = list(&w, &w.bob, "").await[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let path = format!("/api/v3/notifications/threads/{id}/subscription");

    // Watching the repo implies a subscription.
    let res = w.app.get(&path).auth(&w.bob).send().await;
    res.assert_status(200);
    let v = res.json();
    assert_eq!(v["subscribed"], true);
    assert_eq!(v["ignored"], false);
    assert_eq!(v["url"], w.app.url(&path));
    assert_eq!(
        v["thread_url"],
        w.app.url(&format!("/api/v3/notifications/threads/{id}"))
    );

    // Ignore the thread: further comments don't notify bob.
    let res = w
        .app
        .put(&path)
        .auth(&w.bob)
        .json(&json!({"ignored": true}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["ignored"], true);
    assert_eq!(res.json()["subscribed"], false);
    w.app
        .patch(&format!("/api/v3/notifications/threads/{id}"))
        .auth(&w.bob)
        .send()
        .await
        .assert_status(205);
    let c = insert_comment(&w.app, w.repo_id, issue_id, &w.alice, "more @bob").await;
    w.app.state.events.emit(Event::IssueCommentCreated {
        repo_id: w.repo_id,
        issue_id,
        comment_id: c,
        actor_id: w.alice.id,
    });
    w.probe.settle(&w.app).await;
    assert!(
        list(&w, &w.bob, "").await.is_empty(),
        "ignored thread stays quiet, even for mentions"
    );

    // DELETE: unsubscribed (not ignored); a mention brings bob back.
    w.app
        .delete(&path)
        .auth(&w.bob)
        .send()
        .await
        .assert_status(204);
    let v = w.app.get(&path).auth(&w.bob).send().await.json();
    assert_eq!(
        (v["subscribed"].clone(), v["ignored"].clone()),
        (json!(false), json!(false))
    );
    let c = insert_comment(&w.app, w.repo_id, issue_id, &w.alice, "plain comment").await;
    w.app.state.events.emit(Event::IssueCommentCreated {
        repo_id: w.repo_id,
        issue_id,
        comment_id: c,
        actor_id: w.alice.id,
    });
    w.probe.settle(&w.app).await;
    assert!(
        list(&w, &w.bob, "").await.is_empty(),
        "unsubscribed thread ignores watch"
    );
    let c = insert_comment(&w.app, w.repo_id, issue_id, &w.alice, "hey @bob").await;
    w.app.state.events.emit(Event::IssueCommentCreated {
        repo_id: w.repo_id,
        issue_id,
        comment_id: c,
        actor_id: w.alice.id,
    });
    w.probe.settle(&w.app).await;
    let threads = list(&w, &w.bob, "").await;
    assert_eq!(threads.len(), 1);
    assert_eq!(threads[0]["reason"], "mention");
}

#[tokio::test]
async fn repository_watching() {
    let w = world().await;
    let path = "/api/v3/repos/alice/hello/subscription";
    // carol isn't watching: 404.
    w.app
        .get(path)
        .auth(&w.carol)
        .send()
        .await
        .assert_status(404);
    let count = |app: &TestApp| {
        let db = app.state.db.clone();
        async move {
            sqlx::query_scalar::<_, i64>(
                "SELECT watchers_count FROM repositories WHERE name = 'hello'",
            )
            .fetch_one(&db)
            .await
            .unwrap()
        }
    };
    let before = count(&w.app).await;
    let res = w
        .app
        .put(path)
        .auth(&w.carol)
        .json(&json!({"subscribed": true}))
        .send()
        .await;
    res.assert_status(200);
    let v = res.json();
    assert_eq!(v["subscribed"], true);
    assert_eq!(v["ignored"], false);
    assert!(v["reason"].is_null());
    assert_eq!(v["url"], w.app.url(path));
    assert_eq!(v["repository_url"], w.app.url("/api/v3/repos/alice/hello"));
    assert!(v["created_at"].as_str().unwrap().ends_with('Z'));
    assert_eq!(count(&w.app).await, before + 1);
    let get = w.app.get(path).auth(&w.carol).send().await;
    get.assert_status(200);
    assert_eq!(get.json()["subscribed"], true);

    // Ignoring: no notifications at all, not even mentions.
    let res = w
        .app
        .put(path)
        .auth(&w.carol)
        .json(&json!({"ignored": true}))
        .send()
        .await;
    assert_eq!(res.json()["subscribed"], false);
    assert_eq!(res.json()["ignored"], true);
    assert_eq!(count(&w.app).await, before);
    open_issue(&w, &w.alice, "Hi", "hello @carol").await;
    assert!(list(&w, &w.carol, "?all=true").await.is_empty());

    // viewerRepo sync row for the client.
    let watching: Value = sqlx::query_scalar(
        "SELECT data FROM sync_actions WHERE scope = $1 AND model = 'viewerRepo' ORDER BY id DESC LIMIT 1",
    )
    .bind(format!("user:{}", w.carol.id))
    .fetch_one(&w.app.state.db)
    .await
    .unwrap();
    assert_eq!(watching["watching"], "ignored");

    w.app
        .delete(path)
        .auth(&w.carol)
        .send()
        .await
        .assert_status(204);
    w.app
        .get(path)
        .auth(&w.carol)
        .send()
        .await
        .assert_status(404);

    // Private repos: 404 without access.
    w.app.create_private_repo(&w.alice, "private").await;
    w.app
        .put("/api/v3/repos/alice/private/subscription")
        .auth(&w.carol)
        .json(&json!({"subscribed": true}))
        .send()
        .await
        .assert_status(404);
}

#[tokio::test]
async fn custom_repository_watching() {
    let w = world().await;
    let path = "/_bgh/repos/alice/hello/subscription";
    let rest = "/api/v3/repos/alice/hello/subscription";
    let watchers = |app: &TestApp| {
        let db = app.state.db.clone();
        async move {
            sqlx::query_scalar::<_, i64>(
                "SELECT watchers_count FROM repositories WHERE name = 'hello'",
            )
            .fetch_one(&db)
            .await
            .unwrap()
        }
    };
    let viewer_repo = |app: &TestApp, user_id: i64| {
        let db = app.state.db.clone();
        async move {
            sqlx::query_scalar::<_, Value>(
                "SELECT data FROM sync_actions WHERE scope = $1 AND model = 'viewerRepo'
                  ORDER BY id DESC LIMIT 1",
            )
            .bind(format!("user:{user_id}"))
            .fetch_one(&db)
            .await
            .unwrap()
        }
    };

    // Auth required; defaults.
    w.app.get(path).send().await.assert_status(401);
    let v = w.app.get(path).auth(&w.carol).send().await.json();
    assert_eq!(v, json!({"state": "participating", "events": []}));
    let v = w.app.get(path).auth(&w.bob).send().await.json();
    assert_eq!(v, json!({"state": "all", "events": []}));

    // Custom: subscribed (counts as a watcher), viewerRepo `subscribed`.
    let before = watchers(&w.app).await;
    let res = w
        .app
        .put(path)
        .auth(&w.carol)
        .json(&json!({"state": "custom", "events": ["releases", "pulls", "releases"]}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(
        res.json(),
        json!({"state": "custom", "events": ["releases", "pulls"]})
    );
    let v = w.app.get(path).auth(&w.carol).send().await.json();
    assert_eq!(
        v,
        json!({"state": "custom", "events": ["releases", "pulls"]})
    );
    assert_eq!(watchers(&w.app).await, before + 1);
    let vr = viewer_repo(&w.app, w.carol.id).await;
    assert_eq!(vr["watching"], "subscribed");
    assert_eq!(vr["id"], w.repo_id);
    let v = w.app.get(rest).auth(&w.carol).send().await.json();
    assert_eq!(v["subscribed"], true);

    // Validation: unknown / empty events, unknown state.
    for body in [
        json!({"state": "custom", "events": ["issues", "wiki"]}),
        json!({"state": "custom", "events": []}),
        json!({"state": "custom"}),
        json!({"state": "sometimes"}),
        json!({}),
    ] {
        w.app
            .put(path)
            .auth(&w.carol)
            .json(&body)
            .send()
            .await
            .assert_status(422);
    }
    let v = w.app.get(path).auth(&w.carol).send().await.json();
    assert_eq!(v["state"], "custom", "unchanged after rejected writes");

    // Ignore / all / participating.
    let v = w
        .app
        .put(path)
        .auth(&w.carol)
        .json(&json!({"state": "ignore", "events": ["issues"]}))
        .send()
        .await
        .json();
    assert_eq!(v, json!({"state": "ignore", "events": []}));
    assert_eq!(watchers(&w.app).await, before);
    assert_eq!(viewer_repo(&w.app, w.carol.id).await["watching"], "ignored");
    let v = w
        .app
        .put(path)
        .auth(&w.carol)
        .json(&json!({"state": "all"}))
        .send()
        .await
        .json();
    assert_eq!(v, json!({"state": "all", "events": []}));
    assert_eq!(watchers(&w.app).await, before + 1);
    let v = w
        .app
        .put(path)
        .auth(&w.carol)
        .json(&json!({"state": "participating"}))
        .send()
        .await
        .json();
    assert_eq!(v, json!({"state": "participating", "events": []}));
    assert_eq!(watchers(&w.app).await, before);
    assert_eq!(
        viewer_repo(&w.app, w.carol.id).await["watching"],
        "participating"
    );
    w.app
        .get(rest)
        .auth(&w.carol)
        .send()
        .await
        .assert_status(404);

    // The REST PUT resets custom events to all activity.
    w.app
        .put(path)
        .auth(&w.carol)
        .json(&json!({"state": "custom", "events": ["issues"]}))
        .send()
        .await
        .assert_status(200);
    w.app
        .put(rest)
        .auth(&w.carol)
        .json(&json!({"subscribed": true}))
        .send()
        .await
        .assert_status(200);
    let v = w.app.get(path).auth(&w.carol).send().await.json();
    assert_eq!(v, json!({"state": "all", "events": []}));

    // Private repo without read access: 404 for both methods.
    w.app.create_private_repo(&w.alice, "private").await;
    let private = "/_bgh/repos/alice/private/subscription";
    w.app
        .get(private)
        .auth(&w.carol)
        .send()
        .await
        .assert_status(404);
    w.app
        .put(private)
        .auth(&w.carol)
        .json(&json!({"state": "all"}))
        .send()
        .await
        .assert_status(404);
    // The owner can read it (creating a repository watches it).
    let v = w.app.get(private).auth(&w.alice).send().await.json();
    assert_eq!(v["state"], "all");
}

#[tokio::test]
async fn custom_watchers_only_get_their_categories() {
    let w = world().await;
    let dave = w.app.create_user("dave").await;
    let path = "/_bgh/repos/alice/hello/subscription";
    for (user, events) in [(&w.carol, json!(["releases"])), (&dave, json!(["issues"]))] {
        w.app
            .put(path)
            .auth(user)
            .json(&json!({"state": "custom", "events": events}))
            .send()
            .await
            .assert_status(200);
    }
    open_issue(&w, &w.alice, "Custom watching", "body").await;
    assert!(
        list(&w, &w.carol, "?all=true").await.is_empty(),
        "releases-only watcher gets no issue notification"
    );
    let n = list(&w, &dave, "").await;
    assert_eq!(n.len(), 1);
    assert_eq!(n[0]["reason"], "subscribed");
    assert_eq!(n[0]["subject"]["type"], "Issue");
    assert_eq!(list(&w, &w.bob, "").await.len(), 1, "all-activity watcher");

    // Direct reasons still reach custom watchers.
    open_issue(&w, &w.alice, "Ping", "hey @carol").await;
    let n = list(&w, &w.carol, "").await;
    assert_eq!(n.len(), 1);
    assert_eq!(n[0]["reason"], "mention");
}

#[tokio::test]
async fn pull_request_reasons() {
    let w = world().await;
    let (pull_id, number) =
        insert_issue(&w.app, w.repo_id, &w.alice, "Add feature", "", true).await;
    sqlx::query("INSERT INTO pr_requested_reviewers (pull_id, user_id) VALUES ($1, $2)")
        .bind(pull_id)
        .bind(w.carol.id)
        .execute(&w.app.state.db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO issue_assignees (issue_id, user_id) VALUES ($1, $2)")
        .bind(pull_id)
        .bind(w.bob.id)
        .execute(&w.app.state.db)
        .await
        .unwrap();
    w.app.state.events.emit(Event::PullRequestOpened {
        repo_id: w.repo_id,
        pull_id,
        actor_id: w.alice.id,
    });
    w.probe.settle(&w.app).await;
    let carol = list(&w, &w.carol, "").await;
    assert_eq!(carol[0]["reason"], "review_requested");
    assert_eq!(carol[0]["subject"]["type"], "PullRequest");
    assert_eq!(
        carol[0]["subject"]["url"],
        w.app
            .url(&format!("/api/v3/repos/alice/hello/pulls/{number}"))
    );
    assert_eq!(list(&w, &w.bob, "").await[0]["reason"], "assign");

    // A review notifies participants (alice: author) with latest_comment_url.
    let review_id: i64 = sqlx::query_scalar(
        "INSERT INTO pr_reviews (pull_id, repo_id, user_id, body, state, submitted_at)
         VALUES ($1, $2, $3, 'LGTM', 'APPROVED', now()) RETURNING id",
    )
    .bind(pull_id)
    .bind(w.repo_id)
    .bind(w.carol.id)
    .fetch_one(&w.app.state.db)
    .await
    .unwrap();
    w.app.state.events.emit(Event::PullRequestReviewSubmitted {
        repo_id: w.repo_id,
        pull_id,
        review_id,
        actor_id: w.carol.id,
    });
    w.probe.settle(&w.app).await;
    let alice = list(&w, &w.alice, "").await;
    assert_eq!(alice[0]["reason"], "author");
    assert_eq!(
        alice[0]["subject"]["latest_comment_url"],
        w.app.url(&format!(
            "/api/v3/repos/alice/hello/pulls/{number}/reviews/{review_id}"
        ))
    );

    // Merging: state_change subscription for the merger; others notified.
    sqlx::query("UPDATE notifications SET unread = false")
        .execute(&w.app.state.db)
        .await
        .unwrap();
    w.app.state.events.emit(Event::PullRequestMerged {
        repo_id: w.repo_id,
        pull_id,
        actor_id: w.bob.id,
        merge_commit_sha: "c".repeat(40),
    });
    w.probe.settle(&w.app).await;
    assert_eq!(list(&w, &w.alice, "").await.len(), 1);
    assert_eq!(list(&w, &w.carol, "").await.len(), 1);
    assert!(
        list(&w, &w.bob, "").await.is_empty(),
        "merger isn't notified"
    );

    // Title edits propagate to existing threads.
    sqlx::query("UPDATE issues SET title = 'Renamed' WHERE id = $1")
        .bind(pull_id)
        .execute(&w.app.state.db)
        .await
        .unwrap();
    w.app.state.events.emit(Event::PullRequestEdited {
        repo_id: w.repo_id,
        pull_id,
        actor_id: w.alice.id,
        changes: json!({"title": {"from": "Add feature"}}),
    });
    w.probe.settle(&w.app).await;
    assert_eq!(
        list(&w, &w.carol, "?all=true").await[0]["subject"]["title"],
        "Renamed"
    );
}

#[tokio::test]
async fn assignment_review_request_and_team_mentions() {
    let w = world().await;
    let org = w.app.create_org("acme", &w.alice).await;
    w.app.add_org_member(&org, &w.carol, "member").await;
    w.app
        .create_repo_with(&w.alice, Some("acme"), json!({"name": "tools"}))
        .await;
    let rid = repo_id(&w.app, "acme", "tools").await;
    let team_id: i64 = sqlx::query_scalar(
        "INSERT INTO teams (org_id, name, slug) VALUES ($1, 'Core', 'core') RETURNING id",
    )
    .bind(org.id)
    .fetch_one(&w.app.state.db)
    .await
    .unwrap();
    sqlx::query("INSERT INTO team_members (team_id, user_id) VALUES ($1, $2)")
        .bind(team_id)
        .bind(w.carol.id)
        .execute(&w.app.state.db)
        .await
        .unwrap();

    let (id, _) = insert_issue(
        &w.app,
        rid,
        &w.alice,
        "Team",
        "@acme/core please look",
        false,
    )
    .await;
    w.app.state.events.emit(Event::IssueOpened {
        repo_id: rid,
        issue_id: id,
        actor_id: w.alice.id,
    });
    w.probe.settle(&w.app).await;
    let carol = list(&w, &w.carol, "").await;
    assert_eq!(carol.len(), 1);
    assert_eq!(carol[0]["reason"], "team_mention");

    // Assignment notifies only the assignee.
    w.app.state.events.emit(Event::IssueAssigned {
        repo_id: rid,
        issue_id: id,
        assignee_id: w.bob.id,
        actor_id: w.alice.id,
    });
    w.probe.settle(&w.app).await;
    // bob has no access to... acme/tools is public, so he can be assigned.
    let bob = list(&w, &w.bob, "").await;
    assert_eq!(bob.len(), 1);
    assert_eq!(bob[0]["reason"], "assign");
}

#[tokio::test]
async fn issue_page_subscription_and_settings() {
    let w = world().await;
    let (issue_id, number) = insert_issue(&w.app, w.repo_id, &w.alice, "Sub", "", false).await;
    let path = format!("/_bgh/repos/alice/hello/issues/{number}/subscription");
    let v = w.app.get(&path).auth(&w.carol).send().await.json();
    assert_eq!(v["subscribed"], false);
    assert_eq!(v["repository_watching"], "participating");
    let v = w
        .app
        .put(&path)
        .auth(&w.carol)
        .json(&json!({"subscribed": true}))
        .send()
        .await
        .json();
    assert_eq!(v["subscribed"], true);
    assert_eq!(v["reason"], "manual");
    let c = insert_comment(&w.app, w.repo_id, issue_id, &w.alice, "update").await;
    w.app.state.events.emit(Event::IssueCommentCreated {
        repo_id: w.repo_id,
        issue_id,
        comment_id: c,
        actor_id: w.alice.id,
    });
    w.probe.settle(&w.app).await;
    assert_eq!(list(&w, &w.carol, "").await[0]["reason"], "manual");

    // Web inbox settings: turning off `manual` stops new rows.
    let res = w
        .app
        .put("/_bgh/notifications/settings")
        .auth(&w.carol)
        .json(&json!({"web": {"manual": false}, "email": {"subscribed": false}}))
        .send()
        .await;
    res.assert_status(200);
    let s = res.json();
    assert_eq!(s["web"]["manual"], false);
    assert_eq!(s["web"]["mention"], true);
    assert_eq!(s["email"]["subscribed"], false);
    assert_eq!(s["email_enabled"], true);
    w.app
        .put("/_bgh/notifications/settings")
        .auth(&w.carol)
        .json(&json!({"web": {"bogus": true}}))
        .send()
        .await
        .assert_status(422);
    w.app
        .put("/_bgh/notifications/settings")
        .auth(&w.carol)
        .json(&json!({"notification_email": "someone@else.com"}))
        .send()
        .await
        .assert_status(422);
    w.app
        .delete(&format!(
            "/api/v3/notifications/threads/{}",
            list(&w, &w.carol, "").await[0]["id"].as_str().unwrap()
        ))
        .auth(&w.carol)
        .send()
        .await
        .assert_status(204);
    let c = insert_comment(&w.app, w.repo_id, issue_id, &w.alice, "again").await;
    w.app.state.events.emit(Event::IssueCommentCreated {
        repo_id: w.repo_id,
        issue_id,
        comment_id: c,
        actor_id: w.alice.id,
    });
    w.probe.settle(&w.app).await;
    assert!(list(&w, &w.carol, "?all=true").await.is_empty());
    let g = w
        .app
        .get("/_bgh/notifications/settings")
        .auth(&w.carol)
        .send()
        .await;
    assert_eq!(g.json()["web"]["manual"], false);
}

#[tokio::test]
async fn ci_activity_for_failed_suites() {
    let w = world().await;
    let suite: i64 = sqlx::query_scalar(
        "INSERT INTO check_suites (repo_id, head_sha, head_branch, status, conclusion)
         VALUES ($1, $2, 'main', 'completed', 'failure') RETURNING id",
    )
    .bind(w.repo_id)
    .bind("d".repeat(40))
    .fetch_one(&w.app.state.db)
    .await
    .unwrap();
    w.app.state.events.emit(Event::CheckSuiteUpdated {
        repo_id: w.repo_id,
        check_suite_id: suite,
        action: "completed".into(),
        actor_id: Some(w.alice.id),
    });
    w.probe.settle(&w.app).await;
    let alice = list(&w, &w.alice, "").await;
    assert_eq!(alice.len(), 1);
    assert_eq!(alice[0]["reason"], "ci_activity");
    assert_eq!(alice[0]["subject"]["type"], "CheckSuite");
    assert_eq!(
        alice[0]["subject"]["title"],
        "CI workflow run failed for main branch"
    );
    assert!(alice[0]["subject"]["latest_comment_url"].is_null());
    assert!(
        list(&w, &w.bob, "").await.is_empty(),
        "watchers don't get CI noise"
    );
}
