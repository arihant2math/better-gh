//! Review workflow (P38): commit-range diffs, server-side viewed state and
//! batch suggestion commits.

use crate::common;

use bgh_core::testing::{TestApp, TestUser};
use common::*;
use serde_json::{Value, json};

fn filenames(v: &Value) -> Vec<String> {
    let mut out: Vec<String> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["filename"].as_str().unwrap().to_string())
        .collect();
    out.sort();
    out
}

async fn files(
    app: &TestApp,
    user: Option<&TestUser>,
    query: &str,
) -> bgh_core::testing::TestResponse {
    let mut req = app.get(&format!("/_bgh/repos/alice/demo/pulls/1/files{query}"));
    if let Some(u) = user {
        req = req.auth(u);
    }
    req.send().await
}

/// Fixture + PR #1 + a second commit (`src/lib.rs`) pushed after bob's
/// review of the first one. Returns (fixture, bob, second commit).
async fn reviewed_pr() -> (Fixture, TestUser, String) {
    let f = fixture().await;
    let app = &f.app;
    open_pr(app, &f.alice, "alice/demo", "feature", "main").await;
    let bob = app.create_user("bob").await;
    add_collaborator(app, f.repo_id, &bob, "write").await;
    app.post("/api/v3/repos/alice/demo/pulls/1/reviews")
        .auth(&bob)
        .json(&json!({"event": "COMMENT", "body": "first pass"}))
        .send()
        .await
        .assert_status(200);
    let second = commit(
        app,
        f.repo_id,
        "feature",
        Some(&f.feature),
        &[("src/lib.rs", Some("pub fn a() {}\npub fn b() {}\n"))],
        "Add b",
    )
    .await;
    pushed(app, f.repo_id, &f.alice, "feature", &f.feature, &second).await;
    (f, bob, second)
}

#[tokio::test]
async fn commit_range_diffs() {
    let (f, bob, second) = reviewed_pr().await;
    let app = &f.app;

    // Default: the whole PR (merge base → head), REST entry shape.
    let res = files(app, Some(&bob), "").await;
    res.assert_status(200);
    let all = res.json();
    assert_eq!(filenames(&all), ["README.md", "notes.txt", "src/lib.rs"]);
    let entry = all
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["filename"] == "src/lib.rs")
        .unwrap();
    for key in [
        "sha",
        "filename",
        "status",
        "additions",
        "deletions",
        "changes",
        "blob_url",
        "raw_url",
        "contents_url",
        "patch",
    ] {
        assert!(entry.get(key).is_some(), "missing {key}");
    }
    assert_eq!(entry["status"], "modified");
    assert!(
        res.header("cache-control")
            .is_none_or(|c| !c.contains("immutable"))
    );

    // One commit: only its changes.
    let res = files(
        app,
        Some(&bob),
        &format!("?base_sha={}&head_sha={}", f.main, f.feature),
    )
    .await;
    res.assert_status(200);
    assert_eq!(filenames(&res.json()), ["README.md", "notes.txt"]);
    assert_eq!(
        res.header("cache-control"),
        Some("private, max-age=31536000, immutable")
    );
    let res = files(
        app,
        Some(&bob),
        &format!("?base_sha={}&head_sha={second}", f.feature),
    )
    .await;
    assert_eq!(filenames(&res.json()), ["src/lib.rs"]);
    // The entry's URLs point at the range head.
    assert!(res.json()[0]["raw_url"].as_str().unwrap().contains(&second));

    // "Changes since your last review": the review's commit → head.
    let reviews = app
        .get("/api/v3/repos/alice/demo/pulls/1/reviews")
        .auth(&bob)
        .send()
        .await
        .json();
    let reviewed_at = reviews[0]["commit_id"].as_str().unwrap().to_string();
    assert_eq!(reviewed_at, f.feature);
    let res = files(app, Some(&bob), &format!("?base_sha={reviewed_at}")).await;
    res.assert_status(200);
    assert_eq!(filenames(&res.json()), ["src/lib.rs"]);

    // A head inside the PR without a base: merge base → that commit.
    let res = files(app, Some(&bob), &format!("?head_sha={}", f.feature)).await;
    assert_eq!(filenames(&res.json()), ["README.md", "notes.txt"]);

    // Pagination with Link (page_with_total → next + last).
    let res = files(app, Some(&bob), "?per_page=1").await;
    res.assert_status(200);
    assert_eq!(res.json().as_array().unwrap().len(), 1);
    let link = res.header("link").unwrap();
    assert!(
        link.contains("rel=\"next\"") && link.contains("rel=\"last\""),
        "{link}"
    );

    // Errors: malformed SHA (422 with errors[]), unknown commit (422).
    let res = files(app, Some(&bob), "?base_sha=nope").await;
    res.assert_status(422);
    assert_eq!(res.json()["errors"][0]["field"], "base_sha");
    let res = files(app, Some(&bob), &format!("?head_sha={}", "1".repeat(40))).await;
    res.assert_status(422);
    assert!(
        res.json()["message"]
            .as_str()
            .unwrap()
            .contains("No commit found")
    );
    // Unknown PR → 404.
    app.get("/_bgh/repos/alice/demo/pulls/99/files")
        .auth(&bob)
        .send()
        .await
        .assert_status(404);
}

