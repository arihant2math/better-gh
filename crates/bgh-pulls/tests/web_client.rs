//! Web client endpoints added by pulls-web: per-PR sync snapshot, pending
//! review comments, single-file patches.

mod common;

use bgh_core::testing::{TestApp, TestUser};
use common::*;
use serde_json::{Value, json};

async fn rest_comments(app: &TestApp, user: &TestUser) -> usize {
    app.get("/api/v3/repos/alice/demo/pulls/1/comments")
        .auth(user)
        .send()
        .await
        .json()
        .as_array()
        .unwrap()
        .len()
}

fn ids(v: &Value) -> Vec<i64> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_i64().unwrap())
        .collect()
}

/// Fixture + a whitespace-only change to `src/lib.rs` on `feature`, PR #1.
async fn setup() -> (Fixture, String) {
    let f = fixture().await;
    let head = commit(
        &f.app,
        f.repo_id,
        "feature",
        Some(&f.feature),
        &[("src/lib.rs", Some("pub fn  a() {}\n"))],
        "Reformat",
    )
    .await;
    open_pr(&f.app, &f.alice, "alice/demo", "feature", "main").await;
    (f, head)
}

#[tokio::test]
async fn pull_sync_snapshot() {
    let (f, head) = setup().await;
    let app = &f.app;
    let bob = app.create_user("bob").await;
    add_collaborator(app, f.repo_id, &bob, "write").await;

    // Statuses / checks: on the head and on main (the latter is not returned).
    for sha in [&head, &f.main] {
        app.post(&format!("/api/v3/repos/alice/demo/statuses/{sha}"))
            .auth(&f.alice)
            .json(&json!({"state": "success", "context": "ci"}))
            .send()
            .await
            .assert_status(201);
        app.post("/api/v3/repos/alice/demo/check-runs")
            .auth(&f.alice)
            .json(&json!({"name": "lint", "head_sha": sha, "status": "in_progress"}))
            .send()
            .await
            .assert_status(201);
    }

    // A public comment by bob with a reaction by alice.
    let res = app
        .post("/api/v3/repos/alice/demo/pulls/1/comments")
        .auth(&bob)
        .json(&json!({"body": "nice", "path": "README.md", "line": 3, "side": "RIGHT"}))
        .send()
        .await;
    res.assert_status(201);
    let public_id = res.json()["id"].as_i64().unwrap();
    app.post(&format!(
        "/api/v3/repos/alice/demo/pulls/comments/{public_id}/reactions"
    ))
    .auth(&f.alice)
    .json(&json!({"content": "+1"}))
    .send()
    .await
    .assert_status(201);

    // A pending comment by bob.
    let res = app
        .post("/_bgh/repos/alice/demo/pulls/1/reviews/pending/comments")
        .auth(&bob)
        .json(&json!({"body": "hmm", "path": "notes.txt", "line": 1}))
        .send()
        .await;
    res.assert_status(201);
    let pending = res.json();
    let pending_id = pending["comment"]["id"].as_i64().unwrap();
    let pending_review = pending["review"]["id"].as_i64().unwrap();

    // Bob sees his pending comment and review.
    let res = app
        .get("/_bgh/repos/alice/demo/pulls/1/sync")
        .auth(&bob)
        .send()
        .await;
    res.assert_status(200);
    let body = res.json();
    assert!(body["lastSyncId"].as_i64().unwrap() > 0);
    let m = &body["models"];
    for key in [
        "reviewComment",
        "review",
        "reaction",
        "checkSuite",
        "checkRun",
        "commitStatus",
        "user",
    ] {
        assert!(m[key].is_array(), "missing {key}");
    }
    assert_eq!(ids(&m["reviewComment"]), vec![public_id, pending_id]);
    let c = &m["reviewComment"][0];
    assert_eq!(c["authorId"], bob.id);
    assert_eq!(c["path"], "README.md");
    assert_eq!(c["line"], 3);
    assert!(c["diffHunk"].as_str().unwrap().starts_with("@@"));
    assert!(ids(&m["review"]).contains(&pending_review));
    let pr = m["review"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == pending_review)
        .unwrap();
    assert_eq!(pr["state"], "PENDING");
    assert!(pr["submittedAt"].is_null());
    assert_eq!(m["reaction"].as_array().unwrap().len(), 1);
    let r = &m["reaction"][0];
    assert_eq!(r["subjectId"], public_id);
    assert_eq!(r["userId"], f.alice.id);
    assert_eq!(r["content"], "+1");
    assert!(r["issueId"].is_i64());
    assert_eq!(m["checkRun"].as_array().unwrap().len(), 1);
    assert_eq!(m["checkRun"][0]["headSha"], head.as_str());
    assert_eq!(m["checkRun"][0]["name"], "lint");
    assert_eq!(m["checkSuite"].as_array().unwrap().len(), 1);
    assert_eq!(m["checkSuite"][0]["headSha"], head.as_str());
    assert_eq!(
        m["checkRun"][0]["checkSuiteId"],
        m["checkSuite"][0]["id"].clone()
    );
    assert_eq!(m["commitStatus"].as_array().unwrap().len(), 1);
    assert_eq!(m["commitStatus"][0]["sha"], head.as_str());
    assert_eq!(m["commitStatus"][0]["context"], "ci");
    let users = ids(&m["user"]);
    assert!(users.contains(&f.alice.id) && users.contains(&bob.id));
    assert!(m["user"][0]["login"].is_string());

    // Alice and anonymous viewers don't see bob's pending rows.
    for res in [
        app.get("/_bgh/repos/alice/demo/pulls/1/sync")
            .auth(&f.alice)
            .send()
            .await,
        app.get("/_bgh/repos/alice/demo/pulls/1/sync").send().await,
    ] {
        res.assert_status(200);
        let m = &res.json()["models"];
        assert_eq!(ids(&m["reviewComment"]), vec![public_id]);
        assert!(!ids(&m["review"]).contains(&pending_review));
    }

    // No read access → 404.
    app.create_private_repo(&f.alice, "secret").await;
    app.get("/_bgh/repos/alice/secret/pulls/1/sync")
        .auth(&bob)
        .send()
        .await
        .assert_status(404);
    app.get("/_bgh/repos/alice/demo/pulls/99/sync")
        .auth(&bob)
        .send()
        .await
        .assert_status(404);
}

