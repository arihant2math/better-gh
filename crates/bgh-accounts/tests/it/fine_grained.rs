//! P47: fine-grained personal access tokens, organization token policies
//! and narrow classic scopes.

use std::path::Path;

use bgh_core::testing::{TestApp, TestUser};
use serde_json::{Value, json};

async fn git(dir: &Path, args: &[&str]) -> std::process::Output {
    let mut c = tokio::process::Command::new("git");
    for k in [
        "http_proxy",
        "https_proxy",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "all_proxy",
    ] {
        c.env_remove(k);
    }
    c.current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .args(args)
        .output()
        .await
        .expect("run git")
}

async fn private_repo(app: &TestApp, user: &TestUser, org: Option<&str>, name: &str) -> Value {
    app.create_repo_with(
        user,
        org,
        json!({ "name": name, "private": true, "auto_init": true }),
    )
    .await
}

/// Create a fine-grained token through the web API; returns the response.
async fn create(app: &TestApp, cookie: &str, body: Value) -> bgh_core::testing::TestResponse {
    app.post("/_bgh/fine-grained-tokens")
        .cookie(cookie)
        .json(&body)
        .send()
        .await
}

async fn create_ok(app: &TestApp, cookie: &str, body: Value) -> Value {
    let res = create(app, cookie, body).await;
    res.assert_status(201);
    res.json()
}

fn token(v: &Value) -> &str {
    v["token"].as_str().expect("token")
}

#[tokio::test]
async fn selected_repository_contents_read() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    private_repo(&app, &alice, None, "a").await;
    private_repo(&app, &alice, None, "b").await;
    let cookie = app.session_cookie(&alice).await;
    let created = create_ok(
        &app,
        &cookie,
        json!({
            "name": "ci",
            "expires_in_days": 30,
            "repository_selection": "selected",
            "repositories": ["a"],
            "permissions": {"repository": {"contents": "read"}}
        }),
    )
    .await;
    let t = token(&created);
    assert!(t.starts_with("bgh_pat_"), "{t}");
    assert_eq!(created["status"], "active");
    assert_eq!(created["resource_owner"]["login"], "alice");
    assert_eq!(created["repository_selection"], "selected");
    assert_eq!(created["repositories"][0]["full_name"], "alice/a");
    assert_eq!(
        created["permissions"]["repository"],
        json!({"contents": "read", "metadata": "read"})
    );
    assert!(created["expires_at"].is_string());

    // Listed without the secret.
    let list = app
        .get("/_bgh/fine-grained-tokens")
        .cookie(&cookie)
        .send()
        .await;
    list.assert_status(200);
    assert_eq!(list.json()[0]["id"], created["id"]);
    assert!(list.json()[0].get("token").is_none());
    // Classic token list stays classic.
    let classic = app.get("/_bgh/tokens").cookie(&cookie).send().await;
    assert!(
        classic
            .json()
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t["id"] != created["id"]),
    );

    // REST: A readable, contents readable but not writable; B invisible.
    let res = app.get("/api/v3/repos/alice/a").token(t).send().await;
    res.assert_status(200);
    assert!(res.header("x-oauth-scopes").is_none());
    app.get("/api/v3/repos/alice/a/contents/README.md")
        .token(t)
        .send()
        .await
        .assert_status(200);
    let res = app
        .put("/api/v3/repos/alice/a/contents/new.txt")
        .token(t)
        .json(&json!({"message": "x", "content": "aGk="}))
        .send()
        .await;
    res.assert_status(403);
    assert_eq!(
        res.json()["message"],
        "Resource not accessible by personal access token"
    );
    app.get("/api/v3/repos/alice/b")
        .token(t)
        .send()
        .await
        .assert_status(404);
    app.get("/api/v3/repos/alice/b/contents/README.md")
        .token(t)
        .send()
        .await
        .assert_status(404);
    // Issues aren't granted.
    app.get("/api/v3/repos/alice/a/issues")
        .token(t)
        .send()
        .await
        .assert_status(403);
    // Account: the profile is readable, the rest needs account permissions.
    app.get("/api/v3/user")
        .token(t)
        .send()
        .await
        .assert_status(200);
    app.get("/api/v3/user/emails")
        .token(t)
        .send()
        .await
        .assert_status(403);
    app.post("/api/v3/user/repos")
        .token(t)
        .json(&json!({"name": "nope"}))
        .send()
        .await
        .assert_status(403);
    app.get("/api/v3/notifications")
        .token(t)
        .send()
        .await
        .assert_status(403);
    // Listing the user's repositories shows only the selected private one.
    let repos = app.get("/api/v3/user/repos").token(t).send().await;
    repos.assert_status(200);
    let names: Vec<String> = repos
        .json()
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["name"].as_str().unwrap().to_string())
        .collect();
    assert!(!names.contains(&"b".to_string()), "{names:?}");
    // Web JSON writes are refused; token management needs a session.
    app.get("/_bgh/fine-grained-tokens")
        .token(t)
        .send()
        .await
        .assert_status(403);

    // Git: clone A, can't push, B is invisible.
    let remote = |repo: &str| {
        app.url(&format!("/alice/{repo}.git"))
            .replace("http://", &format!("http://alice:{t}@"))
    };
    let tmp = tempfile::tempdir().unwrap();
    let out = git(tmp.path(), &["clone", &remote("a"), "a"]).await;
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let work = tmp.path().join("a");
    std::fs::write(work.join("x.txt"), "hello\n").unwrap();
    git(&work, &["add", "."]).await;
    git(&work, &["commit", "-m", "x"]).await;
    let out = git(&work, &["push", &remote("a"), "HEAD:main"]).await;
    assert!(!out.status.success());
    let out = git(tmp.path(), &["clone", &remote("b"), "b"]).await;
    assert!(!out.status.success());

    // Deleting the token revokes it.
    app.delete(&format!("/_bgh/fine-grained-tokens/{}", created["id"]))
        .cookie(&cookie)
        .send()
        .await
        .assert_status(204);
    app.get("/api/v3/user")
        .token(t)
        .send()
        .await
        .assert_status(401);
}

