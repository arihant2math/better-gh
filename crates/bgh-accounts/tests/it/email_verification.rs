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

/// #329: an org invitation by email must not be claimable by someone who
/// merely types the invited address at sign-up, and on an invite-only
/// instance that claim must not yield a usable account.
#[tokio::test]
async fn invited_address_claim_cannot_take_over_invitation() {
    let app = bgh_server::test_app().await;
    let admin = app.create_admin("root").await;
    app.create_org("acme", &admin).await;
    app.post("/api/v3/orgs/acme/invitations")
        .auth(&admin)
        .json(&json!({"email": "victim@x.test", "role": "admin"}))
        .send()
        .await
        .assert_status(201);
    let gated = crate::signup_policy::GATED;

    // Invite-only: the pending invitation lets the sign-up through, but the
    // claimed address is unproven, so neither sign-up nor sign-in yields a
    // session, on either sign-up endpoint.
    crate::signup_policy::set_signup(&app, &admin, json!({"policy": "invite"})).await;
    for (path, login, email) in [
        ("/_bgh/signup", "mallory", "VICTIM@x.test"),
        ("/_bgh/auth/signup", "mallory2", "eve@x.test"),
    ] {
        if login == "mallory2" {
            app.post("/api/v3/orgs/acme/invitations")
                .auth(&admin)
                .json(&json!({"email": email}))
                .send()
                .await
                .assert_status(201);
        }
        let res = app
            .post(path)
            .json(&json!({"login": login, "email": email, "password": "s3cret-password"}))
            .send()
            .await;
        res.assert_status(403);
        assert_eq!(res.json()["message"], gated, "{path}");
        assert!(res.header("set-cookie").is_none(), "{path}");
        let res = app
            .post("/_bgh/session")
            .json(&json!({"login": login, "password": "s3cret-password"}))
            .send()
            .await;
        res.assert_status(403);
        assert_eq!(res.json()["message"], gated);
    }

    // Even with a session (open policy), the unverified claim doesn't
    // match the invitation: it can be neither seen nor accepted.
    crate::signup_policy::set_signup(&app, &admin, json!({"policy": "open"})).await;
    let res = app
        .post("/_bgh/session")
        .json(&json!({"login": "mallory", "password": "s3cret-password"}))
        .send()
        .await;
    res.assert_status(200);
    let cookie = cookie_from(&res);
    let pending = app
        .get("/api/v3/user/memberships/orgs?state=pending")
        .cookie(&cookie)
        .send()
        .await;
    pending.assert_status(200);
    assert_eq!(pending.json(), json!([]));
    let res = app
        .patch("/api/v3/user/memberships/orgs/acme")
        .cookie(&cookie)
        .json(&json!({"state": "active"}))
        .send()
        .await;
    assert_ne!(res.status(), 200, "{}", res.text());
    app.get("/api/v3/orgs/acme/members/mallory")
        .auth(&admin)
        .send()
        .await
        .assert_status(404);
}
