//! Releases, assets, generated notes and reactions.

use bgh_core::testing::{TestApp, TestUser};
use bgh_git::RepoStore;
use bgh_git::write::{CommitRequest, FileChange, Identity};
use serde_json::{Value, json};

fn store(app: &TestApp) -> RepoStore {
    RepoStore::from_config(&app.state.config)
}

/// Commit `files` on `branch` (on top of its current tip) and return the SHA.
async fn commit(
    app: &TestApp,
    repo_id: i64,
    branch: &str,
    files: &[(&str, &str)],
    msg: &str,
) -> String {
    let store = store(app);
    let b = branch.to_string();
    let parent = store
        .read(repo_id, move |r| r.resolve(&format!("refs/heads/{b}")))
        .await
        .unwrap();
    let changes: Vec<FileChange> = files
        .iter()
        .map(|(p, c)| FileChange::write(*p, c.as_bytes().to_vec()))
        .collect();
    bgh_git::write::commit_changes(
        &store,
        repo_id,
        CommitRequest {
            branch,
            parent: parent.as_deref(),
            changes: &changes,
            message: msg,
            author: &Identity::new("Test", "test@example.com"),
            committer: None,
        },
    )
    .await
    .unwrap()
}

async fn setup() -> (TestApp, TestUser, i64) {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let repo = app.create_repo(&alice, "demo").await;
    let id = repo["id"].as_i64().unwrap();
    commit(&app, id, "main", &[("README.md", "hi\n")], "init").await;
    (app, alice, id)
}

async fn create_release(app: &TestApp, user: &TestUser, body: Value) -> Value {
    let res = app
        .post("/api/v3/repos/alice/demo/releases")
        .auth(user)
        .json(&body)
        .send()
        .await;
    res.assert_status(201);
    res.json()
}

