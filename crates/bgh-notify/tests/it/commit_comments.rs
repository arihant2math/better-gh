//! Commit comments: `commit_comment` webhook, notifications to the commit
//! author and mentioned users, and the `CommitCommentEvent` activity.

use bgh_git::RepoStore;
use bgh_git::write::{CommitRequest, FileChange, Identity};
use serde_json::{Value, json};

#[tokio::test]
async fn commit_comment_webhook_notifications_and_activity() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let carol = app.create_user("carol").await;
    app.create_repo(&alice, "hello").await;
    let repo_id: i64 = sqlx::query_scalar("SELECT id FROM repositories WHERE name = 'hello'")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    // A commit authored by bob (his verified email maps to the account).
    let changes = vec![FileChange::write("a.txt", b"1\n2\n".to_vec())];
    let sha = bgh_git::write::commit_changes(
        &RepoStore::from_config(&app.state.config),
        repo_id,
        CommitRequest {
            branch: "main",
            parent: None,
            changes: &changes,
            message: "Add a.txt\n\nbody",
            author: &Identity::new("Bob", "bob@example.com"),
            committer: None,
        },
    )
    .await
    .unwrap();
    let hook: i64 = sqlx::query_scalar(
        "INSERT INTO webhooks (url, content_type, events)
         VALUES ('http://127.0.0.1:9/hook', 'json', ARRAY['commit_comment']) RETURNING id",
    )
    .fetch_one(&app.state.db)
    .await
    .unwrap();

    let res = app
        .post(&format!("/api/v3/repos/alice/hello/commits/{sha}/comments"))
        .auth(&alice)
        .json(&json!({"body": "Looks good, cc @carol", "path": "a.txt", "line": 2}))
        .send()
        .await;
    res.assert_status(201);
    let comment = res.json();
    app.settle_events().await;

    // Webhook.
    let rows: Vec<(String, Option<String>, String)> = sqlx::query_as(
        "SELECT event, action, payload_raw FROM webhook_deliveries
          WHERE hook_id = $1 AND event <> 'ping'",
    )
    .bind(hook)
    .fetch_all(&app.state.db)
    .await
    .unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].0, "commit_comment");
    assert_eq!(rows[0].1.as_deref(), Some("created"));
    let payload: Value = serde_json::from_str(&rows[0].2).unwrap();
    assert_eq!(payload["action"], "created");
    for k in ["comment", "repository", "sender"] {
        assert!(payload.get(k).is_some(), "missing {k}");
    }
    assert_eq!(payload["comment"]["id"], comment["id"]);
    assert_eq!(payload["comment"]["commit_id"], sha);
    assert_eq!(payload["comment"]["path"], "a.txt");
    assert_eq!(payload["comment"]["body"], "Looks good, cc @carol");
    assert_eq!(payload["sender"]["login"], "alice");
    assert_eq!(payload["repository"]["full_name"], "alice/hello");

    // Notifications: the commit author (author) and the mention.
    let res = app.get("/api/v3/notifications").auth(&bob).send().await;
    res.assert_status(200);
    let n = res.json();
    assert_eq!(n.as_array().unwrap().len(), 1, "{n}");
    assert_eq!(n[0]["reason"], "author");
    assert_eq!(n[0]["subject"]["type"], "Commit");
    assert_eq!(n[0]["subject"]["title"], "Add a.txt");
    assert_eq!(
        n[0]["subject"]["url"],
        app.url(&format!("/api/v3/repos/alice/hello/commits/{sha}"))
    );
    assert_eq!(n[0]["subject"]["latest_comment_url"], comment["url"]);
    let n = app
        .get("/api/v3/notifications")
        .auth(&carol)
        .send()
        .await
        .json();
    assert_eq!(n[0]["reason"], "mention");
    // The commenter is not notified about their own comment.
    let n = app
        .get("/api/v3/notifications")
        .auth(&alice)
        .send()
        .await
        .json();
    assert_eq!(n, json!([]));

    // A reply by bob notifies alice (participant) in the same thread.
    app.post(&format!("/api/v3/repos/alice/hello/commits/{sha}/comments"))
        .auth(&bob)
        .json(&json!({"body": "thanks"}))
        .send()
        .await
        .assert_status(201);
    app.settle_events().await;
    let n = app
        .get("/api/v3/notifications")
        .auth(&alice)
        .send()
        .await
        .json();
    assert_eq!(n.as_array().unwrap().len(), 1, "{n}");
    assert_eq!(n[0]["subject"]["type"], "Commit");
    let threads: i64 = sqlx::query_scalar(
        "SELECT count(DISTINCT subject_id) FROM notifications WHERE subject_type = 'Commit'",
    )
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(threads, 1, "one thread per commit");

    // Activity.
    let events = app
        .get("/api/v3/users/alice/events/public")
        .send()
        .await
        .json();
    let ev = events
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["type"] == "CommitCommentEvent")
        .unwrap_or_else(|| panic!("no CommitCommentEvent in {events}"));
    assert_eq!(ev["payload"]["action"], "created");
    assert_eq!(ev["payload"]["comment"]["id"], comment["id"]);
}
