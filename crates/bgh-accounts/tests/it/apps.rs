//! GitHub Apps: registration, keys, JWT, installation, installation tokens.

use std::path::Path;

use bgh_core::testing::{TestApp, TestUser};
use serde_json::{Value, json};

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn jwt(pem: &str, iss: Value) -> String {
    bgh_core::apps::sign_jwt(pem, &iss, now() - 30, now() + 540).unwrap()
}

struct Setup {
    app: TestApp,
    alice: TestUser,
    cookie: String,
    app_id: i64,
    pem: String,
}

/// alice administers org `acme` (private repos `one` and `two`) and
/// registers the app `My Bot` with a key.
async fn setup(permissions: Value) -> Setup {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    app.create_org("acme", &alice).await;
    for name in ["one", "two"] {
        app.create_repo_with(
            &alice,
            Some("acme"),
            json!({ "name": name, "private": true, "auto_init": true }),
        )
        .await;
    }
    let cookie = app.session_cookie(&alice).await;
    let res = app
        .post("/_bgh/apps")
        .cookie(&cookie)
        .json(&json!({
            "owner": "acme",
            "name": "My Bot",
            "description": "Does things",
            "homepage_url": "https://example.com",
            "callback_urls": ["https://example.com/cb"],
            "setup_url": "https://example.com/setup",
            "webhook_url": "https://example.com/hook",
            "webhook_secret": "s3cret",
            "permissions": permissions,
            "events": ["push", "issues"],
        }))
        .send()
        .await;
    res.assert_status(201);
    let body = res.json();
    assert_eq!(body["slug"], "my-bot");
    assert_eq!(body["owner"]["login"], "acme");
    assert_eq!(body["bot"]["login"], "my-bot[bot]");
    assert_eq!(body["bot"]["type"], "Bot");
    assert_eq!(body["webhook_secret_set"], true);
    assert!(body.get("webhook_secret").is_none());
    let app_id = body["id"].as_i64().unwrap();
    let res = app
        .post("/_bgh/apps/my-bot/keys")
        .cookie(&cookie)
        .send()
        .await;
    res.assert_status(201);
    let key = res.json();
    let pem = key["pem"].as_str().unwrap().to_string();
    assert!(pem.starts_with("-----BEGIN RSA PRIVATE KEY-----"));
    assert!(key["fingerprint"].as_str().unwrap().starts_with("SHA256:"));
    // The PEM is never shown again.
    let res = app.get("/_bgh/apps/my-bot").cookie(&cookie).send().await;
    res.assert_status(200);
    assert!(res.json()["keys"][0].get("pem").is_none());
    Setup {
        app,
        alice,
        cookie,
        app_id,
        pem,
    }
}

async fn install(s: &Setup, repos: &[&str]) -> i64 {
    let mut ids = Vec::new();
    for r in repos {
        let res = s
            .app
            .get(&format!("/api/v3/repos/acme/{r}"))
            .auth(&s.alice)
            .send()
            .await;
        ids.push(res.json()["id"].as_i64().unwrap());
    }
    let res = s
        .app
        .post("/_bgh/apps/my-bot/installations")
        .cookie(&s.cookie)
        .json(&json!({
            "account": "acme",
            "repository_selection": "selected",
            "repository_ids": ids,
        }))
        .send()
        .await;
    res.assert_status(201);
    let body = res.json();
    assert_eq!(body["installation"]["repository_selection"], "selected");
    assert_eq!(body["repositories"].as_array().unwrap().len(), repos.len());
    let id = body["installation"]["id"].as_i64().unwrap();
    assert_eq!(
        body["setup_redirect"],
        format!("https://example.com/setup?installation_id={id}&setup_action=install")
    );
    id
}

async fn mint(s: &Setup, installation: i64, body: Value) -> (u16, Value) {
    let res = s
        .app
        .post(&format!(
            "/api/v3/app/installations/{installation}/access_tokens"
        ))
        .header(
            "authorization",
            &format!("Bearer {}", jwt(&s.pem, json!(s.app_id))),
        )
        .json(&body)
        .send()
        .await;
    (res.status(), res.json())
}

