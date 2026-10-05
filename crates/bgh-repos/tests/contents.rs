//! Contents API (`/contents`, `/readme`) and raw downloads.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use bgh_core::events::Event;
use bgh_core::testing::{TestApp, TestUser};
use serde_json::{Value, json};

fn b64(s: impl AsRef<[u8]>) -> String {
    STANDARD.encode(s)
}

fn decode(v: &Value) -> Vec<u8> {
    let s: String = v
        .as_str()
        .unwrap()
        .chars()
        .filter(|c| *c != '\n')
        .collect();
    STANDARD.decode(s).unwrap()
}

async fn put(app: &TestApp, user: &TestUser, repo: &str, path: &str, body: Value) -> Value {
    let res = app
        .put(&format!("/api/v3/repos/{}/{repo}/contents/{path}", user.login))
        .auth(user)
        .json(&body)
        .send()
        .await;
    assert!(
        res.status() == 201 || res.status() == 200,
        "PUT {path}: {} {}",
        res.status(),
        res.text()
    );
    res.json()
}

async fn create_file(app: &TestApp, user: &TestUser, repo: &str, path: &str, content: &[u8]) {
    put(
        app,
        user,
        repo,
        path,
        json!({"message": format!("add {path}"), "content": b64(content)}),
    )
    .await;
}

/// `alice/r` with README.md, docs/guide.md, docs/notes.txt and a binary file.
async fn fixture() -> (TestApp, TestUser) {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    app.create_repo_with(&alice, None, json!({"name": "r", "auto_init": true}))
        .await;
    create_file(&app, &alice, "r", "docs/guide.md", b"# Guide\n\nSee #1.\n").await;
    create_file(&app, &alice, "r", "docs/notes.txt", b"a < b & c\n").await;
    create_file(&app, &alice, "r", "bin.dat", &[0, 1, 2, 3, 255]).await;
    (app, alice)
}

#[tokio::test]
async fn get_file_and_directory() {
    let (app, _alice) = fixture().await;

    let res = app.get("/api/v3/repos/alice/r/contents/README.md").send().await;
    res.assert_status(200);
    let f = res.json();
    assert_eq!(f["type"], "file");
    assert_eq!(f["encoding"], "base64");
    assert_eq!(f["name"], "README.md");
    assert_eq!(f["path"], "README.md");
    assert_eq!(f["size"], 4);
    assert_eq!(decode(&f["content"]), b"# r\n");
    assert!(f["content"].as_str().unwrap().ends_with('\n'));
    let sha = f["sha"].as_str().unwrap().to_string();
    // `git hash-object` of "# r\n"
    let blob = app
        .get(&format!("/api/v3/repos/alice/r/git/blobs/{sha}"))
        .send()
        .await;
    blob.assert_status(200);
    assert_eq!(decode(&blob.json()["content"]), b"# r\n");
    assert_eq!(
        f["url"],
        app.url("/api/v3/repos/alice/r/contents/README.md?ref=main")
    );
    assert_eq!(
        f["git_url"],
        app.url(&format!("/api/v3/repos/alice/r/git/blobs/{sha}"))
    );
    assert_eq!(f["html_url"], app.url("/alice/r/blob/main/README.md"));
    assert_eq!(f["download_url"], app.url("/alice/r/raw/main/README.md"));
    assert_eq!(f["_links"]["self"], f["url"]);
    assert_eq!(f["_links"]["git"], f["git_url"]);
    assert_eq!(f["_links"]["html"], f["html_url"]);

    // Root directory listing.
    let res = app.get("/api/v3/repos/alice/r/contents").send().await;
    res.assert_status(200);
    let list = res.json();
    let names: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["README.md", "bin.dat", "docs"]);
    let docs = &list[2];
    assert_eq!(docs["type"], "dir");
    assert_eq!(docs["size"], 0);
    assert!(docs["download_url"].is_null());
    assert!(docs.get("content").is_none());
    assert_eq!(docs["html_url"], app.url("/alice/r/tree/main/docs"));
    assert!(
        docs["git_url"]
            .as_str()
            .unwrap()
            .contains("/api/v3/repos/alice/r/git/trees/")
    );
    assert_eq!(list[1]["size"], 5);
    assert!(list[0].get("content").is_none());
    // Trailing slash is the root too.
    app.get("/api/v3/repos/alice/r/contents/")
        .send()
        .await
        .assert_status(200);

    // Subdirectory.
    let res = app.get("/api/v3/repos/alice/r/contents/docs").send().await;
    res.assert_status(200);
    let list = res.json();
    assert_eq!(list[0]["path"], "docs/guide.md");
    assert_eq!(list[1]["path"], "docs/notes.txt");

    // Missing path / ref.
    app.get("/api/v3/repos/alice/r/contents/nope")
        .send()
        .await
        .assert_status(404);
    app.get("/api/v3/repos/alice/r/contents/README.md/x")
        .send()
        .await
        .assert_status(404);
    app.get("/api/v3/repos/alice/r/contents/README.md?ref=nope")
        .send()
        .await
        .assert_status(404);
}

