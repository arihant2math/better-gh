//! Site settings and their enforcement: sign-up policy, default
//! visibility, maintenance mode, announcement, rate limits, quotas.

use bgh_core::testing::{TestApp, TestUser};
use serde_json::{Value, json};

async fn patch_settings(app: &TestApp, admin: &TestUser, body: Value) -> Value {
    let res = app
        .patch("/_bgh/admin/settings")
        .auth(admin)
        .json(&body)
        .send()
        .await;
    res.assert_status(200);
    res.json()
}

#[tokio::test]
async fn reads_and_updates_settings() {
    let app = bgh_server::test_app().await;
    let admin = app.create_admin("root").await;
    let alice = app.create_user("alice").await;

    app.get("/_bgh/admin/settings")
        .auth(&alice)
        .send()
        .await
        .assert_status(403);
    let res = app.get("/_bgh/admin/settings").auth(&admin).send().await;
    res.assert_status(200);
    let s = res.json();
    assert_eq!(s["signup"]["policy"], "open");
    assert_eq!(s["repositories"]["default_visibility"], "public");
    assert_eq!(s["organizations"]["creation"], "all");
    assert_eq!(s["rate_limits"]["enabled"], false);
    assert_eq!(s["auth_providers"]["password_login"], true);
    assert_eq!(s["smtp"]["port"], 587);
    assert_eq!(s["maintenance"]["enabled"], false);

    // Partial merge; secrets are write-only.
    let s = patch_settings(
        &app,
        &admin,
        json!({
            "smtp": {"enabled": true, "host": "smtp.example.com", "from": "noreply@example.com",
                     "password": "hunter2", "username": "mailer"},
            "auth_providers": {"oidc": [{"name": "corp", "issuer": "https://id.example.com",
                                         "client_id": "abc", "client_secret": "xyz"}]},
        }),
    )
    .await;
    assert_eq!(s["smtp"]["password"], "********");
    assert_eq!(s["smtp"]["host"], "smtp.example.com");
    assert_eq!(s["smtp"]["port"], 587);
    assert_eq!(s["auth_providers"]["oidc"][0]["client_secret"], "********");
    assert_eq!(s["auth_providers"]["password_login"], true);
    let stored: Value = sqlx::query_scalar("SELECT value FROM site_settings WHERE key = 'smtp'")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(stored["password"], "hunter2");

    // Sending the placeholder back keeps the secret; other fields update.
    let mut providers = s["auth_providers"].clone();
    providers["oidc"][0]["display_name"] = json!("Corp SSO");
    patch_settings(
        &app,
        &admin,
        json!({"smtp": {"password": "********", "port": 465, "tls": "tls"}, "auth_providers": providers}),
    )
    .await;
    let typed = bgh_core::settings::load_uncached(&app.state.config, &app.state.db)
        .await
        .unwrap();
    assert_eq!(typed.smtp.password.as_deref(), Some("hunter2"));
    assert_eq!(typed.smtp.port, 465);
    assert_eq!(
        typed.auth_providers.oidc[0].client_secret.as_deref(),
        Some("xyz")
    );
    assert_eq!(
        typed.auth_providers.oidc[0].display_name.as_deref(),
        Some("Corp SSO")
    );

    // Validation failures change nothing.
    for body in [
        json!({"bogus": {}}),
        json!({"signup": {"policy": "sometimes"}}),
        json!({"repositories": {"default_visibility": "secret"}}),
        json!({"repositories": {"max_repo_size_mb": 0}}),
        json!({"smtp": {"tls": "maybe"}}),
        json!({"auth_providers": {"password_login": false, "oidc": []}}),
        json!({"signup": {"allowed_email_domains": ["a@b.com"]}}),
        json!({"signup": "open"}),
    ] {
        app.patch("/_bgh/admin/settings")
            .auth(&admin)
            .json(&body)
            .send()
            .await
            .assert_status(422);
    }
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE action = 'business.update_settings'",
    )
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(n, 2);
    // Secrets never reach the audit log.
    let logged: Vec<Value> =
        sqlx::query_scalar("SELECT data FROM audit_log WHERE action = 'business.update_settings'")
            .fetch_all(&app.state.db)
            .await
            .unwrap();
    assert!(!logged.iter().any(|d| d.to_string().contains("hunter2")));

    // Public site info exposes providers without secrets.
    let site = app.get("/_bgh/site").send().await.json();
    assert_eq!(
        site["oidc_providers"],
        json!([{"name": "corp", "display_name": "Corp SSO"}])
    );
    assert!(!site.to_string().contains("xyz"));
}

