//! GHES admin user / organization endpoints.

use bgh_core::events::Event;
use serde_json::json;

#[tokio::test]
async fn admin_endpoints_require_site_admin() {
    let app = bgh_server::test_app().await;
    let admin = app.create_admin("root").await;
    let alice = app.create_user("alice").await;

    let res = app
        .post("/api/v3/admin/users")
        .json(&json!({"login": "x"}))
        .send()
        .await;
    res.assert_status(401);
    let res = app
        .post("/api/v3/admin/users")
        .auth(&alice)
        .json(&json!({"login": "x"}))
        .send()
        .await;
    res.assert_status(403);
    assert_eq!(res.json()["message"], "Must be a site administrator.");

    // An admin's token without the site_admin scope is refused too.
    let token = app.create_token(&admin, &["repo", "admin:org"]).await;
    let res = app
        .post("/api/v3/admin/users")
        .token(&token)
        .json(&json!({"login": "x"}))
        .send()
        .await;
    res.assert_status(403);

    // Sessions carry every scope.
    let cookie = app.session_cookie(&admin).await;
    app.post("/api/v3/admin/users")
        .cookie(&cookie)
        .json(&json!({"login": "via-session"}))
        .send()
        .await
        .assert_status(201);
}

#[tokio::test]
async fn creates_users() {
    let app = bgh_server::test_app().await;
    let admin = app.create_admin("root").await;
    let mut events = app.state.events.subscribe();

    let res = app
        .post("/api/v3/admin/users")
        .auth(&admin)
        .json(&json!({"login": "octo", "email": "octo@example.com"}))
        .send()
        .await;
    res.assert_status(201);
    let body = res.json();
    assert_eq!(body["login"], "octo");
    assert_eq!(body["type"], "User");
    assert_eq!(body["site_admin"], false);
    assert_eq!(body["url"], app.url("/api/v3/users/octo"));
    assert!(body["node_id"].is_string());
    assert!(body["avatar_url"].is_string());

    let ev = events.recv().await.unwrap();
    match &*ev {
        Event::UserAccountChanged { login, action, .. } => {
            assert_eq!(login, "octo");
            assert_eq!(action, "created");
        }
        other => panic!("unexpected event {other:?}"),
    }

    // Duplicate login / email, invalid login.
    let res = app
        .post("/api/v3/admin/users")
        .auth(&admin)
        .json(&json!({"login": "OCTO"}))
        .send()
        .await;
    res.assert_status(422);
    assert_eq!(res.json()["errors"][0]["code"], "already_exists");
    let res = app
        .post("/api/v3/admin/users")
        .auth(&admin)
        .json(&json!({"login": "other", "email": "octo@example.com"}))
        .send()
        .await;
    res.assert_status(422);
    assert_eq!(res.json()["errors"][0]["field"], "email");
    app.post("/api/v3/admin/users")
        .auth(&admin)
        .json(&json!({"login": "bad_login"}))
        .send()
        .await
        .assert_status(422);
    app.post("/api/v3/admin/users")
        .auth(&admin)
        .json(&json!({}))
        .send()
        .await
        .assert_status(422);

    // Created suspended.
    app.post("/api/v3/admin/users")
        .auth(&admin)
        .json(&json!({"login": "sleepy", "suspended": true}))
        .send()
        .await
        .assert_status(201);
    let suspended: bool =
        sqlx::query_scalar("SELECT suspended_at IS NOT NULL FROM users WHERE login = 'sleepy'")
            .fetch_one(&app.state.db)
            .await
            .unwrap();
    assert!(suspended);

    let action: String = sqlx::query_scalar(
        "SELECT action FROM audit_log WHERE action = 'user.create' AND data->>'login' = 'octo'",
    )
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(action, "user.create");
}

