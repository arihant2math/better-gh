//! Wiki pages API: CRUD, rendering, history, revert, compare, search,
//! settings and permissions.

use bgh_core::testing::{TestApp, TestUser};
use serde_json::{Value, json};

fn wiki(path: &str) -> String {
    format!("/_bgh/repos/alice/demo/wiki{path}")
}

async fn create(app: &TestApp, user: &TestUser, title: &str, body: &str) -> Value {
    let res = app
        .post(&wiki("/pages"))
        .auth(user)
        .json(&json!({ "title": title, "body": body }))
        .send()
        .await;
    res.assert_status(201);
    res.json()
}

async fn add_collaborator(app: &TestApp, repo_id: i64, user: &TestUser, perm: &str) {
    sqlx::query("INSERT INTO collaborators (repo_id, user_id, permission) VALUES ($1, $2, $3)")
        .bind(repo_id)
        .bind(user.id)
        .bind(perm)
        .execute(&app.state.db)
        .await
        .unwrap();
}

#[tokio::test]
async fn crud_and_rendering() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    app.create_repo(&alice, "demo").await;

    let v = app.get(&wiki("")).auth(&alice).send().await.json();
    assert_eq!(v["exists"], false);
    assert_eq!(v["canEdit"], true);
    assert_eq!(v["anyoneCanEdit"], false);
    assert_eq!(v["home"], "Home");
    assert_eq!(v["pages"], json!([]));
    assert!(v["sidebar"].is_null());
    app.get(&wiki("/pages/Home"))
        .send()
        .await
        .assert_status(404);

    let home = create(
        &app,
        &alice,
        "Home",
        "# Welcome\n\nSee [[Other Page]], [[the guide|User Guide]] and [rel](User-Guide). \
         Ping @alice. `[[Code]]`\n",
    )
    .await;
    assert_eq!(home["slug"], "Home");
    assert_eq!(home["title"], "Home");
    assert_eq!(home["path"], "Home.md");
    assert_eq!(home["format"], "markdown");
    assert_eq!(home["commit"]["message"], "Created Home (markdown)");
    assert_eq!(home["commit"]["author"]["login"], "alice");
    assert_eq!(home["commit"]["author"]["name"], "alice");
    assert_eq!(home["commit"]["author"]["email"], "alice@example.com");
    assert!(home["commit"]["author"]["avatarUrl"].is_string());
    assert!(home["commit"]["date"].as_str().unwrap().ends_with('Z'));
    let html = home["html"].as_str().unwrap();
    assert!(
        html.contains(r#"class="wiki-link wiki-missing" href="/alice/demo/wiki/Other-Page""#),
        "{html}"
    );
    assert!(html.contains(">Other Page</a>"), "{html}");
    assert!(
        html.contains(r#"class="wiki-link wiki-missing" href="/alice/demo/wiki/User-Guide""#),
        "{html}"
    );
    assert!(html.contains("user-mention"), "{html}");
    assert!(html.contains("<code>[[Code]]</code>"), "{html}");

    create(&app, &alice, "User Guide", "guide v1\n").await;
    create(&app, &alice, "_Sidebar", "* [[Home]]\n").await;
    create(&app, &alice, "_Footer", "Footer *text*\n").await;

    // Duplicate (case-insensitive) → 422 with errors[].
    let res = app
        .post(&wiki("/pages"))
        .auth(&alice)
        .json(&json!({"title": "home", "body": "x"}))
        .send()
        .await;
    res.assert_status(422);
    assert_eq!(res.json()["errors"][0]["field"], "title");
    for bad in ["a/b", "", &"x".repeat(256)] {
        app.post(&wiki("/pages"))
            .auth(&alice)
            .json(&json!({"title": bad, "body": "x"}))
            .send()
            .await
            .assert_status(422);
    }

    let v = app.get(&wiki("/pages/Home")).send().await.json();
    let html = v["html"].as_str().unwrap();
    assert!(
        html.contains(r#"class="wiki-link" href="/alice/demo/wiki/User-Guide""#),
        "{html}"
    );
    assert!(
        html.contains(">the guide</a>") && html.contains(">rel</a>"),
        "{html}"
    );
    assert_eq!(v["sidebar"]["slug"], "_Sidebar");
    assert!(
        v["sidebar"]["html"]
            .as_str()
            .unwrap()
            .contains(r#"href="/alice/demo/wiki/Home""#)
    );
    assert!(
        v["footer"]["html"]
            .as_str()
            .unwrap()
            .contains("<em>text</em>")
    );
    let blob_sha = v["sha"].as_str().unwrap().to_string();
    assert_eq!(blob_sha.len(), 40);

    // Overview lists pages (no special pages) with sidebar/footer.
    let v = app.get(&wiki("")).send().await.json();
    assert_eq!(v["exists"], true);
    assert_eq!(v["canEdit"], false, "anonymous");
    assert_eq!(
        v["pages"],
        json!([
            {"slug": "Home", "title": "Home", "path": "Home.md"},
            {"slug": "User-Guide", "title": "User Guide", "path": "User-Guide.md"},
        ])
    );
    assert!(v["sidebar"]["html"].as_str().unwrap().contains("wiki-link"));
    assert!(v["footer"]["html"].is_string());

    // Raw.
    let res = app.get(&wiki("/pages/User-Guide/raw")).send().await;
    res.assert_status(200);
    assert_eq!(res.text(), "guide v1\n");
    assert!(
        res.header("content-type")
            .unwrap()
            .starts_with("text/plain")
    );

    // Edit with a stale expectedCommit → 409; current → 200.
    let guide = app.get(&wiki("/pages/User-Guide")).send().await.json();
    let base_commit = guide["commit"]["sha"].as_str().unwrap().to_string();
    let v = app
        .put(&wiki("/pages/User-Guide"))
        .auth(&alice)
        .json(&json!({"body": "guide v2\n", "expectedCommit": base_commit}))
        .send()
        .await;
    v.assert_status(200);
    assert_eq!(v.json()["raw"], "guide v2\n");
    assert_eq!(
        v.json()["commit"]["message"],
        "Updated User Guide (markdown)"
    );
    app.put(&wiki("/pages/User-Guide"))
        .auth(&alice)
        .json(&json!({"body": "guide v3\n", "expectedCommit": base_commit}))
        .send()
        .await
        .assert_status(409);
    // Edits to other pages don't make a page's expectedCommit stale.
    let guide = app.get(&wiki("/pages/User-Guide")).send().await.json();
    let current = guide["commit"]["sha"].as_str().unwrap().to_string();
    create(&app, &alice, "Unrelated", "u\n").await;
    app.put(&wiki("/pages/User-Guide"))
        .auth(&alice)
        .json(&json!({"body": "guide v3\n", "expectedCommit": current, "message": "my msg"}))
        .send()
        .await
        .assert_status(200);

    // Rename via title.
    let res = app
        .put(&wiki("/pages/User-Guide"))
        .auth(&alice)
        .json(&json!({"title": "Manual", "body": "manual\n"}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["slug"], "Manual");
    assert_eq!(res.json()["path"], "Manual.md");
    app.get(&wiki("/pages/User-Guide"))
        .send()
        .await
        .assert_status(404);
    // Renaming onto an existing page → 422.
    app.put(&wiki("/pages/Manual"))
        .auth(&alice)
        .json(&json!({"title": "Home", "body": "x"}))
        .send()
        .await
        .assert_status(422);
    // Case-insensitive lookup.
    app.get(&wiki("/pages/manual"))
        .send()
        .await
        .assert_status(200);

    // Older revision via ?rev=.
    let v = app
        .get(&wiki(&format!(
            "/pages/Home?rev={}",
            home["commit"]["sha"].as_str().unwrap()
        )))
        .send()
        .await
        .json();
    assert_eq!(v["sha"], blob_sha);
    app.get(&wiki(
        "/pages/Home?rev=0123456789012345678901234567890123456789",
    ))
    .send()
    .await
    .assert_status(404);

    // Delete (body optional).
    app.delete(&wiki("/pages/Manual"))
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    app.delete(&wiki("/pages/Unrelated"))
        .auth(&alice)
        .json(&json!({"message": "bye"}))
        .send()
        .await
        .assert_status(204);
    app.get(&wiki("/pages/Manual"))
        .send()
        .await
        .assert_status(404);
    app.delete(&wiki("/pages/Manual"))
        .auth(&alice)
        .send()
        .await
        .assert_status(404);
    let h = app.get(&wiki("/history")).send().await.json();
    assert_eq!(h[0]["message"], "bye");
    assert_eq!(h[1]["message"], "Destroyed Manual (markdown)");
}

#[tokio::test]
async fn history_revert_compare_search() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    app.create_repo(&alice, "demo").await;

    let v1 = create(&app, &alice, "Alpha", "The quick brown fox\n").await;
    let c1 = v1["commit"]["sha"].as_str().unwrap().to_string();
    create(
        &app,
        &alice,
        "Beta",
        "nothing here\nbut ALPHA is mentioned\n",
    )
    .await;
    for body in ["v2\n", "v3\n"] {
        app.put(&wiki("/pages/Alpha"))
            .auth(&alice)
            .json(&json!({ "body": body }))
            .send()
            .await
            .assert_status(200);
    }

    // Per-page history, paginated with a Link header.
    let res = app
        .get(&wiki("/pages/Alpha/history?per_page=2"))
        .send()
        .await;
    res.assert_status(200);
    let items = res.json();
    assert_eq!(items.as_array().unwrap().len(), 2);
    assert_eq!(items[0]["message"], "Updated Alpha (markdown)");
    assert_eq!(items[0]["author"]["login"], "alice");
    let link = res.header("link").unwrap();
    assert!(link.contains("rel=\"next\""), "{link}");
    assert!(link.contains("/_bgh/repos/alice/demo/wiki/pages/Alpha/history?per_page=2&page=2"));
    let page2 = app
        .get(&wiki("/pages/Alpha/history?per_page=2&page=2"))
        .send()
        .await
        .json();
    assert_eq!(page2.as_array().unwrap().len(), 1);
    assert_eq!(page2[0]["sha"], c1.as_str());
    app.get(&wiki("/pages/Nope/history"))
        .send()
        .await
        .assert_status(404);

    // Wiki-wide history.
    let all = app.get(&wiki("/history")).send().await.json();
    assert_eq!(all.as_array().unwrap().len(), 4);

    let head = all[0]["sha"].as_str().unwrap().to_string();
    // Compare limited to one page, and wiki-wide.
    let v = app
        .get(&wiki(&format!("/compare/{c1}...{head}?slug=Alpha")))
        .send()
        .await;
    v.assert_status(200);
    let v = v.json();
    assert_eq!(v["base"], c1.as_str());
    assert_eq!(v["head"], head.as_str());
    let diff = v["diff"].as_str().unwrap();
    assert!(
        diff.contains("-The quick brown fox") && diff.contains("+v3"),
        "{diff}"
    );
    assert!(!diff.contains("Beta.md"), "{diff}");
    let v = app
        .get(&wiki(&format!("/compare/{c1}...{head}")))
        .send()
        .await
        .json();
    assert!(v["diff"].as_str().unwrap().contains("+++ b/Beta.md"));
    app.get(&wiki("/compare/nope...HEAD"))
        .send()
        .await
        .assert_status(404);

    // Revert Alpha to its first version: a new commit.
    let res = app
        .post(&wiki("/pages/Alpha/revert"))
        .auth(&alice)
        .json(&json!({ "sha": c1 }))
        .send()
        .await;
    res.assert_status(200);
    let v = res.json();
    assert_eq!(v["raw"], "The quick brown fox\n");
    assert_eq!(
        v["commit"]["message"],
        format!("Reverted Alpha to {}", &c1[..7])
    );
    let hist = app.get(&wiki("/pages/Alpha/history")).send().await.json();
    assert_eq!(hist.as_array().unwrap().len(), 4);
    // Revert to a commit where the page didn't exist / unknown commit.
    app.post(&wiki("/pages/Beta/revert"))
        .auth(&alice)
        .json(&json!({ "sha": c1 }))
        .send()
        .await
        .assert_status(422);
    app.post(&wiki("/pages/Alpha/revert"))
        .auth(&alice)
        .json(&json!({ "sha": "deadbeef" }))
        .send()
        .await
        .assert_status(422);

    // Restore a deleted page via its history + revert.
    app.delete(&wiki("/pages/Beta"))
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    let hist = app.get(&wiki("/pages/Beta/history")).send().await.json();
    assert_eq!(hist.as_array().unwrap().len(), 2);
    let before_delete = hist[1]["sha"].as_str().unwrap();
    app.post(&wiki("/pages/Beta/revert"))
        .auth(&alice)
        .json(&json!({ "sha": before_delete }))
        .send()
        .await
        .assert_status(200);
    app.get(&wiki("/pages/Beta"))
        .send()
        .await
        .assert_status(200);

    // Search: title hits first, then content hits, case-insensitive.
    let v = app.get(&wiki("/search?q=alpha")).send().await.json();
    let results = v["results"].as_array().unwrap();
    assert_eq!(results.len(), 2, "{v}");
    assert_eq!(results[0]["slug"], "Alpha");
    assert_eq!(results[0]["title"], "Alpha");
    assert_eq!(results[0]["snippet"], "The quick brown fox");
    assert_eq!(results[1]["slug"], "Beta");
    assert_eq!(results[1]["snippet"], "but ALPHA is mentioned");
    let v = app.get(&wiki("/search?q=BROWN")).send().await.json();
    assert_eq!(v["results"].as_array().unwrap().len(), 1);
    let v = app.get(&wiki("/search?q=")).send().await.json();
    assert_eq!(v["results"], json!([]));
}

#[tokio::test]
async fn permissions_and_settings() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await; // signed-in reader
    let carol = app.create_user("carol").await; // collaborator (write)
    let repo = app.create_repo(&alice, "demo").await;
    let repo_id = repo["id"].as_i64().unwrap();
    add_collaborator(&app, repo_id, &carol, "write").await;

    let body = json!({"title": "Page", "body": "x"});
    app.post(&wiki("/pages"))
        .json(&body)
        .send()
        .await
        .assert_status(401);
    app.post(&wiki("/pages"))
        .auth(&bob)
        .json(&body)
        .send()
        .await
        .assert_status(403);
    let v = app.get(&wiki("")).auth(&bob).send().await.json();
    assert_eq!(v["canEdit"], false);
    app.post(&wiki("/pages"))
        .auth(&carol)
        .json(&body)
        .send()
        .await
        .assert_status(201);
    app.put(&wiki("/pages/Page"))
        .auth(&bob)
        .json(&json!({"body": "y"}))
        .send()
        .await
        .assert_status(403);
    app.delete(&wiki("/pages/Page"))
        .auth(&bob)
        .send()
        .await
        .assert_status(403);

    // Settings: readable by readers, writable by admins only.
    let v = app.get(&wiki("/settings")).send().await;
    v.assert_status(200);
    assert_eq!(v.json(), json!({"anyoneCanEdit": false, "hasWiki": true}));
    app.patch(&wiki("/settings"))
        .auth(&carol)
        .json(&json!({"anyoneCanEdit": true}))
        .send()
        .await
        .assert_status(403);
    app.patch(&wiki("/settings"))
        .json(&json!({"anyoneCanEdit": true}))
        .send()
        .await
        .assert_status(401);
    let v = app
        .patch(&wiki("/settings"))
        .auth(&alice)
        .json(&json!({"anyoneCanEdit": true}))
        .send()
        .await;
    v.assert_status(200);
    assert_eq!(v.json()["anyoneCanEdit"], true);

    // Anyone signed in can now edit; anonymous still can't.
    let v = app.get(&wiki("")).auth(&bob).send().await.json();
    assert_eq!(v["canEdit"], true);
    assert_eq!(v["anyoneCanEdit"], true);
    let res = app
        .put(&wiki("/pages/Page"))
        .auth(&bob)
        .json(&json!({"body": "by bob"}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["commit"]["author"]["login"], "bob");
    app.post(&wiki("/pages"))
        .json(&json!({"title": "Anon", "body": "x"}))
        .send()
        .await
        .assert_status(401);
    // A token without public_repo scope can't use anyone-can-edit.
    let weak = app.create_token(&bob, &["user"]).await;
    app.put(&wiki("/pages/Page"))
        .token(&weak)
        .json(&json!({"body": "weak"}))
        .send()
        .await
        .assert_status(403);
    // Session cookies work.
    let cookie = app.session_cookie(&bob).await;
    app.put(&wiki("/pages/Page"))
        .cookie(&cookie)
        .json(&json!({"body": "cookie"}))
        .send()
        .await
        .assert_status(200);

    // Archived → read-only.
    sqlx::query("UPDATE repositories SET archived = true WHERE id = $1")
        .bind(repo_id)
        .execute(&app.state.db)
        .await
        .unwrap();
    app.put(&wiki("/pages/Page"))
        .auth(&alice)
        .json(&json!({"body": "z"}))
        .send()
        .await
        .assert_status(403);
    let v = app.get(&wiki("")).auth(&alice).send().await.json();
    assert_eq!(v["canEdit"], false);

    // has_wiki = false → 404 everywhere except settings.
    sqlx::query("UPDATE repositories SET has_wiki = false, archived = false WHERE id = $1")
        .bind(repo_id)
        .execute(&app.state.db)
        .await
        .unwrap();
    app.get(&wiki(""))
        .auth(&alice)
        .send()
        .await
        .assert_status(404);
    app.get(&wiki("/pages/Page"))
        .auth(&alice)
        .send()
        .await
        .assert_status(404);
    app.post(&wiki("/pages"))
        .auth(&alice)
        .json(&json!({"title": "X", "body": "x"}))
        .send()
        .await
        .assert_status(404);
    let v = app.get(&wiki("/settings")).auth(&alice).send().await.json();
    assert_eq!(v["hasWiki"], false);

    // Private repository: 404 for non-collaborators and anonymous.
    app.create_private_repo(&alice, "secret").await;
    let p = "/_bgh/repos/alice/secret/wiki";
    app.get(p).send().await.assert_status(404);
    app.get(p).auth(&bob).send().await.assert_status(404);
    app.get(&format!("{p}/settings"))
        .auth(&bob)
        .send()
        .await
        .assert_status(404);
    let res = app
        .post(&format!("{p}/pages"))
        .auth(&alice)
        .json(&json!({"title": "Home", "body": "private"}))
        .send()
        .await;
    res.assert_status(201);
    app.get(&format!("{p}/pages/Home"))
        .auth(&bob)
        .send()
        .await
        .assert_status(404);
}

#[tokio::test]
async fn repository_deletion_removes_wiki_storage() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let repo = app.create_repo(&alice, "demo").await;
    let repo_id = repo["id"].as_i64().unwrap();
    create(&app, &alice, "Home", "hi\n").await;
    let store = bgh_git::RepoStore::from_config(&app.state.config).wiki();
    assert!(store.exists(repo_id));

    app.delete("/api/v3/repos/alice/demo")
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    // Kept while restorable (P50); the purge enqueues the cleanup job.
    app.settle_events().await;
    app.drain_jobs().await;
    assert!(store.path(repo_id).exists(), "wiki kept until the purge");
    app.purge_deleted_repos().await;
    for _ in 0..100 {
        app.drain_jobs().await;
        if !store.path(repo_id).exists() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("wiki storage was not removed");
}
