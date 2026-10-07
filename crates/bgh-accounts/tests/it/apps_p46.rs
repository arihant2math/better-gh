//! GitHub Apps, part 2 (P46): manifest flow, client secrets, user-to-server
//! tokens, permission-upgrade notification.

use crate::common::session;

use bgh_core::testing::{TestApp, TestUser};
use serde_json::{Value, json};
use std::collections::HashMap;

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn query_of(location: &str) -> HashMap<String, String> {
    url::Url::parse(location)
        .unwrap()
        .query_pairs()
        .into_owned()
        .collect()
}

fn form_body(manifest: &Value) -> String {
    url::form_urlencoded::Serializer::new(String::new())
        .append_pair("manifest", &manifest.to_string())
        .finish()
}

async fn repo_id(app: &TestApp, user: &TestUser, full: &str) -> i64 {
    app.get(&format!("/api/v3/repos/{full}"))
        .auth(user)
        .send()
        .await
        .json()["id"]
        .as_i64()
        .unwrap()
}

#[tokio::test]
async fn manifest_flow() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    app.create_org("acme", &alice).await;
    let cookie = session(&app, &alice).await;
    let manifest = json!({
        "name": "Octo Manifest",
        "url": "https://example.com",
        "description": "From a manifest",
        "hook_attributes": {"url": "https://example.com/hook"},
        "redirect_url": "https://example.com/redirect",
        "callback_urls": ["https://example.com/cb"],
        "setup_url": "https://example.com/setup",
        "public": true,
        "default_permissions": {"issues": "write", "checks": "write"},
        "default_events": ["issues"],
    });

    // Bad posts.
    let res = app
        .post("/settings/apps/new")
        .header("content-type", "application/x-www-form-urlencoded")
        .body("manifest=nope")
        .send()
        .await;
    res.assert_status(422);
    let res = app
        .post("/settings/apps/new")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(form_body(&json!({"name": "x"})))
        .send()
        .await;
    res.assert_status(422);
    app.post("/organizations/nope/settings/apps/new")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(form_body(&manifest))
        .send()
        .await
        .assert_status(404);

    // The integration's form posts the manifest (cross-site: a session
    // cookie without CSRF token is fine here).
    let res = app
        .post("/organizations/acme/settings/apps/new?state=xyz")
        .cookie(&cookie)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(form_body(&manifest))
        .send()
        .await;
    res.assert_status(303);
    let location = res.header("location").unwrap().to_string();
    assert!(
        location.starts_with("/organizations/acme/settings/apps/new?manifest="),
        "{location}"
    );
    let token = location.split("manifest=").nth(1).unwrap().to_string();

    let res = app
        .get(&format!("/_bgh/app-manifests/{token}"))
        .cookie(&cookie)
        .send()
        .await;
    res.assert_status(200);
    let info = res.json();
    assert_eq!(info["owner"]["login"], "acme");
    assert_eq!(info["can_create"], true);
    assert_eq!(info["name"], "Octo Manifest");
    assert_eq!(info["webhook_url"], "https://example.com/hook");
    assert_eq!(info["permissions"]["issues"], "write");
    assert_eq!(info["events"], json!(["issues"]));
    assert_eq!(info["app_slug"], Value::Null);

    // Not an admin of acme.
    let bob_cookie = session(&app, &bob).await;
    let res = app
        .get(&format!("/_bgh/app-manifests/{token}"))
        .cookie(&bob_cookie)
        .send()
        .await;
    assert_eq!(res.json()["can_create"], false);
    app.post(&format!("/_bgh/app-manifests/{token}"))
        .cookie(&bob_cookie)
        .json(&json!({}))
        .send()
        .await
        .assert_status(404);

    let res = app
        .post(&format!("/_bgh/app-manifests/{token}"))
        .cookie(&cookie)
        .json(&json!({}))
        .send()
        .await;
    res.assert_status(201);
    let to = res.json()["redirect_url"].as_str().unwrap().to_string();
    assert!(to.starts_with("https://example.com/redirect?code="), "{to}");
    let q = query_of(&to);
    assert_eq!(q["state"], "xyz");
    let code = q["code"].clone();
    // Once only.
    app.post(&format!("/_bgh/app-manifests/{token}"))
        .cookie(&cookie)
        .json(&json!({}))
        .send()
        .await
        .assert_status(422);

    // The integration converts the code (no authentication).
    let res = app
        .post(&format!("/api/v3/app-manifests/{code}/conversions"))
        .send()
        .await;
    res.assert_status(201);
    let conv = res.json();
    assert_eq!(conv["slug"], "octo-manifest");
    assert_eq!(conv["name"], "Octo Manifest");
    assert_eq!(conv["owner"]["login"], "acme");
    assert_eq!(conv["external_url"], "https://example.com");
    assert_eq!(conv["permissions"]["issues"], "write");
    assert_eq!(conv["permissions"]["metadata"], "read");
    assert_eq!(conv["events"], json!(["issues"]));
    assert!(conv["client_id"].as_str().unwrap().starts_with("Iv23"));
    assert_eq!(conv["client_secret"].as_str().unwrap().len(), 40);
    assert_eq!(conv["webhook_secret"].as_str().unwrap().len(), 40);
    let pem = conv["pem"].as_str().unwrap();
    assert!(pem.starts_with("-----BEGIN RSA PRIVATE KEY-----"));
    let app_id = conv["id"].as_i64().unwrap();
    assert!(app_id > 15368, "real app ids stay above the Actions app");

    // The PEM works for app JWTs.
    let jwt = bgh_core::apps::sign_jwt(pem, &json!(app_id), now() - 30, now() + 540).unwrap();
    let res = app
        .get("/api/v3/app")
        .header("authorization", &format!("Bearer {jwt}"))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["slug"], "octo-manifest");
    // The app's settings carry the hook and a client secret.
    let detail = app
        .get("/_bgh/apps/octo-manifest")
        .cookie(&cookie)
        .send()
        .await
        .json();
    assert_eq!(detail["webhook_url"], "https://example.com/hook");
    assert_eq!(detail["webhook_secret_set"], true);
    assert_eq!(detail["public"], true);
    assert_eq!(detail["client_secrets"].as_array().unwrap().len(), 1);
    assert_eq!(detail["keys"].as_array().unwrap().len(), 1);

    // Codes are single-use.
    app.post(&format!("/api/v3/app-manifests/{code}/conversions"))
        .send()
        .await
        .assert_status(404);
    app.post("/api/v3/app-manifests/0123456789/conversions")
        .send()
        .await
        .assert_status(404);
    let info = app
        .get(&format!("/_bgh/app-manifests/{token}"))
        .cookie(&cookie)
        .send()
        .await
        .json();
    assert_eq!(info["app_slug"], "octo-manifest");

    // A personal manifest with a renamed app and no hook.
    let res = app
        .post("/settings/apps/new")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(form_body(
            &json!({"url": "https://example.org", "name": "Taken"}),
        ))
        .send()
        .await;
    res.assert_status(303);
    let token = res
        .header("location")
        .unwrap()
        .split("manifest=")
        .nth(1)
        .unwrap()
        .to_string();
    let res = app
        .post(&format!("/_bgh/app-manifests/{token}"))
        .cookie(&cookie)
        .json(&json!({"name": "Alice Tool"}))
        .send()
        .await;
    res.assert_status(201);
    // No redirect_url: back to the app's settings.
    assert_eq!(
        res.json()["redirect_url"],
        app.url("/settings/apps/alice-tool")
    );
}

