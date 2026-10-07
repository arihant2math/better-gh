//! OAuth apps, authorization code flow (+PKCE), device flow, /applications.

use crate::common;

use base64::Engine;
use bgh_core::testing::{TestApp, TestUser};
use common::*;
use serde_json::{Value, json};
use std::collections::HashMap;

const GH_CLIENT_ID: &str = "178c6fc778ccc68e1d6a";

fn form(body: &str) -> HashMap<String, String> {
    url::form_urlencoded::parse(body.as_bytes())
        .into_owned()
        .collect()
}

fn query_of(location: &str) -> HashMap<String, String> {
    url::Url::parse(location)
        .unwrap()
        .query_pairs()
        .into_owned()
        .collect()
}

async fn create_app(app: &TestApp, owner: &TestUser, callback: &str) -> Value {
    let cookie = session(app, owner).await;
    let res = app.post("/_bgh/applications").cookie(&cookie)
        .json(&json!({"name": "My App", "homepage_url": "https://app.example", "callback_url": callback}))
        .send().await;
    res.assert_status(201);
    res.json()
}

#[tokio::test]
async fn app_management() {
    let app = bgh_server::test_app().await;
    let ada = app.create_user("ada").await;
    let bob = app.create_user("bob").await;
    let cookie = session(&app, &ada).await;

    let a = create_app(&app, &ada, "https://app.example/cb").await;
    let id = a["id"].as_i64().unwrap();
    assert!(a["client_id"].as_str().unwrap().starts_with("Iv1."));
    let secret = a["client_secret"].as_str().unwrap().to_string();
    assert_eq!(a["client_secret_last_eight"], secret[secret.len() - 8..]);

    let list = app
        .get("/_bgh/applications")
        .cookie(&cookie)
        .send()
        .await
        .json();
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert!(list[0].get("client_secret").is_none());
    app.get(&format!("/_bgh/applications/{id}"))
        .cookie(&session(&app, &bob).await)
        .send()
        .await
        .assert_status(404);
    app.post("/_bgh/applications")
        .cookie(&cookie)
        .json(&json!({"name": "x"}))
        .send()
        .await
        .assert_status(422);
    app.post("/_bgh/applications")
        .cookie(&cookie)
        .json(&json!({"name": "x", "callback_url": "not a url"}))
        .send()
        .await
        .assert_status(422);
    app.get("/_bgh/applications")
        .auth(&ada)
        .send()
        .await
        .assert_status(403);

    let v = app
        .patch(&format!("/_bgh/applications/{id}"))
        .cookie(&cookie)
        .json(&json!({"name": "Renamed", "device_flow_enabled": true}))
        .send()
        .await
        .json();
    assert_eq!(v["name"], "Renamed");
    assert_eq!(v["device_flow_enabled"], true);
    let v = app
        .post(&format!("/_bgh/applications/{id}/client_secret"))
        .cookie(&cookie)
        .send()
        .await
        .json();
    assert_ne!(v["client_secret"], secret);
    app.delete(&format!("/_bgh/applications/{id}"))
        .cookie(&cookie)
        .send()
        .await
        .assert_status(204);
    app.delete(&format!("/_bgh/applications/{id}"))
        .cookie(&cookie)
        .send()
        .await
        .assert_status(404);
}

