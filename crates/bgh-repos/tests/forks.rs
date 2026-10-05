//! Forks, template generation, merge-upstream and fork-safe deletion.

mod common;

use serde_json::json;

#[tokio::test]
async fn fork_list_and_existing_fork() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let carol = app.create_user("carol").await;
    let work = common::seeded(&app, &alice, "lib", &[("src/lib.rs", "pub fn x() {}")]).await;
    common::ok(work.run(&["checkout", "-q", "-b", "dev"]).await);
    work.commit(&[("dev.txt", "d")], "dev").await;
    common::ok(work.push("dev").await);

    let res = app
        .post("/api/v3/repos/alice/lib/forks")
        .auth(&bob)
        .json(&json!({}))
        .send()
        .await;
    res.assert_status(202);
    let fork = res.json();
    assert_eq!(fork["full_name"], "bob/lib");
    assert_eq!(fork["fork"], true);
    assert_eq!(fork["parent"]["full_name"], "alice/lib");
    assert_eq!(fork["source"]["full_name"], "alice/lib");
    assert_eq!(fork["default_branch"], "main");
    assert_eq!(fork["permissions"]["admin"], true);
    let parent = app.get("/api/v3/repos/alice/lib").send().await.json();
    assert_eq!(parent["forks_count"], 1);
    assert_eq!(parent["network_count"], 1);

    // Forking again returns the existing fork.
    let again = app
        .post("/api/v3/repos/alice/lib/forks")
        .auth(&bob)
        .json(&json!({}))
        .send()
        .await;
    again.assert_status(202);
    assert_eq!(again.json()["id"], fork["id"]);

    // Fork of the fork: source stays the network root.
    let res = app
        .post("/api/v3/repos/bob/lib/forks")
        .auth(&carol)
        .json(&json!({"name": "my-lib", "default_branch_only": true}))
        .send()
        .await;
    res.assert_status(202);
    let v = res.json();
    assert_eq!(v["full_name"], "carol/my-lib");
    assert_eq!(v["parent"]["full_name"], "bob/lib");
    assert_eq!(v["source"]["full_name"], "alice/lib");
    let branches = app
        .get("/api/v3/repos/carol/my-lib/branches")
        .send()
        .await
        .json();
    assert_eq!(branches.as_array().unwrap().len(), 1, "{branches}");
    let branches = app
        .get("/api/v3/repos/bob/lib/branches")
        .send()
        .await
        .json();
    assert_eq!(branches.as_array().unwrap().len(), 2);

    // The fork has the history.
    let commits = app.get("/api/v3/repos/bob/lib/commits").send().await.json();
    assert_eq!(commits[0]["commit"]["message"], "initial commit");

    // Lists.
    let list = app.get("/api/v3/repos/alice/lib/forks").send().await;
    list.assert_status(200);
    let list = list.json();
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["full_name"], "bob/lib");
    assert_eq!(list[0]["fork"], true);
    let list = app
        .get("/api/v3/repos/alice/lib/forks?sort=oldest")
        .send()
        .await
        .json();
    assert_eq!(list[0]["full_name"], "bob/lib");

    // Can't fork into the owner's own account.
    app.post("/api/v3/repos/alice/lib/forks")
        .auth(&alice)
        .json(&json!({}))
        .send()
        .await
        .assert_status(422);
}

#[tokio::test]
async fn fork_into_org_and_private_rules() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    app.create_org("acme", &bob).await;
    app.create_repo_with(&alice, None, json!({"name": "pub", "auto_init": true}))
        .await;
    app.create_repo_with(
        &alice,
        None,
        json!({"name": "priv", "private": true, "auto_init": true}),
    )
    .await;

    let v = app
        .post("/api/v3/repos/alice/pub/forks")
        .auth(&bob)
        .json(&json!({"organization": "acme"}))
        .send()
        .await;
    v.assert_status(202);
    assert_eq!(v.json()["full_name"], "acme/pub");
    assert_eq!(v.json()["organization"]["login"], "acme");

    // Private repositories: invisible to outsiders.
    app.post("/api/v3/repos/alice/priv/forks")
        .auth(&bob)
        .json(&json!({}))
        .send()
        .await
        .assert_status(404);
    // Collaborators can fork; the fork stays private.
    let id: i64 = app
        .get("/api/v3/repos/alice/priv")
        .auth(&alice)
        .send()
        .await
        .json()["id"]
        .as_i64()
        .unwrap();
    sqlx::query("INSERT INTO collaborators (repo_id, user_id, permission) VALUES ($1, $2, 'read')")
        .bind(id)
        .bind(bob.id)
        .execute(&app.state.db)
        .await
        .unwrap();
    let v = app
        .post("/api/v3/repos/alice/priv/forks")
        .auth(&bob)
        .json(&json!({}))
        .send()
        .await;
    v.assert_status(202);
    assert_eq!(v.json()["private"], true);
    // ... unless forking is disabled.
    sqlx::query("UPDATE repositories SET allow_forking = false WHERE id = $1")
        .bind(id)
        .execute(&app.state.db)
        .await
        .unwrap();
    let carol = app.create_user("carol").await;
    sqlx::query("INSERT INTO collaborators (repo_id, user_id, permission) VALUES ($1, $2, 'read')")
        .bind(id)
        .bind(carol.id)
        .execute(&app.state.db)
        .await
        .unwrap();
    app.post("/api/v3/repos/alice/priv/forks")
        .auth(&carol)
        .json(&json!({}))
        .send()
        .await
        .assert_status(403);
}