#[tokio::test]
async fn create_release_creates_tag_and_has_github_shape() {
    let (app, alice, repo_id) = setup().await;
    let res = app
        .post("/api/v3/repos/alice/demo/releases")
        .auth(&alice)
        .json(&json!({"tag_name": "v1.0.0", "name": "First", "body": "Hello @alice"}))
        .send()
        .await;
    res.assert_status(201);
    let v = res.json();
    let id = v["id"].as_i64().unwrap();
    assert_eq!(
        res.header("location").unwrap(),
        app.url(&format!("/api/v3/repos/alice/demo/releases/{id}"))
    );
    assert_eq!(v["tag_name"], "v1.0.0");
    assert_eq!(v["target_commitish"], "main");
    assert_eq!(v["name"], "First");
    assert_eq!(v["body"], "Hello @alice");
    assert_eq!(v["draft"], false);
    assert_eq!(v["prerelease"], false);
    assert_eq!(v["author"]["login"], "alice");
    assert_eq!(v["mentions_count"], 1);
    assert!(v["published_at"].as_str().unwrap().ends_with('Z'));
    assert_eq!(
        v["url"],
        app.url(&format!("/api/v3/repos/alice/demo/releases/{id}"))
    );
    assert_eq!(
        v["assets_url"],
        app.url(&format!("/api/v3/repos/alice/demo/releases/{id}/assets"))
    );
    assert_eq!(
        v["upload_url"],
        app.url(&format!(
            "/api/uploads/repos/alice/demo/releases/{id}/assets{{?name,label}}"
        ))
    );
    assert_eq!(v["html_url"], app.url("/alice/demo/releases/tag/v1.0.0"));
    assert_eq!(
        v["tarball_url"],
        app.url("/api/v3/repos/alice/demo/tarball/v1.0.0")
    );
    assert_eq!(
        v["zipball_url"],
        app.url("/api/v3/repos/alice/demo/zipball/v1.0.0")
    );
    assert!(v["node_id"].is_string());
    assert_eq!(v["assets"], json!([]));

    // The tag now exists at the default branch tip.
    let main = store(&app)
        .read(repo_id, |r| r.resolve_commit("main"))
        .await
        .unwrap();
    let tag = store(&app)
        .read(repo_id, |r| r.find_ref("refs/tags/v1.0.0"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(tag.peeled, main);

    // Same tag again → 422 already_exists.
    let res = app
        .post("/api/v3/repos/alice/demo/releases")
        .auth(&alice)
        .json(&json!({"tag_name": "v1.0.0"}))
        .send()
        .await;
    res.assert_status(422);
    assert_eq!(res.json()["errors"][0]["code"], "already_exists");

    // Missing tag name → 422 missing_field.
    let res = app
        .post("/api/v3/repos/alice/demo/releases")
        .auth(&alice)
        .json(&json!({"name": "x"}))
        .send()
        .await;
    res.assert_status(422);
    assert_eq!(res.json()["errors"][0]["code"], "missing_field");

    // Unknown target → 422 invalid target_commitish.
    let res = app
        .post("/api/v3/repos/alice/demo/releases")
        .auth(&alice)
        .json(&json!({"tag_name": "v9", "target_commitish": "nope"}))
        .send()
        .await;
    res.assert_status(422);
    assert_eq!(res.json()["errors"][0]["field"], "target_commitish");

    // Media types.
    let res = app
        .get(&format!("/api/v3/repos/alice/demo/releases/{id}"))
        .header("accept", "application/vnd.github.full+json")
        .send()
        .await;
    res.assert_status(200);
    let v = res.json();
    assert!(v["body_html"].as_str().unwrap().contains("<p>"));
    assert_eq!(v["body_text"], "Hello @alice");
    assert_eq!(v["body"], "Hello @alice");
}

#[tokio::test]
async fn existing_tag_is_reused_and_target_branch_works() {
    let (app, alice, repo_id) = setup().await;
    let base = store(&app)
        .read(repo_id, |r| r.resolve_commit("main"))
        .await
        .unwrap();
    // A second branch with its own commit.
    bgh_git::write::update_ref(&store(&app), repo_id, "refs/heads/dev", &base, None)
        .await
        .unwrap();
    let dev = commit(&app, repo_id, "dev", &[("dev.txt", "x")], "dev work").await;
    let v = create_release(
        &app,
        &alice,
        json!({"tag_name": "v2", "target_commitish": "dev", "prerelease": true}),
    )
    .await;
    assert_eq!(v["target_commitish"], "dev");
    assert_eq!(v["prerelease"], true);
    let tag = store(&app)
        .read(repo_id, |r| r.find_ref("refs/tags/v2"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(tag.peeled, dev);
}

#[tokio::test]
async fn drafts_latest_by_tag_list_and_publish() {
    let (app, alice, repo_id) = setup().await;
    let bob = app.create_user("bob").await;
    let draft = create_release(&app, &alice, json!({"tag_name": "v0.1", "draft": true})).await;
    let draft_id = draft["id"].as_i64().unwrap();
    assert_eq!(draft["draft"], true);
    assert!(draft["published_at"].is_null());
    assert!(draft["tarball_url"].is_null());
    // Drafts don't create tags.
    let tag = store(&app)
        .read(repo_id, |r| r.find_ref("refs/tags/v0.1"))
        .await
        .unwrap();
    assert!(tag.is_none());

    // Drafts are hidden from readers.
    let res = app
        .get(&format!("/api/v3/repos/alice/demo/releases/{draft_id}"))
        .auth(&bob)
        .send()
        .await;
    res.assert_status(404);
    let list = app
        .get("/api/v3/repos/alice/demo/releases")
        .auth(&bob)
        .send()
        .await;
    assert_eq!(list.json().as_array().unwrap().len(), 0);
    let list = app
        .get("/api/v3/repos/alice/demo/releases")
        .auth(&alice)
        .send()
        .await;
    assert_eq!(list.json().as_array().unwrap().len(), 1);
    app.get("/api/v3/repos/alice/demo/releases/latest")
        .send()
        .await
        .assert_status(404);
    app.get("/api/v3/repos/alice/demo/releases/tags/v0.1")
        .send()
        .await
        .assert_status(404);

    // Readers can't create releases.
    app.post("/api/v3/repos/alice/demo/releases")
        .auth(&bob)
        .json(&json!({"tag_name": "x"}))
        .send()
        .await
        .assert_status(403);

    // Publish the draft.
    let res = app
        .patch(&format!("/api/v3/repos/alice/demo/releases/{draft_id}"))
        .auth(&alice)
        .json(&json!({"draft": false, "name": "Zero point one"}))
        .send()
        .await;
    res.assert_status(200);
    let v = res.json();
    assert_eq!(v["draft"], false);
    assert_eq!(v["name"], "Zero point one");
    assert!(v["published_at"].is_string());
    assert!(
        store(&app)
            .read(repo_id, |r| r.find_ref("refs/tags/v0.1"))
            .await
            .unwrap()
            .is_some()
    );

    let latest = app
        .get("/api/v3/repos/alice/demo/releases/latest")
        .send()
        .await;
    latest.assert_status(200);
    assert_eq!(latest.json()["id"], draft_id);

    // A newer prerelease doesn't become latest; a newer release does.
    create_release(
        &app,
        &alice,
        json!({"tag_name": "v0.2-rc", "prerelease": true}),
    )
    .await;
    assert_eq!(
        app.get("/api/v3/repos/alice/demo/releases/latest")
            .send()
            .await
            .json()["tag_name"],
        "v0.1"
    );
    let v03 = create_release(&app, &alice, json!({"tag_name": "v0.3"})).await;
    assert_eq!(
        app.get("/api/v3/repos/alice/demo/releases/latest")
            .send()
            .await
            .json()["id"],
        v03["id"]
    );
    // make_latest=false on a newer release keeps the previous one.
    create_release(
        &app,
        &alice,
        json!({"tag_name": "v0.4", "make_latest": "false"}),
    )
    .await;
    assert_eq!(
        app.get("/api/v3/repos/alice/demo/releases/latest")
            .send()
            .await
            .json()["tag_name"],
        "v0.3"
    );
    // Explicitly mark an older one as latest.
    app.patch(&format!("/api/v3/repos/alice/demo/releases/{draft_id}"))
        .auth(&alice)
        .json(&json!({"make_latest": "true"}))
        .send()
        .await
        .assert_status(200);
    assert_eq!(
        app.get("/api/v3/repos/alice/demo/releases/latest")
            .send()
            .await
            .json()["tag_name"],
        "v0.1"
    );
    app.patch(&format!("/api/v3/repos/alice/demo/releases/{draft_id}"))
        .auth(&alice)
        .json(&json!({"make_latest": "bogus"}))
        .send()
        .await
        .assert_status(422);

    // By tag.
    let res = app
        .get("/api/v3/repos/alice/demo/releases/tags/v0.3")
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["id"], v03["id"]);

    // List order (newest first) and pagination.
    let res = app
        .get("/api/v3/repos/alice/demo/releases?per_page=2")
        .send()
        .await;
    res.assert_status(200);
    let tags: Vec<String> = res
        .json()
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["tag_name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(tags, vec!["v0.4", "v0.3"]);
    assert!(res.header("link").unwrap().contains("rel=\"next\""));

    // Delete.
    app.delete(&format!("/api/v3/repos/alice/demo/releases/{}", v03["id"]))
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    app.get(&format!("/api/v3/repos/alice/demo/releases/{}", v03["id"]))
        .send()
        .await
        .assert_status(404);
    // The tag is kept.
    assert!(
        store(&app)
            .read(repo_id, |r| r.find_ref("refs/tags/v0.3"))
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn private_repo_releases_are_hidden() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    app.create_repo_with(
        &alice,
        None,
        json!({"name": "secret", "private": true, "auto_init": true}),
    )
    .await;
    let res = app
        .post("/api/v3/repos/alice/secret/releases")
        .auth(&alice)
        .json(&json!({"tag_name": "v1"}))
        .send()
        .await;
    res.assert_status(201);
    let id = res.json()["id"].as_i64().unwrap();
    app.get("/api/v3/repos/alice/secret/releases")
        .auth(&bob)
        .send()
        .await
        .assert_status(404);
    app.get(&format!("/api/v3/repos/alice/secret/releases/{id}"))
        .send()
        .await
        .assert_status(404);
    app.get(&format!("/api/v3/repos/alice/secret/releases/{id}"))
        .auth(&alice)
        .send()
        .await
        .assert_status(200);
}

#[tokio::test]
async fn assets_upload_download_update_delete() {
    let (app, alice, _) = setup().await;
    let bob = app.create_user("bob").await;
    let rel = create_release(&app, &alice, json!({"tag_name": "v1"})).await;
    let id = rel["id"].as_i64().unwrap();

    // Upload via the uploads host path advertised in upload_url.
    let res = app
        .post(&format!(
            "/api/uploads/repos/alice/demo/releases/{id}/assets?name=app%20linux.tar.gz&label=Linux"
        ))
        .auth(&alice)
        .header("content-type", "application/gzip")
        .body(b"binary-content".to_vec())
        .send()
        .await;
    res.assert_status(201);
    let a = res.json();
    let asset_id = a["id"].as_i64().unwrap();
    assert_eq!(a["name"], "app.linux.tar.gz");
    assert_eq!(a["label"], "Linux");
    assert_eq!(a["content_type"], "application/gzip");
    assert_eq!(a["size"], 14);
    assert_eq!(a["state"], "uploaded");
    assert_eq!(a["download_count"], 0);
    assert_eq!(a["uploader"]["login"], "alice");
    assert!(a["digest"].as_str().unwrap().starts_with("sha256:"));
    assert_eq!(
        a["url"],
        app.url(&format!(
            "/api/v3/repos/alice/demo/releases/assets/{asset_id}"
        ))
    );
    assert_eq!(
        a["browser_download_url"],
        app.url("/alice/demo/releases/download/v1/app.linux.tar.gz")
    );

    // Upload via the API path too; duplicate names are rejected.
    let res = app
        .post(&format!(
            "/api/v3/repos/alice/demo/releases/{id}/assets?name=notes.txt"
        ))
        .auth(&alice)
        .header("content-type", "text/plain")
        .body(b"binary-content".to_vec())
        .send()
        .await;
    res.assert_status(201);
    let notes_id = res.json()["id"].as_i64().unwrap();
    let res = app
        .post(&format!(
            "/api/v3/repos/alice/demo/releases/{id}/assets?name=notes.txt"
        ))
        .auth(&alice)
        .body(b"x".to_vec())
        .send()
        .await;
    res.assert_status(422);
    assert_eq!(res.json()["errors"][0]["code"], "already_exists");
    app.post(&format!("/api/v3/repos/alice/demo/releases/{id}/assets"))
        .auth(&alice)
        .body(b"x".to_vec())
        .send()
        .await
        .assert_status(422);
    app.post(&format!(
        "/api/v3/repos/alice/demo/releases/{id}/assets?name=b.txt"
    ))
    .auth(&bob)
    .body(b"x".to_vec())
    .send()
    .await
    .assert_status(403);

    // Release JSON embeds assets.
    let rel = app
        .get(&format!("/api/v3/repos/alice/demo/releases/{id}"))
        .send()
        .await
        .json();
    assert_eq!(rel["assets"].as_array().unwrap().len(), 2);
    let list = app
        .get(&format!("/api/v3/repos/alice/demo/releases/{id}/assets"))
        .send()
        .await;
    list.assert_status(200);
    assert_eq!(list.json()[0]["id"], asset_id);

    // Download through the API with octet-stream, and the browser route.
    let res = app
        .get(&format!(
            "/api/v3/repos/alice/demo/releases/assets/{asset_id}"
        ))
        .header("accept", "application/octet-stream")
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.text(), "binary-content");
    assert_eq!(
        res.header("content-type").unwrap(),
        "application/octet-stream"
    );
    assert!(
        res.header("content-disposition")
            .unwrap()
            .contains("app.linux.tar.gz")
    );
    let res = app
        .get("/alice/demo/releases/download/v1/app.linux.tar.gz")
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.text(), "binary-content");
    app.get("/alice/demo/releases/download/v1/missing.bin")
        .send()
        .await
        .assert_status(404);
    let a = app
        .get(&format!(
            "/api/v3/repos/alice/demo/releases/assets/{asset_id}"
        ))
        .send()
        .await
        .json();
    assert_eq!(a["download_count"], 2);

    // Update.
    let res = app
        .patch(&format!(
            "/api/v3/repos/alice/demo/releases/assets/{asset_id}"
        ))
        .auth(&alice)
        .json(&json!({"name": "app.tgz", "label": "Linux x64"}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["name"], "app.tgz");
    assert_eq!(res.json()["label"], "Linux x64");
    app.patch(&format!(
        "/api/v3/repos/alice/demo/releases/assets/{asset_id}"
    ))
    .auth(&alice)
    .json(&json!({"name": "notes.txt"}))
    .send()
    .await
    .assert_status(422);

    // Both assets share one blob (same content); deleting one keeps it.
    app.delete(&format!(
        "/api/v3/repos/alice/demo/releases/assets/{notes_id}"
    ))
    .auth(&alice)
    .send()
    .await
    .assert_status(204);
    let res = app
        .get("/alice/demo/releases/download/v1/app.tgz")
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.text(), "binary-content");

    // Deleting the release removes assets and the orphaned blob.
    let digest = a["digest"]
        .as_str()
        .unwrap()
        .trim_start_matches("sha256:")
        .to_string();
    let blob = app
        .state
        .config
        .data_dir
        .join("files/release-assets")
        .join(&digest[..2])
        .join(&digest);
    assert!(blob.exists());
    app.delete(&format!("/api/v3/repos/alice/demo/releases/{id}"))
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    assert!(!blob.exists());
    app.get(&format!(
        "/api/v3/repos/alice/demo/releases/assets/{asset_id}"
    ))
    .send()
    .await
    .assert_status(404);
}

#[tokio::test]
async fn draft_assets_need_write_access() {
    let (app, alice, _) = setup().await;
    let rel = create_release(&app, &alice, json!({"tag_name": "v1", "draft": true})).await;
    let id = rel["id"].as_i64().unwrap();
    let res = app
        .post(&format!(
            "/api/v3/repos/alice/demo/releases/{id}/assets?name=a.bin"
        ))
        .auth(&alice)
        .body(b"abc".to_vec())
        .send()
        .await;
    res.assert_status(201);
    let asset_id = res.json()["id"].as_i64().unwrap();
    app.get(&format!(
        "/api/v3/repos/alice/demo/releases/assets/{asset_id}"
    ))
    .send()
    .await
    .assert_status(404);
    app.get(&format!(
        "/api/v3/repos/alice/demo/releases/assets/{asset_id}"
    ))
    .auth(&alice)
    .send()
    .await
    .assert_status(200);
    app.get("/alice/demo/releases/download/v1/a.bin")
        .send()
        .await
        .assert_status(404);
}

async fn merged_pr(
    app: &TestApp,
    repo_id: i64,
    author: &TestUser,
    number: i64,
    title: &str,
    sha: &str,
) {
    let issue_id: i64 = sqlx::query_scalar(
        "INSERT INTO issues (repo_id, number, title, author_id, is_pull_request, state, closed_at)
         VALUES ($1, $2, $3, $4, true, 'closed', now()) RETURNING id",
    )
    .bind(repo_id)
    .bind(number)
    .bind(title)
    .bind(author.id)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO pull_requests (issue_id, repo_id, head_repo_id, head_ref, head_sha, base_ref,
                                    base_sha, merge_commit_sha, merged, merged_at, merged_by_id)
         VALUES ($1, $2, $2, 'topic', $3, 'main', $3, $3, true, now(), $4)",
    )
    .bind(issue_id)
    .bind(repo_id)
    .bind(sha)
    .bind(author.id)
    .execute(&app.state.db)
    .await
    .unwrap();
}

#[tokio::test]
async fn generate_notes_lists_prs_since_previous_release() {
    let (app, alice, repo_id) = setup().await;
    let bob = app.create_user("bob").await;
    let c1 = commit(&app, repo_id, "main", &[("a.txt", "1")], "feature one").await;
    merged_pr(&app, repo_id, &alice, 1, "Add feature one", &c1).await;
    create_release(&app, &alice, json!({"tag_name": "v1.0"})).await;

    let c2 = commit(&app, repo_id, "main", &[("b.txt", "2")], "feature two").await;
    merged_pr(&app, repo_id, &bob, 2, "Add feature two", &c2).await;
    let c3 = commit(&app, repo_id, "main", &[("c.txt", "3")], "fix").await;
    merged_pr(&app, repo_id, &alice, 3, "Fix bug", &c3).await;

    let res = app
        .post("/api/v3/repos/alice/demo/releases/generate-notes")
        .auth(&alice)
        .json(&json!({"tag_name": "v1.1"}))
        .send()
        .await;
    res.assert_status(200);
    let v = res.json();
    assert_eq!(v["name"], "v1.1");
    let body = v["body"].as_str().unwrap();
    assert!(body.starts_with("## What's Changed\n"), "{body}");
    assert!(
        body.contains(&format!(
            "* Add feature two by @bob in {}",
            app.url("/alice/demo/pull/2")
        )),
        "{body}"
    );
    assert!(body.contains("* Fix bug by @alice"), "{body}");
    assert!(!body.contains("feature one"), "{body}");
    assert!(
        body.contains("## New Contributors\n* @bob made their first contribution"),
        "{body}"
    );
    assert!(!body.contains("@alice made their first"), "{body}");
    assert!(
        body.contains(&format!(
            "**Full Changelog**: {}",
            app.url("/alice/demo/compare/v1.0...v1.1")
        )),
        "{body}"
    );

    // Explicit previous tag (whole history from v1.0 is the same here),
    // unknown previous tag → 422, readers → 403.
    let res = app
        .post("/api/v3/repos/alice/demo/releases/generate-notes")
        .auth(&alice)
        .json(&json!({"tag_name": "v1.1", "previous_tag_name": "nope"}))
        .send()
        .await;
    res.assert_status(422);
    app.post("/api/v3/repos/alice/demo/releases/generate-notes")
        .auth(&bob)
        .json(&json!({"tag_name": "v1.1"}))
        .send()
        .await
        .assert_status(403);

    // generate_release_notes on create prepends the given body.
    let rel = create_release(
        &app,
        &alice,
        json!({"tag_name": "v1.1", "body": "Intro", "generate_release_notes": true}),
    )
    .await;
    assert_eq!(rel["name"], "v1.1");
    let body = rel["body"].as_str().unwrap();
    assert!(body.starts_with("Intro\n\n## What's Changed"), "{body}");

    // First release ever: everything, changelog links to commits.
    let res = app
        .post("/api/v3/repos/alice/demo/releases/generate-notes")
        .auth(&alice)
        .json(&json!({"tag_name": "v1.0"}))
        .send()
        .await;
    let body = res.json()["body"].as_str().unwrap().to_string();
    assert!(body.contains("Add feature one"), "{body}");
    assert!(
        body.contains(&app.url("/alice/demo/commits/v1.0")),
        "{body}"
    );
}

#[tokio::test]
async fn release_reactions() {
    let (app, alice, _) = setup().await;
    let bob = app.create_user("bob").await;
    let rel = create_release(&app, &alice, json!({"tag_name": "v1"})).await;
    let id = rel["id"].as_i64().unwrap();
    assert!(rel.get("reactions").is_none());
    let path = format!("/api/v3/repos/alice/demo/releases/{id}/reactions");

    let res = app
        .post(&path)
        .auth(&bob)
        .json(&json!({"content": "rocket"}))
        .send()
        .await;
    res.assert_status(201);
    let r = res.json();
    assert_eq!(r["content"], "rocket");
    assert_eq!(r["user"]["login"], "bob");
    assert!(r["node_id"].is_string());
    // Same reaction again → 200 with the existing one.
    let again = app
        .post(&path)
        .auth(&bob)
        .json(&json!({"content": "rocket"}))
        .send()
        .await;
    again.assert_status(200);
    assert_eq!(again.json()["id"], r["id"]);
    app.post(&path)
        .auth(&alice)
        .json(&json!({"content": "heart"}))
        .send()
        .await
        .assert_status(201);
    app.post(&path)
        .auth(&alice)
        .json(&json!({"content": "-1"}))
        .send()
        .await
        .assert_status(422);
    app.post(&path)
        .auth(&alice)
        .json(&json!({"content": "bogus"}))
        .send()
        .await
        .assert_status(422);
    app.post(&path)
        .json(&json!({"content": "heart"}))
        .send()
        .await
        .assert_status(401);

    let list = app.get(&path).send().await;
    list.assert_status(200);
    assert_eq!(list.json().as_array().unwrap().len(), 2);
    let list = app.get(&format!("{path}?content=heart")).send().await;
    assert_eq!(list.json().as_array().unwrap().len(), 1);

    let rel = app
        .get(&format!("/api/v3/repos/alice/demo/releases/{id}"))
        .send()
        .await
        .json();
    assert_eq!(rel["reactions"]["total_count"], 2);
    assert_eq!(rel["reactions"]["rocket"], 1);
    assert_eq!(rel["reactions"]["heart"], 1);
    assert_eq!(rel["reactions"]["url"], app.url(&path));

    // Only the author (or an admin) can delete a reaction.
    let rid = r["id"].as_i64().unwrap();
    let carol = app.create_user("carol").await;
    app.delete(&format!("{path}/{rid}"))
        .auth(&carol)
        .send()
        .await
        .assert_status(404);
    app.delete(&format!("{path}/{rid}"))
        .auth(&bob)
        .send()
        .await
        .assert_status(204);
    app.delete(&format!("{path}/{rid}"))
        .auth(&bob)
        .send()
        .await
        .assert_status(404);
}

#[tokio::test]
async fn events_are_emitted() {
    let (app, alice, repo_id) = setup().await;
    let mut events = app.state.events.subscribe();
    let rel = create_release(&app, &alice, json!({"tag_name": "v1"})).await;
    let names: Vec<&'static str> = std::iter::from_fn(|| events.try_recv().ok())
        .map(|e| e.name())
        .collect();
    assert_eq!(names, vec!["push", "release_created", "release_published"]);
    app.delete(&format!("/api/v3/repos/alice/demo/releases/{}", rel["id"]))
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    let ev = events.try_recv().unwrap();
    assert_eq!(ev.name(), "release_deleted");
    assert_eq!(ev.repo_id(), Some(repo_id));
}