#[tokio::test]
async fn refs_and_history() {
    let (app, alice) = fixture().await;
    let first = app
        .get("/api/v3/repos/alice/r/contents/README.md")
        .send()
        .await
        .json();
    // Update README; the old commit still serves the old content.
    let res = put(
        &app,
        &alice,
        "r",
        "README.md",
        json!({"message": "update", "content": b64("v2\n"), "sha": first["sha"]}),
    )
    .await;
    let parent = res["commit"]["parents"][0]["sha"]
        .as_str()
        .unwrap()
        .to_string();
    let old = app
        .get(&format!(
            "/api/v3/repos/alice/r/contents/README.md?ref={parent}"
        ))
        .send()
        .await;
    old.assert_status(200);
    assert_eq!(decode(&old.json()["content"]), b"# r\n");
    assert_eq!(
        old.json()["url"],
        app.url(&format!(
            "/api/v3/repos/alice/r/contents/README.md?ref={parent}"
        ))
    );
    let new = app
        .get("/api/v3/repos/alice/r/contents/README.md?ref=main")
        .send()
        .await;
    assert_eq!(decode(&new.json()["content"]), b"v2\n");
    // Served again (cache hit) with identical content.
    let again = app
        .get("/api/v3/repos/alice/r/contents/README.md?ref=main")
        .send()
        .await;
    assert_eq!(again.json(), new.json());

    // Tags resolve too.
    let res = app
        .post("/api/v3/repos/alice/r/git/refs")
        .auth(&alice)
        .json(&json!({"ref": "refs/tags/v1", "sha": parent}))
        .send()
        .await;
    res.assert_status(201);
    let tagged = app
        .get("/api/v3/repos/alice/r/contents/README.md?ref=v1")
        .send()
        .await;
    assert_eq!(decode(&tagged.json()["content"]), b"# r\n");
}

#[tokio::test]
async fn media_types() {
    let (app, _alice) = fixture().await;

    let res = app
        .get("/api/v3/repos/alice/r/contents/bin.dat")
        .header("accept", "application/vnd.github.raw")
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.body.as_ref(), &[0, 1, 2, 3, 255]);
    assert_eq!(res.header("content-type"), Some("application/octet-stream"));
    assert_eq!(
        res.header("x-github-media-type"),
        Some("github.v3; param=raw")
    );

    let res = app
        .get("/api/v3/repos/alice/r/contents/docs/notes.txt")
        .header("accept", "application/vnd.github.v3.raw")
        .send()
        .await;
    assert_eq!(res.text(), "a < b & c\n");
    assert_eq!(res.header("content-type"), Some("text/plain; charset=utf-8"));

    let res = app
        .get("/api/v3/repos/alice/r/contents/docs/guide.md")
        .header("accept", "application/vnd.github.html")
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.header("content-type"), Some("text/html; charset=utf-8"));
    let html = res.text();
    assert!(html.contains("<h1"), "{html}");
    assert!(html.contains("Guide"), "{html}");
    assert!(html.contains("/alice/r/issues/1"), "{html}");

    let res = app
        .get("/api/v3/repos/alice/r/contents/docs/notes.txt")
        .header("accept", "application/vnd.github.html")
        .send()
        .await;
    assert!(res.text().contains("<pre>a &lt; b &amp; c\n</pre>"));

    // Object media type: directory as an object with entries.
    let res = app
        .get("/api/v3/repos/alice/r/contents/docs")
        .header("accept", "application/vnd.github.object")
        .send()
        .await;
    res.assert_status(200);
    let v = res.json();
    assert_eq!(v["type"], "dir");
    assert_eq!(v["name"], "docs");
    assert_eq!(v["path"], "docs");
    assert_eq!(v["entries"].as_array().unwrap().len(), 2);
    assert_eq!(v["entries"][0]["name"], "guide.md");

    // Raw on a directory falls back to JSON.
    let res = app
        .get("/api/v3/repos/alice/r/contents/docs")
        .header("accept", "application/vnd.github.raw")
        .send()
        .await;
    assert!(res.json().is_array());
}

