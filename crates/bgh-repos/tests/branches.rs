//! Branches (list/get/rename), merges, and derived stats (languages,
//! contributors, tags, teams).

mod common;

use serde_json::json;

#[tokio::test]
async fn list_and_get_branches() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let work = common::seeded(&app, &alice, "r", &[("a.txt", "a")]).await;
    let main_sha = work.head().await;
    common::ok(work.run(&["checkout", "-q", "-b", "feature/deep"]).await);
    let feat = work.commit(&[("b.txt", "b")], "feature").await;
    common::ok(work.push("feature/deep").await);
    let id = app.get("/api/v3/repos/alice/r").send().await.json()["id"]
        .as_i64()
        .unwrap();
    sqlx::query(
        "INSERT INTO branch_protections (repo_id, pattern, required_status_checks, enforce_admins)
         VALUES ($1, 'main', '{\"strict\": true, \"contexts\": [\"ci\"]}', true)",
    )
    .bind(id)
    .execute(&app.state.db)
    .await
    .unwrap();

    let res = app.get("/api/v3/repos/alice/r/branches").send().await;
    res.assert_status(200);
    let v = res.json();
    assert_eq!(v.as_array().unwrap().len(), 2);
    assert_eq!(v[0]["name"], "feature/deep");
    assert_eq!(v[0]["commit"]["sha"], feat);
    assert_eq!(
        v[0]["commit"]["url"],
        app.url(&format!("/api/v3/repos/alice/r/commits/{feat}"))
    );
    assert_eq!(v[0]["protected"], false);
    assert_eq!(v[0]["protection"]["enabled"], false);
    assert_eq!(v[1]["name"], "main");
    assert_eq!(v[1]["protected"], true);
    assert_eq!(
        v[1]["protection"],
        json!({"enabled": true, "required_status_checks": {
            "enforcement_level": "everyone", "contexts": ["ci"], "checks": [{"context": "ci", "app_id": null}]}})
    );
    assert_eq!(
        v[1]["protection_url"],
        app.url("/api/v3/repos/alice/r/branches/main/protection")
    );
    let v = app
        .get("/api/v3/repos/alice/r/branches?protected=true")
        .send()
        .await
        .json();
    assert_eq!(v.as_array().unwrap().len(), 1);
    let res = app
        .get("/api/v3/repos/alice/r/branches?per_page=1")
        .send()
        .await;
    assert!(res.header("link").unwrap().contains("rel=\"last\""));

    // Single branch, including names with slashes.
    let v = app.get("/api/v3/repos/alice/r/branches/main").send().await;
    v.assert_status(200);
    let v = v.json();
    assert_eq!(v["name"], "main");
    assert_eq!(v["commit"]["sha"], main_sha);
    assert_eq!(v["commit"]["commit"]["message"], "initial commit");
    assert_eq!(v["_links"]["html"], app.url("/alice/r/tree/main"));
    assert_eq!(
        v["_links"]["self"],
        app.url("/api/v3/repos/alice/r/branches/main")
    );
    assert_eq!(v["protected"], true);
    assert_eq!(v["pattern"], "main");
    let v = app
        .get("/api/v3/repos/alice/r/branches/feature/deep")
        .send()
        .await
        .json();
    assert_eq!(v["commit"]["sha"], feat);
    let res = app.get("/api/v3/repos/alice/r/branches/nope").send().await;
    res.assert_status(404);
    assert_eq!(res.json()["message"], "Branch not found");
}