#[tokio::test]
async fn renames_and_deletes_users() {
    let app = bgh_server::test_app().await;
    let admin = app.create_admin("root").await;
    let alice = app.create_user("alice").await;
    app.create_repo(&alice, "hello").await;

    let res = app
        .patch("/api/v3/admin/users/alice")
        .auth(&admin)
        .json(&json!({"login": "alicia"}))
        .send()
        .await;
    res.assert_status(202);
    assert_eq!(
        res.json()["message"],
        "Job queued to rename user. It may take a few minutes to complete."
    );
    assert_eq!(res.json()["url"], app.url("/api/v3/users/alicia"));
    app.get("/api/v3/repos/alicia/hello")
        .auth(&admin)
        .send()
        .await
        .assert_status(200);
    // The old login redirects (P50) and stays reserved.
    let res = app.get("/api/v3/users/alice").send().await;
    res.assert_status(301);
    assert_eq!(
        res.header("location").unwrap(),
        app.url(&format!("/api/v3/user/{}", alice.id))
    );

    // Taken login.
    let res = app
        .patch("/api/v3/admin/users/alicia")
        .auth(&admin)
        .json(&json!({"login": "root"}))
        .send()
        .await;
    res.assert_status(422);
    app.patch("/api/v3/admin/users/nobody")
        .auth(&admin)
        .json(&json!({"login": "x"}))
        .send()
        .await
        .assert_status(404);

    // Can't delete yourself.
    app.delete("/api/v3/admin/users/root")
        .auth(&admin)
        .send()
        .await
        .assert_status(403);

    let repo_id: i64 = sqlx::query_scalar("SELECT id FROM repositories WHERE name = 'hello'")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    app.delete("/api/v3/admin/users/alicia")
        .auth(&admin)
        .send()
        .await
        .assert_status(204);
    app.get("/api/v3/users/alicia")
        .send()
        .await
        .assert_status(404);
    let repos: i64 = sqlx::query_scalar("SELECT count(*) FROM repositories")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(repos, 0);
    // Owned repositories are soft-deleted (P50); storage is purged after
    // the retention by `repos.purge_deleted`.
    let deleted: i64 =
        sqlx::query_scalar("SELECT count(*) FROM deleted_repositories WHERE id = $1")
            .bind(repo_id)
            .fetch_one(&app.state.db)
            .await
            .unwrap();
    assert_eq!(deleted, 1);
    // The user's token no longer works.
    app.get("/api/v3/user")
        .auth(&alice)
        .send()
        .await
        .assert_status(401);
}

#[tokio::test]
async fn impersonation_tokens() {
    let app = bgh_server::test_app().await;
    let admin = app.create_admin("root").await;
    app.create_user("alice").await;

    let res = app
        .post("/api/v3/admin/users/alice/authorizations")
        .auth(&admin)
        .json(&json!({"scopes": ["repo", "read:org"]}))
        .send()
        .await;
    res.assert_status(201);
    let body = res.json();
    let token = body["token"].as_str().unwrap().to_string();
    assert!(token.starts_with("bghp_"));
    assert_eq!(body["scopes"], json!(["read:org", "repo"]));
    assert_eq!(body["user"]["login"], "alice");
    assert_eq!(body["token_last_eight"], &token[token.len() - 8..]);
    assert!(body["hashed_token"].is_string());
    assert!(body["app"]["name"].is_string());
    assert!(body["id"].is_i64());
    assert_eq!(
        body["url"],
        app.url(&format!("/api/v3/authorizations/{}", body["id"]))
    );

    // The token acts as alice.
    let res = app.get("/api/v3/user").token(&token).send().await;
    res.assert_status(200);
    assert_eq!(res.json()["login"], "alice");

    // Same scopes → 200 with the existing authorization (secret not shown).
    let res = app
        .post("/api/v3/admin/users/alice/authorizations")
        .auth(&admin)
        .json(&json!({"scopes": ["read:org", "repo"]}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["id"], body["id"]);
    assert_eq!(res.json()["token"], "");

    // site_admin can't be granted through impersonation.
    app.post("/api/v3/admin/users/alice/authorizations")
        .auth(&admin)
        .json(&json!({"scopes": ["site_admin"]}))
        .send()
        .await
        .assert_status(422);

    app.delete("/api/v3/admin/users/alice/authorizations")
        .auth(&admin)
        .send()
        .await
        .assert_status(204);
    app.get("/api/v3/user")
        .token(&token)
        .send()
        .await
        .assert_status(401);

    let logged: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE action IN ('oauth_access.create', 'oauth_access.destroy')",
    )
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(logged, 2);
}