#[tokio::test]
async fn write_permissions_and_user_role() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    private_repo(&app, &alice, None, "a").await;
    app.create_repo(&bob, "pub").await;
    let cookie = app.session_cookie(&alice).await;
    let created = create_ok(
        &app,
        &cookie,
        json!({
            "name": "issues",
            "expires_in_days": 7,
            "repository_selection": "all",
            "permissions": {
                "repository": {"issues": "write", "contents": "write"},
                "account": {"email_addresses": "read"}
            }
        }),
    )
    .await;
    let t = token(&created);
    app.post("/api/v3/repos/alice/a/issues")
        .token(t)
        .json(&json!({"title": "from token"}))
        .send()
        .await
        .assert_status(201);
    app.get("/api/v3/user/emails")
        .token(t)
        .send()
        .await
        .assert_status(200);
    // Bob's public repository: readable, not writable.
    app.get("/api/v3/repos/bob/pub")
        .token(t)
        .send()
        .await
        .assert_status(200);
    app.post("/api/v3/repos/bob/pub/issues")
        .token(t)
        .json(&json!({"title": "nope"}))
        .send()
        .await
        .assert_status(403);
    // Administration isn't grantable.
    app.patch("/api/v3/repos/alice/a")
        .token(t)
        .json(&json!({"description": "x"}))
        .send()
        .await
        .assert_status(403);
    // Basic auth with the token works like for classic tokens.
    app.get("/api/v3/repos/alice/a")
        .basic("whoever", t)
        .send()
        .await
        .assert_status(200);
}