#[tokio::test]
async fn symlinks_and_submodules() {
    let (app, alice) = fixture().await;
    let main = app
        .get("/api/v3/repos/alice/r/git/ref/heads/main")
        .send()
        .await
        .json();
    let head = main["object"]["sha"].as_str().unwrap().to_string();
    let commit = app
        .get(&format!("/api/v3/repos/alice/r/git/commits/{head}"))
        .send()
        .await
        .json();
    let sub_sha = "1111111111111111111111111111111111111111";
    let tree = app
        .post("/api/v3/repos/alice/r/git/trees")
        .auth(&alice)
        .json(&json!({
            "base_tree": commit["tree"]["sha"],
            "tree": [
                {"path": "link", "mode": "120000", "type": "blob", "content": "README.md"},
                {"path": "docs/up", "mode": "120000", "type": "blob", "content": "../bin.dat"},
                {"path": "broken", "mode": "120000", "type": "blob", "content": "missing/file"},
                {"path": "vendor/lib", "mode": "160000", "type": "commit", "sha": sub_sha},
                {"path": ".gitmodules", "mode": "100644", "type": "blob",
                 "content": "[submodule \"lib\"]\n\tpath = vendor/lib\n\turl = https://example.com/lib.git\n"}
            ]
        }))
        .send()
        .await;
    tree.assert_status(201);
    let c = app
        .post("/api/v3/repos/alice/r/git/commits")
        .auth(&alice)
        .json(&json!({"message": "links", "tree": tree.json()["sha"], "parents": [head]}))
        .send()
        .await;
    c.assert_status(201);
    app.patch("/api/v3/repos/alice/r/git/refs/heads/main")
        .auth(&alice)
        .json(&json!({"sha": c.json()["sha"]}))
        .send()
        .await
        .assert_status(200);

    // A symlink to a regular file returns the target file.
    let res = app.get("/api/v3/repos/alice/r/contents/link").send().await;
    res.assert_status(200);
    let v = res.json();
    assert_eq!(v["type"], "file");
    assert_eq!(v["path"], "README.md");
    assert_eq!(decode(&v["content"]), b"# r\n");
    let v = app
        .get("/api/v3/repos/alice/r/contents/docs/up")
        .send()
        .await
        .json();
    assert_eq!(v["path"], "bin.dat");

    // Dangling symlink: described as a symlink.
    let v = app
        .get("/api/v3/repos/alice/r/contents/broken")
        .send()
        .await
        .json();
    assert_eq!(v["type"], "symlink");
    assert_eq!(v["target"], "missing/file");
    assert_eq!(v["size"], 12);

    // Submodule.
    let v = app
        .get("/api/v3/repos/alice/r/contents/vendor/lib")
        .send()
        .await
        .json();
    assert_eq!(v["type"], "submodule");
    assert_eq!(v["sha"], sub_sha);
    assert_eq!(v["submodule_git_url"], "https://example.com/lib.git");
    assert!(v["download_url"].is_null());

    // Listings: symlinks as `symlink`, submodules as `file` (GitHub compat);
    // the object media type reports `submodule`.
    let list = app.get("/api/v3/repos/alice/r/contents").send().await.json();
    let link = list
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "link")
        .unwrap();
    assert_eq!(link["type"], "symlink");
    let list = app
        .get("/api/v3/repos/alice/r/contents/vendor")
        .send()
        .await
        .json();
    assert_eq!(list[0]["type"], "file");
    let obj = app
        .get("/api/v3/repos/alice/r/contents/vendor")
        .header("accept", "application/vnd.github.object")
        .send()
        .await
        .json();
    assert_eq!(obj["entries"][0]["type"], "submodule");
    assert_eq!(
        obj["entries"][0]["submodule_git_url"],
        "https://example.com/lib.git"
    );
}

