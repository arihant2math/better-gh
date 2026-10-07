//! Self-service sign-up starts with an unverified primary email (#138).

use crate::common::*;
use bgh_core::testing::TestApp;
use serde_json::json;

async fn emails(app: &TestApp, cookie: &str) -> serde_json::Value {
    app.get("/api/v3/user/emails")
        .cookie(cookie)
        .send()
        .await
        .json()
}

async fn mails_to(app: &TestApp, to: &str) -> usize {
    mails(app).await.iter().filter(|m| m.to == to).count()
}

#[tokio::test]
async fn signup_email_starts_unverified_until_mailed_token() {
    let app = bgh_server::test_app().await;
    for (path, login) in [("/_bgh/signup", "ada"), ("/_bgh/auth/signup", "bob")] {
        let email = format!("{login}@corp.example");
        let res = app
            .post(path)
            .json(&json!({"login": login, "email": email, "password": "s3cret-password"}))
            .send()
            .await;
        res.assert_status(201);
        let cookie = cookie_from(&res);
        let v = emails(&app, &cookie).await;
        assert_eq!(v[0]["email"], email.as_str());
        assert_eq!(v[0]["primary"], true);
        assert_eq!(v[0]["verified"], false, "{path}");

        // Password resets never go to an unverified address.
        app.post("/_bgh/password_reset")
            .json(&json!({"email": login}))
            .send()
            .await
            .assert_status(202);
        let mail = last_mail_to(&app, &email).await;
        assert!(!mail.text.contains("/password_reset/"), "{}", mail.text);
        assert_eq!(mails_to(&app, &email).await, 1);

        let token = token_after(&mail.text, "token=");
        let res = app
            .post("/_bgh/emails/verify")
            .json(&json!({"token": token}))
            .send()
            .await;
        res.assert_status(200);
        assert_eq!(emails(&app, &cookie).await[0]["verified"], true);

        app.post("/_bgh/password_reset")
            .json(&json!({"email": login}))
            .send()
            .await
            .assert_status(202);
        let mail = last_mail_to(&app, &email).await;
        assert!(mail.text.contains("/password_reset/"), "{}", mail.text);
    }
}

#[tokio::test]
async fn unverified_primary_cannot_be_made_public() {
    let app = bgh_server::test_app().await;
    let res = app
        .post("/_bgh/signup")
        .json(&json!({"login": "mallory", "email": "alice@corp.example",
                      "password": "s3cret-password"}))
        .send()
        .await;
    res.assert_status(201);
    let cookie = cookie_from(&res);
    let public = json!({"visibility": "public"});
    app.patch("/api/v3/user/email/visibility")
        .cookie(&cookie)
        .json(&public)
        .send()
        .await
        .assert_status(422);
    let profile = app.get("/api/v3/users/mallory").send().await.json();
    assert_eq!(profile["email"], json!(null));

    // Once the address is proven it can be published.
    let mail = last_mail_to(&app, "alice@corp.example").await;
    app.post("/_bgh/emails/verify")
        .json(&json!({"token": token_after(&mail.text, "token=")}))
        .send()
        .await
        .assert_status(200);
    app.patch("/api/v3/user/email/visibility")
        .cookie(&cookie)
        .json(&public)
        .send()
        .await
        .assert_status(200);
    let profile = app.get("/api/v3/users/mallory").send().await.json();
    assert_eq!(profile["email"], "alice@corp.example");
}

#[tokio::test]
async fn admin_created_users_are_verified() {
    let app = bgh_server::test_app().await;
    let admin = app.create_admin("root").await;
    app.post("/_bgh/admin/users")
        .auth(&admin)
        .json(&json!({"login": "carol", "email": "carol@corp.example",
                      "password": "s3cret-password"}))
        .send()
        .await
        .assert_status(201);
    let verified: bool =
        sqlx::query_scalar("SELECT verified FROM user_emails WHERE email = 'carol@corp.example'")
            .fetch_one(&app.state.db)
            .await
            .unwrap();
    assert!(verified);
    assert_eq!(mails_to(&app, "carol@corp.example").await, 0);
}