#[tokio::test]
async fn full_flow() {
    let s = setup(json!({"contents": "write", "issues": "write", "pull_requests": "read"})).await;
    let app = &s.app;
    let bearer = format!("Bearer {}", jwt(&s.pem, json!(s.app_id)));

    // GET /app with the JWT.
    let res = app
        .get("/api/v3/app")
        .header("authorization", &bearer)
        .send()
        .await;
    res.assert_status(200);
    let me = res.json();
    assert_eq!(me["id"], s.app_id);
    assert_eq!(me["slug"], "my-bot");
    assert_eq!(me["name"], "My Bot");
    assert_eq!(me["description"], "Does things");
    assert_eq!(me["external_url"], "https://example.com");
    assert_eq!(me["html_url"], app.url("/apps/my-bot"));
    assert_eq!(me["owner"]["login"], "acme");
    assert_eq!(me["owner"]["type"], "Organization");
    assert_eq!(me["installations_count"], 0);
    assert_eq!(
        me["permissions"],
        json!({"contents": "write", "issues": "write", "metadata": "read", "pull_requests": "read"})
    );
    assert_eq!(me["events"], json!(["push", "issues"]));
    assert!(me["client_id"].as_str().unwrap().starts_with("Iv23"));
    assert_eq!(
        me["node_id"],
        bgh_core::node_id::encode(bgh_core::node_id::NodeType::Integration, s.app_id)
    );
    assert!(res.header("x-oauth-scopes").is_none());
    // `iss` may also be the client id (string).
    let by_client = jwt(&s.pem, me["client_id"].clone());
    app.get("/api/v3/app")
        .header("authorization", &format!("Bearer {by_client}"))
        .send()
        .await
        .assert_status(200);

    // Install on acme with `one` selected.
    let inst = install(&s, &["one"]).await;

    let res = app
        .get("/api/v3/app/installations")
        .header("authorization", &bearer)
        .send()
        .await;
    res.assert_status(200);
    let list = res.json();
    assert_eq!(list.as_array().unwrap().len(), 1);
    let i = &list[0];
    assert_eq!(i["id"], inst);
    assert_eq!(i["account"]["login"], "acme");
    assert_eq!(i["app_id"], s.app_id);
    assert_eq!(i["app_slug"], "my-bot");
    assert_eq!(i["target_type"], "Organization");
    assert_eq!(i["repository_selection"], "selected");
    assert_eq!(
        i["access_tokens_url"],
        app.url(&format!("/api/v3/app/installations/{inst}/access_tokens"))
    );
    assert_eq!(
        i["repositories_url"],
        app.url("/api/v3/installation/repositories")
    );
    assert_eq!(
        i["html_url"],
        app.url(&format!(
            "/organizations/acme/settings/installations/{inst}"
        ))
    );
    assert_eq!(i["permissions"]["contents"], "write");
    assert_eq!(i["events"], json!(["push", "issues"]));
    assert!(i["suspended_at"].is_null() && i["suspended_by"].is_null());
    assert_eq!(i["single_file_paths"], json!([]));
    for path in [
        format!("/api/v3/app/installations/{inst}"),
        "/api/v3/orgs/acme/installation".into(),
        "/api/v3/repos/acme/one/installation".into(),
    ] {
        let res = app.get(&path).header("authorization", &bearer).send().await;
        res.assert_status(200);
        assert_eq!(res.json()["id"], inst, "{path}");
    }
    app.get("/api/v3/repos/acme/two/installation")
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(404);
    app.get("/api/v3/users/alice/installation")
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(404);

    // Mint a token.
    let (status, tok) = mint(&s, inst, json!({})).await;
    assert_eq!(status, 201, "{tok}");
    let token = tok["token"].as_str().unwrap().to_string();
    assert!(token.starts_with("bghs_"));
    assert!(tok["expires_at"].as_str().unwrap().ends_with('Z'));
    assert_eq!(tok["repository_selection"], "selected");
    assert_eq!(tok["permissions"]["issues"], "write");
    assert_eq!(tok["permissions"]["metadata"], "read");
    assert_eq!(tok["repositories"][0]["full_name"], "acme/one");

    // It reads the selected repo, 404s on the other.
    let res = app.get("/api/v3/repos/acme/one").token(&token).send().await;
    res.assert_status(200);
    assert!(res.header("x-oauth-scopes").is_none());
    app.get("/api/v3/repos/acme/two")
        .token(&token)
        .send()
        .await
        .assert_status(404);
    let res = app
        .get("/api/v3/installation/repositories")
        .token(&token)
        .send()
        .await;
    res.assert_status(200);
    let repos = res.json();
    assert_eq!(repos["total_count"], 1);
    assert_eq!(repos["repository_selection"], "selected");
    assert_eq!(repos["repositories"][0]["name"], "one");

    // Writes are attributed to the bot.
    let res = app
        .post("/api/v3/repos/acme/one/issues")
        .token(&token)
        .json(&json!({ "title": "From the app" }))
        .send()
        .await;
    res.assert_status(201);
    assert_eq!(res.json()["user"]["login"], "my-bot[bot]");
    assert_eq!(res.json()["user"]["type"], "Bot");
    app.post("/api/v3/repos/acme/two/issues")
        .token(&token)
        .json(&json!({ "title": "nope" }))
        .send()
        .await
        .assert_status(404);

    // Installation tokens can't use user endpoints or the app endpoints.
    app.get("/api/v3/user")
        .token(&token)
        .send()
        .await
        .assert_status(403);
    let res = app.get("/api/v3/app").token(&token).send().await;
    res.assert_status(401);
    assert_eq!(
        res.json()["message"],
        "A JSON web token could not be decoded"
    );

    // Narrowed to contents:read: writes are refused.
    let (status, ro) = mint(&s, inst, json!({"permissions": {"contents": "read"}})).await;
    assert_eq!(status, 201, "{ro}");
    assert_eq!(
        ro["permissions"],
        json!({"contents": "read", "metadata": "read"})
    );
    let ro = ro["token"].as_str().unwrap().to_string();
    app.get("/api/v3/repos/acme/one")
        .token(&ro)
        .send()
        .await
        .assert_status(200);
    let res = app
        .post("/api/v3/repos/acme/one/issues")
        .token(&ro)
        .json(&json!({ "title": "nope" }))
        .send()
        .await;
    res.assert_status(403);
    let res = app
        .put("/api/v3/repos/acme/one/contents/x.txt")
        .token(&ro)
        .json(&json!({ "message": "x", "content": "eA==" }))
        .send()
        .await;
    res.assert_status(403);

    // Narrowing beyond the installation is a 422.
    let (status, err) = mint(
        &s,
        inst,
        json!({"permissions": {"administration": "write"}}),
    )
    .await;
    assert_eq!(status, 422);
    assert_eq!(
        err["message"],
        "The permissions requested are not granted to this installation."
    );
    let (status, _) = mint(&s, inst, json!({"repositories": ["two"]})).await;
    assert_eq!(status, 422);
    let (status, narrowed) = mint(&s, inst, json!({"repositories": ["ONE"]})).await;
    assert_eq!(status, 201);
    assert_eq!(narrowed["repositories"].as_array().unwrap().len(), 1);

    // The JWT can only call app endpoints; PATs can't call them.
    app.get("/api/v3/repos/acme/one")
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(403);
    let res = app.get("/api/v3/app").auth(&s.alice).send().await;
    res.assert_status(401);

    // Revoke the token.
    app.delete("/api/v3/installation/token")
        .token(&token)
        .send()
        .await
        .assert_status(204);
    app.get("/api/v3/repos/acme/one")
        .token(&token)
        .send()
        .await
        .assert_status(401);

    // Suspension revokes tokens and blocks minting.
    let res = app.get("/api/v3/repos/acme/one").token(&ro).send().await;
    res.assert_status(200);
    app.put(&format!("/api/v3/app/installations/{inst}/suspended"))
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(204);
    app.get("/api/v3/repos/acme/one")
        .token(&ro)
        .send()
        .await
        .assert_status(401);
    let (status, err) = mint(&s, inst, json!({})).await;
    assert_eq!(status, 403);
    assert_eq!(err["message"], "This installation has been suspended");
    let res = app
        .get(&format!("/api/v3/app/installations/{inst}"))
        .header("authorization", &bearer)
        .send()
        .await;
    assert_eq!(res.json()["suspended_by"]["login"], "my-bot[bot]");
    app.delete(&format!("/api/v3/app/installations/{inst}/suspended"))
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(204);
    let (status, _) = mint(&s, inst, json!({})).await;
    assert_eq!(status, 201);

    // `GET /app` counts installations.
    let res = app
        .get("/api/v3/app")
        .header("authorization", &bearer)
        .send()
        .await;
    assert_eq!(res.json()["installations_count"], 1);
}

