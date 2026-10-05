//! OIDC SSO against a mock provider, avatars, API rate limiting.

use crate::common;

use std::collections::HashMap;

use axum::Router;
use axum::routing::{get, post};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use bgh_core::testing::TestApp;
use common::*;
use serde_json::{Value, json};

/// Mock OIDC provider: the "code" is base64url JSON of the claims to put
/// in the ID token (tests craft it after reading the nonce).
async fn mock_provider() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let issuer = base.clone();
    let router = Router::new()
        .route(
            "/.well-known/openid-configuration",
            get({
                let issuer = issuer.clone();
                move || async move {
                    axum::Json(json!({
                        "issuer": issuer,
                        "authorization_endpoint": format!("{issuer}/authorize"),
                        "token_endpoint": format!("{issuer}/token"),
                    }))
                }
            }),
        )
        .route(
            "/token",
            post(|body: String| async move {
                let form: HashMap<String, String> = url::form_urlencoded::parse(body.as_bytes())
                    .into_owned()
                    .collect();
                assert_eq!(form["grant_type"], "authorization_code");
                assert_eq!(form["client_secret"], "s3cret");
                assert!(form.contains_key("code_verifier"));
                let claims =
                    String::from_utf8(URL_SAFE_NO_PAD.decode(&form["code"]).unwrap()).unwrap();
                let jwt = format!(
                    "{}.{}.sig",
                    URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256"}"#),
                    URL_SAFE_NO_PAD.encode(claims)
                );
                axum::Json(json!({"access_token": "at", "token_type": "Bearer", "id_token": jwt}))
            }),
        );
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    base
}

/// Configure the `corp` provider through the `auth_providers` site setting
/// (what the admin settings API stores).
async fn configure(app: &TestApp, issuer: &str, extra: Value) {
    let mut cfg = json!({"name": "corp", "display_name": "Corp SSO", "issuer": issuer,
                         "client_id": "bgh", "client_secret": "s3cret"});
    cfg.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    bgh_core::settings::store_section(&app.state.db, "auth_providers", &json!({"oidc": [cfg]}))
        .await
        .unwrap();
    bgh_core::settings::invalidate(&app.state);
}

/// Start a login and return (state, nonce).
async fn start(app: &TestApp, return_to: &str) -> (String, String) {
    let res = app
        .get(&format!("/_bgh/sso/corp/login?return_to={return_to}"))
        .send()
        .await;
    res.assert_status(303);
    let loc = res.header("location").unwrap().to_string();
    let q: HashMap<String, String> = url::Url::parse(&loc)
        .unwrap()
        .query_pairs()
        .into_owned()
        .collect();
    assert_eq!(q["client_id"], "bgh");
    assert_eq!(q["code_challenge_method"], "S256");
    assert_eq!(q["redirect_uri"], app.url("/_bgh/sso/corp/callback"));
    (q["state"].clone(), q["nonce"].clone())
}

fn code(issuer: &str, nonce: &str, sub: &str, email: &str, extra: Value) -> String {
    let mut claims = json!({"iss": issuer, "aud": "bgh", "exp": chrono::Utc::now().timestamp() + 300,
                            "nonce": nonce, "sub": sub, "email": email, "email_verified": true,
                            "preferred_username": "Jane.Doe", "name": "Jane Doe"});
    claims
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    URL_SAFE_NO_PAD.encode(claims.to_string())
}