#[tokio::test]
async fn rename_branch() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let work = common::seeded(&app, &alice, "r", &[("a.txt", "a")]).await;
    common::ok(work.run(&["checkout", "-q", "-b", "dev"]).await);
    common::ok(work.push("dev").await);
    let id = app.get("/api/v3/repos/alice/r").send().await.json()["id"]
        .as_i64()
        .unwrap();
    sqlx::query(
        "INSERT INTO collaborators (repo_id, user_id, permission) VALUES ($1, $2, 'write')",
    )
    .bind(id)
    .bind(bob.id)
    .execute(&app.state.db)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO branch_protections (repo_id, pattern, allow_deletions) VALUES ($1, 'main', false)",
    )
    .bind(id)
    .execute(&app.state.db)
    .await
    .unwrap();

    // Writers rename non-default branches.
    let res = app
        .post("/api/v3/repos/alice/r/branches/dev/rename")
        .auth(&bob)
        .json(&json!({"new_name": "develop"}))
        .send()
        .await;
    res.assert_status(201);
    assert_eq!(res.json()["name"], "develop");
    app.get("/api/v3/repos/alice/r/branches/dev")
        .send()
        .await
        .assert_status(404);

    // The default branch needs admin; protection and default follow.
    app.post("/api/v3/repos/alice/r/branches/main/rename")
        .auth(&bob)
        .json(&json!({"new_name": "trunk"}))
        .send()
        .await
        .assert_status(403);
    app.post("/api/v3/repos/alice/r/branches/main/rename")
        .auth(&alice)
        .json(&json!({"new_name": "develop"}))
        .send()
        .await
        .assert_status(422);
    let res = app
        .post("/api/v3/repos/alice/r/branches/main/rename")
        .auth(&alice)
        .json(&json!({"new_name": "trunk"}))
        .send()
        .await;
    res.assert_status(201);
    let v = res.json();
    assert_eq!(v["name"], "trunk");
    assert_eq!(v["protected"], true);
    let repo = app.get("/api/v3/repos/alice/r").send().await.json();
    assert_eq!(repo["default_branch"], "trunk");
    app.drain_jobs().await;
    app.post("/api/v3/repos/alice/r/branches/nope/rename")
        .auth(&alice)
        .json(&json!({"new_name": "x"}))
        .send()
        .await
        .assert_status(404);
}

#[tokio::test]
async fn merges_endpoint() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let work = common::seeded(&app, &alice, "r", &[("a.txt", "a\n")]).await;
    common::ok(work.run(&["checkout", "-q", "-b", "topic"]).await);
    let topic = work.commit(&[("b.txt", "b\n")], "topic").await;
    common::ok(work.push("topic").await);
    common::ok(work.run(&["checkout", "-q", "main"]).await);
    let main = work.commit(&[("c.txt", "c\n")], "main").await;
    common::ok(work.push("main").await);

    let res = app
        .post("/api/v3/repos/alice/r/merges")
        .auth(&alice)
        .json(&json!({"base": "main", "head": "topic", "commit_message": "Shipit"}))
        .send()
        .await;
    res.assert_status(201);
    let v = res.json();
    assert_eq!(v["commit"]["message"], "Shipit");
    assert_eq!(v["parents"][0]["sha"], main);
    assert_eq!(v["parents"][1]["sha"], topic);
    let b = app
        .get("/api/v3/repos/alice/r/branches/main")
        .send()
        .await
        .json();
    assert_eq!(b["commit"]["sha"], v["sha"]);

    // Already merged → 204.
    app.post("/api/v3/repos/alice/r/merges")
        .auth(&alice)
        .json(&json!({"base": "main", "head": "topic"}))
        .send()
        .await
        .assert_status(204);
    // Missing refs → 404, conflicts → 409.
    let res = app
        .post("/api/v3/repos/alice/r/merges")
        .auth(&alice)
        .json(&json!({"base": "nope", "head": "topic"}))
        .send()
        .await;
    res.assert_status(404);
    assert_eq!(res.json()["message"], "Base does not exist");
    common::ok(work.run(&["checkout", "-q", "-b", "c1"]).await);
    work.commit(&[("a.txt", "x\n")], "x").await;
    common::ok(work.push("c1").await);
    common::ok(work.run(&["checkout", "-q", "-b", "c2", "main"]).await);
    work.commit(&[("a.txt", "y\n")], "y").await;
    common::ok(work.push("c2").await);
    app.post("/api/v3/repos/alice/r/merges")
        .auth(&alice)
        .json(&json!({"base": "c1", "head": "c2"}))
        .send()
        .await
        .assert_status(409);

    // Post-receive ran for the API merge (Event::Push path, pushed_at).
    assert!(app.drain_jobs().await >= 1);
}