#[tokio::test]
async fn jwt_errors() {
    let s = setup(json!({"contents": "read"})).await;
    let app = &s.app;
    let cases = [
        (
            bgh_core::apps::sign_jwt(&s.pem, &json!(s.app_id), now() - 1200, now() - 600).unwrap(),
            "'Expiration time' claim ('exp') must be a numeric value representing the future time at which the assertion expires",
        ),
        (
            bgh_core::apps::sign_jwt(&s.pem, &json!(s.app_id), now(), now() + 3600).unwrap(),
            "'Expiration time' claim ('exp') is too far in the future",
        ),
        (
            bgh_core::apps::sign_jwt(&s.pem, &json!(999_999), now() - 30, now() + 500).unwrap(),
            "Integration not found",
        ),
    ];
    for (token, message) in cases {
        let res = app
            .get("/api/v3/app")
            .header("authorization", &format!("Bearer {token}"))
            .send()
            .await;
        res.assert_status(401);
        assert_eq!(res.json()["message"], message);
    }
    // Signed by another key.
    let other = bgh_core::apps::generate_key().unwrap();
    let res = app
        .get("/api/v3/app")
        .header(
            "authorization",
            &format!("Bearer {}", jwt(&other.private_pem, json!(s.app_id))),
        )
        .send()
        .await;
    res.assert_status(401);
    assert_eq!(
        res.json()["message"],
        "A JSON web token could not be decoded"
    );
    // Deleting the key invalidates JWTs signed with it.
    let keys = app
        .get("/_bgh/apps/my-bot")
        .cookie(&s.cookie)
        .send()
        .await
        .json();
    let key_id = keys["keys"][0]["id"].as_i64().unwrap();
    app.delete(&format!("/_bgh/apps/my-bot/keys/{key_id}"))
        .cookie(&s.cookie)
        .send()
        .await
        .assert_status(204);
    app.get("/api/v3/app")
        .header(
            "authorization",
            &format!("Bearer {}", jwt(&s.pem, json!(s.app_id))),
        )
        .send()
        .await
        .assert_status(401);
}

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