#[tokio::test]
async fn range_diff_of_private_repo_is_hidden() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let repo = app.create_private_repo(&alice, "demo").await;
    let repo_id = repo["id"].as_i64().unwrap();
    let main = commit(
        &app,
        repo_id,
        "main",
        None,
        &[("a.txt", Some("a\n"))],
        "init",
    )
    .await;
    branch(&app, repo_id, "feature", &main).await;
    commit(
        &app,
        repo_id,
        "feature",
        Some(&main),
        &[("a.txt", Some("b\n"))],
        "change",
    )
    .await;
    open_pr(&app, &alice, "alice/demo", "feature", "main").await;
    let eve = app.create_user("eve").await;
    files(&app, None, "").await.assert_status(404);
    files(&app, Some(&eve), "").await.assert_status(404);
    files(&app, Some(&alice), "").await.assert_status(200);
}

async fn viewed(app: &TestApp, user: &TestUser) -> Value {
    let res = app
        .get("/_bgh/repos/alice/demo/pulls/1/viewed")
        .auth(user)
        .send()
        .await;
    res.assert_status(200);
    res.json()
}

#[tokio::test]
async fn viewed_files_persist_and_reset_on_change() {
    let f = fixture().await;
    let app = &f.app;
    open_pr(app, &f.alice, "alice/demo", "feature", "main").await;
    let bob = app.create_user("bob").await;
    let url = "/_bgh/repos/alice/demo/pulls/1/viewed";

    // Anonymous: 401.
    app.put(url)
        .json(&json!({"path": "README.md"}))
        .send()
        .await
        .assert_status(401);

    // Mark (blob from the PR diff).
    let res = app
        .put(url)
        .auth(&bob)
        .json(&json!({"path": "README.md"}))
        .send()
        .await;
    res.assert_status(200);
    let marked = res.json();
    assert_eq!(marked["path"], "README.md");
    assert_eq!(marked["state"], "VIEWED");
    let entries = files(app, Some(&bob), "").await.json();
    let readme_sha = entries
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["filename"] == "README.md")
        .unwrap()["sha"]
        .clone();
    assert_eq!(marked["blob_sha"], readme_sha);
    // Idempotent.
    app.put(url)
        .auth(&bob)
        .json(&json!({"path": "README.md"}))
        .send()
        .await
        .assert_status(200);
    // Paths outside the diff and bad blobs: 422.
    let res = app
        .put(url)
        .auth(&bob)
        .json(&json!({"path": "nope.txt"}))
        .send()
        .await;
    res.assert_status(422);
    app.put(url)
        .auth(&bob)
        .json(&json!({"path": "notes.txt", "blob_sha": "xyz"}))
        .send()
        .await
        .assert_status(422);
    app.put(url)
        .auth(&bob)
        .json(&json!({}))
        .send()
        .await
        .assert_status(422);

    let v = viewed(app, &bob).await;
    assert_eq!(v.as_array().unwrap().len(), 1);
    assert_eq!(v[0]["state"], "VIEWED");
    // Per user: alice has nothing.
    assert_eq!(viewed(app, &f.alice).await, json!([]));

    // Synced in bob's user scope; the delta equals the `/sync` row; other
    // users' snapshots don't include it.
    let (scope, data): (String, Value) = sqlx::query_as(
        "SELECT scope, data FROM sync_actions WHERE model = 'viewedFile' ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(scope, format!("user:{}", bob.id));
    let snap = app
        .get("/_bgh/repos/alice/demo/pulls/1/sync")
        .auth(&bob)
        .send()
        .await
        .json();
    assert_eq!(snap["models"]["viewedFile"], json!([data.clone()]));
    assert_eq!(data["path"], "README.md");
    assert_eq!(data["blobSha"], readme_sha);
    assert_eq!(data["userId"], bob.id);
    let snap = app
        .get("/_bgh/repos/alice/demo/pulls/1/sync")
        .auth(&f.alice)
        .send()
        .await
        .json();
    assert_eq!(snap["models"]["viewedFile"], json!([]));
    let snap = app
        .get("/_bgh/repos/alice/demo/pulls/1/sync")
        .send()
        .await
        .json();
    assert_eq!(snap["models"]["viewedFile"], json!([]));

    // The file changes: no longer viewed (DISMISSED); untouched files keep it.
    app.put(url)
        .auth(&bob)
        .json(&json!({"path": "notes.txt"}))
        .send()
        .await
        .assert_status(200);
    let next = commit(
        app,
        f.repo_id,
        "feature",
        Some(&f.feature),
        &[(
            "README.md",
            Some("# Demo\n\nline 2 changed again\nline 3\nline 4\nline 5\n"),
        )],
        "More readme",
    )
    .await;
    pushed(app, f.repo_id, &f.alice, "feature", &f.feature, &next).await;
    let v = viewed(app, &bob).await;
    let state = |p: &str| {
        v.as_array()
            .unwrap()
            .iter()
            .find(|r| r["path"] == p)
            .unwrap_or_else(|| panic!("{p} missing in {v}"))["state"]
            .clone()
    };
    assert_eq!(state("README.md"), "DISMISSED");
    assert_eq!(state("notes.txt"), "VIEWED");
    let pull = bgh_pulls::model::load(&app.state, f.repo_id, 1)
        .await
        .unwrap();
    let states = bgh_pulls::viewed::states(&app.state, &pull, bob.id)
        .await
        .unwrap();
    assert_eq!(
        states["README.md"],
        bgh_pulls::viewed::ViewedState::Dismissed
    );
    assert_eq!(states["notes.txt"], bgh_pulls::viewed::ViewedState::Viewed);
    let alice = bgh_pulls::viewed::states(&app.state, &pull, f.alice.id)
        .await
        .unwrap();
    assert_eq!(alice["README.md"], bgh_pulls::viewed::ViewedState::Unviewed);

    // Unmark: 204, delete action in bob's scope, idempotent.
    let id = data["id"].as_i64().unwrap();
    app.delete(&format!("{url}?path=README.md"))
        .auth(&bob)
        .send()
        .await
        .assert_status(204);
    let (scope, action, d): (String, String, Value) = sqlx::query_as(
        "SELECT scope, action::text, data FROM sync_actions
          WHERE model = 'viewedFile' AND model_id = $1 ORDER BY id DESC LIMIT 1",
    )
    .bind(id)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(
        (scope.as_str(), action.as_str()),
        (format!("user:{}", bob.id).as_str(), "D")
    );
    assert_eq!(d, Value::Null);
    app.delete(&format!("{url}?path=README.md"))
        .auth(&bob)
        .send()
        .await
        .assert_status(204);
    app.delete(url).auth(&bob).send().await.assert_status(422);
    let v = viewed(app, &bob).await;
    assert_eq!(v.as_array().unwrap().len(), 1);
}