#[tokio::test]
async fn large_files_have_no_content() {
    let app = TestApp::spawn_with_config(bgh_server::factory(), |c| c.max_blob_size = 16).await;
    let alice = app.create_user("alice").await;
    app.create_repo_with(&alice, None, json!({"name": "r", "auto_init": true}))
        .await;
    create_file(&app, &alice, "r", "big.txt", &[b'x'; 64]).await;
    let v = app
        .get("/api/v3/repos/alice/r/contents/big.txt")
        .send()
        .await
        .json();
    assert_eq!(v["encoding"], "none");
    assert_eq!(v["content"], "");
    assert_eq!(v["size"], 64);
    // Raw still works.
    let res = app
        .get("/api/v3/repos/alice/r/contents/big.txt")
        .header("accept", "application/vnd.github.raw")
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.body.len(), 64);
}

#[tokio::test]
async fn create_update_delete_files() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let repo = app
        .create_repo_with(&alice, None, json!({"name": "r", "auto_init": true}))
        .await;
    let repo_id = repo["id"].as_i64().unwrap();
    let mut events = app.state.events.subscribe();

    // Create.
    let res = app
        .put("/api/v3/repos/alice/r/contents/src/hello.txt")
        .auth(&alice)
        .json(&json!({
            "message": "Add hello",
            "content": b64("hello\n"),
            "committer": {"name": "Robo", "email": "robo@example.com", "date": "2024-01-02T03:04:05Z"}
        }))
        .send()
        .await;
    res.assert_status(201);
    let v = res.json();
    assert_eq!(v["content"]["name"], "hello.txt");
    assert_eq!(v["content"]["path"], "src/hello.txt");
    assert_eq!(v["content"]["type"], "file");
    assert_eq!(v["content"]["size"], 6);
    assert_eq!(
        v["content"]["sha"],
        "ce013625030ba8dba906f756967f9e9ca394464a"
    );
    assert!(v["content"].get("content").is_none());
    assert_eq!(
        v["content"]["url"],
        app.url("/api/v3/repos/alice/r/contents/src/hello.txt?ref=main")
    );
    let commit = &v["commit"];
    assert_eq!(commit["message"], "Add hello");
    // Only committer given: used for both.
    assert_eq!(commit["author"]["name"], "Robo");
    assert_eq!(commit["committer"]["email"], "robo@example.com");
    assert_eq!(commit["author"]["date"], "2024-01-02T03:04:05Z");
    let commit_sha = commit["sha"].as_str().unwrap().to_string();
    assert_eq!(
        commit["url"],
        app.url(&format!("/api/v3/repos/alice/r/git/commits/{commit_sha}"))
    );
    assert_eq!(commit["parents"].as_array().unwrap().len(), 1);
    let main = app
        .get("/api/v3/repos/alice/r/git/ref/heads/main")
        .send()
        .await
        .json();
    assert_eq!(main["object"]["sha"], commit_sha.as_str());

    // Post-receive ran: push event.
    app.drain_jobs().await;
    let mut saw_push = false;
    while let Ok(ev) = events.try_recv() {
        if let Event::Push(p) = &*ev {
            assert_eq!(p.repo_id, repo_id);
            assert_eq!(p.pusher_id, Some(alice.id));
            assert_eq!(p.updates[0].refname, "refs/heads/main");
            assert_eq!(p.updates[0].new, commit_sha);
            saw_push = true;
        }
    }
    assert!(saw_push);

    // Update: sha required and checked.
    let res = app
        .put("/api/v3/repos/alice/r/contents/src/hello.txt")
        .auth(&alice)
        .json(&json!({"message": "Update", "content": b64("bye\n")}))
        .send()
        .await;
    res.assert_status(422);
    assert!(
        res.json()["message"]
            .as_str()
            .unwrap()
            .contains("\"sha\" wasn't supplied.")
    );
    let wrong = "0123456789012345678901234567890123456789";
    let res = app
        .put("/api/v3/repos/alice/r/contents/src/hello.txt")
        .auth(&alice)
        .json(&json!({"message": "Update", "content": b64("bye\n"), "sha": wrong}))
        .send()
        .await;
    res.assert_status(409);
    assert_eq!(
        res.json()["message"],
        format!("src/hello.txt does not match {wrong}")
    );
    let res = app
        .put("/api/v3/repos/alice/r/contents/src/hello.txt")
        .auth(&alice)
        .json(&json!({
            "message": "Update",
            "content": b64("bye\n"),
            "sha": "ce013625030ba8dba906f756967f9e9ca394464a",
            "author": {"name": "Ann", "email": "ann@example.com"}
        }))
        .send()
        .await;
    res.assert_status(200);
    let v = res.json();
    assert_eq!(v["commit"]["author"]["name"], "Ann");
    assert_eq!(v["commit"]["committer"]["name"], "Ann");
    assert_eq!(v["commit"]["parents"][0]["sha"], commit_sha.as_str());
    let got = app
        .get("/api/v3/repos/alice/r/contents/src/hello.txt")
        .send()
        .await
        .json();
    assert_eq!(decode(&got["content"]), b"bye\n");

    // Default identity: the caller.
    let res = app
        .put("/api/v3/repos/alice/r/contents/other.txt")
        .auth(&alice)
        .json(&json!({"message": "x", "content": b64("x")}))
        .send()
        .await;
    res.assert_status(201);
    let author = &res.json()["commit"]["author"];
    assert_eq!(author["name"], "alice");
    assert!(author["email"].as_str().unwrap().contains('@'));

    // Validation.
    let res = app
        .put("/api/v3/repos/alice/r/contents/x.txt")
        .auth(&alice)
        .json(&json!({"message": "x", "content": "!!not base64!!"}))
        .send()
        .await;
    res.assert_status(422);
    app.put("/api/v3/repos/alice/r/contents/x.txt")
        .auth(&alice)
        .json(&json!({"content": b64("x")}))
        .send()
        .await
        .assert_status(422);
    let res = app
        .put("/api/v3/repos/alice/r/contents/x.txt")
        .auth(&alice)
        .json(&json!({"message": "x", "content": b64("x"), "branch": "nope"}))
        .send()
        .await;
    res.assert_status(404);
    assert_eq!(res.json()["message"], "Branch nope not found");

    // Another branch.
    let res = app
        .post("/api/v3/repos/alice/r/git/refs")
        .auth(&alice)
        .json(&json!({"ref": "refs/heads/feature/x", "sha": commit_sha}))
        .send()
        .await;
    res.assert_status(201);
    let res = app
        .put("/api/v3/repos/alice/r/contents/feat.txt")
        .auth(&alice)
        .json(&json!({"message": "feat", "content": b64("f"), "branch": "feature/x"}))
        .send()
        .await;
    res.assert_status(201);
    assert_eq!(
        res.json()["content"]["html_url"],
        app.url("/alice/r/blob/feature/x/feat.txt")
    );
    app.get("/api/v3/repos/alice/r/contents/feat.txt")
        .send()
        .await
        .assert_status(404);
    app.get("/api/v3/repos/alice/r/contents/feat.txt?ref=feature/x")
        .send()
        .await
        .assert_status(200);

    // Delete.
    let res = app
        .delete("/api/v3/repos/alice/r/contents/other.txt")
        .auth(&alice)
        .json(&json!({"message": "rm"}))
        .send()
        .await;
    res.assert_status(422);
    let res = app
        .delete("/api/v3/repos/alice/r/contents/other.txt")
        .auth(&alice)
        .json(&json!({"message": "rm", "sha": wrong}))
        .send()
        .await;
    res.assert_status(409);
    app.delete("/api/v3/repos/alice/r/contents/missing.txt")
        .auth(&alice)
        .json(&json!({"message": "rm", "sha": wrong}))
        .send()
        .await
        .assert_status(404);
    let x_sha = "c1b0730e0133447badcfd47fd144e254807b06e1"; // blob "x"
    let res = app
        .delete("/api/v3/repos/alice/r/contents/other.txt")
        .auth(&alice)
        .json(&json!({"message": "rm other", "sha": x_sha}))
        .send()
        .await;
    res.assert_status(200);
    let v = res.json();
    assert!(v["content"].is_null());
    assert_eq!(v["commit"]["message"], "rm other");
    app.get("/api/v3/repos/alice/r/contents/other.txt")
        .send()
        .await
        .assert_status(404);
}