#[tokio::test]
async fn deleting_the_source_keeps_forks_working() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let carol = app.create_user("carol").await;
    let work = common::seeded(&app, &alice, "src", &[("a.txt", "one")]).await;
    let head = work.head().await;
    app.post("/api/v3/repos/alice/src/forks")
        .auth(&bob)
        .json(&json!({}))
        .send()
        .await
        .assert_status(202);
    app.post("/api/v3/repos/bob/src/forks")
        .auth(&carol)
        .json(&json!({}))
        .send()
        .await
        .assert_status(202);

    // Delete the network root, then the middle fork.
    app.delete("/api/v3/repos/alice/src")
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    app.drain_jobs().await;
    let v = app.get("/api/v3/repos/bob/src").send().await.json();
    assert_eq!(v["fork"], true);
    assert!(v.get("parent").is_none());

    app.delete("/api/v3/repos/bob/src")
        .auth(&bob)
        .send()
        .await
        .assert_status(204);
    app.drain_jobs().await;

    // Carol's fork still has every object: clone + read through the API.
    let tmp = tempfile::tempdir().unwrap();
    common::ok(
        common::git(
            tmp.path(),
            &["clone", "-q", &app.url("/carol/src.git"), "c"],
        )
        .await,
    );
    let out = common::ok(common::git(&tmp.path().join("c"), &["fsck", "--full"]).await);
    assert!(!out.stderr.contains("missing"), "{}", out.stderr);
    let c = app
        .get(&format!("/api/v3/repos/carol/src/commits/{head}"))
        .send()
        .await;
    c.assert_status(200);
    assert_eq!(c.json()["files"][0]["filename"], "a.txt");
    let store = bgh_git::RepoStore::from_config(&app.state.config);
    let id = app.get("/api/v3/repos/carol/src").send().await.json()["id"]
        .as_i64()
        .unwrap();
    assert!(!store.path(id).join("objects/info/alternates").exists());
}

#[tokio::test]
async fn generate_from_template() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let work = common::seeded(
        &app,
        &alice,
        "tpl",
        &[("README.md", "# Template"), ("src/a.rs", "x")],
    )
    .await;
    work.commit(&[("second.txt", "2")], "second").await;
    common::ok(work.push("main").await);
    common::ok(work.run(&["checkout", "-q", "-b", "extra"]).await);
    common::ok(work.push("extra").await);

    // Not a template yet.
    app.post("/api/v3/repos/alice/tpl/generate")
        .auth(&bob)
        .json(&json!({"name": "mine"}))
        .send()
        .await
        .assert_status(422);
    app.patch("/api/v3/repos/alice/tpl")
        .auth(&alice)
        .json(&json!({"is_template": true}))
        .send()
        .await
        .assert_status(200);
    app.post("/api/v3/repos/alice/tpl/generate")
        .auth(&bob)
        .json(&json!({}))
        .send()
        .await
        .assert_status(422);

    let res = app
        .post("/api/v3/repos/alice/tpl/generate")
        .auth(&bob)
        .json(&json!({"name": "mine", "description": "From template", "private": true}))
        .send()
        .await;
    res.assert_status(201);
    let v = res.json();
    assert_eq!(v["full_name"], "bob/mine");
    assert_eq!(v["private"], true);
    assert_eq!(v["fork"], false);
    assert_eq!(v["description"], "From template");
    assert_eq!(v["template_repository"]["full_name"], "alice/tpl");

    // One fresh commit with the template's tree; only the default branch.
    let commits = app
        .get("/api/v3/repos/bob/mine/commits")
        .auth(&bob)
        .send()
        .await
        .json();
    assert_eq!(commits.as_array().map(Vec::len), Some(1), "{commits}");
    assert_eq!(commits[0]["commit"]["message"], "Initial commit");
    assert!(commits[0]["parents"].as_array().unwrap().is_empty());
    let branches = app
        .get("/api/v3/repos/bob/mine/branches")
        .auth(&bob)
        .send()
        .await
        .json();
    assert_eq!(branches.as_array().unwrap().len(), 1);
    // Objects were copied: the new repo has no alternates.
    let id = v["id"].as_i64().unwrap();
    let store = bgh_git::RepoStore::from_config(&app.state.config);
    assert!(!store.path(id).join("objects/info/alternates").exists());
    let readme = store
        .read(id, |r| match r.lookup_path("main", "src/a.rs")? {
            bgh_git::PathLookup::Entry(e) => r.blob(&e.sha),
            _ => panic!("file"),
        })
        .await
        .unwrap();
    assert_eq!(readme.data, b"x");

    let res = app
        .post("/api/v3/repos/alice/tpl/generate")
        .auth(&bob)
        .json(&json!({"name": "all", "include_all_branches": true}))
        .send()
        .await;
    res.assert_status(201);
    let branches = app
        .get("/api/v3/repos/bob/all/branches")
        .send()
        .await
        .json();
    assert_eq!(branches.as_array().unwrap().len(), 2);

    // Duplicate name.
    app.post("/api/v3/repos/alice/tpl/generate")
        .auth(&bob)
        .json(&json!({"name": "mine"}))
        .send()
        .await
        .assert_status(422);
}