struct Fx {
    app: TestApp,
    alice: TestUser,
    cookie: String,
    client_id: String,
    secret: String,
    inst: i64,
    one: i64,
}

/// acme (alice admin) has private `one` and `two`; alice has private
/// `mine`. "User Bot" (issues: write, contents: read) is installed on acme
/// for `one`.
async fn fixture() -> Fx {
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
    app.create_private_repo(&alice, "mine").await;
    let one = repo_id(&app, &alice, "acme/one").await;
    let cookie = session(&app, &alice).await;
    let res = app
        .post("/_bgh/apps")
        .cookie(&cookie)
        .json(&json!({
            "owner": "acme",
            "name": "User Bot",
            "homepage_url": "https://example.com",
            "callback_urls": ["https://example.com/cb", "http://127.0.0.1/cb"],
            "permissions": {"issues": "write", "contents": "read"},
            "events": [],
            "public": true,
        }))
        .send()
        .await;
    res.assert_status(201);
    let client_id = res.json()["client_id"].as_str().unwrap().to_string();

    // Client secrets: shown once, listed by their last eight characters.
    let res = app
        .post("/_bgh/apps/user-bot/client_secrets")
        .cookie(&cookie)
        .send()
        .await;
    res.assert_status(201);
    let created = res.json();
    let secret = created["client_secret"].as_str().unwrap().to_string();
    assert_eq!(created["last_eight"], secret[secret.len() - 8..]);
    let detail = app
        .get("/_bgh/apps/user-bot")
        .cookie(&cookie)
        .send()
        .await
        .json();
    let secrets = detail["client_secrets"].as_array().unwrap();
    assert_eq!(secrets.len(), 1);
    assert!(secrets[0].get("client_secret").is_none());
    assert_eq!(secrets[0]["last_used_at"], Value::Null);

    let res = app
        .post("/_bgh/apps/user-bot/installations")
        .cookie(&cookie)
        .json(&json!({"account": "acme", "repository_selection": "selected", "repository_ids": [one]}))
        .send()
        .await;
    res.assert_status(201);
    let inst = res.json()["installation"]["id"].as_i64().unwrap();
    Fx {
        app,
        alice,
        cookie,
        client_id,
        secret,
        inst,
        one,
    }
}