const GATED: &str = "Confirm your email address before signing in: open the link we emailed you.";

#[tokio::test]
async fn enforces_signup_policy() {
    let app = bgh_server::test_app().await;
    let admin = app.create_admin("root").await;
    let signup = |login: &str, email: &str| {
        app.post("/_bgh/signup").json(&json!({
            "login": login, "email": email, "password": "long-enough-pw"
        }))
    };

    patch_settings(
        &app,
        &admin,
        json!({"signup": {"allowed_email_domains": ["example.com"]}}),
    )
    .await;
    signup("eve", "eve@evil.test")
        .send()
        .await
        .assert_status(403);
    // An allowed address passes the policy, but the account can't sign in
    // until the address is proven by mail.
    let res = signup("bob", "bob@EXAMPLE.com").send().await;
    res.assert_status(403);
    assert_eq!(res.json()["message"], GATED);

    patch_settings(&app, &admin, json!({"signup": {"policy": "closed"}})).await;
    let res = signup("carol", "carol@example.com").send().await;
    res.assert_status(403);
    assert_eq!(
        res.json()["message"],
        "Sign up is disabled on this instance."
    );

    patch_settings(&app, &admin, json!({"signup": {"policy": "invite"}})).await;
    signup("dave", "dave@example.com")
        .send()
        .await
        .assert_status(403);
    let org = app.create_org("acme", &admin).await;
    sqlx::query("INSERT INTO org_invitations (org_id, email, inviter_id) VALUES ($1, 'dave@example.com', $2)")
        .bind(org.id)
        .bind(admin.id)
        .execute(&app.state.db)
        .await
        .unwrap();
    let res = signup("dave", "dave@example.com").send().await;
    res.assert_status(403);
    assert_eq!(res.json()["message"], GATED);

    // Admins can still create users when sign-up is closed.
    app.post("/api/v3/admin/users")
        .auth(&admin)
        .json(&json!({"login": "frank"}))
        .send()
        .await
        .assert_status(201);
}

#[tokio::test]
async fn default_repository_visibility() {
    let app = bgh_server::test_app().await;
    let admin = app.create_admin("root").await;
    let alice = app.create_user("alice").await;
    app.create_org("acme", &alice).await;

    patch_settings(
        &app,
        &admin,
        json!({"repositories": {"default_visibility": "internal"}}),
    )
    .await;
    let r = app
        .create_repo_with(&alice, None, json!({"name": "mine"}))
        .await;
    assert_eq!(r["visibility"], "private");
    let r = app
        .create_repo_with(&alice, Some("acme"), json!({"name": "shared"}))
        .await;
    assert_eq!(r["visibility"], "internal");
    // Explicit choices win.
    let r = app
        .create_repo_with(&alice, None, json!({"name": "pub", "private": false}))
        .await;
    assert_eq!(r["visibility"], "public");

    let s = bgh_core::settings::load_uncached(&app.state.config, &app.state.db)
        .await
        .unwrap();
    assert!(
        s.can_create_org(
            &bgh_core::models::db::User::find(&app.state.db, alice.id)
                .await
                .unwrap()
                .unwrap()
        )
    );
    patch_settings(
        &app,
        &admin,
        json!({"organizations": {"creation": "admins_only"}}),
    )
    .await;
    let s = bgh_core::settings::load_uncached(&app.state.config, &app.state.db)
        .await
        .unwrap();
    let a = bgh_core::models::db::User::find(&app.state.db, alice.id)
        .await
        .unwrap()
        .unwrap();
    let root = bgh_core::models::db::User::find(&app.state.db, admin.id)
        .await
        .unwrap()
        .unwrap();
    assert!(!s.can_create_org(&a));
    assert!(s.can_create_org(&root));
}

