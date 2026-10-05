//! Sign-up, sessions, tokens, user and organization endpoints.

use bgh_core::testing::TestApp;
use serde_json::json;

fn cookie_from(res: &bgh_core::testing::TestResponse) -> String {
    let set = res.header("set-cookie").expect("set-cookie");
    assert!(
        set.contains("HttpOnly") && set.contains("SameSite=Lax"),
        "{set}"
    );
    set.split(';').next().unwrap().to_string()
}

async fn signup(app: &TestApp, login: &str) -> bgh_core::testing::TestResponse {
    app.post("/_bgh/signup")
        .json(&json!({"login": login, "email": format!("{login}@example.com"), "password": "s3cret-password"}))
        .send()
        .await
}

#[tokio::test]
async fn signup_login_logout_flow() {
    let app = bgh_server::test_app().await;

    let res = signup(&app, "first").await;
    res.assert_status(201);
    let first = res.json();
    assert_eq!(first["login"], "first");
    assert_eq!(
        first["site_admin"], true,
        "first account becomes site admin"
    );
    assert_eq!(first["type"], "User");
    assert!(first["plan"].is_object());
    let cookie = cookie_from(&res);

    // The session cookie authenticates API calls.
    let me = app.get("/api/v3/user").cookie(&cookie).send().await;
    me.assert_status(200);
    assert_eq!(me.json()["login"], "first");
    assert!(
        me.header("x-oauth-scopes").is_none(),
        "sessions have no scopes header"
    );

    let res = signup(&app, "second").await;
    res.assert_status(201);
    assert_eq!(res.json()["site_admin"], false);

    // Logout invalidates the session.
    let res = app.delete("/_bgh/session").cookie(&cookie).send().await;
    res.assert_status(204);
    assert!(res.header("set-cookie").unwrap().contains("Max-Age=0"));
    app.get("/api/v3/user")
        .cookie(&cookie)
        .send()
        .await
        .assert_status(401);

    // Login by login and by email.
    let res = app
        .post("/_bgh/session")
        .json(&json!({"login": "first", "password": "s3cret-password"}))
        .send()
        .await;
    res.assert_status(200);
    let cookie = cookie_from(&res);
    app.get("/api/v3/user")
        .cookie(&cookie)
        .send()
        .await
        .assert_status(200);
    app.post("/_bgh/session")
        .json(&json!({"login": "second@example.com", "password": "s3cret-password"}))
        .send()
        .await
        .assert_status(200);

    let res = app
        .post("/_bgh/session")
        .json(&json!({"login": "first", "password": "wrong-password"}))
        .send()
        .await;
    res.assert_status(401);
    assert_eq!(res.json()["message"], "Bad credentials");
}

#[tokio::test]
async fn signup_validation() {
    let app = bgh_server::test_app().await;
    signup(&app, "taken").await.assert_status(201);

    let res = signup(&app, "Taken").await;
    res.assert_status(422);
    let v = res.json();
    assert_eq!(v["message"], "Validation Failed");
    assert_eq!(v["errors"][0]["field"], "login");
    assert_eq!(v["errors"][0]["code"], "already_exists");

    let res = app
        .post("/_bgh/signup")
        .json(&json!({"login": "-bad-", "email": "nope", "password": "short"}))
        .send()
        .await;
    res.assert_status(422);
    let fields: Vec<String> = res.json()["errors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["field"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(fields, vec!["login", "email", "password"]);

    signup(&app, "api").await.assert_status(422);

    let res = app.post("/_bgh/signup").body("{not json").send().await;
    res.assert_status(400);
    assert_eq!(res.json()["message"], "Problems parsing JSON");
}

#[tokio::test]
async fn signup_can_be_disabled() {
    let app = TestApp::spawn_with_config(bgh_server::factory(), |c| c.signup_enabled = false).await;
    signup(&app, "someone").await.assert_status(403);
}

#[tokio::test]
async fn users_endpoints() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;

    let res = app.get("/api/v3/users/ALICE").send().await;
    res.assert_status(200);
    let v = res.json();
    assert_eq!(v["login"], "alice");
    assert_eq!(v["id"], alice.id);
    assert_eq!(v["public_repos"], 0);
    assert_eq!(v["html_url"], app.url("/alice"));
    assert_eq!(v["url"], app.url("/api/v3/users/alice"));
    assert!(
        v.get("private_gists").is_none(),
        "public profile has no private fields"
    );

    app.get("/api/v3/users/nobody")
        .send()
        .await
        .assert_status(404);

    let res = app.get("/api/v3/user").send().await;
    res.assert_status(401);
    assert_eq!(res.json()["message"], "Requires authentication");

    let res = app.get("/api/v3/user").auth(&alice).send().await;
    res.assert_status(200);
    assert_eq!(res.json()["total_private_repos"], 0);

    // Bad tokens are rejected even on endpoints allowing anonymous access.
    let res = app
        .get("/api/v3/users/alice")
        .token("bghp_nope")
        .send()
        .await;
    res.assert_status(401);
    assert_eq!(res.json()["message"], "Bad credentials");

    // Basic auth with a token works; with a password it doesn't (API).
    app.get("/api/v3/user")
        .basic("alice", &alice.token)
        .send()
        .await
        .assert_status(200);
    app.get("/api/v3/user")
        .basic("alice", &alice.password)
        .send()
        .await
        .assert_status(401);
}

