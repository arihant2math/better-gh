//! Access policy (P7): internal visibility, private mode and the allowed
//! repository visibilities.

use bgh_core::testing::{TestApp, TestOrg, TestUser};
use serde_json::{Value, json};

use crate::gitwork::{Work, git, ok};

/// `acme` (base permission `none`, admin `owner`) with an internal
/// repository `inner` holding one commit.
async fn internal_fixture(app: &TestApp) -> (TestUser, TestOrg, Value) {
    let owner = app.create_user("owner").await;
    let org = app.create_org("acme", &owner).await;
    sqlx::query("UPDATE org_settings SET default_repository_permission = 'none' WHERE org_id = $1")
        .bind(org.id)
        .execute(&app.state.db)
        .await
        .unwrap();
    let repo = app
        .create_repo_with(
            &owner,
            Some("acme"),
            json!({"name": "inner", "visibility": "internal"}),
        )
        .await;
    let work = Work {
        dir: tempfile::tempdir().unwrap(),
        remote: app.git_remote(&owner, "acme", "inner"),
    };
    ok(git(work.dir.path(), &["init", "-q", "-b", "main"]).await);
    work.commit(&[("README.md", "# inner\n")], "initial").await;
    ok(work.push("main").await);
    app.drain_jobs().await;
    (owner, org, repo)
}

#[tokio::test]
async fn internal_repo_json_and_permissions() {
    let app = bgh_server::test_app().await;
    let (_owner, org, repo) = internal_fixture(&app).await;
    assert_eq!(repo["private"], true);
    assert_eq!(repo["visibility"], "internal");

    // A signed-in user outside the organization can read, not write.
    let bob = app.create_user("bob").await;
    let res = app.get("/api/v3/repos/acme/inner").auth(&bob).send().await;
    res.assert_status(200);
    let body = res.json();
    assert_eq!(body["visibility"], "internal");
    assert_eq!(body["private"], true);
    assert_eq!(body["permissions"]["pull"], true);
    assert_eq!(body["permissions"]["push"], false);
    assert_eq!(body["permissions"]["admin"], false);
    app.get("/api/v3/repos/acme/inner/contents/README.md")
        .auth(&bob)
        .send()
        .await
        .assert_status(200);
    app.put("/api/v3/repos/acme/inner/contents/x.txt")
        .auth(&bob)
        .json(&json!({"message": "x", "content": "eA=="}))
        .send()
        .await
        .assert_status(403);
    app.patch("/api/v3/repos/acme/inner")
        .auth(&bob)
        .json(&json!({"description": "nope"}))
        .send()
        .await
        .assert_status(403);
    app.get("/api/v3/repos/acme/inner/collaborators/bob/permission")
        .auth(&bob)
        .send()
        .await
        .assert_status(200);

    // Org members get read even with base permission `none`.
    let carol = app.create_user("carol").await;
    app.add_org_member(&org, &carol, "member").await;
    let res = app
        .get("/api/v3/repos/acme/inner")
        .auth(&carol)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["permissions"]["push"], false);

    // Anonymous callers and tokens without `repo` see nothing.
    app.get("/api/v3/repos/acme/inner")
        .send()
        .await
        .assert_status(404);
    let public_only = app.create_token(&bob, &["public_repo"]).await;
    app.get("/api/v3/repos/acme/inner")
        .token(&public_only)
        .send()
        .await
        .assert_status(404);

    // Suspended users have no credentials at all.
    sqlx::query("UPDATE users SET suspended_at = now() WHERE id = $1")
        .bind(bob.id)
        .execute(&app.state.db)
        .await
        .unwrap();
    let res = app.get("/api/v3/repos/acme/inner").auth(&bob).send().await;
    assert!(matches!(res.status(), 401 | 403), "{}", res.status());
    let raw = bgh_core::perms::users_repo_permissions(
        &app.state.db,
        &bgh_core::models::db::Repository::find(&app.state.db, repo["id"].as_i64().unwrap())
            .await
            .unwrap()
            .unwrap(),
        &[bob.id, carol.id],
    )
    .await
    .unwrap();
    assert_eq!(raw[&bob.id], bgh_core::perms::Permission::None);
    assert_eq!(raw[&carol.id], bgh_core::perms::Permission::Read);
}