/// Run the browser part of the OAuth flow; returns the code.
async fn authorize(fx: &Fx) -> String {
    let info = fx
        .app
        .get(&format!(
            "/_bgh/oauth/authorize?client_id={}&redirect_uri=https://example.com/cb&state=s1",
            fx.client_id
        ))
        .cookie(&fx.cookie)
        .send()
        .await;
    info.assert_status(200);
    let info = info.json();
    assert_eq!(info["app"]["name"], "User Bot");
    assert_eq!(info["scopes"], json!([]));
    let to = fx
        .app
        .post("/_bgh/oauth/authorize")
        .cookie(&fx.cookie)
        .json(&json!({"consent": info["consent"], "authorize": true}))
        .send()
        .await
        .json()["redirect_url"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(to.starts_with("https://example.com/cb?code="), "{to}");
    assert_eq!(query_of(&to)["state"], "s1");
    query_of(&to)["code"].clone()
}

async fn exchange(fx: &Fx, body: Value) -> Value {
    fx.app
        .post("/login/oauth/access_token")
        .header("accept", "application/json")
        .json(&body)
        .send()
        .await
        .json()
}

#[tokio::test]
async fn user_to_server_tokens() {
    let fx = fixture().await;
    let app = &fx.app;
    let code = authorize(&fx).await;

    let bad = exchange(
        &fx,
        json!({"client_id": fx.client_id, "client_secret": "wrong", "code": code}),
    )
    .await;
    assert_eq!(bad["error"], "incorrect_client_credentials");
    let code = authorize(&fx).await;
    let v = exchange(
        &fx,
        json!({"client_id": fx.client_id, "client_secret": fx.secret, "code": code}),
    )
    .await;
    let token = v["access_token"].as_str().unwrap().to_string();
    assert!(token.starts_with("bghu_"), "{v}");
    let refresh = v["refresh_token"].as_str().unwrap().to_string();
    assert!(refresh.starts_with("bghr_"));
    assert_eq!(v["expires_in"], 28800);
    assert_eq!(v["refresh_token_expires_in"], 15897600);
    assert_eq!(v["token_type"], "bearer");
    assert_eq!(v["scope"], "");
    // Codes are single-use.
    let again = exchange(
        &fx,
        json!({"client_id": fx.client_id, "client_secret": fx.secret, "code": code}),
    )
    .await;
    assert_eq!(again["error"], "bad_verification_code");

    // The token acts as alice.
    let res = app.get("/api/v3/user").token(&token).send().await;
    res.assert_status(200);
    assert_eq!(res.json()["login"], "alice");
    assert!(res.header("x-oauth-scopes").is_none());
    // ... on the installation's repositories only.
    app.get("/api/v3/repos/acme/one")
        .token(&token)
        .send()
        .await
        .assert_status(200);
    app.get("/api/v3/repos/acme/two")
        .token(&token)
        .send()
        .await
        .assert_status(404);
    app.get("/api/v3/repos/alice/mine")
        .token(&token)
        .send()
        .await
        .assert_status(404);
    // Writes within the app's permissions are authored by the user.
    let res = app
        .post("/api/v3/repos/acme/one/issues")
        .token(&token)
        .json(&json!({"title": "via app"}))
        .send()
        .await;
    res.assert_status(201);
    assert_eq!(res.json()["user"]["login"], "alice");
    // ... and nothing beyond them (contents: read).
    let res = app
        .put("/api/v3/repos/acme/one/contents/x.txt")
        .token(&token)
        .json(&json!({"message": "x", "content": "eA=="}))
        .send()
        .await;
    res.assert_status(403);
    // Basic auth accepts the token too.
    app.get("/api/v3/repos/acme/one")
        .basic("x-access-token", &token)
        .send()
        .await
        .assert_status(200);

    // /user/installations: this app's installations.
    let res = app
        .get("/api/v3/user/installations")
        .token(&token)
        .send()
        .await;
    res.assert_status(200);
    let list = res.json();
    assert_eq!(list["total_count"], 1);
    assert_eq!(list["installations"][0]["id"], fx.inst);
    assert_eq!(list["installations"][0]["app_slug"], "user-bot");
    let res = app
        .get(&format!(
            "/api/v3/user/installations/{}/repositories",
            fx.inst
        ))
        .token(&token)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["repositories"][0]["full_name"], "acme/one");

    // Already authorized: the browser goes straight back with a code.
    let res = app
        .get(&format!(
            "/login/oauth/authorize?client_id={}&redirect_uri=https://example.com/cb",
            fx.client_id
        ))
        .cookie(&fx.cookie)
        .send()
        .await;
    assert!(
        res.header("location")
            .unwrap()
            .starts_with("https://example.com/cb?code="),
        "{:?}",
        res.header("location")
    );
    // Unregistered redirect URI.
    let res = app
        .get(&format!(
            "/login/oauth/authorize?client_id={}&redirect_uri=https://evil.example/cb",
            fx.client_id
        ))
        .cookie(&fx.cookie)
        .send()
        .await;
    res.assert_status(400);

    // Refresh: a new pair; the refresh token is single-use.
    let v = exchange(
        &fx,
        json!({"client_id": fx.client_id, "client_secret": fx.secret,
               "grant_type": "refresh_token", "refresh_token": refresh}),
    )
    .await;
    let token2 = v["access_token"].as_str().unwrap().to_string();
    assert!(token2.starts_with("bghu_"));
    assert_ne!(token2, token);
    let reuse = exchange(
        &fx,
        json!({"client_id": fx.client_id, "client_secret": fx.secret,
               "grant_type": "refresh_token", "refresh_token": refresh}),
    )
    .await;
    assert_eq!(reuse["error"], "bad_refresh_token");
    let detail = app
        .get("/_bgh/apps/user-bot")
        .cookie(&fx.cookie)
        .send()
        .await
        .json();
    assert!(detail["client_secrets"][0]["last_used_at"].is_string());

    // Removing the repository from the installation strips it from live
    // tokens.
    let two = repo_id(app, &fx.alice, "acme/two").await;
    app.patch(&format!("/_bgh/installations/{}", fx.inst))
        .cookie(&fx.cookie)
        .json(&json!({"repository_selection": "selected", "repository_ids": [two]}))
        .send()
        .await
        .assert_status(200);
    app.get("/api/v3/repos/acme/one")
        .token(&token2)
        .send()
        .await
        .assert_status(404);
    // Suspension removes the installation's repositories altogether.
    app.put(&format!("/_bgh/installations/{}/suspended", fx.inst))
        .cookie(&fx.cookie)
        .send()
        .await
        .assert_status(204);
    let _ = fx.one;
    let token3 = {
        let code = authorize_again(&fx).await;
        exchange(
            &fx,
            json!({"client_id": fx.client_id, "client_secret": fx.secret, "code": code}),
        )
        .await["access_token"]
            .as_str()
            .unwrap()
            .to_string()
    };
    app.get("/api/v3/repos/acme/two")
        .token(&token3)
        .send()
        .await
        .assert_status(404);

    // Client secrets can be deleted.
    let id = detail["client_secrets"][0]["id"].as_i64().unwrap();
    app.delete(&format!("/_bgh/apps/user-bot/client_secrets/{id}"))
        .cookie(&fx.cookie)
        .send()
        .await
        .assert_status(204);
    let v = exchange(
        &fx,
        json!({"client_id": fx.client_id, "client_secret": fx.secret,
               "grant_type": "refresh_token", "refresh_token": "bghr_x"}),
    )
    .await;
    assert_eq!(v["error"], "incorrect_client_credentials");
}