#[tokio::test]
async fn authorization_code_flow() {
    let app = bgh_server::test_app().await;
    let owner = app.create_user("owner").await;
    let ada = app.create_user("ada").await;
    let a = create_app(&app, &owner, "https://app.example/cb").await;
    let client_id = a["client_id"].as_str().unwrap().to_string();
    let secret = a["client_secret"].as_str().unwrap().to_string();
    let authorize = format!(
        "/login/oauth/authorize?client_id={client_id}&redirect_uri=https%3A%2F%2Fapp.example%2Fcb%2Fdone&scope=repo%20read:org&state=xyz"
    );

    // Anonymous → login page with return_to.
    let res = app.get(&authorize).send().await;
    res.assert_status(303);
    assert!(
        res.header("location")
            .unwrap()
            .starts_with("/login?return_to=%2Flogin%2Foauth%2Fauthorize")
    );
    // Bad redirect / client → error page, no redirect.
    let cookie = session(&app, &ada).await;
    app.get(&format!(
        "/login/oauth/authorize?client_id={client_id}&redirect_uri=https://evil.example/cb"
    ))
    .cookie(&cookie)
    .send()
    .await
    .assert_status(400);
    app.get("/login/oauth/authorize?client_id=nope")
        .cookie(&cookie)
        .send()
        .await
        .assert_status(400);

    // Consent page.
    let res = app.get(&authorize).cookie(&cookie).send().await;
    res.assert_status(200);
    let html = res.text();
    assert!(html.contains("Authorize My App") && html.contains("<code>read:org</code>"));
    assert_eq!(res.header("x-frame-options"), Some("DENY"));
    let consent = token_after(&html, "name=\"consent\" value=\"");

    // Another user can't use the consent token.
    let bob = app.create_user("bob").await;
    app.post("/login/oauth/authorize")
        .cookie(&session(&app, &bob).await)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(format!("consent={consent}&authorize=1"))
        .send()
        .await
        .assert_status(400);
    let res = app.get(&authorize).cookie(&cookie).send().await;
    let consent = token_after(&res.text(), "name=\"consent\" value=\"");
    let res = app
        .post("/login/oauth/authorize")
        .cookie(&cookie)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(format!("consent={consent}&authorize=1"))
        .send()
        .await;
    res.assert_status(303);
    let q = query_of(res.header("location").unwrap());
    assert!(
        res.header("location")
            .unwrap()
            .starts_with("https://app.example/cb/done?")
    );
    assert_eq!(q["state"], "xyz");
    let code = q["code"].clone();

    // Exchange: wrong secret, then success (form response by default).
    let res = app
        .post("/login/oauth/access_token")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(format!(
            "client_id={client_id}&client_secret=wrong&code={code}"
        ))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(form(&res.text())["error"], "incorrect_client_credentials");
    let res = app
        .post("/login/oauth/access_token")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(format!(
            "client_id={client_id}&client_secret={secret}&code={code}"
        ))
        .send()
        .await;
    res.assert_status(200);
    assert!(
        res.header("content-type")
            .unwrap()
            .starts_with("application/x-www-form-urlencoded")
    );
    let f = form(&res.text());
    assert_eq!(f["token_type"], "bearer");
    assert_eq!(f["scope"], "repo,read:org");
    let token = f["access_token"].clone();
    assert!(token.starts_with("bgho_"));
    // Codes are single use.
    let res = app
        .post("/login/oauth/access_token")
        .header("accept", "application/json")
        .json(&json!({"client_id": client_id, "client_secret": secret, "code": code}))
        .send()
        .await;
    assert_eq!(res.json()["error"], "bad_verification_code");

    let res = app.get("/api/v3/user").token(&token).send().await;
    res.assert_status(200);
    assert_eq!(res.json()["login"], "ada");
    assert_eq!(res.header("x-oauth-scopes"), Some("repo, read:org"));

    // Second authorization with the same scopes skips the consent screen.
    let res = app.get(&authorize).cookie(&cookie).send().await;
    res.assert_status(303);
    assert!(query_of(res.header("location").unwrap()).contains_key("code"));

    // Denying redirects with access_denied.
    let res = app
        .get(&format!(
            "/login/oauth/authorize?client_id={client_id}&scope=gist&state=s2"
        ))
        .cookie(&cookie)
        .send()
        .await;
    let consent = token_after(&res.text(), "name=\"consent\" value=\"");
    let res = app
        .post("/login/oauth/authorize")
        .cookie(&cookie)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(format!("consent={consent}&authorize=0"))
        .send()
        .await;
    let q = query_of(res.header("location").unwrap());
    assert_eq!(q["error"], "access_denied");
    assert_eq!(q["state"], "s2");

    // Grants are listed and revocable (tokens die with them).
    let grants = app
        .get("/_bgh/authorizations")
        .cookie(&cookie)
        .send()
        .await
        .json();
    assert_eq!(grants[0]["app"]["name"], "My App");
    assert_eq!(grants[0]["scopes"], json!(["repo", "read:org"]));
    let gid = grants[0]["id"].as_i64().unwrap();
    // OAuth tokens aren't listed as PATs.
    assert!(
        app.get("/_bgh/tokens")
            .cookie(&cookie)
            .send()
            .await
            .json()
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t["name"] != "My App")
    );
    app.delete(&format!("/_bgh/authorizations/{gid}"))
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
async fn pkce_and_web_client_consent() {
    let app = bgh_server::test_app().await;
    let owner = app.create_user("owner").await;
    let ada = app.create_user("ada").await;
    let a = create_app(&app, &owner, "http://127.0.0.1/callback").await;
    let client_id = a["client_id"].as_str().unwrap();
    let secret = a["client_secret"].as_str().unwrap();
    let cookie = session(&app, &ada).await;
    let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    let challenge = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

    // Loopback callbacks accept any port.
    let info = app.get(&format!("/_bgh/oauth/authorize?client_id={client_id}&redirect_uri=http://127.0.0.1:5555/callback&scope=repo&code_challenge={challenge}&code_challenge_method=S256"))
        .cookie(&cookie).send().await;
    info.assert_status(200);
    let info = info.json();
    assert_eq!(info["app"]["name"], "My App");
    assert_eq!(info["app"]["owner"]["login"], "owner");
    assert_eq!(info["already_authorized"], false);
    let res = app
        .post("/_bgh/oauth/authorize")
        .cookie(&cookie)
        .json(&json!({"consent": info["consent"], "authorize": true}))
        .send()
        .await;
    let to = res.json()["redirect_url"].as_str().unwrap().to_string();
    assert!(to.starts_with("http://127.0.0.1:5555/callback?code="));
    let code = query_of(&to)["code"].clone();

    let res = app.post("/login/oauth/access_token").header("accept", "application/json")
        .json(&json!({"client_id": client_id, "client_secret": secret, "code": code, "code_verifier": "wrong"})).send().await;
    assert_eq!(res.json()["error"], "bad_verification_code");
    // The failed attempt consumed the code; start over.
    let info = app.get(&format!("/_bgh/oauth/authorize?client_id={client_id}&scope=repo&code_challenge={challenge}&code_challenge_method=S256")).cookie(&cookie).send().await.json();
    let to = app
        .post("/_bgh/oauth/authorize")
        .cookie(&cookie)
        .json(&json!({"consent": info["consent"], "authorize": true}))
        .send()
        .await
        .json()["redirect_url"]
        .as_str()
        .unwrap()
        .to_string();
    let code = query_of(&to)["code"].clone();
    let res = app.post("/login/oauth/access_token").header("accept", "application/json")
        .json(&json!({"client_id": client_id, "client_secret": secret, "code": code, "code_verifier": verifier})).send().await;
    let v = res.json();
    assert!(
        v["access_token"].as_str().unwrap().starts_with("bgho_"),
        "{v}"
    );
    assert_eq!(v["scope"], "repo");
}