#[tokio::test]
async fn validation_owners_and_catalog() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let carol = app.create_user("carol").await;
    let acme = app.create_org("acme", &carol).await;
    app.add_org_member(&acme, &alice, "member").await;
    app.create_org("other", &carol).await;
    let cookie = app.session_cookie(&alice).await;
    let base = json!({
        "name": "t", "expires_in_days": 30, "repository_selection": "public",
        "permissions": {"repository": {"contents": "read"}}
    });
    let with = |patch: Value| {
        let mut b = base.clone();
        for (k, v) in patch.as_object().unwrap() {
            b[k] = v.clone();
        }
        b
    };
    for (body, field) in [
        (with(json!({"expires_in_days": null})), "expires_in_days"),
        (with(json!({"expires_in_days": 400})), "expires_in_days"),
        (with(json!({"name": ""})), "name"),
        (
            with(json!({"permissions": {"organization": {"members": "read"}}})),
            "permissions",
        ),
        (
            with(json!({"permissions": {"repository": {"contents": "admin"}}})),
            "permissions",
        ),
        (
            with(json!({"permissions": {"repository": {"contents": "write"}}})),
            "permissions",
        ),
        (with(json!({"resource_owner": "other"})), "resource_owner"),
        (
            with(json!({"repository_selection": "selected", "repositories": ["missing"]})),
            "repositories",
        ),
        (
            with(json!({"repository_selection": "some"})),
            "repository_selection",
        ),
    ] {
        let res = create(&app, &cookie, body.clone()).await;
        assert_eq!(res.status(), 422, "{body} → {}", res.text());
        assert_eq!(res.json()["errors"][0]["field"], field, "{body}");
    }
    // Token callers can't create tokens.
    app.post("/_bgh/fine-grained-tokens")
        .auth(&alice)
        .json(&base)
        .send()
        .await
        .assert_status(403);

    app.patch("/_bgh/orgs/acme/pat-policy")
        .cookie(&app.session_cookie(&carol).await)
        .json(&json!({"fine_grained_require_approval": true, "fine_grained_max_lifetime_days": 14}))
        .send()
        .await
        .assert_status(200);
    let owners = app
        .get("/_bgh/fine-grained-tokens/owners")
        .cookie(&cookie)
        .send()
        .await;
    owners.assert_status(200);
    let owners = owners.json();
    assert_eq!(owners[0]["login"], "alice");
    assert_eq!(owners[0]["max_lifetime_days"], 366);
    assert_eq!(owners[1]["login"], "acme");
    assert_eq!(owners[1]["type"], "Organization");
    assert_eq!(owners[1]["requires_approval"], true);
    assert_eq!(owners[1]["max_lifetime_days"], 14);
    // The org's maximum lifetime applies.
    let res = create(
        &app,
        &cookie,
        with(json!({"resource_owner": "acme", "expires_in_days": 30})),
    )
    .await;
    res.assert_status(422);

    let catalog = app
        .get("/_bgh/fine-grained-tokens/permissions")
        .cookie(&cookie)
        .send()
        .await;
    catalog.assert_status(200);
    let catalog = catalog.json();
    assert!(
        catalog["repository"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["name"] == "contents" && p["access"] == json!(["read", "write"]))
    );
    assert!(catalog["organization"].as_array().unwrap().len() >= 2);
    assert!(catalog["account"].as_array().unwrap().len() >= 5);
}

