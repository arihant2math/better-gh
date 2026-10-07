//! The site sign-up policy (`closed`, `invite`, `allowed_email_domains`) is
//! enforced on both self-service sign-up routes (#317). OIDC and SAML
//! just-in-time provisioning are covered in `sso_avatars_ratelimit.rs` and
//! `saml.rs`.

use bgh_core::testing::{TestApp, TestResponse, TestUser};
use serde_json::{Value, json};

const ROUTES: [&str; 2] = ["/_bgh/signup", "/_bgh/auth/signup"];

pub async fn set_signup(app: &TestApp, admin: &TestUser, signup: Value) {
    app.patch("/_bgh/admin/settings")
        .auth(admin)
        .json(&json!({ "signup": signup }))
        .send()
        .await
        .assert_status(200);
}

async fn signup(app: &TestApp, path: &str, login: &str, email: &str) -> TestResponse {
    app.post(path)
        .json(&json!({"login": login, "email": email, "password": "s3cret-password"}))
        .send()
        .await
}

fn assert_refused(res: &TestResponse, message: &str) {
    res.assert_status(403);
    assert_eq!(res.json()["message"], message);
}

async fn user_exists(app: &TestApp, login: &str) -> bool {
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM users WHERE login = $1)")
        .bind(login)
        .fetch_one(&app.state.db)
        .await
        .unwrap()
}

#[tokio::test]
async fn closed_policy_refuses_self_service_signup() {
    let app = bgh_server::test_app().await;
    let admin = app.create_admin("root").await;
    set_signup(&app, &admin, json!({"policy": "closed"})).await;
    for (i, path) in ROUTES.into_iter().enumerate() {
        let login = format!("closed{i}");
        let res = signup(&app, path, &login, &format!("{login}@example.com")).await;
        assert_refused(&res, "Sign up is disabled on this instance.");
        assert!(res.header("set-cookie").is_none(), "{path}");
        assert!(!user_exists(&app, &login).await, "{path}");
    }
}

#[tokio::test]
async fn invite_policy_requires_a_pending_invitation() {
    let app = bgh_server::test_app().await;
    let admin = app.create_admin("root").await;
    app.create_org("acme", &admin).await;
    set_signup(&app, &admin, json!({"policy": "invite"})).await;
    for (i, path) in ROUTES.into_iter().enumerate() {
        let login = format!("stranger{i}");
        let res = signup(&app, path, &login, &format!("{login}@example.com")).await;
        assert_refused(&res, "Sign up on this instance requires an invitation.");
        assert!(!user_exists(&app, &login).await, "{path}");

        let login = format!("invited{i}");
        let email = format!("{login}@example.com");
        app.post("/api/v3/orgs/acme/invitations")
            .auth(&admin)
            .json(&json!({"email": email, "role": "direct_member"}))
            .send()
            .await
            .assert_status(201);
        signup(&app, path, &login, &email.to_uppercase())
            .await
            .assert_status(201);
    }
}

#[tokio::test]
async fn domain_allowlist_limits_self_service_signup() {
    let app = bgh_server::test_app().await;
    let admin = app.create_admin("root").await;
    set_signup(
        &app,
        &admin,
        json!({"allowed_email_domains": ["example.com"]}),
    )
    .await;
    for (i, path) in ROUTES.into_iter().enumerate() {
        let login = format!("eve{i}");
        let res = signup(&app, path, &login, &format!("{login}@evil.test")).await;
        assert_refused(&res, "Sign up is not allowed for this email domain.");
        assert!(!user_exists(&app, &login).await, "{path}");

        let login = format!("bob{i}");
        signup(&app, path, &login, &format!("{login}@EXAMPLE.com"))
            .await
            .assert_status(201);
    }
}

#[tokio::test]
async fn site_admins_still_create_accounts_when_closed() {
    let app = bgh_server::test_app().await;
    let admin = app.create_admin("root").await;
    set_signup(
        &app,
        &admin,
        json!({"policy": "closed", "allowed_email_domains": ["example.com"]}),
    )
    .await;
    app.post("/_bgh/admin/users")
        .auth(&admin)
        .json(&json!({"login": "carol", "email": "carol@elsewhere.test",
                      "password": "s3cret-password"}))
        .send()
        .await
        .assert_status(201);
    assert!(user_exists(&app, "carol").await);
}