/// The exact requests `gh auth login --hostname ...` makes (cli/oauth).
#[tokio::test]
async fn device_flow_like_gh() {
    let app = bgh_server::test_app().await;
    let ada = app.create_user("ada").await;

    let res = app
        .post("/login/device/code")
        .header("content-type", "application/x-www-form-urlencoded")
        .header("accept", "application/json")
        .body(format!(
            "client_id={GH_CLIENT_ID}&scope=repo+read%3Aorg+gist"
        ))
        .send()
        .await;
    res.assert_status(200);
    let v = res.json();
    let device_code = v["device_code"].as_str().unwrap().to_string();
    let user_code = v["user_code"].as_str().unwrap().to_string();
    assert_eq!(v["verification_uri"], app.url("/login/device"));
    assert_eq!(v["expires_in"], 900);
    assert_eq!(v["interval"], 5);
    assert_eq!(user_code.len(), 9);

    let poll = || {
        app.post("/login/oauth/access_token").header("content-type", "application/x-www-form-urlencoded")
            .body(format!("client_id={GH_CLIENT_ID}&device_code={device_code}&grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code"))
            .send()
    };
    let res = poll().await;
    res.assert_status(200);
    assert_eq!(form(&res.text())["error"], "authorization_pending");
    // Polling faster than the interval → slow_down with a larger interval.
    let f = form(&poll().await.text());
    assert_eq!(f["error"], "slow_down");
    assert_eq!(f["interval"], "10");

    // The user enters the code in the browser (HTML form).
    let cookie = session(&app, &ada).await;
    let res = app.get("/login/device").send().await;
    res.assert_status(303);
    let page = app.get("/login/device").cookie(&cookie).send().await.text();
    let csrf = token_after(&page, "name=\"csrf\" value=\"");
    let res = app
        .post("/login/device")
        .cookie(&cookie)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(format!(
            "csrf={csrf}&user_code={}&authorize=1",
            user_code.to_lowercase().replace('-', "")
        ))
        .send()
        .await;
    res.assert_status(200);
    assert!(res.text().contains("GitHub CLI"));

    // Reset the poll clock, then the token is issued once.
    let key = format!(
        "{}device:{}",
        app.state.config.redis_prefix,
        bgh_core::crypto::sha256_hex(&device_code)
    );
    let mut redis = app.state.redis.clone();
    let raw: String = redis::cmd("GET")
        .arg(&key)
        .query_async(&mut redis)
        .await
        .unwrap();
    let mut g: Value = serde_json::from_str(&raw).unwrap();
    g["last_poll"] = json!(0);
    let _: () = redis::cmd("SET")
        .arg(&key)
        .arg(g.to_string())
        .query_async(&mut redis)
        .await
        .unwrap();
    let f = form(&poll().await.text());
    assert_eq!(f["token_type"], "bearer");
    assert_eq!(f["scope"], "repo,read:org,gist");
    let token = f["access_token"].clone();
    assert_eq!(form(&poll().await.text())["error"], "expired_token");

    // gh then checks scopes on the API root and reads the user.
    let res = app.get("/api/v3/").token(&token).send().await;
    res.assert_status(200);
    assert_eq!(res.header("x-oauth-scopes"), Some("repo, read:org, gist"));
    assert_eq!(
        app.get("/api/v3/user")
            .header("authorization", &format!("token {token}"))
            .send()
            .await
            .json()["login"],
        "ada"
    );
    // ...and uses the token as a git password (Basic).
    let basic = base64::engine::general_purpose::STANDARD.encode(format!("ada:{token}"));
    app.get("/api/v3/user")
        .header("authorization", &format!("Basic {basic}"))
        .send()
        .await
        .assert_status(200);
}