#[tokio::test]
async fn internal_repo_in_lists() {
    let app = bgh_server::test_app().await;
    let (owner, _org, _repo) = internal_fixture(&app).await;
    app.create_repo_with(&owner, Some("acme"), json!({"name": "open"}))
        .await;
    app.create_repo_with(
        &owner,
        Some("acme"),
        json!({"name": "secret", "visibility": "private"}),
    )
    .await;
    let bob = app.create_user("bob").await;
    let names = |v: Value| -> Vec<String> {
        let mut n: Vec<String> = v
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["name"].as_str().unwrap().to_string())
            .collect();
        n.sort();
        n
    };
    let res = app.get("/api/v3/orgs/acme/repos").auth(&bob).send().await;
    res.assert_status(200);
    assert_eq!(names(res.json()), ["inner", "open"]);
    let res = app.get("/api/v3/orgs/acme/repos").send().await;
    assert_eq!(names(res.json()), ["open"]);
    let res = app
        .get("/api/v3/orgs/acme/repos?type=internal")
        .auth(&owner)
        .send()
        .await;
    assert_eq!(names(res.json()), ["inner"]);
}

#[tokio::test]
async fn internal_repo_git_transport() {
    let app = bgh_server::test_app().await;
    let (_owner, _org, _repo) = internal_fixture(&app).await;
    let bob = app.create_user("bob").await;
    let tmp = tempfile::tempdir().unwrap();

    // Signed-in non-member: clone works, push is refused.
    let remote = app.git_remote(&bob, "acme", "inner");
    ok(git(tmp.path(), &["clone", "-q", &remote, "bob"]).await);
    let clone = tmp.path().join("bob");
    std::fs::write(clone.join("b.txt"), "b").unwrap();
    ok(git(&clone, &["add", "."]).await);
    ok(git(&clone, &["commit", "-q", "-m", "b"]).await);
    let out = git(&clone, &["push", "-q", "origin", "main"]).await;
    assert!(!out.ok);
    assert!(
        out.stderr.contains("403") || out.stderr.contains("denied"),
        "{}",
        out.stderr
    );

    // Anonymous: credentials are requested.
    let anon = app.url("/acme/inner.git");
    let out = git(tmp.path(), &["clone", "-q", &anon, "anon"]).await;
    assert!(!out.ok);
    let res = app
        .get("/acme/inner.git/info/refs?service=git-upload-pack")
        .send()
        .await;
    res.assert_status(401);
    assert!(res.header("www-authenticate").is_some());
}

#[tokio::test]
async fn forks_and_transfers_of_internal_repos() {
    let app = bgh_server::test_app().await;
    let (owner, org, _repo) = internal_fixture(&app).await;
    sqlx::query(
        "UPDATE org_settings SET members_can_fork_private_repositories = true WHERE org_id = $1",
    )
    .bind(org.id)
    .execute(&app.state.db)
    .await
    .unwrap();
    let bob = app.create_user("bob").await;
    // Into a user account: private.
    let res = app
        .post("/api/v3/repos/acme/inner/forks")
        .auth(&bob)
        .json(&json!({}))
        .send()
        .await;
    res.assert_status(202);
    assert_eq!(res.json()["visibility"], "private");
    // Into another organization: stays internal.
    app.create_org("beta", &bob).await;
    let res = app
        .post("/api/v3/repos/acme/inner/forks")
        .auth(&bob)
        .json(&json!({"organization": "beta"}))
        .send()
        .await;
    res.assert_status(202);
    assert_eq!(res.json()["visibility"], "internal");
    // Transfer to a user: private.
    let res = app
        .post("/api/v3/repos/acme/inner/transfer")
        .auth(&owner)
        .json(&json!({"new_owner": "owner"}))
        .send()
        .await;
    res.assert_status(202);
    assert_eq!(res.json()["visibility"], "private");
    // User repositories can't be internal.
    app.patch("/api/v3/repos/owner/inner")
        .auth(&owner)
        .json(&json!({"visibility": "internal"}))
        .send()
        .await
        .assert_status(422);
}