#[tokio::test]
async fn organization_approval_flow() {
    let app = bgh_server::test_app().await;
    let owner = app.create_user("owner").await;
    let bob = app.create_user("bob").await;
    let acme = app.create_org("acme", &owner).await;
    app.add_org_member(&acme, &bob, "member").await;
    sqlx::query("UPDATE org_settings SET default_repository_permission = 'read' WHERE org_id = $1")
        .bind(acme.id)
        .execute(&app.state.db)
        .await
        .unwrap();
    private_repo(&app, &owner, Some("acme"), "secret").await;
    let owner_cookie = app.session_cookie(&owner).await;
    let bob_cookie = app.session_cookie(&bob).await;

    // Members can't see or change the policy.
    app.get("/_bgh/orgs/acme/pat-policy")
        .cookie(&bob_cookie)
        .send()
        .await
        .assert_status(403);
    let policy = app
        .patch("/_bgh/orgs/acme/pat-policy")
        .cookie(&owner_cookie)
        .json(&json!({"fine_grained_require_approval": true}))
        .send()
        .await;
    policy.assert_status(200);
    assert_eq!(
        policy.json(),
        json!({
            "fine_grained_allowed": true,
            "fine_grained_require_approval": true,
            "fine_grained_max_lifetime_days": null,
            "classic_allowed": true,
            "classic_max_lifetime_days": null
        })
    );

    let body = json!({
        "name": "acme-ci", "resource_owner": "acme", "expires_in_days": 30,
        "repository_selection": "all", "reason": "deploys",
        "permissions": {"repository": {"contents": "read"}}
    });
    let first = create_ok(&app, &bob_cookie, body.clone()).await;
    assert_eq!(first["status"], "pending");
    let second = create_ok(&app, &bob_cookie, body.clone()).await;
    let t = token(&first);
    // Pending: the organization's private repositories stay invisible.
    app.get("/api/v3/repos/acme/secret")
        .token(t)
        .send()
        .await
        .assert_status(404);

    // Requests: org admins only, GitHub's shape, paginated.
    app.get("/api/v3/orgs/acme/personal-access-token-requests")
        .auth(&bob)
        .send()
        .await
        .assert_status(403);
    let res = app
        .get("/api/v3/orgs/acme/personal-access-token-requests?per_page=1&direction=asc")
        .auth(&owner)
        .send()
        .await;
    res.assert_status(200);
    assert!(res.header("link").unwrap().contains("rel=\"next\""));
    let req = &res.json()[0];
    assert_eq!(req["id"], first["id"]);
    assert_eq!(req["token_id"], first["id"]);
    assert_eq!(req["token_name"], "acme-ci");
    assert_eq!(req["reason"], "deploys");
    assert_eq!(req["owner"]["login"], "bob");
    assert_eq!(req["repository_selection"], "all");
    assert_eq!(req["token_expired"], false);
    assert!(req["created_at"].is_string());
    assert_eq!(
        req["permissions"],
        json!({
            "organization": {},
            "repository": {"contents": "read", "metadata": "read"},
            "other": {}
        })
    );
    assert_eq!(
        req["repositories_url"],
        app.url(&format!(
            "/api/v3/orgs/acme/personal-access-token-requests/{}/repositories",
            first["id"]
        ))
    );
    let repos = app
        .get(&format!(
            "/api/v3/orgs/acme/personal-access-token-requests/{}/repositories",
            first["id"]
        ))
        .auth(&owner)
        .send()
        .await;
    repos.assert_status(200);
    assert_eq!(repos.json()[0]["full_name"], "acme/secret");
    // Filter by owner login.
    let none = app
        .get("/api/v3/orgs/acme/personal-access-token-requests?owner[]=owner")
        .auth(&owner)
        .send()
        .await;
    assert_eq!(none.json(), json!([]));

    // Bad action / unknown id.
    app.post(&format!(
        "/api/v3/orgs/acme/personal-access-token-requests/{}",
        first["id"]
    ))
    .auth(&owner)
    .json(&json!({"action": "maybe"}))
    .send()
    .await
    .assert_status(422);
    app.post("/api/v3/orgs/acme/personal-access-token-requests/999999")
        .auth(&owner)
        .json(&json!({"action": "approve"}))
        .send()
        .await
        .assert_status(404);

    // Approve the first: it works now.
    app.post(&format!(
        "/api/v3/orgs/acme/personal-access-token-requests/{}",
        first["id"]
    ))
    .auth(&owner)
    .json(&json!({"action": "approve"}))
    .send()
    .await
    .assert_status(204);
    let me = app
        .get(&format!("/_bgh/fine-grained-tokens/{}", first["id"]))
        .cookie(&bob_cookie)
        .send()
        .await;
    assert_eq!(me.json()["status"], "active");
    app.get("/api/v3/repos/acme/secret")
        .token(t)
        .send()
        .await
        .assert_status(200);
    // Deny the second in bulk.
    let res = app
        .post("/api/v3/orgs/acme/personal-access-token-requests")
        .auth(&owner)
        .json(&json!({"pat_request_ids": [second["id"]], "action": "deny", "reason": "no"}))
        .send()
        .await;
    res.assert_status(202);
    assert_eq!(res.json(), json!({}));
    app.get("/api/v3/repos/acme/secret")
        .token(token(&second))
        .send()
        .await
        .assert_status(404);

    // Grants: the approved token.
    let grants = app
        .get("/api/v3/orgs/acme/personal-access-tokens")
        .auth(&owner)
        .send()
        .await;
    grants.assert_status(200);
    let grants = grants.json();
    assert_eq!(grants.as_array().unwrap().len(), 1);
    assert_eq!(grants[0]["token_id"], first["id"]);
    assert!(grants[0]["access_granted_at"].is_string());
    assert!(grants[0].get("reason").is_none());
    // Revoke: access is gone.
    app.post(&format!(
        "/api/v3/orgs/acme/personal-access-tokens/{}",
        first["id"]
    ))
    .auth(&owner)
    .json(&json!({"action": "revoke"}))
    .send()
    .await
    .assert_status(204);
    app.get("/api/v3/repos/acme/secret")
        .token(t)
        .send()
        .await
        .assert_status(404);
    let me = app
        .get(&format!("/_bgh/fine-grained-tokens/{}", first["id"]))
        .cookie(&bob_cookie)
        .send()
        .await;
    assert_eq!(me.json()["status"], "revoked");
    // Admin-created tokens skip approval.
    let own = create_ok(&app, &owner_cookie, body).await;
    assert_eq!(own["status"], "active");
}