#[tokio::test]
async fn merge_upstream() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let up = common::seeded(&app, &alice, "up", &[("a.txt", "a\n")]).await;
    app.post("/api/v3/repos/alice/up/forks")
        .auth(&bob)
        .json(&json!({}))
        .send()
        .await
        .assert_status(202);

    // Nothing to do.
    let v = app
        .post("/api/v3/repos/bob/up/merge-upstream")
        .auth(&bob)
        .json(&json!({"branch": "main"}))
        .send()
        .await;
    v.assert_status(200);
    assert_eq!(v.json()["merge_type"], "none");

    // Upstream moves: fast-forward.
    let new_up = up.commit(&[("b.txt", "b\n")], "upstream change").await;
    common::ok(up.push("main").await);
    let v = app
        .post("/api/v3/repos/bob/up/merge-upstream")
        .auth(&bob)
        .json(&json!({"branch": "main"}))
        .send()
        .await;
    v.assert_status(200);
    assert_eq!(v.json()["merge_type"], "fast-forward");
    assert_eq!(v.json()["base_branch"], "alice:main");
    let b = app
        .get("/api/v3/repos/bob/up/branches/main")
        .send()
        .await
        .json();
    assert_eq!(b["commit"]["sha"], new_up);

    // Both moved (different files): merge commit.
    let fork_work = common::Work {
        dir: tempfile::tempdir().unwrap(),
        remote: app.git_remote(&bob, "bob", "up"),
    };
    common::ok(
        common::git(
            fork_work.dir.path(),
            &["clone", "-q", &fork_work.remote, "."],
        )
        .await,
    );
    fork_work.commit(&[("c.txt", "c\n")], "fork change").await;
    common::ok(fork_work.push("main").await);
    up.commit(&[("d.txt", "d\n")], "upstream again").await;
    common::ok(up.push("main").await);
    let v = app
        .post("/api/v3/repos/bob/up/merge-upstream")
        .auth(&bob)
        .json(&json!({"branch": "main"}))
        .send()
        .await;
    v.assert_status(200);
    assert_eq!(v.json()["merge_type"], "merge");
    let b = app
        .get("/api/v3/repos/bob/up/branches/main")
        .send()
        .await
        .json();
    assert_eq!(
        b["commit"]["parents"].as_array().map(Vec::len),
        Some(2),
        "{b}"
    );

    // Conflicting edits: 409.
    common::ok(
        fork_work
            .run(&["pull", "-q", &fork_work.remote, "main"])
            .await,
    );
    fork_work
        .commit(&[("a.txt", "fork\n")], "fork edits a")
        .await;
    common::ok(fork_work.push("main").await);
    up.commit(&[("a.txt", "upstream\n")], "upstream edits a")
        .await;
    common::ok(up.push("main").await);
    app.post("/api/v3/repos/bob/up/merge-upstream")
        .auth(&bob)
        .json(&json!({"branch": "main"}))
        .send()
        .await
        .assert_status(409);

    // Errors: not a fork, missing branch, no write access.
    app.post("/api/v3/repos/alice/up/merge-upstream")
        .auth(&alice)
        .json(&json!({"branch": "main"}))
        .send()
        .await
        .assert_status(422);
    app.post("/api/v3/repos/bob/up/merge-upstream")
        .auth(&bob)
        .json(&json!({"branch": "nope"}))
        .send()
        .await
        .assert_status(404);
    app.post("/api/v3/repos/bob/up/merge-upstream")
        .auth(&alice)
        .json(&json!({"branch": "main"}))
        .send()
        .await
        .assert_status(403);
}