#[tokio::test]
async fn oidc_login_creates_and_links_accounts() {
    let app = bgh_server::test_app().await;
    let issuer = mock_provider().await;
    configure(&app, &issuer, json!({})).await;

    let v = app.get("/_bgh/sso").send().await.json();
    assert_eq!(
        v,
        json!([{"id": "corp", "name": "Corp SSO", "login_url": app.url("/_bgh/sso/corp/login")}])
    );

    // New identity → new account (login from preferred_username).
    let (st, nonce) = start(&app, "/dashboard").await;
    let res = app
        .get(&format!(
            "/_bgh/sso/corp/callback?state={st}&code={}",
            code(&issuer, &nonce, "u-1", "jane@corp.example", json!({}))
        ))
        .send()
        .await;
    res.assert_status(303);
    assert_eq!(res.header("location"), Some("/dashboard"));
    let cookie = cookie_from(&res);
    let me = app.get("/api/v3/user").cookie(&cookie).send().await.json();
    assert_eq!(me["login"], "Jane-Doe");
    assert_eq!(me["name"], "Jane Doe");
    let ids = app
        .get("/_bgh/user/identities")
        .cookie(&cookie)
        .send()
        .await
        .json();
    assert_eq!(ids[0]["provider"], "corp");
    assert_eq!(ids[0]["subject"], "u-1");
    // The only sign-in method of a password-less user can't be unlinked.
    app.delete(&format!("/_bgh/user/identities/{}", ids[0]["id"]))
        .cookie(&cookie)
        .send()
        .await
        .assert_status(422);

    // State is single use.
    app.get(&format!("/_bgh/sso/corp/callback?state={st}&code=x"))
        .send()
        .await
        .assert_status(303);
    let res = app
        .get(&format!("/_bgh/sso/corp/callback?state={st}&code=x"))
        .send()
        .await;
    assert!(res.header("location").unwrap().starts_with("/login?error="));

    // Same subject → same account.
    let (st, nonce) = start(&app, "/").await;
    let res = app
        .get(&format!(
            "/_bgh/sso/corp/callback?state={st}&code={}",
            code(&issuer, &nonce, "u-1", "jane@corp.example", json!({}))
        ))
        .send()
        .await;
    assert_eq!(
        app.get("/api/v3/user")
            .cookie(&cookie_from(&res))
            .send()
            .await
            .json()["login"],
        "Jane-Doe"
    );

    // A verified email of an existing user links to it.
    let ada = app.create_user("ada").await;
    let (st, nonce) = start(&app, "//evil.example").await;
    let res = app
        .get(&format!(
            "/_bgh/sso/corp/callback?state={st}&code={}",
            code(&issuer, &nonce, "u-2", "ada@example.com", json!({}))
        ))
        .send()
        .await;
    assert_eq!(
        res.header("location"),
        Some("/"),
        "open redirects are refused"
    );
    assert_eq!(
        app.get("/api/v3/user")
            .cookie(&cookie_from(&res))
            .send()
            .await
            .json()["id"],
        ada.id
    );

    // Bad nonce / audience are rejected.
    let (st, _) = start(&app, "/").await;
    let res = app
        .get(&format!(
            "/_bgh/sso/corp/callback?state={st}&code={}",
            code(&issuer, "wrong", "u-3", "x@corp.example", json!({}))
        ))
        .send()
        .await;
    assert!(res.header("location").unwrap().starts_with("/login?error="));
    assert!(res.header("set-cookie").is_none());
    let (st, nonce) = start(&app, "/").await;
    let res = app
        .get(&format!(
            "/_bgh/sso/corp/callback?state={st}&code={}",
            code(
                &issuer,
                &nonce,
                "u-3",
                "x@corp.example",
                json!({"aud": "other"})
            )
        ))
        .send()
        .await;
    assert!(res.header("set-cookie").is_none());
    // Unverified emails don't create accounts.
    let (st, nonce) = start(&app, "/").await;
    let res = app
        .get(&format!(
            "/_bgh/sso/corp/callback?state={st}&code={}",
            code(
                &issuer,
                &nonce,
                "u-4",
                "ada@example.com",
                json!({"email_verified": false})
            )
        ))
        .send()
        .await;
    assert!(res.header("set-cookie").is_none());
    app.get("/_bgh/sso/unknown/login")
        .send()
        .await
        .assert_status(404);
}