#[tokio::test]
async fn organization_policy_blocks_tokens() {
    let app = bgh_server::test_app().await;
    let owner = app.create_user("owner").await;
    let bob = app.create_user("bob").await;
    let acme = app.create_org("acme", &owner).await;
    app.add_org_member(&acme, &bob, "admin").await;
    private_repo(&app, &owner, Some("acme"), "secret").await;
    let classic = app.create_token(&bob, &["repo", "read:org"]).await;
    let bob_cookie = app.session_cookie(&bob).await;
    let fine = create_ok(
        &app,
        &bob_cookie,
        json!({
            "name": "f", "resource_owner": "acme", "expires_in_days": 60,
            "repository_selection": "all", "permissions": {"repository": {"contents": "read"}}
        }),
    )
    .await;
    let fine = token(&fine).to_string();
    for t in [&classic, &fine] {
        app.get("/api/v3/repos/acme/secret")
            .token(t)
            .send()
            .await
            .assert_status(200);
    }
    let owner_cookie = app.session_cookie(&owner).await;
    let set = |body: Value| {
        let app = &app;
        let owner_cookie = owner_cookie.clone();
        async move {
            app.patch("/_bgh/orgs/acme/pat-policy")
                .cookie(&owner_cookie)
                .json(&body)
                .send()
                .await
                .assert_status(200);
        }
    };

    // Classic tokens forbidden.
    set(json!({"classic_allowed": false})).await;
    app.get("/api/v3/repos/acme/secret")
        .token(&classic)
        .send()
        .await
        .assert_status(404);
    let res = app.get("/api/v3/orgs/acme").token(&classic).send().await;
    res.assert_status(403);
    assert!(res.json()["message"].as_str().unwrap().contains("acme"));
    assert!(
        res.header("x-oauth-scopes")
            .is_some_and(|s| !s.contains("blocked"))
    );
    // Sessions and fine-grained tokens are unaffected.
    app.get("/api/v3/repos/acme/secret")
        .token(&fine)
        .send()
        .await
        .assert_status(200);

    // Classic lifetime limit: a token without expiry is blocked, a short one isn't.
    set(json!({"classic_allowed": true, "classic_max_lifetime_days": 30})).await;
    app.get("/api/v3/repos/acme/secret")
        .token(&classic)
        .send()
        .await
        .assert_status(404);
    let res = app
        .post("/_bgh/tokens")
        .cookie(&bob_cookie)
        .json(&json!({"name": "short", "scopes": ["repo"], "expires_in_days": 7}))
        .send()
        .await;
    res.assert_status(201);
    let short = res.json()["token"].as_str().unwrap().to_string();
    app.get("/api/v3/repos/acme/secret")
        .token(&short)
        .send()
        .await
        .assert_status(200);

    // Fine-grained lifetime limit and prohibition.
    set(json!({"fine_grained_max_lifetime_days": 30})).await;
    app.get("/api/v3/repos/acme/secret")
        .token(&fine)
        .send()
        .await
        .assert_status(404);
    set(json!({"fine_grained_max_lifetime_days": null, "fine_grained_allowed": false})).await;
    app.get("/api/v3/repos/acme/secret")
        .token(&fine)
        .send()
        .await
        .assert_status(404);
    let res = create(
        &app,
        &bob_cookie,
        json!({
            "name": "g", "resource_owner": "acme", "expires_in_days": 5,
            "repository_selection": "all", "permissions": {}
        }),
    )
    .await;
    res.assert_status(422);
    assert_eq!(res.json()["errors"][0]["field"], "resource_owner");
}