async fn suggest(
    app: &TestApp,
    user: &TestUser,
    path: &str,
    start: Option<i64>,
    line: i64,
    text: &str,
) -> i64 {
    let mut body = json!({
        "body": format!("How about:\n```suggestion\n{text}```\n"),
        "path": path,
        "line": line,
        "side": "RIGHT",
    });
    if let Some(s) = start {
        body["start_line"] = json!(s);
        body["start_side"] = json!("RIGHT");
    }
    let res = app
        .post("/api/v3/repos/alice/demo/pulls/1/comments")
        .auth(user)
        .json(&body)
        .send()
        .await;
    res.assert_status(201);
    res.json()["id"].as_i64().unwrap()
}

async fn read_file(app: &TestApp, repo_id: i64, rev: &str, path: &str) -> Vec<u8> {
    let (rev, path) = (rev.to_string(), path.to_string());
    store(app)
        .read(repo_id, move |r| match r.lookup_path(&rev, &path)? {
            bgh_git::PathLookup::Entry(e) => Ok(r.blob(&e.sha)?.data),
            _ => panic!("not a file"),
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn batch_suggestions_make_one_commit() {
    let f = fixture().await;
    let app = &f.app;
    let head = commit(
        app,
        f.repo_id,
        "feature",
        Some(&f.feature),
        &[("crlf.txt", Some("one\r\ntwo\r\nthree\r\nfour\r\n"))],
        "Add crlf file",
    )
    .await;
    open_pr(app, &f.alice, "alice/demo", "feature", "main").await;
    let bob = app.create_user("bob").await;
    let carol = app.create_user("carol").await;
    add_collaborator(app, f.repo_id, &bob, "write").await;

    let s1 = suggest(app, &bob, "crlf.txt", None, 1, "ONE\n").await;
    let s2 = suggest(app, &carol, "crlf.txt", Some(3), 4, "THREE\nFOUR\nFIVE\n").await;
    let s3 = suggest(app, &carol, "README.md", None, 3, "line 2 suggested\n").await;
    // A reply on s1's thread (the thread is resolved via its root).
    app.post(&format!(
        "/api/v3/repos/alice/demo/pulls/1/comments/{s1}/replies"
    ))
    .auth(&f.alice)
    .json(&json!({"body": "good idea"}))
    .send()
    .await
    .assert_status(201);
    let plain = app
        .post("/api/v3/repos/alice/demo/pulls/1/comments")
        .auth(&bob)
        .json(
            &json!({"body": "no suggestion here", "path": "notes.txt", "line": 1, "side": "RIGHT"}),
        )
        .send()
        .await
        .json()["id"]
        .as_i64()
        .unwrap();
    let url = "/_bgh/repos/alice/demo/pulls/1/suggestions/apply";

    // Readers can't apply; anonymous is 401.
    app.post(url)
        .json(&json!({"comment_ids": [s1]}))
        .send()
        .await
        .assert_status(401);
    app.post(url)
        .auth(&carol)
        .json(&json!({"comment_ids": [s1]}))
        .send()
        .await
        .assert_status(403);
    // Validation.
    app.post(url)
        .auth(&f.alice)
        .json(&json!({"comment_ids": []}))
        .send()
        .await
        .assert_status(422);
    app.post(url)
        .auth(&f.alice)
        .json(&json!({"comment_ids": [plain]}))
        .send()
        .await
        .assert_status(422);
    app.post(url)
        .auth(&f.alice)
        .json(&json!({"comment_ids": [s1], "expected_head_sha": f.main}))
        .send()
        .await
        .assert_status(422);
    app.post(url)
        .auth(&f.alice)
        .json(&json!({"comment_ids": [999_999]}))
        .send()
        .await
        .assert_status(422);
    assert_eq!(
        tip(app, f.repo_id, "feature").await.as_deref(),
        Some(head.as_str())
    );

    // Bob applies all three as one commit.
    let res = app
        .post(url)
        .auth(&bob)
        .json(&json!({
            "comment_ids": [s1, s2, s3],
            "message": "Apply review suggestions",
            "description": "Batch of three.",
            "expected_head_sha": head,
        }))
        .send()
        .await;
    res.assert_status(201);
    let out = res.json();
    let sha = out["commit_sha"].as_str().unwrap().to_string();
    let mut resolved: Vec<i64> = out["resolved_thread_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_i64().unwrap())
        .collect();
    resolved.sort();
    assert_eq!(resolved, vec![s1, s2, s3]);
    assert_eq!(
        tip(app, f.repo_id, "feature").await.as_deref(),
        Some(sha.as_str())
    );

    // One commit on top of the head, three files' worth of edits, CRLF kept.
    let c = app
        .get(&format!("/api/v3/repos/alice/demo/git/commits/{sha}"))
        .auth(&bob)
        .send()
        .await
        .json();
    assert_eq!(c["parents"].as_array().unwrap().len(), 1);
    assert_eq!(c["parents"][0]["sha"], head);
    let msg = c["message"].as_str().unwrap();
    assert!(
        msg.starts_with("Apply review suggestions\n\nBatch of three.\n\n"),
        "{msg}"
    );
    let carol_db = bgh_core::models::db::User::find(&app.state.db, carol.id)
        .await
        .unwrap()
        .unwrap();
    let carol_id = bgh_pulls::git::identity(&app.state, &carol_db)
        .await
        .unwrap();
    let trailers: Vec<&str> = msg
        .lines()
        .filter(|l| l.starts_with("Co-authored-by:"))
        .collect();
    // Bob is the author himself; carol wrote two suggestions: one trailer.
    assert_eq!(
        trailers,
        vec![format!("Co-authored-by: {} <{}>", carol_id.name, carol_id.email).as_str()]
    );
    let bob_db = bgh_core::models::db::User::find(&app.state.db, bob.id)
        .await
        .unwrap()
        .unwrap();
    let bob_id = bgh_pulls::git::identity(&app.state, &bob_db).await.unwrap();
    assert_eq!(c["author"]["email"], bob_id.email);
    assert_eq!(
        read_file(app, f.repo_id, &sha, "crlf.txt").await,
        b"ONE\r\ntwo\r\nTHREE\r\nFOUR\r\nFIVE\r\n"
    );
    assert_eq!(
        read_file(app, f.repo_id, &sha, "README.md").await,
        b"# Demo\n\nline 2 suggested\nline 3\nline 4\nline 5\n"
    );

    // Threads resolved by bob and synced.
    let rows: Vec<(i64, Option<i64>)> = sqlx::query_as(
        "SELECT id, resolved_by_id FROM pr_review_comments
          WHERE id = ANY($1) AND resolved_at IS NOT NULL ORDER BY id",
    )
    .bind(vec![s1, s2, s3])
    .fetch_all(&app.state.db)
    .await
    .unwrap();
    assert_eq!(
        rows,
        vec![(s1, Some(bob.id)), (s2, Some(bob.id)), (s3, Some(bob.id))]
    );
    let synced: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM sync_actions WHERE model = 'reviewComment' AND model_id = ANY($1)
            AND (data->>'resolvedById')::bigint = $2",
    )
    .bind(vec![s1, s2, s3])
    .bind(bob.id)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(synced, 3);

    // The push synchronizes the PR.
    settle(app).await;
    let pr = app
        .get("/api/v3/repos/alice/demo/pulls/1")
        .auth(&bob)
        .send()
        .await
        .json();
    assert_eq!(pr["head"]["sha"], sha);
    assert_eq!(pr["commits"], 3);
}

#[tokio::test]
async fn single_suggestion_default_message_and_conflicts() {
    let f = fixture().await;
    let app = &f.app;
    open_pr(app, &f.alice, "alice/demo", "feature", "main").await;
    let bob = app.create_user("bob").await;
    let a = suggest(app, &bob, "README.md", Some(3), 4, "x\n").await;
    let b = suggest(app, &bob, "README.md", None, 4, "y\n").await;
    let url = "/_bgh/repos/alice/demo/pulls/1/suggestions/apply";
    // Overlapping lines in one batch.
    let res = app
        .post(url)
        .auth(&f.alice)
        .json(&json!({"comment_ids": [a, b]}))
        .send()
        .await;
    res.assert_status(422);
    assert!(res.json()["message"].as_str().unwrap().contains("overlap"));
    // A pending suggestion can't be applied.
    let res = app
        .post("/_bgh/repos/alice/demo/pulls/1/reviews/pending/comments")
        .auth(&bob)
        .json(&json!({"body": "```suggestion\nz\n```", "path": "notes.txt", "line": 1}))
        .send()
        .await;
    res.assert_status(201);
    let pending = res.json()["comment"]["id"].as_i64().unwrap();
    app.post(url)
        .auth(&f.alice)
        .json(&json!({"comment_ids": [pending]}))
        .send()
        .await
        .assert_status(422);

    // Single: default headline, bob's trailer.
    let res = app
        .post(url)
        .auth(&f.alice)
        .json(&json!({"comment_ids": [b]}))
        .send()
        .await;
    res.assert_status(201);
    let sha = res.json()["commit_sha"].as_str().unwrap().to_string();
    let c = app
        .get(&format!("/api/v3/repos/alice/demo/git/commits/{sha}"))
        .auth(&f.alice)
        .send()
        .await
        .json();
    let msg = c["message"].as_str().unwrap();
    assert!(
        msg.starts_with("Apply suggestion from code review\n\nCo-authored-by: bob <"),
        "{msg}"
    );
    assert_eq!(
        read_file(app, f.repo_id, &sha, "README.md").await,
        b"# Demo\n\nline 2 changed\ny\nline 4\nline 5\n"
    );
    // The PR head moved: applying against the stale head is refused until
    // the PR is synchronized.
    let res = app
        .post(url)
        .auth(&f.alice)
        .json(&json!({"comment_ids": [a]}))
        .send()
        .await;
    res.assert_status(422);
}