#[tokio::test]
async fn maintenance_mode() {
    let app = bgh_server::test_app().await;
    let admin = app.create_admin("root").await;
    let alice = app.create_user("alice").await;
    app.create_repo(&alice, "web").await;

    patch_settings(
        &app,
        &admin,
        json!({"maintenance": {"enabled": true, "message": "Upgrading to v2"}}),
    )
    .await;

    let res = app.get("/api/v3/user").auth(&alice).send().await;
    res.assert_status(503);
    assert_eq!(res.json()["message"], "Upgrading to v2");
    assert_eq!(res.header("retry-after"), Some("300"));
    app.get("/api/v3/repos/alice/web")
        .send()
        .await
        .assert_status(503);
    app.get("/alice/web.git/info/refs?service=git-upload-pack")
        .send()
        .await
        .assert_status(503);

    // Exempt: health, banner, login; site admins keep working.
    app.get("/healthz").send().await.assert_status(200);
    let site = app.get("/_bgh/site").send().await;
    site.assert_status(200);
    assert_eq!(site.json()["maintenance"]["enabled"], true);
    assert_eq!(site.json()["maintenance"]["message"], "Upgrading to v2");
    app.post("/_bgh/session")
        .json(&json!({"login": "root", "password": admin.password}))
        .send()
        .await
        .assert_status(200);
    // The web client's sign-in endpoints pass the maintenance gate too.
    for path in ["/_bgh/auth/login", "/_bgh/boot"] {
        let res = app.post(path).json(&json!({})).send().await;
        assert_ne!(
            res.status(),
            503,
            "{path} must not be blocked by maintenance"
        );
    }
    app.get("/api/v3/user")
        .auth(&admin)
        .send()
        .await
        .assert_status(200);

    patch_settings(&app, &admin, json!({"maintenance": {"enabled": false}})).await;
    app.get("/api/v3/user")
        .auth(&alice)
        .send()
        .await
        .assert_status(200);
}