#[tokio::test]
async fn languages_contributors_tags_teams() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let work = common::seeded(
        &app,
        &alice,
        "r",
        &[
            ("src/main.rs", &"fn main() {}\n".repeat(10)),
            ("web/app.ts", "let x = 1;\n"),
            ("README.md", &"docs ".repeat(1000)),
            ("vendor/lib.go", &"package x\n".repeat(100)),
        ],
    )
    .await;
    work.commit_as(
        &[("x.rs", "//")],
        "by alice",
        "Alice",
        "alice@example.com",
        None,
    )
    .await;
    work.commit_as(
        &[("y.rs", "//")],
        "by alice 2",
        "Alice",
        "alice@example.com",
        None,
    )
    .await;
    work.commit_as(&[("z.rs", "//")], "by bob", "Bob", "bob@example.com", None)
        .await;
    common::ok(work.push("main").await);
    common::ok(work.run(&["tag", "v1.2.0"]).await);
    common::ok(work.run(&["tag", "-a", "v1.10.0", "-m", "release"]).await);
    common::ok(work.push("--tags").await);
    app.drain_jobs().await;

    // Languages: bytes per language, vendored/prose excluded, ordered.
    let res = app.get("/api/v3/repos/alice/r/languages").send().await;
    res.assert_status(200);
    let text = res.text();
    assert!(text.starts_with("{\"Rust\":"), "{text}");
    let v = res.json();
    assert_eq!(v["Rust"], 13 * 10 + 6);
    assert_eq!(v["TypeScript"], 11);
    assert!(v.get("Markdown").is_none() && v.get("Go").is_none());
    assert_eq!(
        app.get("/api/v3/repos/alice/r").send().await.json()["language"],
        "Rust"
    );
    // Stale after a push until the job runs again.
    work.commit(&[("more.py", &"print(1)\n".repeat(100))], "python")
        .await;
    common::ok(work.push("main").await);
    app.drain_jobs().await;
    let v = app
        .get("/api/v3/repos/alice/r/languages")
        .send()
        .await
        .json();
    assert_eq!(v["Python"], 900);
    assert_eq!(
        app.get("/api/v3/repos/alice/r").send().await.json()["language"],
        "Python"
    );

    // Contributors: mapped by verified email, anonymous on request.
    let res = app.get("/api/v3/repos/alice/r/contributors").send().await;
    res.assert_status(200);
    let v = res.json();
    let arr = v.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["login"], "alice");
    assert_eq!(arr[0]["contributions"], 2);
    assert_eq!(arr[0]["type"], "User");
    assert_eq!(arr[1]["login"], "bob");
    let v = app
        .get("/api/v3/repos/alice/r/contributors?anon=1")
        .send()
        .await
        .json();
    let anon = v
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["type"] == "Anonymous")
        .unwrap();
    assert_eq!(anon["email"], "test@example.com");
    assert_eq!(anon["name"], "Test");
    assert_eq!(anon["contributions"], 2);
    let _ = bob;
    app.create_repo(&alice, "empty").await;
    app.get("/api/v3/repos/alice/empty/contributors")
        .send()
        .await
        .assert_status(204);

    // Tags: newest version first, peeled commit.
    let head = work.head().await;
    let _ = head;
    let v = app.get("/api/v3/repos/alice/r/tags").send().await.json();
    assert_eq!(v[0]["name"], "v1.10.0");
    assert_eq!(v[1]["name"], "v1.2.0");
    assert_eq!(v[0]["commit"]["sha"].as_str().unwrap().len(), 40);
    assert_eq!(
        v[0]["zipball_url"],
        app.url("/api/v3/repos/alice/r/zipball/refs/tags/v1.10.0")
    );
    assert_eq!(
        v[0]["tarball_url"],
        app.url("/api/v3/repos/alice/r/tarball/refs/tags/v1.10.0")
    );
    assert!(v[0]["node_id"].is_string());

    // Teams of an org repository.
    let org = app.create_org("acme", &alice).await;
    let repo = app
        .create_repo_with(&alice, Some("acme"), json!({"name": "svc"}))
        .await;
    let parent: i64 = sqlx::query_scalar(
        "INSERT INTO teams (org_id, name, slug) VALUES ($1, 'Eng', 'eng') RETURNING id",
    )
    .bind(org.id)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    let child: i64 = sqlx::query_scalar(
        "INSERT INTO teams (org_id, parent_id, name, slug) VALUES ($1, $2, 'Backend', 'backend') RETURNING id",
    )
    .bind(org.id)
    .bind(parent)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO team_repos (team_id, repo_id, permission) VALUES ($1, $2, 'maintain')",
    )
    .bind(child)
    .bind(repo["id"].as_i64().unwrap())
    .execute(&app.state.db)
    .await
    .unwrap();
    let v = app
        .get("/api/v3/repos/acme/svc/teams")
        .auth(&alice)
        .send()
        .await
        .json();
    assert_eq!(v[0]["slug"], "backend");
    assert_eq!(v[0]["permission"], "maintain");
    assert_eq!(v[0]["parent"]["slug"], "eng");
    assert_eq!(v[0]["html_url"], app.url("/orgs/acme/teams/backend"));
}
