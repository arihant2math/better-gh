//! Web-client boot data, /_bgh/auth/*, CSRF enforcement and boot injection
//! into index.html.

mod common;

use bgh_core::events::Event;
use bgh_core::testing::TestApp;
use common::*;
use serde_json::json;

#[tokio::test]
async fn boot_and_auth_endpoints() {
    let app = bgh_server::test_app().await;

    // Signed out.
    let res = app.get("/_bgh/boot").send().await;
    res.assert_status(200);
    assert_eq!(res.header("cache-control"), Some("no-store"));
    let v = res.json();
    assert_eq!(v["user"], serde_json::Value::Null);
    assert_eq!(v["config"]["siteName"], "Better GitHub");
    assert_eq!(v["config"]["signupEnabled"], true);
    assert!(v["csrf"].as_str().unwrap().len() >= 20);
    assert!(v["ts"].as_str().unwrap().ends_with('Z'));

    // Sign up → 201 boot + cookie.
    let res = app
        .post("/_bgh/auth/signup")
        .json(&json!({"login": "ada", "email": "ada@example.com", "password": "s3cret-password"}))
        .send()
        .await;
    res.assert_status(201);
    let boot = res.json();
    assert_eq!(boot["user"]["login"], "ada");
    assert!(
        boot["user"]["avatarUrl"]
            .as_str()
            .unwrap()
            .contains("/avatars/u/")
    );
    let cookie = cookie_from(&res);
    let csrf = boot["csrf"].as_str().unwrap().to_string();
    assert_eq!(
        app.get("/_bgh/boot")
            .header("cookie", &cookie)
            .send()
            .await
            .json()["csrf"],
        csrf
    );
    app.post("/_bgh/auth/signup")
        .json(&json!({"login": "ada", "email": "x@example.com", "password": "s3cret-password"}))
        .send()
        .await
        .assert_status(422);

    // Logout needs CSRF; emits SessionEnded.
    let mut events = app.state.events.subscribe();
    app.post("/_bgh/auth/logout")
        .header("cookie", &cookie)
        .send()
        .await
        .assert_status(403);
    let res = app
        .post("/_bgh/auth/logout")
        .header("cookie", &cookie)
        .header("x-csrf-token", &csrf)
        .send()
        .await;
    res.assert_status(204);
    assert!(res.header("set-cookie").unwrap().contains("Max-Age=0"));
    let ev = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if let Event::SessionEnded {
                user_id,
                session_id,
            } = &*events.recv().await.unwrap()
            {
                break (*user_id, *session_id);
            }
        }
    })
    .await
    .unwrap();
    assert!(ev.1.is_some());
    assert_eq!(
        app.get("/_bgh/boot")
            .header("cookie", &cookie)
            .send()
            .await
            .json()["user"],
        serde_json::Value::Null
    );

    // Login: 422 on bad credentials, 200 boot + cookie.
    let res = app
        .post("/_bgh/auth/login")
        .json(&json!({"login": "ada", "password": "nope"}))
        .send()
        .await;
    res.assert_status(422);
    assert_eq!(res.json()["message"], "Incorrect username or password.");
    let res = app
        .post("/_bgh/auth/login")
        .json(&json!({"login": "ada@example.com", "password": "s3cret-password"}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["user"]["login"], "ada");
    let c2 = cookie_from(&res);
    app.get("/api/v3/user")
        .cookie(&c2)
        .send()
        .await
        .assert_status(200);
}