#[tokio::test]
async fn empty_repository() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    app.create_repo(&alice, "empty").await;
    let res = app.get("/api/v3/repos/alice/empty/contents").send().await;
    res.assert_status(404);
    assert_eq!(res.json()["message"], "This repository is empty.");
    app.get("/api/v3/repos/alice/empty/readme")
        .send()
        .await
        .assert_status(404);

    let res = app
        .put("/api/v3/repos/alice/empty/contents/README.md")
        .auth(&alice)
        .json(&json!({"message": "first", "content": b64("# hi\n")}))
        .send()
        .await;
    res.assert_status(201);
    assert!(
        res.json()["commit"]["parents"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    app.drain_jobs().await;
    let v = app
        .get("/api/v3/repos/alice/empty/contents/README.md")
        .send()
        .await
        .json();
    assert_eq!(decode(&v["content"]), b"# hi\n");
}

#[tokio::test]
async fn permissions_and_protection() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let repo = app
        .create_repo_with(&alice, None, json!({"name": "r", "auto_init": true}))
        .await;
    app.create_repo_with(
        &alice,
        None,
        json!({"name": "secret", "auto_init": true, "private": true}),
    )
    .await;

    // Readers can't write; anonymous must authenticate.
    let body = json!({"message": "x", "content": b64("x")});
    app.put("/api/v3/repos/alice/r/contents/x.txt")
        .auth(&bob)
        .json(&body)
        .send()
        .await
        .assert_status(403);
    app.put("/api/v3/repos/alice/r/contents/x.txt")
        .json(&body)
        .send()
        .await
        .assert_status(401);

    // Private repositories are invisible to others.
    app.get("/api/v3/repos/alice/secret/contents/README.md")
        .auth(&bob)
        .send()
        .await
        .assert_status(404);
    app.get("/api/v3/repos/alice/secret/contents/README.md")
        .send()
        .await
        .assert_status(404);
    app.put("/api/v3/repos/alice/secret/contents/x.txt")
        .auth(&bob)
        .json(&body)
        .send()
        .await
        .assert_status(404);
    app.get("/api/v3/repos/alice/secret/contents/README.md")
        .auth(&alice)
        .send()
        .await
        .assert_status(200);

    // Protected branch: requires pull requests.
    sqlx::query(
        "INSERT INTO branch_protections (repo_id, pattern, required_pull_request_reviews, enforce_admins)
         VALUES ($1, 'main', '{}', true)",
    )
    .bind(repo["id"].as_i64().unwrap())
    .execute(&app.state.db)
    .await
    .unwrap();
    let res = app
        .put("/api/v3/repos/alice/r/contents/x.txt")
        .auth(&alice)
        .json(&body)
        .send()
        .await;
    res.assert_status(422);
    app.get("/api/v3/repos/alice/r/contents/x.txt")
        .send()
        .await
        .assert_status(404);
}