#[tokio::test]
async fn git_with_installation_token() {
    let s = setup(json!({"contents": "write"})).await;
    let inst = install(&s, &["one"]).await;
    let (_, rw) = mint(&s, inst, json!({})).await;
    let (_, ro) = mint(&s, inst, json!({"permissions": {"contents": "read"}})).await;
    let remote = |token: &str, repo: &str| {
        s.app
            .url(&format!("/acme/{repo}.git"))
            .replace("http://", &format!("http://x-access-token:{token}@"))
    };
    let tmp = tempfile::tempdir().unwrap();
    let ro_url = remote(ro["token"].as_str().unwrap(), "one");
    let out = git(tmp.path(), &["clone", &ro_url, "one"]).await;
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let work = tmp.path().join("one");
    std::fs::write(work.join("app.txt"), "hello\n").unwrap();
    git(&work, &["add", "."]).await;
    git(&work, &["commit", "-m", "from app"]).await;
    // contents:read can't push.
    let out = git(&work, &["push", &ro_url, "HEAD:main"]).await;
    assert!(!out.status.success());
    // contents:write can.
    let rw_url = remote(rw["token"].as_str().unwrap(), "one");
    let out = git(&work, &["push", &rw_url, "HEAD:main"]).await;
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Repositories outside the installation are invisible.
    let out = git(
        tmp.path(),
        &[
            "clone",
            &remote(rw["token"].as_str().unwrap(), "two"),
            "two",
        ],
    )
    .await;
    assert!(!out.status.success());
}