#[tokio::test]
async fn members_can_create_internal_repositories() {
    let app = bgh_server::test_app().await;
    let owner = app.create_user("owner").await;
    let org = app.create_org("acme", &owner).await;
    let member = app.create_user("member").await;
    app.add_org_member(&org, &member, "member").await;
    let res = app
        .patch("/api/v3/orgs/acme")
        .auth(&owner)
        .json(&json!({"members_can_create_internal_repositories": false}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(
        res.json()["members_can_create_internal_repositories"],
        false
    );
    app.post("/api/v3/orgs/acme/repos")
        .auth(&member)
        .json(&json!({"name": "i", "visibility": "internal"}))
        .send()
        .await
        .assert_status(403);
    app.post("/api/v3/orgs/acme/repos")
        .auth(&member)
        .json(&json!({"name": "p", "visibility": "private"}))
        .send()
        .await
        .assert_status(201);
    // Admins are not restricted.
    app.post("/api/v3/orgs/acme/repos")
        .auth(&owner)
        .json(&json!({"name": "i", "visibility": "internal"}))
        .send()
        .await
        .assert_status(201);
    let res = app
        .patch("/api/v3/orgs/acme")
        .auth(&owner)
        .json(&json!({"members_can_create_internal_repositories": true}))
        .send()
        .await;
    assert_eq!(res.json()["members_can_create_internal_repositories"], true);
    app.post("/api/v3/orgs/acme/repos")
        .auth(&member)
        .json(&json!({"name": "i2", "visibility": "internal"}))
        .send()
        .await
        .assert_status(201);
}

#[tokio::test]
async fn allowed_visibilities_are_enforced() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let org = app.create_org("acme", &alice).await;
    let _ = org;
    // Created before the policy changes.
    app.create_repo(&alice, "old-public").await;
    let tpl = app
        .create_repo_with(
            &alice,
            None,
            json!({"name": "tpl", "private": true, "is_template": true, "auto_init": true}),
        )
        .await;
    assert_eq!(tpl["is_template"], true);
    app.set_settings(
        "privacy",
        json!({"allowed_visibilities": ["private", "internal"]}),
    )
    .await;

    let res = app
        .post("/api/v3/user/repos")
        .auth(&alice)
        .json(&json!({"name": "pub", "private": false}))
        .send()
        .await;
    res.assert_status(422);
    let body = res.json();
    assert_eq!(body["message"], "Validation Failed");
    assert_eq!(body["errors"][0]["resource"], "Repository");
    assert_eq!(body["errors"][0]["field"], "visibility");
    assert_eq!(body["errors"][0]["code"], "custom");
    app.post("/api/v3/orgs/acme/repos")
        .auth(&alice)
        .json(&json!({"name": "pub", "visibility": "public"}))
        .send()
        .await
        .assert_status(422);
    // Without an explicit visibility the default (public) falls back to an
    // allowed one.
    let res = app
        .post("/api/v3/user/repos")
        .auth(&alice)
        .json(&json!({"name": "dflt"}))
        .send()
        .await;
    res.assert_status(201);
    assert_eq!(res.json()["visibility"], "private");
    app.post("/api/v3/orgs/acme/repos")
        .auth(&alice)
        .json(&json!({"name": "inner", "visibility": "internal"}))
        .send()
        .await
        .assert_status(201);

    // PATCH to a disallowed visibility; unrelated edits of an existing
    // public repository still work.
    app.patch("/api/v3/repos/alice/dflt")
        .auth(&alice)
        .json(&json!({"private": false}))
        .send()
        .await
        .assert_status(422);
    app.patch("/api/v3/repos/alice/old-public")
        .auth(&alice)
        .json(&json!({"description": "still editable"}))
        .send()
        .await
        .assert_status(200);

    // Fork, transfer and template generation create repositories too.
    let bob = app.create_user("bob").await;
    app.post("/api/v3/repos/alice/old-public/forks")
        .auth(&bob)
        .json(&json!({}))
        .send()
        .await
        .assert_status(422);
    app.post("/api/v3/repos/alice/old-public/transfer")
        .auth(&alice)
        .json(&json!({"new_owner": "acme"}))
        .send()
        .await
        .assert_status(422);
    app.post("/api/v3/repos/alice/tpl/generate")
        .auth(&alice)
        .json(&json!({"name": "gen-pub", "private": false}))
        .send()
        .await
        .assert_status(422);
    app.post("/api/v3/repos/alice/tpl/generate")
        .auth(&alice)
        .json(&json!({"name": "gen-priv", "private": true}))
        .send()
        .await
        .assert_status(201);

    // The web client learns the policy from /_bgh/site.
    let site = app.get("/_bgh/site").send().await.json();
    assert_eq!(
        site["repository_visibilities"]["allowed"],
        json!(["private", "internal"])
    );
    assert_eq!(site["repository_visibilities"]["default_user"], "private");
    assert_eq!(site["private_mode"], false);
}

#[tokio::test]
async fn private_mode_refuses_anonymous_requests() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let work = crate::gitwork::seeded(&app, &alice, "demo", &[("a.txt", "a\n")]).await;
    drop(work);
    app.set_settings("privacy", json!({"private_mode": true}))
        .await;

    // API: 401 "Requires authentication".
    let res = app.get("/api/v3/repos/alice/demo").send().await;
    res.assert_status(401);
    assert_eq!(res.json()["message"], "Requires authentication");
    for path in [
        "/api/v3/users",
        "/api/v3/organizations",
        "/api/v3/users/alice",
        "/api/v3/search/repositories?q=demo",
        "/api/v3/repos/alice/demo/tarball",
        "/_bgh/sync/bootstrap",
        "/_bgh/sync/ws",
        "/_bgh/search?q=demo",
    ] {
        app.get(path).send().await.assert_status(401);
    }
    app.post("/api/graphql")
        .json(&json!({"query": "{ viewer { login } }"}))
        .send()
        .await
        .assert_status(401);
    // Raw files, archives and avatars need auth too.
    for path in [
        "/alice/demo/raw/main/a.txt",
        "/alice/demo/archive/main.zip",
        &format!("/avatars/u/{}", alice.id),
    ] {
        app.get(path).send().await.assert_status(401);
    }

    // Git: 401 with a challenge, clone fails anonymously.
    let res = app
        .get("/alice/demo.git/info/refs?service=git-upload-pack")
        .send()
        .await;
    res.assert_status(401);
    assert!(
        res.header("www-authenticate")
            .is_some_and(|h| h.starts_with("Basic"))
    );
    let tmp = tempfile::tempdir().unwrap();
    let out = git(
        tmp.path(),
        &["clone", "-q", &app.url("/alice/demo.git"), "anon"],
    )
    .await;
    assert!(!out.ok);
    // Credentials (token or password) still work.
    ok(git(
        tmp.path(),
        &[
            "clone",
            "-q",
            &app.git_remote(&alice, "alice", "demo"),
            "tok",
        ],
    )
    .await);
    let pw = format!(
        "http://alice:{}@{}/alice/demo.git",
        alice.password, app.addr
    );
    ok(git(tmp.path(), &["clone", "-q", &pw, "pw"]).await);
    // Bad credentials are still a 401.
    app.get("/alice/demo.git/info/refs?service=git-upload-pack")
        .basic("alice", "wrong")
        .send()
        .await
        .assert_status(401);

    // Web pages redirect to the sign-in page.
    let res = app
        .get("/alice/demo/issues?q=1")
        .header("accept", "text/html")
        .send()
        .await;
    res.assert_status(302);
    assert_eq!(
        res.header("location"),
        Some("/login?return_to=%2Falice%2Fdemo%2Fissues%3Fq%3D1")
    );
    let res = app.get("/").header("accept", "text/html").send().await;
    res.assert_status(302);
    assert_eq!(res.header("location"), Some("/login"));
    for page in ["/login", "/signup", "/password_reset", "/favicon.svg"] {
        let res = app.get(page).header("accept", "text/html").send().await;
        assert!(
            !matches!(res.status(), 302 | 401),
            "{page}: {}",
            res.status()
        );
    }

    // Exempt endpoints.
    app.get("/healthz").send().await.assert_status(200);
    app.get("/api/v3/meta").send().await.assert_status(200);
    let site = app.get("/_bgh/site").send().await;
    site.assert_status(200);
    assert_eq!(site.json()["private_mode"], true);
    let res = app
        .post("/_bgh/auth/login")
        .json(&json!({"login": "alice", "password": alice.password}))
        .send()
        .await;
    res.assert_status(200);

    // Signed in, everything works.
    app.get("/api/v3/repos/alice/demo")
        .auth(&alice)
        .send()
        .await
        .assert_status(200);
    app.get("/api/v3/users")
        .auth(&alice)
        .send()
        .await
        .assert_status(200);
    app.get("/alice/demo/raw/main/a.txt")
        .auth(&alice)
        .send()
        .await
        .assert_status(200);
    let cookie = app.session_cookie(&alice).await;
    app.get(&format!("/avatars/u/{}", alice.id))
        .cookie(&cookie)
        .send()
        .await;
    // The tarball redirect carries a download token usable without
    // credentials.
    let res = app
        .get("/api/v3/repos/alice/demo/tarball/main")
        .auth(&alice)
        .send()
        .await;
    res.assert_status(302);
    let location = res.header("location").unwrap().to_string();
    assert!(location.contains("?token="), "{location}");
    let path = location
        .strip_prefix(&app.state.config.base_url)
        .unwrap_or(&location)
        .to_string();
    app.get(&path).send().await.assert_status(200);
    app.get("/alice/demo/legacy.tar.gz/main?token=bogus")
        .send()
        .await
        .assert_status(404);
}

#[tokio::test]
async fn anonymous_directory_can_be_disabled() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    app.create_org("acme", &alice).await;
    app.get("/api/v3/users").send().await.assert_status(200);
    app.get("/api/v3/organizations")
        .send()
        .await
        .assert_status(200);
    app.set_settings("privacy", json!({"allow_anonymous_directory": false}))
        .await;
    let res = app.get("/api/v3/users").send().await;
    res.assert_status(401);
    assert_eq!(res.json()["message"], "Requires authentication");
    app.get("/api/v3/organizations")
        .send()
        .await
        .assert_status(401);
    app.get("/api/v3/users")
        .auth(&alice)
        .send()
        .await
        .assert_status(200);
    app.get("/api/v3/organizations")
        .auth(&alice)
        .send()
        .await
        .assert_status(200);
    // Profiles and public repositories stay readable.
    app.get("/api/v3/users/alice")
        .send()
        .await
        .assert_status(200);
}