#[tokio::test]
async fn readme() {
    let (app, alice) = fixture().await;
    create_file(&app, &alice, "r", "docs/readme.txt", b"plain").await;
    create_file(&app, &alice, "r", "docs/README.rst", b"Title\n=====\n").await;
    create_file(&app, &alice, "r", "docs/README", b"bare").await;

    let res = app.get("/api/v3/repos/alice/r/readme").send().await;
    res.assert_status(200);
    let v = res.json();
    assert_eq!(v["type"], "file");
    assert_eq!(v["name"], "README.md");
    assert_eq!(v["path"], "README.md");
    assert_eq!(decode(&v["content"]), b"# r\n");
    assert_eq!(v["html_url"], app.url("/alice/r/blob/main/README.md"));

    let v = app.get("/api/v3/repos/alice/r/readme/docs").send().await.json();
    assert_eq!(v["path"], "docs/README.rst");

    let res = app
        .get("/api/v3/repos/alice/r/readme")
        .header("accept", "application/vnd.github.raw")
        .send()
        .await;
    assert_eq!(res.text(), "# r\n");
    let res = app
        .get("/api/v3/repos/alice/r/readme")
        .header("accept", "application/vnd.github.html")
        .send()
        .await;
    let html = res.text();
    assert!(html.contains("id=\"readme\""), "{html}");
    assert!(html.contains("<h1"), "{html}");

    app.get("/api/v3/repos/alice/r/readme/nope")
        .send()
        .await
        .assert_status(404);
    app.get("/api/v3/repos/alice/r/readme?ref=nope")
        .send()
        .await
        .assert_status(404);
}