#[tokio::test]
async fn oidc_respects_auto_create_domains_and_two_factor() {
    let app = bgh_server::test_app().await;
    let issuer = mock_provider().await;
    configure(
        &app,
        &issuer,
        json!({"auto_create_users": false, "allowed_domains": ["example.com"]}),
    )
    .await;
    let ada = app.create_user("ada").await;

    let (st, nonce) = start(&app, "/").await;
    let res = app
        .get(&format!(
            "/_bgh/sso/corp/callback?state={st}&code={}",
            code(&issuer, &nonce, "n-1", "new@example.com", json!({}))
        ))
        .send()
        .await;
    assert!(
        res.header("location").unwrap().contains("No%20account")
            || res.header("location").unwrap().contains("No+account")
    );
    let (st, nonce) = start(&app, "/").await;
    let res = app
        .get(&format!(
            "/_bgh/sso/corp/callback?state={st}&code={}",
            code(&issuer, &nonce, "n-2", "ada@other.example", json!({}))
        ))
        .send()
        .await;
    assert!(res.header("set-cookie").is_none());

    // 2FA users must still present the second factor.
    sqlx::query("INSERT INTO user_two_factor (user_id, totp_secret, enabled_at) VALUES ($1, 'JBSWY3DPEHPK3PXP', now())")
        .bind(ada.id).execute(&app.state.db).await.unwrap();
    let (st, nonce) = start(&app, "/x").await;
    let res = app
        .get(&format!(
            "/_bgh/sso/corp/callback?state={st}&code={}",
            code(&issuer, &nonce, "a-1", "ada@example.com", json!({}))
        ))
        .send()
        .await;
    assert!(res.header("set-cookie").is_none());
    let loc = res.header("location").unwrap();
    assert!(loc.starts_with("/login/two-factor?token="), "{loc}");
    let token = token_after(loc, "token=");
    let code_now =
        bgh_accounts::totp::code_at("JBSWY3DPEHPK3PXP", chrono::Utc::now().timestamp()).unwrap();
    app.post("/_bgh/session/two_factor")
        .json(&json!({"two_factor_token": token, "code": code_now}))
        .send()
        .await
        .assert_status(200);
}

#[tokio::test]
async fn avatars() {
    let app = bgh_server::test_app().await;
    let ada = app.create_user("ada").await;

    // Identicon fallback.
    let res = app.get(&format!("/avatars/u/{}?s=64", ada.id)).send().await;
    res.assert_status(200);
    assert_eq!(res.header("content-type"), Some("image/png"));
    assert_eq!(res.header("cache-control"), Some("public, max-age=86400"));
    let etag = res.header("etag").unwrap().to_string();
    let png = res.body.clone();
    assert!(png.starts_with(b"\x89PNG"));
    let again = app.get(&format!("/avatars/u/{}?s=64", ada.id)).send().await;
    assert_eq!(again.body, png, "deterministic");
    app.get(&format!("/avatars/u/{}?s=64", ada.id))
        .header("if-none-match", &etag)
        .send()
        .await
        .assert_status(304);
    app.get("/avatars/u/abc").send().await.assert_status(404);
    let me = app.get("/api/v3/user").auth(&ada).send().await.json();
    assert_eq!(
        me["avatar_url"],
        app.url(&format!("/avatars/u/{}?v=4", ada.id))
    );

    // Upload (session only, images only).
    let cookie = session(&app, &ada).await;
    let image = bgh_accounts::avatars::identicon_png("custom", 32);
    app.put("/_bgh/user/avatar")
        .auth(&ada)
        .body(image.clone())
        .send()
        .await
        .assert_status(403);
    app.put("/_bgh/user/avatar")
        .cookie(&cookie)
        .body(b"not an image".to_vec())
        .send()
        .await
        .assert_status(422);
    app.put("/_bgh/user/avatar")
        .cookie(&cookie)
        .body(vec![0u8; 1024 * 1024 + 1])
        .send()
        .await
        .assert_status(413);
    let res = app
        .put("/_bgh/user/avatar")
        .cookie(&cookie)
        .body(image.clone())
        .send()
        .await;
    res.assert_status(200);
    let url = res.json()["avatar_url"].as_str().unwrap().to_string();
    assert!(url.starts_with(&app.url(&format!("/avatars/u/{}?v=", ada.id))));
    assert_eq!(
        app.get("/api/v3/users/ada").send().await.json()["avatar_url"],
        url
    );
    let path = url.trim_start_matches(&app.base_url).to_string();
    let res = app.get(&path).send().await;
    res.assert_status(200);
    assert_eq!(
        res.header("cache-control"),
        Some("public, max-age=31536000, immutable")
    );
    assert_eq!(res.body.as_ref(), image.as_slice());

    // Back to the identicon.
    app.delete("/_bgh/user/avatar")
        .cookie(&cookie)
        .send()
        .await
        .assert_status(200);
    assert_eq!(
        app.get("/api/v3/users/ada").send().await.json()["avatar_url"],
        app.url(&format!("/avatars/u/{}?v=4", ada.id))
    );

    // Org avatars: owners only.
    let bob = app.create_user("bob").await;
    let org = app.create_org("acme", &ada).await;
    app.add_org_member(&org, &bob, "member").await;
    app.put("/_bgh/orgs/acme/avatar")
        .auth(&bob)
        .body(image.clone())
        .send()
        .await
        .assert_status(403);
    app.put("/_bgh/orgs/acme/avatar")
        .auth(&ada)
        .body(image)
        .send()
        .await
        .assert_status(200);
    assert!(
        app.get("/api/v3/orgs/acme").send().await.json()["avatar_url"]
            .as_str()
            .unwrap()
            .contains("?v=")
    );
}