#[tokio::test]
async fn personal_access_tokens() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let cookie = app.session_cookie(&alice).await;

    let res = app
        .post("/_bgh/tokens")
        .cookie(&cookie)
        .json(&json!({"name": "ci", "scopes": ["repo", "read:org"], "expires_in_days": 30}))
        .send()
        .await;
    res.assert_status(201);
    let v = res.json();
    let token = v["token"].as_str().unwrap().to_string();
    assert!(token.starts_with("bghp_"));
    assert_eq!(v["scopes"], json!(["repo", "read:org"]));
    assert!(v["expires_at"].is_string());

    let res = app.get("/api/v3/user").token(&token).send().await;
    res.assert_status(200);
    assert_eq!(res.header("x-oauth-scopes"), Some("repo, read:org"));
    app.get("/api/v3/user")
        .header("authorization", &format!("Bearer {token}"))
        .send()
        .await
        .assert_status(200);

    // Tokens cannot mint tokens; unknown scopes are rejected.
    app.post("/_bgh/tokens")
        .token(&token)
        .json(&json!({"scopes": []}))
        .send()
        .await
        .assert_status(403);
    app.post("/_bgh/tokens")
        .cookie(&cookie)
        .json(&json!({"scopes": ["root"]}))
        .send()
        .await
        .assert_status(422);
    app.post("/_bgh/tokens")
        .cookie(&cookie)
        .json(&json!({"scopes": ["site_admin"]}))
        .send()
        .await
        .assert_status(422);

    let list = app.get("/_bgh/tokens").cookie(&cookie).send().await;
    list.assert_status(200);
    let tokens = list.json();
    let ci = tokens
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "ci")
        .unwrap()
        .clone();
    assert!(ci.get("token").is_none(), "secrets are never listed");

    app.delete(&format!("/_bgh/tokens/{}", ci["id"]))
        .cookie(&cookie)
        .send()
        .await
        .assert_status(204);
    app.get("/api/v3/user")
        .token(&token)
        .send()
        .await
        .assert_status(401);
}

#[tokio::test]
async fn admin_creates_organizations() {
    let app = bgh_server::test_app().await;
    let root = app.create_admin("root").await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;

    let body = json!({"login": "acme", "admin": "alice", "profile_name": "Acme Inc"});
    let res = app
        .post("/api/v3/admin/organizations")
        .auth(&alice)
        .json(&body)
        .send()
        .await;
    res.assert_status(403);

    let res = app
        .post("/api/v3/admin/organizations")
        .auth(&root)
        .json(&body)
        .send()
        .await;
    res.assert_status(201);
    let v = res.json();
    assert_eq!(v["login"], "acme");
    assert_eq!(v["url"], app.url("/api/v3/orgs/acme"));
    assert_eq!(
        v["members_url"],
        app.url("/api/v3/orgs/acme/members{/member}")
    );

    let res = app
        .post("/api/v3/admin/organizations")
        .auth(&root)
        .json(&body)
        .send()
        .await;
    res.assert_status(422);
    assert_eq!(res.json()["errors"][0]["code"], "already_exists");

    // Members see member-only settings; others don't.
    let res = app.get("/api/v3/orgs/acme").auth(&alice).send().await;
    res.assert_status(200);
    let v = res.json();
    assert_eq!(v["name"], "Acme Inc");
    assert_eq!(v["type"], "Organization");
    assert_eq!(v["default_repository_permission"], "read");
    let res = app.get("/api/v3/orgs/acme").auth(&bob).send().await;
    assert!(res.json().get("default_repository_permission").is_none());

    // Organizations also resolve through /users/{login}.
    let res = app.get("/api/v3/users/acme").send().await;
    assert_eq!(res.json()["type"], "Organization");
    app.get("/api/v3/orgs/alice")
        .send()
        .await
        .assert_status(404);

    // Audit trail.
    let actions: Vec<String> = sqlx::query_scalar("SELECT action FROM audit_log ORDER BY id")
        .fetch_all(&app.state.db)
        .await
        .unwrap();
    assert!(actions.contains(&"org.create".to_string()));
}