#[tokio::test]
async fn user_installation_endpoints() {
    let s = setup(json!({"issues": "read"})).await;
    let app = &s.app;
    let inst = install(&s, &["one"]).await;
    let bob = app.create_user("bob").await;
    let org = bgh_core::testing::TestOrg {
        id: sqlx::query_scalar("SELECT id FROM users WHERE login = 'acme'")
            .fetch_one(&app.state.db)
            .await
            .unwrap(),
        login: "acme".into(),
    };
    app.add_org_member(&org, &bob, "member").await;
    sqlx::query("UPDATE org_settings SET default_repository_permission = 'none' WHERE org_id = $1")
        .bind(org.id)
        .execute(&app.state.db)
        .await
        .unwrap();

    // Alice (org admin) sees the installation and its repositories.
    let res = app
        .get("/api/v3/user/installations")
        .auth(&s.alice)
        .send()
        .await;
    res.assert_status(200);
    let body = res.json();
    assert_eq!(body["total_count"], 1);
    assert_eq!(body["installations"][0]["id"], inst);
    let res = app
        .get(&format!("/api/v3/user/installations/{inst}/repositories"))
        .auth(&s.alice)
        .send()
        .await;
    res.assert_status(200);
    let repos = res.json();
    assert_eq!(repos["total_count"], 1);
    assert_eq!(repos["repository_selection"], "selected");
    assert_eq!(repos["repositories"][0]["full_name"], "acme/one");
    assert_eq!(repos["repositories"][0]["permissions"]["admin"], true);

    // Bob (member without access to the private repos) sees it, empty.
    let res = app
        .get("/api/v3/user/installations")
        .auth(&bob)
        .send()
        .await;
    assert_eq!(res.json()["total_count"], 1);
    let res = app
        .get(&format!("/api/v3/user/installations/{inst}/repositories"))
        .auth(&bob)
        .send()
        .await;
    assert_eq!(res.json()["total_count"], 0);

    // Add `two`, then remove `one` (admins only).
    let two = app
        .get("/api/v3/repos/acme/two")
        .auth(&s.alice)
        .send()
        .await
        .json()["id"]
        .as_i64()
        .unwrap();
    let one = app
        .get("/api/v3/repos/acme/one")
        .auth(&s.alice)
        .send()
        .await
        .json()["id"]
        .as_i64()
        .unwrap();
    app.put(&format!(
        "/api/v3/user/installations/{inst}/repositories/{two}"
    ))
    .auth(&bob)
    .send()
    .await
    .assert_status(403);
    app.put(&format!(
        "/api/v3/user/installations/{inst}/repositories/{two}"
    ))
    .auth(&s.alice)
    .send()
    .await
    .assert_status(204);
    let (_, tok) = mint(&s, inst, json!({})).await;
    let token = tok["token"].as_str().unwrap().to_string();
    assert_eq!(tok["repositories"].as_array().unwrap().len(), 2);
    app.delete(&format!(
        "/api/v3/user/installations/{inst}/repositories/{one}"
    ))
    .auth(&s.alice)
    .send()
    .await
    .assert_status(204);
    // The existing token lost `one`.
    app.get("/api/v3/repos/acme/one")
        .token(&token)
        .send()
        .await
        .assert_status(404);
    app.get("/api/v3/repos/acme/two")
        .token(&token)
        .send()
        .await
        .assert_status(200);

    // Org installations (admins).
    let res = app
        .get("/api/v3/orgs/acme/installations")
        .auth(&s.alice)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["total_count"], 1);
    assert_eq!(res.json()["installations"][0]["app_slug"], "my-bot");
    app.get("/api/v3/orgs/acme/installations")
        .auth(&bob)
        .send()
        .await
        .assert_status(403);

    // Public app page: private apps are hidden from others.
    app.get("/api/v3/apps/my-bot")
        .auth(&bob)
        .send()
        .await
        .assert_status(404);
    let res = app.get("/api/v3/apps/my-bot").auth(&s.alice).send().await;
    res.assert_status(200);
    assert_eq!(res.json()["slug"], "my-bot");
    // The bot is a regular (Bot) user.
    let res = app.get("/api/v3/users/my-bot[bot]").send().await;
    res.assert_status(200);
    assert_eq!(res.json()["type"], "Bot");

    // Uninstall from the settings page removes its tokens.
    app.delete(&format!("/_bgh/installations/{inst}"))
        .cookie(&s.cookie)
        .send()
        .await
        .assert_status(204);
    app.get("/api/v3/repos/acme/two")
        .token(&token)
        .send()
        .await
        .assert_status(401);
}