/// Already authorized: `GET /login/oauth/authorize` redirects with a code.
async fn authorize_again(fx: &Fx) -> String {
    let res = fx
        .app
        .get(&format!(
            "/login/oauth/authorize?client_id={}&redirect_uri=https://example.com/cb",
            fx.client_id
        ))
        .cookie(&fx.cookie)
        .send()
        .await;
    query_of(res.header("location").unwrap())["code"].clone()
}

#[tokio::test]
async fn permission_upgrade_mails_account_admins() {
    let fx = fixture().await;
    let app = &fx.app;
    // Events-only and unchanged updates don't mail.
    app.patch("/_bgh/apps/user-bot")
        .cookie(&fx.cookie)
        .json(&json!({"description": "new words"}))
        .send()
        .await
        .assert_status(200);
    app.drain_jobs().await;
    assert!(bgh_core::mail::outbox(&app.state.config).await.is_empty());
    app.patch("/_bgh/apps/user-bot")
        .cookie(&fx.cookie)
        .json(&json!({"permissions": {"issues": "write", "contents": "write"}}))
        .send()
        .await
        .assert_status(200);
    app.drain_jobs().await;
    let mails = bgh_core::mail::outbox(&app.state.config).await;
    assert_eq!(mails.len(), 1, "{mails:?}");
    assert_eq!(mails[0].to, "alice@example.com");
    assert!(
        mails[0]
            .subject
            .contains("User Bot is requesting updated permissions")
    );
    assert!(mails[0].text.contains(&format!(
        "/organizations/acme/settings/installations/{}",
        fx.inst
    )));
    // The installation keeps its permissions until accepted.
    let res = app
        .get(&format!("/_bgh/installations/{}", fx.inst))
        .cookie(&fx.cookie)
        .send()
        .await
        .json();
    assert_eq!(res["permissions_outdated"], true);
    assert_eq!(res["installation"]["permissions"]["contents"], "read");
}

/// PyGithub mints installation tokens with `{"permissions": {}}`: an empty
/// map narrows nothing (GitHub's behaviour).
#[tokio::test]
async fn empty_permission_map_narrows_nothing() {
    let fx = fixture().await;
    let app = &fx.app;
    let res = app
        .post("/_bgh/apps/user-bot/keys")
        .cookie(&fx.cookie)
        .send()
        .await;
    let pem = res.json()["pem"].as_str().unwrap().to_string();
    let app_id = app
        .get("/_bgh/apps/user-bot")
        .cookie(&fx.cookie)
        .send()
        .await
        .json()["id"]
        .as_i64()
        .unwrap();
    let jwt = bgh_core::apps::sign_jwt(&pem, &json!(app_id), now() - 30, now() + 540).unwrap();
    let res = app
        .post(&format!(
            "/api/v3/app/installations/{}/access_tokens",
            fx.inst
        ))
        .header("authorization", &format!("Bearer {jwt}"))
        .json(&json!({"permissions": {}}))
        .send()
        .await;
    res.assert_status(201);
    assert_eq!(
        res.json()["permissions"],
        json!({"contents": "read", "issues": "write", "metadata": "read"})
    );
}
