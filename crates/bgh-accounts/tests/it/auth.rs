//! Login (throttling, 2FA), sessions, passwords, tokens.

use crate::common;

use bgh_accounts::totp;
use bgh_core::testing::TestApp;
use common::*;
use serde_json::{Value, json};

async fn login(app: &TestApp, login: &str, password: &str) -> bgh_core::testing::TestResponse {
    app.post("/_bgh/session")
        .json(&json!({"login": login, "password": password}))
        .send()
        .await
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Enable TOTP for the session user; returns (secret, recovery codes).
async fn enable_2fa(app: &TestApp, cookie: &str) -> (String, Vec<String>) {
    let res = app
        .post("/_bgh/user/two_factor/totp")
        .cookie(cookie)
        .send()
        .await;
    res.assert_status(201);
    let v = res.json();
    let secret = v["secret"].as_str().unwrap().to_string();
    assert!(
        v["otpauth_uri"]
            .as_str()
            .unwrap()
            .starts_with("otpauth://totp/")
    );
    // A wrong code doesn't enable it.
    app.post("/_bgh/user/two_factor/totp/enable")
        .cookie(cookie)
        .json(&json!({"code": "000000"}))
        .send()
        .await
        .assert_status(422);
    // Use the previous step so the login below can use the current one.
    let code = totp::code_at(&secret, now() - 30).unwrap();
    let res = app
        .post("/_bgh/user/two_factor/totp/enable")
        .cookie(cookie)
        .json(&json!({"code": code}))
        .send()
        .await;
    res.assert_status(200);
    let codes: Vec<String> = res.json()["recovery_codes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_str().unwrap().to_string())
        .collect();
    assert_eq!(codes.len(), 10);
    (secret, codes)
}

#[tokio::test]
async fn login_throttling() {
    let app = bgh_server::test_app().await;
    app.create_user("ada").await;
    for _ in 0..10 {
        login(&app, "ada", "wrong-password")
            .await
            .assert_status(401);
    }
    // Locked even with the right password.
    let res = login(&app, "ada", bgh_core::testing::TEST_PASSWORD).await;
    res.assert_status(429);
    assert!(res.json()["message"].as_str().unwrap().contains("Too many"));
}

#[tokio::test]
async fn two_factor_login_flow() {
    let app = bgh_server::test_app().await;
    let ada = app.create_user("ada").await;
    let cookie = session(&app, &ada).await;

    let status = app
        .get("/_bgh/user/two_factor")
        .cookie(&cookie)
        .send()
        .await
        .json();
    assert_eq!(status["enabled"], false);
    // Token-authenticated requests can't manage 2FA.
    app.post("/_bgh/user/two_factor/totp")
        .auth(&ada)
        .send()
        .await
        .assert_status(403);

    let (secret, codes) = enable_2fa(&app, &cookie).await;
    assert_eq!(
        app.get("/api/v3/user").auth(&ada).send().await.json()["two_factor_authentication"],
        true
    );
    let status = app
        .get("/_bgh/user/two_factor")
        .cookie(&cookie)
        .send()
        .await
        .json();
    assert_eq!(status["enabled"], true);
    assert_eq!(status["recovery_codes_remaining"], 10);
    assert!(
        mails(&app)
            .await
            .iter()
            .any(|m| m.subject.contains("Two-factor authentication enabled"))
    );

    // Password alone is not enough: 202 + pending token, no cookie.
    let res = login(&app, "ada", &ada.password).await;
    res.assert_status(202);
    assert!(res.header("set-cookie").is_none());
    assert_eq!(res.header("x-github-otp"), Some("required; app"));
    let pending = res.json()["two_factor_token"].as_str().unwrap().to_string();
    assert_eq!(res.json()["two_factor_required"], true);

    let res = app
        .post("/_bgh/session/two_factor")
        .json(&json!({"two_factor_token": pending, "code": "123456"}))
        .send()
        .await;
    res.assert_status(401);
    let code = totp::code_at(&secret, now()).unwrap();
    let res = app
        .post("/_bgh/session/two_factor")
        .json(&json!({"two_factor_token": pending, "code": code}))
        .send()
        .await;
    res.assert_status(200);
    let c2 = cookie_from(&res);
    app.get("/api/v3/user")
        .cookie(&c2)
        .send()
        .await
        .assert_status(200);
    // The pending token is single use; the TOTP code can't be replayed.
    app.post("/_bgh/session/two_factor")
        .json(&json!({"two_factor_token": pending, "code": code}))
        .send()
        .await
        .assert_status(401);
    let res = login(&app, "ada", &ada.password).await;
    let p2 = res.json()["two_factor_token"].as_str().unwrap().to_string();
    app.post("/_bgh/session/two_factor")
        .json(&json!({"two_factor_token": p2, "code": code}))
        .send()
        .await
        .assert_status(401);

    // Recovery codes work once (any case / without dash).
    let rc = codes[0].to_uppercase().replace('-', "");
    app.post("/_bgh/session/two_factor")
        .json(&json!({"two_factor_token": p2, "code": rc}))
        .send()
        .await
        .assert_status(200);
    let res = login(&app, "ada", &ada.password).await;
    let p3 = res.json()["two_factor_token"].as_str().unwrap().to_string();
    app.post("/_bgh/session/two_factor")
        .json(&json!({"two_factor_token": p3, "code": codes[0]}))
        .send()
        .await
        .assert_status(401);

    // Attempts per pending login are limited.
    for _ in 0..4 {
        app.post("/_bgh/session/two_factor")
            .json(&json!({"two_factor_token": p3, "code": "000000"}))
            .send()
            .await
            .assert_status(401);
    }
    app.post("/_bgh/session/two_factor")
        .json(&json!({"two_factor_token": p3, "code": codes[1]}))
        .send()
        .await
        .assert_status(429);

    // One-shot login with `otp`.
    let res = app
        .post("/_bgh/session")
        .json(&json!({"login": "ada", "password": ada.password, "otp": codes[2]}))
        .send()
        .await;
    res.assert_status(200);

    // Regenerate and disable require the password.
    app.post("/_bgh/user/two_factor/recovery_codes")
        .cookie(&cookie)
        .json(&json!({"password": "wrong"}))
        .send()
        .await
        .assert_status(403);
    let res = app
        .post("/_bgh/user/two_factor/recovery_codes")
        .cookie(&cookie)
        .json(&json!({"password": ada.password}))
        .send()
        .await;
    res.assert_status(200);
    assert_ne!(res.json()["recovery_codes"][0], codes[3]);
    app.delete("/_bgh/user/two_factor")
        .cookie(&cookie)
        .json(&json!({"password": ada.password}))
        .send()
        .await
        .assert_status(204);
    login(&app, "ada", &ada.password).await.assert_status(200);
}

#[tokio::test]
async fn sessions_list_and_revoke() {
    let app = bgh_server::test_app().await;
    let ada = app.create_user("ada").await;
    let res = app
        .post("/_bgh/session")
        .header("user-agent", "Firefox")
        .json(&json!({"login": "ada", "password": ada.password}))
        .send()
        .await;
    res.assert_status(200);
    let c1 = cookie_from(&res);
    let c2 = cookie_from(&login(&app, "ada", &ada.password).await);
    let c3 = cookie_from(&login(&app, "ada", &ada.password).await);

    let v = app.get("/_bgh/sessions").cookie(&c1).send().await.json();
    let list = v.as_array().unwrap();
    assert_eq!(list.len(), 3);
    let current: Vec<&Value> = list.iter().filter(|s| s["current"] == true).collect();
    assert_eq!(current.len(), 1);
    assert_eq!(current[0]["user_agent"], "Firefox");
    let other = list.iter().find(|s| s["current"] == false).unwrap()["id"]
        .as_i64()
        .unwrap();
    app.get("/_bgh/sessions")
        .auth(&ada)
        .send()
        .await
        .assert_status(403);

    app.delete(&format!("/_bgh/sessions/{other}"))
        .cookie(&c1)
        .send()
        .await
        .assert_status(204);
    app.delete(&format!("/_bgh/sessions/{other}"))
        .cookie(&c1)
        .send()
        .await
        .assert_status(404);
    assert_eq!(
        app.get("/_bgh/sessions")
            .cookie(&c1)
            .send()
            .await
            .json()
            .as_array()
            .unwrap()
            .len(),
        2
    );

    // Revoke all others: the current one survives.
    app.delete("/_bgh/sessions")
        .cookie(&c1)
        .send()
        .await
        .assert_status(204);
    app.get("/api/v3/user")
        .cookie(&c1)
        .send()
        .await
        .assert_status(200);
    app.get("/api/v3/user")
        .cookie(&c2)
        .send()
        .await
        .assert_status(401);
    app.get("/api/v3/user")
        .cookie(&c3)
        .send()
        .await
        .assert_status(401);
}

#[tokio::test]
async fn password_change() {
    let app = bgh_server::test_app().await;
    let ada = app.create_user("ada").await;
    let c1 = cookie_from(&login(&app, "ada", &ada.password).await);
    let c2 = cookie_from(&login(&app, "ada", &ada.password).await);

    let res = app
        .put("/_bgh/user/password")
        .cookie(&c1)
        .json(&json!({"current_password": "nope", "password": "new-password-1"}))
        .send()
        .await;
    res.assert_status(422);
    assert_eq!(res.json()["errors"][0]["field"], "current_password");
    app.put("/_bgh/user/password")
        .cookie(&c1)
        .json(&json!({"current_password": ada.password, "password": "short"}))
        .send()
        .await
        .assert_status(422);
    app.put("/_bgh/user/password")
        .auth(&ada)
        .json(&json!({"current_password": ada.password, "password": "new-password-1"}))
        .send()
        .await
        .assert_status(403);
    app.put("/_bgh/user/password")
        .cookie(&c1)
        .json(&json!({"current_password": ada.password, "password": "new-password-1"}))
        .send()
        .await
        .assert_status(204);

    app.get("/api/v3/user")
        .cookie(&c1)
        .send()
        .await
        .assert_status(200);
    app.get("/api/v3/user")
        .cookie(&c2)
        .send()
        .await
        .assert_status(401);
    login(&app, "ada", &ada.password).await.assert_status(401);
    login(&app, "ada", "new-password-1")
        .await
        .assert_status(200);
    assert!(
        last_mail_to(&app, "ada@example.com")
            .await
            .subject
            .contains("password was changed")
    );
}

#[tokio::test]
async fn password_reset() {
    let app = bgh_server::test_app().await;
    let ada = app.create_user("ada").await;
    let c1 = cookie_from(&login(&app, "ada", &ada.password).await);

    // Unknown accounts get the same answer.
    app.post("/_bgh/password_reset")
        .json(&json!({"email": "nobody@example.com"}))
        .send()
        .await
        .assert_status(202);
    let res = app
        .post("/_bgh/password_reset")
        .json(&json!({"email": "ADA@example.com"}))
        .send()
        .await;
    res.assert_status(202);
    let mail = last_mail_to(&app, "ada@example.com").await;
    assert!(mail.subject.contains("reset your password"));
    let token = token_after(&mail.text, "/password_reset/");

    let v = app
        .get(&format!("/_bgh/password_reset/{token}"))
        .send()
        .await
        .json();
    assert_eq!(v, json!({"login": "ada", "two_factor_required": false}));
    app.get("/_bgh/password_reset/bogus")
        .send()
        .await
        .assert_status(404);
    app.post(&format!("/_bgh/password_reset/{token}"))
        .json(&json!({"password": "x"}))
        .send()
        .await
        .assert_status(422);
    app.post(&format!("/_bgh/password_reset/{token}"))
        .json(&json!({"password": "brand-new-pass"}))
        .send()
        .await
        .assert_status(204);
    // Single use; all sessions signed out.
    app.post(&format!("/_bgh/password_reset/{token}"))
        .json(&json!({"password": "brand-new-pass2"}))
        .send()
        .await
        .assert_status(404);
    app.get("/api/v3/user")
        .cookie(&c1)
        .send()
        .await
        .assert_status(401);
    login(&app, "ada", "brand-new-pass")
        .await
        .assert_status(200);

    // Requests are throttled per address.
    for _ in 0..4 {
        app.post("/_bgh/password_reset")
            .json(&json!({"email": "ada@example.com"}))
            .send()
            .await
            .assert_status(202);
    }
    app.post("/_bgh/password_reset")
        .json(&json!({"email": "ada@example.com"}))
        .send()
        .await
        .assert_status(429);
}

#[tokio::test]
async fn password_reset_requires_second_factor() {
    let app = bgh_server::test_app().await;
    let ada = app.create_user("ada").await;
    let cookie = session(&app, &ada).await;
    let (secret, _) = enable_2fa(&app, &cookie).await;
    app.post("/_bgh/password_reset")
        .json(&json!({"login": "ada"}))
        .send()
        .await
        .assert_status(202);
    let token = token_after(
        &last_mail_to(&app, "ada@example.com").await.text,
        "/password_reset/",
    );
    assert_eq!(
        app.get(&format!("/_bgh/password_reset/{token}"))
            .send()
            .await
            .json()["two_factor_required"],
        true
    );
    let res = app
        .post(&format!("/_bgh/password_reset/{token}"))
        .json(&json!({"password": "brand-new-pass"}))
        .send()
        .await;
    res.assert_status(422);
    assert_eq!(res.json()["errors"][0]["field"], "otp");
    let otp = totp::code_at(&secret, now()).unwrap();
    app.post(&format!("/_bgh/password_reset/{token}"))
        .json(&json!({"password": "brand-new-pass", "otp": otp}))
        .send()
        .await
        .assert_status(204);
}

#[tokio::test]
async fn personal_access_tokens() {
    let app = bgh_server::test_app().await;
    let ada = app.create_user("ada").await;
    let cookie = session(&app, &ada).await;

    let res = app
        .post("/_bgh/tokens")
        .cookie(&cookie)
        .json(&json!({"note": "ci", "scopes": ["repo", "read:org"], "expires_in_days": 30}))
        .send()
        .await;
    res.assert_status(201);
    let v = res.json();
    let token = v["token"].as_str().unwrap().to_string();
    assert!(token.starts_with("bghp_"));
    assert!(v["expires_at"].as_str().is_some());
    let res = app.get("/api/v3/user").token(&token).send().await;
    res.assert_status(200);
    assert_eq!(res.header("x-oauth-scopes"), Some("repo, read:org"));
    app.post("/_bgh/tokens")
        .cookie(&cookie)
        .json(&json!({"scopes": ["bogus"]}))
        .send()
        .await
        .assert_status(422);
    app.post("/_bgh/tokens")
        .cookie(&cookie)
        .json(&json!({"scopes": ["site_admin"]}))
        .send()
        .await
        .assert_status(422);
    app.post("/_bgh/tokens")
        .token(&token)
        .json(&json!({"scopes": []}))
        .send()
        .await
        .assert_status(403);

    // Expired tokens stop working.
    sqlx::query("UPDATE access_tokens SET expires_at = now() - interval '1 minute' WHERE token_last_eight = $1")
        .bind(&token[token.len() - 8..])
        .execute(&app.state.db)
        .await
        .unwrap();
    app.get("/api/v3/user")
        .token(&token)
        .send()
        .await
        .assert_status(401);

    let list = app.get("/_bgh/tokens").cookie(&cookie).send().await.json();
    let id = list
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "ci")
        .unwrap()["id"]
        .as_i64()
        .unwrap();
    app.delete(&format!("/_bgh/tokens/{id}"))
        .cookie(&cookie)
        .send()
        .await
        .assert_status(204);
}

#[tokio::test]
async fn basic_password_auth_refused_with_two_factor() {
    use base64::Engine;
    use bgh_core::auth::{AuthOptions, authenticate};
    let app = bgh_server::test_app().await;
    let ada = app.create_user("ada").await;
    let mut headers = axum::http::HeaderMap::new();
    let basic = base64::engine::general_purpose::STANDARD.encode(format!("ada:{}", ada.password));
    headers.insert("authorization", format!("Basic {basic}").parse().unwrap());
    let opts = AuthOptions {
        allow_password: true,
    };
    assert!(
        authenticate(&app.state, &headers, opts)
            .await
            .unwrap()
            .is_some()
    );
    let cookie = session(&app, &ada).await;
    enable_2fa(&app, &cookie).await;
    assert!(authenticate(&app.state, &headers, opts).await.is_err());
}