#[tokio::test]
async fn registration_rules() {
    let s = setup(json!({})).await;
    let app = &s.app;
    let bob = app.create_user("bob").await;
    let bob_cookie = app.session_cookie(&bob).await;
    // Names are unique; invalid permissions and events are rejected.
    for (body, field) in [
        (
            json!({"name": "my bot", "homepage_url": "https://x.test"}),
            "name",
        ),
        (
            json!({"name": "Other", "homepage_url": "https://x.test", "permissions": {"metadata": "write"}}),
            "permissions",
        ),
        (
            json!({"name": "Other", "homepage_url": "https://x.test", "events": ["nope"]}),
            "events",
        ),
        (
            json!({"name": "Other", "homepage_url": "notaurl"}),
            "homepage_url",
        ),
    ] {
        let res = app
            .post("/_bgh/apps")
            .cookie(&bob_cookie)
            .json(&body)
            .send()
            .await;
        res.assert_status(422);
        assert_eq!(res.json()["errors"][0]["field"], field, "{body}");
    }
    // Only org admins register apps for an org.
    app.post("/_bgh/apps")
        .cookie(&bob_cookie)
        .json(&json!({"owner": "acme", "name": "Sneaky", "homepage_url": "https://x.test"}))
        .send()
        .await
        .assert_status(404);
    // Tokens can't register apps (a token could grant itself more access).
    app.post("/_bgh/apps")
        .auth(&bob)
        .json(&json!({"name": "Tok", "homepage_url": "https://x.test"}))
        .send()
        .await
        .assert_status(403);
    // Bob can't see or install alice's private app.
    app.get("/_bgh/apps/my-bot")
        .cookie(&bob_cookie)
        .send()
        .await
        .assert_status(404);
    app.get("/_bgh/apps/my-bot/install")
        .cookie(&bob_cookie)
        .send()
        .await
        .assert_status(404);

    // Make it public: bob can install it on his account.
    app.patch("/_bgh/apps/my-bot")
        .cookie(&s.cookie)
        .json(&json!({"public": true, "name": "My Bot 2"}))
        .send()
        .await
        .assert_status(200);
    let res = app
        .get("/_bgh/apps/my-bot-2/install")
        .cookie(&bob_cookie)
        .send()
        .await;
    res.assert_status(200);
    let info = res.json();
    assert_eq!(info["accounts"].as_array().unwrap().len(), 1);
    assert_eq!(info["accounts"][0]["account"]["login"], "bob");
    let res = app
        .post("/_bgh/apps/my-bot-2/installations")
        .cookie(&bob_cookie)
        .json(&json!({"account": "bob"}))
        .send()
        .await;
    res.assert_status(201);
    assert_eq!(res.json()["installation"]["repository_selection"], "all");
    assert_eq!(res.json()["installation"]["target_type"], "User");
    // Renaming moved the bot login.
    app.get("/api/v3/users/my-bot-2[bot]")
        .send()
        .await
        .assert_status(200);
    // Installing twice is a 422.
    app.post("/_bgh/apps/my-bot-2/installations")
        .cookie(&bob_cookie)
        .json(&json!({"account": "bob"}))
        .send()
        .await
        .assert_status(422);

    // Permission upgrades wait for acceptance.
    app.patch("/_bgh/apps/my-bot-2")
        .cookie(&s.cookie)
        .json(&json!({"permissions": {"issues": "write"}}))
        .send()
        .await
        .assert_status(200);
    let list = app
        .get("/_bgh/installations")
        .cookie(&bob_cookie)
        .send()
        .await
        .json();
    let inst = list[0]["id"].as_i64().unwrap();
    let res = app
        .get(&format!("/_bgh/installations/{inst}"))
        .cookie(&bob_cookie)
        .send()
        .await;
    assert_eq!(res.json()["permissions_outdated"], true);
    assert!(res.json()["installation"]["permissions"]["issues"].is_null());
    let res = app
        .post(&format!("/_bgh/installations/{inst}/accept_permissions"))
        .cookie(&bob_cookie)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["permissions_outdated"], false);
    assert_eq!(res.json()["installation"]["permissions"]["issues"], "write");

    // Deleting the app removes its bot and installations.
    app.delete("/_bgh/apps/my-bot-2")
        .cookie(&s.cookie)
        .send()
        .await
        .assert_status(204);
    app.get("/api/v3/users/my-bot-2[bot]")
        .send()
        .await
        .assert_status(404);
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM app_installations")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(n, 0);
}

#[tokio::test]
async fn separate_rate_limit_bucket() {
    let s = setup(json!({"contents": "read"})).await;
    let inst = install(&s, &["one"]).await;
    let (_, tok) = mint(&s, inst, json!({})).await;
    let token = tok["token"].as_str().unwrap();
    let used = |r: &bgh_core::testing::TestResponse| {
        r.header("x-ratelimit-used")
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap()
    };
    // The installation starts with a fresh bucket (the JWT calls that
    // minted the token and alice's setup calls count elsewhere).
    let a = s
        .app
        .get("/api/v3/repos/acme/one")
        .token(token)
        .send()
        .await;
    assert_eq!(used(&a), 1);
    let b = s
        .app
        .get("/api/v3/repos/acme/one")
        .token(token)
        .send()
        .await;
    assert_eq!(used(&b), 2);
    let c = s
        .app
        .get("/api/v3/repos/acme/one")
        .auth(&s.alice)
        .send()
        .await;
    assert!(used(&c) > 2);
}