#[tokio::test]
async fn narrow_classic_scopes() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    private_repo(&app, &alice, None, "p").await;
    let sha = app
        .get("/api/v3/repos/alice/p/commits/main")
        .auth(&alice)
        .send()
        .await
        .json()["sha"]
        .as_str()
        .unwrap()
        .to_string();
    let status = app.create_token(&alice, &["repo:status"]).await;
    let deploy = app.create_token(&alice, &["repo_deployment"]).await;

    // repo:status posts statuses to the private repository…
    let res = app
        .post(&format!("/api/v3/repos/alice/p/statuses/{sha}"))
        .token(&status)
        .json(&json!({"state": "success", "context": "ci"}))
        .send()
        .await;
    res.assert_status(201);
    assert_eq!(res.json()["state"], "success");
    app.get(&format!("/api/v3/repos/alice/p/commits/{sha}/status"))
        .token(&status)
        .send()
        .await
        .assert_status(200);
    // …but can't read its contents or create deployments.
    app.get("/api/v3/repos/alice/p/contents/README.md")
        .token(&status)
        .send()
        .await
        .assert_status(404);
    app.post("/api/v3/repos/alice/p/deployments")
        .token(&status)
        .json(&json!({"ref": "main", "required_contexts": []}))
        .send()
        .await
        .assert_status(404);

    // repo_deployment creates deployments but posts no statuses.
    app.post("/api/v3/repos/alice/p/deployments")
        .token(&deploy)
        .json(&json!({"ref": "main", "required_contexts": []}))
        .send()
        .await
        .assert_status(201);
    app.post(&format!("/api/v3/repos/alice/p/statuses/{sha}"))
        .token(&deploy)
        .json(&json!({"state": "success"}))
        .send()
        .await
        .assert_status(404);

    // Git stays closed.
    let tmp = tempfile::tempdir().unwrap();
    let url = app
        .url("/alice/p.git")
        .replace("http://", &format!("http://alice:{status}@"));
    let out = git(tmp.path(), &["clone", &url, "p"]).await;
    assert!(!out.status.success());
}