#[tokio::test]
async fn login_with_two_factor() {
    let app = bgh_server::test_app().await;
    let ada = app.create_user("ada").await;
    let secret = "JBSWY3DPEHPK3PXP";
    sqlx::query(
        "INSERT INTO user_two_factor (user_id, totp_secret, enabled_at) VALUES ($1, $2, now())",
    )
    .bind(ada.id)
    .bind(secret)
    .execute(&app.state.db)
    .await
    .unwrap();

    let res = app
        .post("/_bgh/auth/login")
        .json(&json!({"login": "ada", "password": ada.password}))
        .send()
        .await;
    res.assert_status(401);
    assert!(res.header("set-cookie").is_none());
    let v = res.json();
    assert_eq!(v["twoFactorRequired"], true);
    let token = v["twoFactorToken"].as_str().unwrap().to_string();

    let res = app
        .post("/_bgh/auth/2fa")
        .json(&json!({"twoFactorToken": token, "code": "000000"}))
        .send()
        .await;
    res.assert_status(422);
    let code = bgh_accounts::totp::code_at(secret, chrono::Utc::now().timestamp()).unwrap();
    let res = app
        .post("/_bgh/auth/2fa")
        .json(&json!({"twoFactorToken": token, "code": code}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["user"]["login"], "ada");
    assert!(res.header("set-cookie").is_some());
    app.post("/_bgh/auth/2fa")
        .json(&json!({"twoFactorToken": token, "code": code}))
        .send()
        .await
        .assert_status(401);
}

#[tokio::test]
async fn csrf_is_enforced_for_cookie_mutations() {
    let app = bgh_server::test_app().await;
    let ada = app.create_user("ada").await;
    let cookie = session(&app, &ada).await;
    let token = cookie.split_once('=').unwrap().1;
    let csrf = bgh_core::auth::csrf_token(token);

    // Missing / wrong token → 403; GET is fine.
    let res = app
        .patch("/api/v3/user")
        .header("cookie", &cookie)
        .json(&json!({"name": "x"}))
        .send()
        .await;
    res.assert_status(403);
    assert!(res.json()["message"].as_str().unwrap().contains("CSRF"));
    app.patch("/api/v3/user")
        .header("cookie", &cookie)
        .header("x-csrf-token", "nope")
        .json(&json!({"name": "x"}))
        .send()
        .await
        .assert_status(403);
    app.get("/api/v3/user")
        .header("cookie", &cookie)
        .send()
        .await
        .assert_status(200);
    app.delete("/_bgh/session")
        .header("cookie", &cookie)
        .send()
        .await
        .assert_status(403);
    app.post("/_bgh/tokens")
        .header("cookie", &cookie)
        .json(&json!({"scopes": []}))
        .send()
        .await
        .assert_status(403);
    // Correct token → OK.
    app.patch("/api/v3/user")
        .header("cookie", &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({"name": "x"}))
        .send()
        .await
        .assert_status(200);
    // Token-authenticated clients don't need it (even with a stray cookie).
    app.patch("/api/v3/user")
        .auth(&ada)
        .header("cookie", &cookie)
        .json(&json!({"name": "y"}))
        .send()
        .await
        .assert_status(200);
    // Login endpoints are exempt (a stale cookie must not block signing in).
    app.post("/_bgh/auth/login")
        .header("cookie", &cookie)
        .json(&json!({"login": "ada", "password": ada.password}))
        .send()
        .await
        .assert_status(200);
}

#[tokio::test]
async fn index_html_gets_boot_data() {
    let web = std::env::temp_dir().join(format!("bgh-web-{}", std::process::id()));
    std::fs::create_dir_all(web.join("assets")).unwrap();
    std::fs::write(
        web.join("index.html"),
        "<!doctype html><head><!--BGH_BOOT--></head><div id=app></div>",
    )
    .unwrap();
    std::fs::write(web.join("assets/app-1.js"), "console.log(1)").unwrap();
    std::fs::write(web.join("robots.txt"), "User-agent: *").unwrap();
    let dir = web.clone();
    let app = TestApp::spawn_with_config(bgh_server::factory(), move |c| c.web_dir = dir).await;
    let ada = app.create_user("ada").await;
    sqlx::query("UPDATE users SET name = '</script><x>' WHERE id = $1")
        .bind(ada.id)
        .execute(&app.state.db)
        .await
        .unwrap();

    let res = app.get("/ada/repo/issues/1").send().await;
    res.assert_status(200);
    assert_eq!(res.header("cache-control"), Some("no-cache, private"));
    let html = res.text();
    assert!(
        html.contains("<script>window.__BGH_BOOT__={\"user\":null"),
        "{html}"
    );
    assert!(!html.contains("<!--BGH_BOOT-->"));

    let cookie = session(&app, &ada).await;
    let html = app.get("/").header("cookie", &cookie).send().await.text();
    let start = html.find("window.__BGH_BOOT__=").unwrap() + "window.__BGH_BOOT__=".len();
    let end = html[start..].find("</script>").unwrap() + start;
    let boot: serde_json::Value = serde_json::from_str(&html[start..end]).unwrap();
    assert_eq!(boot["user"]["login"], "ada");
    assert_eq!(boot["user"]["name"], "</script><x>", "escaped, round-trips");
    assert_eq!(
        boot["csrf"],
        bgh_core::auth::csrf_token(cookie.split_once('=').unwrap().1)
    );
    assert!(
        app.get("/index.html")
            .header("cookie", &cookie)
            .send()
            .await
            .text()
            .contains("\"login\":\"ada\"")
    );

    // Real files are served as-is.
    assert_eq!(app.get("/robots.txt").send().await.text(), "User-agent: *");
    let res = app.get("/assets/app-1.js").send().await;
    assert_eq!(
        res.header("cache-control"),
        Some("public, max-age=31536000, immutable")
    );
    app.get("/assets/missing.js")
        .send()
        .await
        .assert_status(404);
    let _ = std::fs::remove_dir_all(web);
}