#[tokio::test]
async fn promotes_and_demotes_site_admins() {
    let app = bgh_server::test_app().await;
    let admin = app.create_admin("root").await;
    let alice = app.create_user("alice").await;
    let mut events = app.state.events.subscribe();

    app.put("/api/v3/users/alice/site_admin")
        .auth(&admin)
        .send()
        .await
        .assert_status(204);
    let res = app.get("/api/v3/users/alice").send().await;
    assert_eq!(res.json()["site_admin"], true);
    match &*events.recv().await.unwrap() {
        Event::UserAccountChanged { action, .. } => assert_eq!(action, "promoted"),
        other => panic!("unexpected {other:?}"),
    }

    // Alice (now admin, but her token lacks site_admin scope) can use a session.
    let cookie = app.session_cookie(&alice).await;
    app.delete("/api/v3/users/root/site_admin")
        .cookie(&cookie)
        .send()
        .await
        .assert_status(204);
    // Now alice is the last admin: she can't be demoted.
    let res = app
        .delete("/api/v3/users/alice/site_admin")
        .cookie(&cookie)
        .send()
        .await;
    res.assert_status(422);
    // Root lost access.
    app.put("/api/v3/users/root/site_admin")
        .auth(&admin)
        .send()
        .await
        .assert_status(403);
    // Organizations can't be promoted.
    app.create_org("acme", &alice).await;
    app.put("/api/v3/users/acme/site_admin")
        .cookie(&cookie)
        .send()
        .await
        .assert_status(404);
}

#[tokio::test]
async fn suspension_blocks_tokens_sessions_and_login() {
    let app = bgh_server::test_app().await;
    let admin = app.create_admin("root").await;
    let alice = app.create_user("alice").await;
    let cookie = app.session_cookie(&alice).await;
    app.get("/api/v3/user")
        .cookie(&cookie)
        .send()
        .await
        .assert_status(200);

    app.put("/api/v3/users/alice/suspended")
        .auth(&admin)
        .json(&json!({"reason": "spam"}))
        .send()
        .await
        .assert_status(204);
    let reason: Option<String> =
        sqlx::query_scalar("SELECT suspended_reason FROM users WHERE login = 'alice'")
            .fetch_one(&app.state.db)
            .await
            .unwrap();
    assert_eq!(reason.as_deref(), Some("spam"));

    let res = app.get("/api/v3/user").auth(&alice).send().await;
    res.assert_status(403);
    assert_eq!(res.json()["message"], "Sorry. Your account was suspended.");
    // Sessions were destroyed.
    app.get("/api/v3/user")
        .cookie(&cookie)
        .send()
        .await
        .assert_status(401);
    // Password login refused.
    let res = app
        .post("/_bgh/session")
        .json(&json!({"login": "alice", "password": alice.password}))
        .send()
        .await;
    res.assert_status(403);
    // Git over HTTP refused as well (Basic auth with a token).
    let res = app
        .get("/alice/x.git/info/refs?service=git-upload-pack")
        .basic("alice", &alice.token)
        .send()
        .await;
    res.assert_status(403);

    // Admins and yourself can't be suspended.
    app.put("/api/v3/users/root/suspended")
        .auth(&admin)
        .send()
        .await
        .assert_status(403);

    app.delete("/api/v3/users/alice/suspended")
        .auth(&admin)
        .json(&json!({"reason": "appeal accepted"}))
        .send()
        .await
        .assert_status(204);
    app.get("/api/v3/user")
        .auth(&alice)
        .send()
        .await
        .assert_status(200);

    let actions: Vec<String> = sqlx::query_scalar(
        "SELECT action FROM audit_log WHERE action LIKE 'user.%suspend' ORDER BY id",
    )
    .fetch_all(&app.state.db)
    .await
    .unwrap();
    assert_eq!(actions, vec!["user.suspend", "user.unsuspend"]);
}

#[tokio::test]
async fn renames_organizations() {
    let app = bgh_server::test_app().await;
    let admin = app.create_admin("root").await;
    let alice = app.create_user("alice").await;
    app.create_org("acme", &alice).await;
    let mut events = app.state.events.subscribe();

    let res = app
        .patch("/api/v3/admin/organizations/acme")
        .auth(&admin)
        .json(&json!({"login": "acme-corp"}))
        .send()
        .await;
    res.assert_status(202);
    assert_eq!(
        res.json()["message"],
        "Job queued to rename organization. It may take a few minutes to complete."
    );
    assert_eq!(res.json()["url"], app.url("/api/v3/orgs/acme-corp"));
    app.get("/api/v3/orgs/acme-corp")
        .send()
        .await
        .assert_status(200);
    match &*events.recv().await.unwrap() {
        Event::OrganizationChanged { action, data, .. } => {
            assert_eq!(action, "renamed");
            assert_eq!(data["login"]["from"], "acme");
        }
        other => panic!("unexpected {other:?}"),
    }
    // Users are not organizations.
    app.patch("/api/v3/admin/organizations/alice")
        .auth(&admin)
        .json(&json!({"login": "x"}))
        .send()
        .await
        .assert_status(404);
    // Org audit entry.
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE action = 'org.rename'")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(n, 1);
}