#[tokio::test]
async fn announcement_banner() {
    let app = bgh_server::test_app().await;
    let admin = app.create_admin("root").await;

    let res = app
        .get("/api/v3/enterprise/announcement")
        .auth(&admin)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(
        res.json(),
        json!({"announcement": null, "expires_at": null, "user_dismissible": false})
    );

    let res = app
        .patch("/api/v3/enterprise/announcement")
        .auth(&admin)
        .json(&json!({"announcement": "Scheduled downtime Friday", "expires_at": "2999-01-01T00:00:00Z", "user_dismissible": true}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(
        res.json(),
        json!({"announcement": "Scheduled downtime Friday", "expires_at": "2999-01-01T00:00:00Z", "user_dismissible": true})
    );
    let site = app.get("/_bgh/site").send().await.json();
    assert_eq!(site["announcement"]["message"], "Scheduled downtime Friday");
    assert_eq!(site["announcement"]["user_dismissible"], true);

    // Expired announcements are hidden from the banner.
    app.patch("/api/v3/enterprise/announcement")
        .auth(&admin)
        .json(&json!({"announcement": "old", "expires_at": "2000-01-01T00:00:00Z"}))
        .send()
        .await
        .assert_status(200);
    assert_eq!(
        app.get("/_bgh/site").send().await.json()["announcement"],
        json!(null)
    );

    app.patch("/api/v3/enterprise/announcement")
        .auth(&admin)
        .json(&json!({}))
        .send()
        .await
        .assert_status(422);
    app.delete("/api/v3/enterprise/announcement")
        .auth(&admin)
        .send()
        .await
        .assert_status(204);
    let res = app
        .get("/api/v3/enterprise/announcement")
        .auth(&admin)
        .send()
        .await;
    assert_eq!(res.json()["announcement"], json!(null));
}

#[tokio::test]
async fn rate_limits() {
    let app = bgh_server::test_app().await;
    let admin = app.create_admin("root").await;
    let alice = app.create_user("alice").await;

    // Not enforced by default, but budgets are counted and reported.
    let res = app.get("/api/v3/user").auth(&admin).send().await;
    res.assert_status(200);
    assert_eq!(res.header("x-ratelimit-limit"), Some("5000"));
    assert_eq!(res.header("x-ratelimit-used"), Some("1"));
    let res = app.get("/api/v3/rate_limit").auth(&admin).send().await;
    res.assert_status(200);
    let body = res.json();
    assert_eq!(body["resources"]["core"]["limit"], 5000);
    assert_eq!(body["resources"]["core"]["used"], 1);
    assert_eq!(body["resources"]["search"]["limit"], 30);
    assert_eq!(body["resources"]["graphql"]["limit"], 5000);
    assert_eq!(body["rate"], body["resources"]["core"]);
    let s = app
        .get("/_bgh/admin/settings")
        .auth(&admin)
        .send()
        .await
        .json();
    assert_eq!(s["rate_limits"]["authenticated_per_hour"], 5000);

    patch_settings(
        &app,
        &admin,
        json!({"rate_limits": {"enabled": true, "authenticated_per_hour": 3, "unauthenticated_per_hour": 2}}),
    )
    .await;
    for i in 1..=3 {
        let res = app.get("/api/v3/user").auth(&alice).send().await;
        res.assert_status(200);
        assert_eq!(res.header("x-ratelimit-limit"), Some("3"));
        assert_eq!(res.header("x-ratelimit-used"), Some(i.to_string().as_str()));
        assert_eq!(
            res.header("x-ratelimit-remaining"),
            Some((3 - i).to_string().as_str())
        );
        assert_eq!(res.header("x-ratelimit-resource"), Some("core"));
        assert!(res.header("x-ratelimit-reset").is_some());
    }
    let res = app.get("/api/v3/user").auth(&alice).send().await;
    res.assert_status(403);
    assert!(
        res.json()["message"]
            .as_str()
            .unwrap()
            .starts_with("API rate limit exceeded for user ID")
    );
    assert_eq!(res.header("x-ratelimit-remaining"), Some("0"));

    // /rate_limit is not counted.
    let res = app.get("/api/v3/rate_limit").auth(&alice).send().await;
    res.assert_status(200);
    let body = res.json();
    assert_eq!(body["resources"]["core"]["limit"], 3);
    assert_eq!(body["rate"]["used"], 3, "capped at the limit");
    assert_eq!(body["rate"]["remaining"], 0);

    // Separate buckets per user and for anonymous IPs.
    app.get("/api/v3/user")
        .auth(&admin)
        .send()
        .await
        .assert_status(200);
    let anon = |ip: &'static str| app.get("/api/v3/users/alice").header("x-forwarded-for", ip);
    anon("10.0.0.1").send().await.assert_status(200);
    anon("10.0.0.1").send().await.assert_status(200);
    let res = anon("10.0.0.1").send().await;
    res.assert_status(403);
    assert!(res.json()["message"].as_str().unwrap().contains("10.0.0.1"));
    anon("10.0.0.2").send().await.assert_status(200);
    // Web endpoints are not rate limited.
    app.get("/_bgh/site").send().await.assert_status(200);
}

#[tokio::test]
async fn storage_quotas_block_pushes() {
    let app = bgh_server::test_app().await;
    let admin = app.create_admin("root").await;
    let alice = app.create_user("alice").await;
    let repo = app.create_repo(&alice, "big").await;
    sqlx::query("UPDATE repositories SET size = 3072 WHERE id = $1")
        .bind(repo["id"].as_i64().unwrap())
        .execute(&app.state.db)
        .await
        .unwrap();
    let push = || {
        app.post("/alice/big.git/git-receive-pack")
            .basic("alice", &alice.token)
            .header("content-type", "application/x-git-receive-pack-request")
            .body(b"0000".to_vec())
    };
    // No limit: the request reaches git (whatever it answers, not 403).
    assert_ne!(push().send().await.status(), 403);

    // Per-owner quota (2 MB per repo).
    let res = app
        .put("/_bgh/admin/accounts/alice/quota")
        .auth(&admin)
        .json(&json!({"max_repo_size_mb": 2}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["max_repo_size_mb"], 2);
    assert_eq!(res.json()["effective_max_repo_size_mb"], 2);
    assert_eq!(res.json()["used_kb"], 3072);
    let res = push().send().await;
    res.assert_status(403);
    assert!(
        res.json()["message"]
            .as_str()
            .unwrap()
            .contains("size limit")
    );

    // Total quota.
    app.put("/_bgh/admin/accounts/alice/quota")
        .auth(&admin)
        .json(&json!({"max_total_size_mb": 1}))
        .send()
        .await
        .assert_status(200);
    let res = push().send().await;
    res.assert_status(403);
    assert!(
        res.json()["message"]
            .as_str()
            .unwrap()
            .contains("storage quota")
    );

    // Removing the quota falls back to the site default.
    app.delete("/_bgh/admin/accounts/alice/quota")
        .auth(&admin)
        .send()
        .await
        .assert_status(204);
    assert_ne!(push().send().await.status(), 403);
    patch_settings(
        &app,
        &admin,
        json!({"repositories": {"max_repo_size_mb": 1}}),
    )
    .await;
    push().send().await.assert_status(403);
    let res = app
        .get("/_bgh/admin/accounts/alice/quota")
        .auth(&admin)
        .send()
        .await;
    assert_eq!(res.json()["max_repo_size_mb"], json!(null));
    assert_eq!(res.json()["effective_max_repo_size_mb"], 1);

    app.put("/_bgh/admin/accounts/alice/quota")
        .auth(&admin)
        .json(&json!({"max_repo_size_mb": -1}))
        .send()
        .await
        .assert_status(422);
}

#[tokio::test]
async fn privacy_settings_are_validated() {
    let app = bgh_server::test_app().await;
    let admin = app.create_admin("root").await;
    let s = app
        .get("/_bgh/admin/settings")
        .auth(&admin)
        .send()
        .await
        .json();
    assert_eq!(
        s["privacy"],
        json!({
            "private_mode": false,
            "allow_anonymous_directory": true,
            "allowed_visibilities": ["public", "internal", "private"],
        })
    );
    let bad = |body: Value| {
        let app = &app;
        let admin = &admin;
        async move {
            let res = app
                .patch("/_bgh/admin/settings")
                .auth(admin)
                .json(&body)
                .send()
                .await;
            res.assert_status(422);
            res.json()
        }
    };
    // The default visibility (public) must stay allowed.
    let e = bad(json!({"privacy": {"allowed_visibilities": ["private"]}})).await;
    assert_eq!(e["errors"][0]["field"], "repositories.default_visibility");
    bad(json!({"privacy": {"allowed_visibilities": []}})).await;
    bad(json!({"privacy": {"allowed_visibilities": ["secret"]}})).await;
    bad(json!({"privacy": {"private_mode": "yes"}})).await;
    // Both together are fine.
    let s = patch_settings(
        &app,
        &admin,
        json!({
            "repositories": {"default_visibility": "private"},
            "privacy": {"allowed_visibilities": ["private", "internal"], "private_mode": true},
        }),
    )
    .await;
    assert_eq!(
        s["privacy"]["allowed_visibilities"],
        json!(["private", "internal"])
    );
    assert_eq!(s["privacy"]["private_mode"], true);
    // Changing the default to a disallowed one is refused too.
    bad(json!({"repositories": {"default_visibility": "public"}})).await;
    // Private mode is in effect right away (the admin is signed in).
    app.get("/api/v3/meta").send().await.assert_status(200);
    app.get("/api/v3/users").send().await.assert_status(401);
    app.get("/api/v3/users")
        .auth(&admin)
        .send()
        .await
        .assert_status(200);
    let audit: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE action = 'business.update_settings'",
    )
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert!(audit >= 1);
}