#[tokio::test]
async fn device_flow_json_api_and_denial() {
    let app = bgh_server::test_app().await;
    let owner = app.create_user("owner").await;
    let ada = app.create_user("ada").await;
    let a = create_app(&app, &owner, "https://app.example/cb").await;
    let client_id = a["client_id"].as_str().unwrap().to_string();

    // Device flow must be enabled per app.
    let res = app
        .post("/login/device/code")
        .header("accept", "application/json")
        .json(&json!({"client_id": client_id}))
        .send()
        .await;
    res.assert_status(400);
    assert_eq!(res.json()["error"], "device_flow_disabled");
    app.post("/login/device/code")
        .json(&json!({"client_id": "unknown"}))
        .send()
        .await
        .assert_status(404);

    let v = app
        .post("/login/device/code")
        .header("accept", "application/json")
        .json(&json!({"client_id": GH_CLIENT_ID, "scope": "repo site_admin"}))
        .send()
        .await
        .json();
    let user_code = v["user_code"].as_str().unwrap();
    let device_code = v["device_code"].as_str().unwrap();
    let cookie = session(&app, &ada).await;
    let info = app
        .get(&format!("/_bgh/device/{user_code}"))
        .cookie(&cookie)
        .send()
        .await
        .json();
    assert_eq!(info["app"]["name"], "GitHub CLI");
    assert_eq!(
        info["scopes"],
        json!(["repo"]),
        "site_admin can't be requested"
    );
    app.post("/_bgh/device")
        .cookie(&cookie)
        .json(&json!({"user_code": user_code, "authorize": false}))
        .send()
        .await
        .assert_status(204);
    app.post("/_bgh/device")
        .cookie(&cookie)
        .json(&json!({"user_code": user_code}))
        .send()
        .await
        .assert_status(404);
    let res = app.post("/login/oauth/access_token").header("accept", "application/json")
        .json(&json!({"client_id": GH_CLIENT_ID, "device_code": device_code, "grant_type": "urn:ietf:params:oauth:grant-type:device_code"})).send().await;
    assert_eq!(res.json()["error"], "access_denied");
    app.get("/_bgh/device/AAAA-BBBB")
        .cookie(&cookie)
        .send()
        .await
        .assert_status(404);
}