#[tokio::test]
async fn raw_downloads() {
    let (app, alice) = fixture().await;
    let res = app.get("/alice/r/raw/main/docs/notes.txt").send().await;
    res.assert_status(200);
    assert_eq!(res.text(), "a < b & c\n");
    assert_eq!(res.header("content-type"), Some("text/plain; charset=utf-8"));
    assert_eq!(res.header("x-content-type-options"), Some("nosniff"));
    assert_eq!(
        res.header("content-security-policy"),
        Some("default-src 'none'; sandbox")
    );
    assert!(res.header("cache-control").is_none());

    let res = app.get("/alice/r/raw/main/bin.dat").send().await;
    assert_eq!(res.header("content-type"), Some("application/octet-stream"));
    assert_eq!(res.body.as_ref(), &[0, 1, 2, 3, 255]);

    // Branch names with slashes; longest matching prefix wins.
    let head = app
        .get("/api/v3/repos/alice/r/git/ref/heads/main")
        .send()
        .await
        .json()["object"]["sha"]
        .as_str()
        .unwrap()
        .to_string();
    app.post("/api/v3/repos/alice/r/git/refs")
        .auth(&alice)
        .json(&json!({"ref": "refs/heads/feature/deep", "sha": head}))
        .send()
        .await
        .assert_status(201);
    put(
        &app,
        &alice,
        "r",
        "README.md",
        json!({"message": "deep", "content": b64("deep\n"), "branch": "feature/deep",
               "sha": "a98c46c71c932a57a1ec95007803ea5509cc6316"}),
    )
    .await;
    let res = app.get("/alice/r/raw/feature/deep/README.md").send().await;
    res.assert_status(200);
    assert_eq!(res.text(), "deep\n");
    let res = app.get("/alice/r/raw/refs/heads/main/README.md").send().await;
    assert_eq!(res.text(), "# r\n");

    // Full SHA: immutable.
    let res = app.get(&format!("/alice/r/raw/{head}/README.md")).send().await;
    res.assert_status(200);
    assert_eq!(
        res.header("cache-control"),
        Some("public, max-age=31536000, immutable")
    );
    app.get("/alice/r/raw/main/nope.txt")
        .send()
        .await
        .assert_status(404);
    app.get("/alice/r/raw/nope/README.md")
        .send()
        .await
        .assert_status(404);
    app.get("/alice/r/raw/main/docs")
        .send()
        .await
        .assert_status(404);

    // Private repositories need read access.
    app.create_repo_with(
        &alice,
        None,
        json!({"name": "secret", "auto_init": true, "private": true}),
    )
    .await;
    app.get("/alice/secret/raw/main/README.md")
        .send()
        .await
        .assert_status(404);
    let res = app
        .get("/alice/secret/raw/main/README.md")
        .auth(&alice)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.text(), "# secret\n");
}