#[tokio::test]
async fn pending_review_comments() {
    let (f, _head) = setup().await;
    let app = &f.app;
    let bob = app.create_user("bob").await;
    add_collaborator(app, f.repo_id, &bob, "write").await;
    let url = "/_bgh/repos/alice/demo/pulls/1/reviews/pending/comments";

    let res = app
        .post(url)
        .auth(&bob)
        .json(&json!({"body": "first", "path": "README.md", "line": 3, "side": "RIGHT"}))
        .send()
        .await;
    res.assert_status(201);
    let out = res.json();
    let review = &out["review"];
    let rid = review["id"].as_i64().unwrap();
    assert_eq!(review["state"], "PENDING");
    assert_eq!(review["authorId"], bob.id);
    assert_eq!(review["body"], "");
    assert!(review["submittedAt"].is_null());
    let first = &out["comment"];
    let first_id = first["id"].as_i64().unwrap();
    assert_eq!(first["reviewId"], rid);
    assert_eq!(first["line"], 3);
    assert_eq!(first["side"], "RIGHT");
    assert!(first["inReplyToId"].is_null());
    assert!(
        first["diffHunk"]
            .as_str()
            .unwrap()
            .contains("+line 2 changed")
    );

    // A reply into the pending review (threaded on the root).
    let res = app
        .post(url)
        .auth(&bob)
        .json(&json!({"body": "reply", "in_reply_to": first_id}))
        .send()
        .await;
    res.assert_status(201);
    let reply = res.json();
    assert_eq!(reply["review"]["id"], rid);
    assert_eq!(reply["comment"]["inReplyToId"], first_id);
    assert_eq!(reply["comment"]["path"], "README.md");
    let reply_id = reply["comment"]["id"].as_i64().unwrap();
    let res = app
        .post(url)
        .auth(&bob)
        .json(&json!({"body": "reply to reply", "in_reply_to": reply_id}))
        .send()
        .await;
    res.assert_status(201);
    assert_eq!(res.json()["comment"]["inReplyToId"], first_id);

    // Another top-level comment goes into the same pending review.
    let res = app
        .post(url)
        .auth(&bob)
        .json(&json!({"body": "file", "path": "notes.txt", "subject_type": "file"}))
        .send()
        .await;
    res.assert_status(201);
    assert_eq!(res.json()["review"]["id"], rid);
    assert_eq!(res.json()["comment"]["subjectType"], "file");

    // Validation.
    app.post(url)
        .auth(&bob)
        .json(&json!({"body": "  ", "path": "README.md", "line": 3}))
        .send()
        .await
        .assert_status(422);
    app.post(url)
        .auth(&bob)
        .json(&json!({"body": "x", "path": "README.md", "line": 99}))
        .send()
        .await
        .assert_status(422);
    app.post(url)
        .auth(&bob)
        .json(&json!({"body": "x", "path": "missing.txt", "line": 1}))
        .send()
        .await
        .assert_status(422);
    app.post(url)
        .json(&json!({"body": "x", "path": "README.md", "line": 3}))
        .send()
        .await
        .assert_status(401);

    // Private until submitted: not in REST lists of others, no sync actions.
    assert_eq!(rest_comments(app, &f.alice).await, 0);
    assert_eq!(rest_comments(app, &bob).await, 4);
    let synced: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM sync_actions WHERE model IN ('reviewComment', 'review')",
    )
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(synced, 0);
    let alice_sync = app
        .get("/_bgh/repos/alice/demo/pulls/1/sync")
        .auth(&f.alice)
        .send()
        .await
        .json();
    assert_eq!(
        alice_sync["models"]["reviewComment"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    let bob_sync = app
        .get("/_bgh/repos/alice/demo/pulls/1/sync")
        .auth(&bob)
        .send()
        .await
        .json();
    assert_eq!(
        bob_sync["models"]["reviewComment"]
            .as_array()
            .unwrap()
            .len(),
        4
    );

    // Submitting through REST makes everything public.
    let res = app
        .post(&format!(
            "/api/v3/repos/alice/demo/pulls/1/reviews/{rid}/events"
        ))
        .auth(&bob)
        .json(&json!({"event": "COMMENT"}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["state"], "COMMENTED");
    assert_eq!(rest_comments(app, &f.alice).await, 4);
    let alice_sync = app
        .get("/_bgh/repos/alice/demo/pulls/1/sync")
        .auth(&f.alice)
        .send()
        .await
        .json();
    assert_eq!(
        alice_sync["models"]["reviewComment"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    assert!(ids(&alice_sync["models"]["review"]).contains(&rid));

    // The next pending comment starts a new pending review.
    let res = app
        .post(url)
        .auth(&bob)
        .json(&json!({"body": "again", "path": "README.md", "line": 3}))
        .send()
        .await;
    res.assert_status(201);
    assert_ne!(res.json()["review"]["id"], rid);
}

#[tokio::test]
async fn single_file_patch() {
    let (f, _head) = setup().await;
    let app = &f.app;
    let get = |q: &str| app.get(&format!("/_bgh/repos/alice/demo/pulls/1/patch?{q}"));

    let res = get("path=src/lib.rs").send().await;
    res.assert_status(200);
    let p = res.json();
    assert_eq!(p["filename"], "src/lib.rs");
    assert!(p["previous_filename"].is_null());
    assert_eq!(p["status"], "modified");
    assert_eq!(p["additions"], 1);
    assert_eq!(p["deletions"], 1);
    assert_eq!(p["truncated"], false);
    assert!(p["patch"].as_str().unwrap().starts_with("@@"));
    assert!(p["patch"].as_str().unwrap().contains("+pub fn  a() {}"));

    // Whitespace-only change disappears with w=1.
    for w in ["w=1", "w=true"] {
        let p = get(&format!("path=src/lib.rs&{w}")).send().await.json();
        assert_eq!(p["status"], "modified");
        assert_eq!(p["additions"], 0);
        assert_eq!(p["deletions"], 0);
        assert!(p["patch"].is_null());
    }
    let p = get("path=README.md&w=1").send().await.json();
    assert_eq!(p["additions"], 1);
    assert!(p["patch"].as_str().unwrap().contains("+line 2 changed"));
    let p = get("path=notes.txt").send().await.json();
    assert_eq!(p["status"], "added");
    assert_eq!(p["patch"], "@@ -0,0 +1 @@\n+notes");

    get("path=nope.txt").send().await.assert_status(404);
    get("path=src").send().await.assert_status(404);
    get("w=1").send().await.assert_status(422);
    app.get("/_bgh/repos/alice/demo/pulls/42/patch?path=README.md")
        .send()
        .await
        .assert_status(404);

    // Renames: previous_filename and status.
    branch(app, f.repo_id, "rename", &f.main).await;
    commit(
        app,
        f.repo_id,
        "rename",
        Some(&f.main),
        &[
            ("README.md", None),
            (
                "GUIDE.md",
                Some("# Demo\n\nline 2\nline 3\nline 4\nline 5 changed\n"),
            ),
        ],
        "Rename readme",
    )
    .await;
    open_pr(app, &f.alice, "alice/demo", "rename", "main").await;
    let res = app
        .get("/_bgh/repos/alice/demo/pulls/2/patch?path=GUIDE.md")
        .send()
        .await;
    res.assert_status(200);
    let p = res.json();
    assert_eq!(p["status"], "renamed");
    assert_eq!(p["previous_filename"], "README.md");
    assert_eq!(p["additions"], 1);
    assert_eq!(p["deletions"], 1);
    assert!(p["patch"].as_str().unwrap().contains("+line 5 changed"));
    // The old name is not a file of the diff.
    app.get("/_bgh/repos/alice/demo/pulls/2/patch?path=README.md")
        .send()
        .await
        .assert_status(404);
}