#[tokio::test]
async fn applications_token_api() {
    let app = bgh_server::test_app().await;
    let owner = app.create_user("owner").await;
    let ada = app.create_user("ada").await;
    let a = create_app(&app, &owner, "https://app.example/cb").await;
    let client_id = a["client_id"].as_str().unwrap().to_string();
    let secret = a["client_secret"].as_str().unwrap().to_string();
    let cookie = session(&app, &ada).await;
    let info = app
        .get(&format!(
            "/_bgh/oauth/authorize?client_id={client_id}&scope=gist"
        ))
        .cookie(&cookie)
        .send()
        .await
        .json();
    let to = app
        .post("/_bgh/oauth/authorize")
        .cookie(&cookie)
        .json(&json!({"consent": info["consent"], "authorize": true}))
        .send()
        .await
        .json()["redirect_url"]
        .as_str()
        .unwrap()
        .to_string();
    let code = query_of(&to)["code"].clone();
    // Client credentials via Basic.
    let basic = base64::engine::general_purpose::STANDARD.encode(format!("{client_id}:{secret}"));
    let token = app
        .post("/login/oauth/access_token")
        .header("authorization", &format!("Basic {basic}"))
        .header("accept", "application/json")
        .json(&json!({"code": code}))
        .send()
        .await
        .json()["access_token"]
        .as_str()
        .unwrap()
        .to_string();
    let auth = format!("Basic {basic}");

    let res = app
        .post(&format!("/api/v3/applications/{client_id}/token"))
        .header("authorization", &auth)
        .json(&json!({"access_token": token}))
        .send()
        .await;
    res.assert_status(200);
    let v = res.json();
    assert_eq!(v["token"], token);
    assert_eq!(v["scopes"], json!(["gist"]));
    assert_eq!(v["app"]["client_id"], client_id);
    assert_eq!(v["user"]["login"], "ada");
    assert_eq!(v["token_last_eight"], token[token.len() - 8..]);

    let wrong = base64::engine::general_purpose::STANDARD.encode(format!("{client_id}:nope"));
    app.post(&format!("/api/v3/applications/{client_id}/token"))
        .header("authorization", &format!("Basic {wrong}"))
        .json(&json!({"access_token": token}))
        .send()
        .await
        .assert_status(401);
    app.post(&format!("/api/v3/applications/{client_id}/token"))
        .header("authorization", &auth)
        .json(&json!({"access_token": "bgho_nope"}))
        .send()
        .await
        .assert_status(404);

    let reset = app
        .patch(&format!("/api/v3/applications/{client_id}/token"))
        .header("authorization", &auth)
        .json(&json!({"access_token": token}))
        .send()
        .await
        .json();
    let new_token = reset["token"].as_str().unwrap().to_string();
    assert_ne!(new_token, token);
    app.get("/api/v3/user")
        .token(&token)
        .send()
        .await
        .assert_status(401);
    app.get("/api/v3/user")
        .token(&new_token)
        .send()
        .await
        .assert_status(200);

    app.delete(&format!("/api/v3/applications/{client_id}/grant"))
        .header("authorization", &auth)
        .json(&json!({"access_token": new_token}))
        .send()
        .await
        .assert_status(204);
    app.get("/api/v3/user")
        .token(&new_token)
        .send()
        .await
        .assert_status(401);
    assert_eq!(
        app.get("/_bgh/authorizations")
            .cookie(&cookie)
            .send()
            .await
            .json(),
        json!([])
    );
}