#[tokio::test]
async fn rate_limits_are_enforced() {
    let app = TestApp::spawn_with_config(bgh_server::factory(), |c| {
        c.rate_limits.enabled = true;
        c.rate_limits.authenticated_per_hour = 5;
        c.rate_limits.unauthenticated_per_hour = 3;
    })
    .await;
    let ada = app.create_user("ada").await;
    for i in 0..3 {
        let res = app.get("/api/v3/users/ada").send().await;
        res.assert_status(200);
        assert_eq!(res.header("x-ratelimit-limit"), Some("3"));
        assert_eq!(
            res.header("x-ratelimit-remaining"),
            Some((2 - i).to_string().as_str())
        );
        assert!(
            res.header("x-ratelimit-reset")
                .unwrap()
                .parse::<i64>()
                .unwrap()
                > chrono::Utc::now().timestamp()
        );
    }
    let res = app.get("/api/v3/users/ada").send().await;
    res.assert_status(403);
    assert!(
        res.json()["message"]
            .as_str()
            .unwrap()
            .starts_with("API rate limit exceeded")
    );
    assert_eq!(res.header("x-ratelimit-remaining"), Some("0"));
    assert!(res.header("retry-after").is_some());

    // Authenticated callers have their own bucket.
    for _ in 0..5 {
        app.get("/api/v3/user")
            .auth(&ada)
            .send()
            .await
            .assert_status(200);
    }
    let res = app.get("/api/v3/user").auth(&ada).send().await;
    res.assert_status(403);
    assert!(
        res.json()["message"]
            .as_str()
            .unwrap()
            .contains(&format!("user ID {}", ada.id))
    );
    // /rate_limit still answers.
    let v = app.get("/api/v3/rate_limit").auth(&ada).send().await.json();
    assert_eq!(v["rate"]["remaining"], 0);
    assert_eq!(v["rate"]["used"], 5);

    // Not enforced (the default): still counted and reported.
    let off = TestApp::spawn_with_config(bgh_server::factory(), |c| {
        c.rate_limits.unauthenticated_per_hour = 2;
    })
    .await;
    for remaining in ["1", "0", "0"] {
        let res = off.get("/api/v3/users/nobody").send().await;
        res.assert_status(404);
        assert_eq!(res.header("x-ratelimit-limit"), Some("2"));
        assert_eq!(res.header("x-ratelimit-remaining"), Some(remaining));
    }
    let v = off.get("/api/v3/rate_limit").send().await.json();
    assert_eq!(v["resources"]["core"]["used"], 2, "capped at the limit");
    assert_eq!(v["resources"]["core"]["remaining"], 0);
    assert_eq!(v["resources"]["search"]["limit"], 10);
}
