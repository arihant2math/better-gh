//! P36 account security: WebAuthn security keys and passkeys (with the
//! `webauthn-authenticator-rs` software authenticator), sudo mode, TOTP
//! encryption at rest, org and site 2FA requirements, PAT expiry.

use bgh_core::testing::{TestApp, TestUser};
use serde_json::{Value, json};
use webauthn_authenticator_rs::WebauthnAuthenticator;
use webauthn_authenticator_rs::softpasskey::SoftPasskey;
use webauthn_rs::prelude::*;

use crate::common::{cookie_from, mails, ssh_ed25519};

/// The origin browsers (and the soft authenticator) use for the test app.
fn origin(app: &TestApp) -> Url {
    bgh_accounts::webauthn::relying_party(&app.base_url)
        .unwrap()
        .1
}

/// Enable TOTP for `user` through the API; returns the secret.
async fn enable_totp(app: &TestApp, cookie: &str) -> String {
    let res = app
        .post("/_bgh/user/two_factor/totp")
        .cookie(cookie)
        .send()
        .await;
    res.assert_status(201);
    let secret = res.json()["secret"].as_str().unwrap().to_string();
    let code = bgh_accounts::totp::code_at(&secret, chrono::Utc::now().timestamp()).unwrap();
    app.post("/_bgh/user/two_factor/totp/enable")
        .cookie(cookie)
        .json(&json!({ "code": code }))
        .send()
        .await
        .assert_status(200);
    secret
}

/// Register a credential of `kind` with the soft authenticator.
async fn register(
    app: &TestApp,
    cookie: &str,
    authn: &mut WebauthnAuthenticator<SoftPasskey>,
    kind: &str,
    name: &str,
) -> Value {
    let res = app
        .post("/_bgh/user/webauthn/registrations")
        .cookie(cookie)
        .json(&json!({ "kind": kind }))
        .send()
        .await;
    res.assert_status(201);
    let v = res.json();
    let mut options: CreationChallengeResponse =
        serde_json::from_value(v["options"].clone()).unwrap();
    if kind == "passkey" {
        let sel = options.public_key.authenticator_selection.as_mut().unwrap();
        assert!(sel.require_resident_key, "passkeys are discoverable");
        // The soft authenticator can't store resident keys; the user handle
        // is added to its assertions by hand below.
        sel.require_resident_key = false;
        sel.resident_key = None;
    }
    let credential = authn.do_registration(origin(app), options).unwrap();
    let res = app
        .post(&format!(
            "/_bgh/user/webauthn/registrations/{}",
            v["id"].as_str().unwrap()
        ))
        .cookie(cookie)
        .json(&json!({ "name": name, "credential": credential }))
        .send()
        .await;
    res.assert_status(201);
    res.json()
}

async fn handle_of(app: &TestApp, user: &TestUser) -> Uuid {
    sqlx::query_scalar("SELECT handle FROM user_webauthn_handles WHERE user_id = $1")
        .bind(user.id)
        .fetch_one(&app.state.db)
        .await
        .unwrap()
}

async fn credential_ids(app: &TestApp, user: &TestUser) -> Vec<Vec<u8>> {
    sqlx::query_scalar("SELECT credential_id FROM user_webauthn_credentials WHERE user_id = $1")
        .bind(user.id)
        .fetch_all(&app.state.db)
        .await
        .unwrap()
}

async fn sign_in_password(app: &TestApp, user: &TestUser) -> Value {
    let res = app
        .post("/_bgh/auth/login")
        .json(&json!({"login": user.login, "password": user.password}))
        .send()
        .await;
    res.assert_status(401);
    res.json()
}

#[tokio::test]
async fn security_key_as_second_factor() {
    let app = bgh_server::test_app().await;
    let ada = app.create_user("ada").await;
    let cookie = app.session_cookie(&ada).await;
    let mut authn = WebauthnAuthenticator::new(SoftPasskey::new(true));

    // Security keys need TOTP first.
    let res = app
        .post("/_bgh/user/webauthn/registrations")
        .cookie(&cookie)
        .json(&json!({ "kind": "security_key" }))
        .send()
        .await;
    res.assert_status(422);
    app.post("/_bgh/user/webauthn/registrations")
        .cookie(&cookie)
        .json(&json!({ "kind": "nope" }))
        .send()
        .await
        .assert_status(422);
    // Not with a token.
    app.get("/_bgh/user/webauthn")
        .auth(&ada)
        .send()
        .await
        .assert_status(403);

    enable_totp(&app, &cookie).await;
    let key = register(&app, &cookie, &mut authn, "security_key", "YubiKey").await;
    assert_eq!(key["name"], "YubiKey");
    assert_eq!(key["kind"], "security_key");
    assert!(key["id"].is_i64());
    assert!(key["created_at"].is_string());
    assert!(key["last_used_at"].is_null());

    let list = app.get("/_bgh/user/webauthn").cookie(&cookie).send().await;
    list.assert_status(200);
    assert_eq!(list.json().as_array().unwrap().len(), 1);
    let status = app
        .get("/_bgh/user/two_factor")
        .cookie(&cookie)
        .send()
        .await
        .json();
    assert_eq!(status["security_keys"], 1);
    assert_eq!(status["passkeys"], 0);

    // Password sign-in offers the key as second factor.
    let pending = sign_in_password(&app, &ada).await;
    assert_eq!(
        pending["twoFactorMethods"],
        json!(["totp", "recovery_code", "webauthn"])
    );
    let token = pending["twoFactorToken"].as_str().unwrap();
    let res = app
        .post("/_bgh/auth/2fa/webauthn/challenge")
        .json(&json!({ "twoFactorToken": token }))
        .send()
        .await;
    res.assert_status(200);
    let ch = res.json();
    let options: RequestChallengeResponse = serde_json::from_value(ch["options"].clone()).unwrap();
    assert_eq!(options.public_key.allow_credentials.len(), 1);
    let assertion = authn.do_authentication(origin(&app), options).unwrap();
    let res = app
        .post("/_bgh/auth/2fa/webauthn")
        .json(&json!({ "twoFactorToken": token, "id": ch["id"], "credential": assertion }))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["user"]["login"], "ada");
    let fresh = cookie_from(&res);
    app.get("/api/v3/user")
        .cookie(&fresh)
        .send()
        .await
        .assert_status(200);
    // The ceremony and the pending login are single use.
    app.post("/_bgh/auth/2fa/webauthn")
        .json(&json!({ "twoFactorToken": token, "id": ch["id"], "credential": assertion }))
        .send()
        .await
        .assert_status(401);

    // A ceremony of another pending login can't be replayed.
    let pending = sign_in_password(&app, &ada).await;
    let token2 = pending["twoFactorToken"].as_str().unwrap();
    let res = app
        .post("/_bgh/auth/2fa/webauthn")
        .json(&json!({ "twoFactorToken": token2, "id": ch["id"], "credential": assertion }))
        .send()
        .await;
    res.assert_status(422);

    let used = app
        .get("/_bgh/user/webauthn")
        .cookie(&cookie)
        .send()
        .await
        .json();
    assert!(used[0]["last_used_at"].is_string());

    // Rename and delete.
    let id = key["id"].as_i64().unwrap();
    let res = app
        .patch(&format!("/_bgh/user/webauthn/{id}"))
        .cookie(&cookie)
        .json(&json!({ "name": "Backup key" }))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["name"], "Backup key");
    app.patch(&format!("/_bgh/user/webauthn/{id}"))
        .cookie(&cookie)
        .json(&json!({ "name": "" }))
        .send()
        .await
        .assert_status(422);
    app.delete(&format!("/_bgh/user/webauthn/{id}"))
        .cookie(&cookie)
        .send()
        .await
        .assert_status(204);
    app.delete(&format!("/_bgh/user/webauthn/{id}"))
        .cookie(&cookie)
        .send()
        .await
        .assert_status(404);
    let pending = sign_in_password(&app, &ada).await;
    assert_eq!(
        pending["twoFactorMethods"],
        json!(["totp", "recovery_code"])
    );
}

#[tokio::test]
async fn passkey_passwordless_sign_in() {
    let app = bgh_server::test_app().await;
    let ada = app.create_user("ada").await;
    let cookie = app.session_cookie(&ada).await;
    let mut authn = WebauthnAuthenticator::new(SoftPasskey::new(true));
    // Passkeys don't need TOTP.
    let key = register(&app, &cookie, &mut authn, "passkey", "Laptop").await;
    assert_eq!(key["kind"], "passkey");

    let res = app.post("/_bgh/auth/login/passkey/challenge").send().await;
    res.assert_status(200);
    let ch = res.json();
    assert!(ch["options"]["publicKey"]["challenge"].is_string());
    let mut options: RequestChallengeResponse =
        serde_json::from_value(ch["options"].clone()).unwrap();
    assert!(
        options.public_key.allow_credentials.is_empty(),
        "discoverable"
    );
    // Stand in for the resident key: name the credential and return the
    // user handle like a platform authenticator would.
    let cred_id = credential_ids(&app, &ada).await.remove(0);
    options.public_key.allow_credentials = vec![webauthn_rs_proto::AllowCredentials {
        type_: "public-key".into(),
        id: cred_id.into(),
        transports: None,
    }];
    let mut assertion = authn.do_authentication(origin(&app), options).unwrap();
    let handle = handle_of(&app, &ada).await;
    assertion.response.user_handle = Some(handle.as_bytes().to_vec().into());

    let res = app
        .post("/_bgh/auth/login/passkey")
        .json(&json!({ "id": ch["id"], "credential": assertion }))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["user"]["login"], "ada");
    let c = cookie_from(&res);
    app.get("/api/v3/user")
        .cookie(&c)
        .send()
        .await
        .assert_status(200);
    // Single use.
    app.post("/_bgh/auth/login/passkey")
        .json(&json!({ "id": ch["id"], "credential": assertion }))
        .send()
        .await
        .assert_status(422);

    // A user handle nobody has.
    let ch = app
        .post("/_bgh/auth/login/passkey/challenge")
        .send()
        .await
        .json();
    let mut options: RequestChallengeResponse =
        serde_json::from_value(ch["options"].clone()).unwrap();
    options.public_key.allow_credentials = vec![webauthn_rs_proto::AllowCredentials {
        type_: "public-key".into(),
        id: credential_ids(&app, &ada).await.remove(0).into(),
        transports: None,
    }];
    let mut assertion = authn.do_authentication(origin(&app), options).unwrap();
    assertion.response.user_handle = Some(Uuid::new_v4().as_bytes().to_vec().into());
    app.post("/_bgh/auth/login/passkey")
        .json(&json!({ "id": ch["id"], "credential": assertion }))
        .send()
        .await
        .assert_status(422);

    // Disabling TOTP keeps passkeys.
    let audit: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE action = 'passkey.register'")
            .fetch_one(&app.state.db)
            .await
            .unwrap();
    assert_eq!(audit, 1);
}

/// Put a session out of sudo mode.
async fn expire_sudo(app: &TestApp, user: &TestUser) {
    sqlx::query("UPDATE sessions SET sudo_at = now() - interval '3 hours' WHERE user_id = $1")
        .bind(user.id)
        .execute(&app.state.db)
        .await
        .unwrap();
}

#[tokio::test]
async fn sudo_mode_guards_sensitive_actions() {
    let app = bgh_server::test_app().await;
    let ada = app.create_user("ada").await;
    app.create_repo(&ada, "doomed").await;
    let cookie = app.session_cookie(&ada).await;

    // Fresh sessions start in sudo mode.
    let s = app.get("/_bgh/sudo").cookie(&cookie).send().await;
    s.assert_status(200);
    let v = s.json();
    assert_eq!(v["active"], true);
    assert!(v["expires_at"].is_string());
    assert_eq!(
        v["methods"],
        json!({"password": true, "totp": false, "webauthn": false})
    );

    expire_sudo(&app, &ada).await;
    let v = app.get("/_bgh/sudo").cookie(&cookie).send().await.json();
    assert_eq!(v["active"], false);
    assert!(v["expires_at"].is_null());

    // PATs, SSH keys, emails, repo deletion: refused without sudo.
    let res = app
        .post("/_bgh/tokens")
        .cookie(&cookie)
        .json(&json!({ "name": "ci", "scopes": ["repo"] }))
        .send()
        .await;
    res.assert_status(401);
    assert_eq!(res.json()["message"], bgh_core::sudo::SUDO_REQUIRED);
    app.post("/api/v3/user/keys")
        .cookie(&cookie)
        .json(&json!({ "title": "k", "key": ssh_ed25519(1) }))
        .send()
        .await
        .assert_status(401);
    app.post("/api/v3/user/emails")
        .cookie(&cookie)
        .json(&json!({ "emails": ["new@example.com"] }))
        .send()
        .await
        .assert_status(401);
    app.delete("/api/v3/repos/ada/doomed")
        .cookie(&cookie)
        .send()
        .await
        .assert_status(401);
    app.post("/_bgh/applications")
        .cookie(&cookie)
        .json(&json!({"name": "x", "homepage_url": "http://x", "callback_url": "http://x/cb"}))
        .send()
        .await
        .assert_status(401);
    // Tokens are not subject to sudo mode.
    app.post("/api/v3/user/keys")
        .auth(&ada)
        .json(&json!({ "title": "k", "key": ssh_ed25519(2) }))
        .send()
        .await
        .assert_status(201);

    // Re-authenticate.
    app.post("/_bgh/sudo")
        .cookie(&cookie)
        .json(&json!({ "password": "wrong-password" }))
        .send()
        .await
        .assert_status(403);
    app.post("/_bgh/sudo")
        .cookie(&cookie)
        .json(&json!({}))
        .send()
        .await
        .assert_status(422);
    // Not with a token.
    app.post("/_bgh/sudo")
        .auth(&ada)
        .json(&json!({ "password": ada.password }))
        .send()
        .await
        .assert_status(403);
    let res = app
        .post("/_bgh/sudo")
        .cookie(&cookie)
        .json(&json!({ "password": ada.password }))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["active"], true);
    app.post("/_bgh/tokens")
        .cookie(&cookie)
        .json(&json!({ "name": "ci", "scopes": ["repo"] }))
        .send()
        .await
        .assert_status(201);
    app.delete("/api/v3/repos/ada/doomed")
        .cookie(&cookie)
        .send()
        .await
        .assert_status(204);
    let audit: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE action = 'user.sudo'")
            .fetch_one(&app.state.db)
            .await
            .unwrap();
    assert_eq!(audit, 1);
}

#[tokio::test]
async fn sudo_with_totp_and_security_key() {
    let app = bgh_server::test_app().await;
    let ada = app.create_user("ada").await;
    let cookie = app.session_cookie(&ada).await;
    let mut authn = WebauthnAuthenticator::new(SoftPasskey::new(true));
    enable_totp(&app, &cookie).await;
    register(&app, &cookie, &mut authn, "security_key", "Key").await;
    expire_sudo(&app, &ada).await;
    app.post("/_bgh/sudo")
        .cookie(&cookie)
        .json(&json!({ "otp": "000000" }))
        .send()
        .await
        .assert_status(403);

    let res = app
        .post("/_bgh/sudo/webauthn/challenge")
        .cookie(&cookie)
        .send()
        .await;
    res.assert_status(200);
    let ch = res.json();
    let options: RequestChallengeResponse = serde_json::from_value(ch["options"].clone()).unwrap();
    let assertion = authn.do_authentication(origin(&app), options).unwrap();
    let res = app
        .post("/_bgh/sudo")
        .cookie(&cookie)
        .json(&json!({ "webauthn": { "id": ch["id"], "credential": assertion } }))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["methods"]["webauthn"], true);
    assert_eq!(res.json()["methods"]["totp"], true);
}

#[tokio::test]
async fn totp_secrets_are_encrypted_at_rest() {
    let app = bgh_server::test_app().await;
    let ada = app.create_user("ada").await;
    let cookie = app.session_cookie(&ada).await;
    let secret = enable_totp(&app, &cookie).await;
    let (plain, sealed): (Option<String>, Option<Vec<u8>>) = sqlx::query_as(
        "SELECT totp_secret, totp_secret_enc FROM user_two_factor WHERE user_id = $1",
    )
    .bind(ada.id)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert!(plain.is_none());
    let sealed = sealed.unwrap();
    assert!(!String::from_utf8_lossy(&sealed).contains(&secret));
    assert_eq!(
        bgh_core::secretbox::open(&app.state, &sealed).unwrap(),
        secret
    );

    // Rows written by older versions are encrypted by the start-up pass…
    let bob = app.create_user("bob").await;
    let legacy = "JBSWY3DPEHPK3PXP";
    sqlx::query(
        "INSERT INTO user_two_factor (user_id, totp_secret, enabled_at) VALUES ($1, $2, now())",
    )
    .bind(bob.id)
    .bind(legacy)
    .execute(&app.state.db)
    .await
    .unwrap();
    assert_eq!(
        bgh_accounts::security::encrypt_legacy_totp(&app.state)
            .await
            .unwrap(),
        1
    );
    let plaintext: i64 =
        sqlx::query_scalar("SELECT count(*) FROM user_two_factor WHERE totp_secret IS NOT NULL")
            .fetch_one(&app.state.db)
            .await
            .unwrap();
    assert_eq!(plaintext, 0, "no plaintext TOTP secrets remain");
    // …and still verify.
    let pending = sign_in_password(&app, &bob).await;
    let code = bgh_accounts::totp::code_at(legacy, chrono::Utc::now().timestamp()).unwrap();
    app.post("/_bgh/auth/2fa")
        .json(&json!({ "twoFactorToken": pending["twoFactorToken"], "code": code }))
        .send()
        .await
        .assert_status(200);

    // …or on first use, when the pass hasn't run yet.
    let cy = app.create_user("cy").await;
    sqlx::query(
        "INSERT INTO user_two_factor (user_id, totp_secret, enabled_at) VALUES ($1, $2, now())",
    )
    .bind(cy.id)
    .bind(legacy)
    .execute(&app.state.db)
    .await
    .unwrap();
    let pending = sign_in_password(&app, &cy).await;
    app.post("/_bgh/auth/2fa")
        .json(&json!({ "twoFactorToken": pending["twoFactorToken"], "code": code }))
        .send()
        .await
        .assert_status(200);
    let plain: Option<String> =
        sqlx::query_scalar("SELECT totp_secret FROM user_two_factor WHERE user_id = $1")
            .bind(cy.id)
            .fetch_one(&app.state.db)
            .await
            .unwrap();
    assert!(plain.is_none());
}

#[tokio::test]
async fn org_two_factor_requirement() {
    let app = bgh_server::test_app().await;
    let owner = app.create_user("owner").await;
    let compliant = app.create_user("compliant").await;
    let lax = app.create_user("lax").await;
    let outside = app.create_user("outside").await;
    let org = app.create_org("acme", &owner).await;
    app.add_org_member(&org, &compliant, "member").await;
    app.add_org_member(&org, &lax, "member").await;
    app.create_repo_with(
        &owner,
        Some("acme"),
        json!({"name": "widgets", "private": true}),
    )
    .await;
    let team = app
        .post("/api/v3/orgs/acme/teams")
        .auth(&owner)
        .json(&json!({ "name": "devs" }))
        .send()
        .await;
    team.assert_status(201);
    app.put("/api/v3/orgs/acme/teams/devs/memberships/lax")
        .auth(&owner)
        .json(&json!({}))
        .send()
        .await
        .assert_status(200);
    sqlx::query(
        "INSERT INTO collaborators (repo_id, user_id, permission)
         SELECT id, $1, 'write' FROM repositories WHERE name = 'widgets'",
    )
    .bind(outside.id)
    .execute(&app.state.db)
    .await
    .unwrap();

    // The owner needs 2FA first.
    let res = app
        .patch("/api/v3/orgs/acme")
        .auth(&owner)
        .json(&json!({ "two_factor_requirement_enabled": true }))
        .send()
        .await;
    res.assert_status(422);
    assert_eq!(
        res.json()["errors"][0]["field"],
        "two_factor_requirement_enabled"
    );

    for u in [&owner, &compliant] {
        let c = app.session_cookie(u).await;
        enable_totp(&app, &c).await;
    }
    let res = app
        .patch("/api/v3/orgs/acme")
        .auth(&owner)
        .json(&json!({ "two_factor_requirement_enabled": true }))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["two_factor_requirement_enabled"], true);

    // Non-compliant member and outside collaborator are gone.
    app.get("/api/v3/orgs/acme/members/lax")
        .auth(&owner)
        .send()
        .await
        .assert_status(404);
    app.get("/api/v3/orgs/acme/members/compliant")
        .auth(&owner)
        .send()
        .await
        .assert_status(204);
    let collab: i64 = sqlx::query_scalar("SELECT count(*) FROM collaborators WHERE user_id = $1")
        .bind(outside.id)
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(collab, 0);
    let removals: Vec<(String, String, Vec<i64>)> = sqlx::query_as(
        "SELECT u.login, r.kind, r.team_ids FROM org_two_factor_removals r
           JOIN users u ON u.id = r.user_id ORDER BY u.login",
    )
    .fetch_all(&app.state.db)
    .await
    .unwrap();
    let team_id = team.json()["id"].as_i64().unwrap();
    assert_eq!(
        removals,
        vec![
            ("lax".into(), "member".into(), vec![team_id]),
            ("outside".into(), "outside_collaborator".into(), vec![]),
        ]
    );
    let sent = mails(&app).await;
    for to in ["lax@example.com", "outside@example.com"] {
        assert!(
            sent.iter()
                .any(|m| m.to == to && m.subject.contains("removed from the @acme")),
            "{to} notified"
        );
    }

    // Invitations to accounts without 2FA are refused.
    let res = app
        .post("/api/v3/orgs/acme/invitations")
        .auth(&owner)
        .json(&json!({ "invitee_id": lax.id }))
        .send()
        .await;
    res.assert_status(422);

    // Once lax enables 2FA and rejoins, the team comes back.
    let lax_cookie = app.session_cookie(&lax).await;
    enable_totp(&app, &lax_cookie).await;
    app.post("/api/v3/orgs/acme/invitations")
        .auth(&owner)
        .json(&json!({ "invitee_id": lax.id }))
        .send()
        .await
        .assert_status(201);
    app.patch("/api/v3/user/memberships/orgs/acme")
        .auth(&lax)
        .json(&json!({ "state": "active" }))
        .send()
        .await
        .assert_status(200);
    app.get("/api/v3/orgs/acme/teams/devs/memberships/lax")
        .auth(&owner)
        .send()
        .await
        .assert_status(200);

    // Members of a 2FA org can't turn it off.
    let res = app
        .delete("/_bgh/user/two_factor")
        .cookie(&lax_cookie)
        .json(&json!({ "password": lax.password }))
        .send()
        .await;
    res.assert_status(422);
}

#[tokio::test]
async fn token_expiry_reminders_and_header() {
    let app = bgh_server::test_app().await;
    let ada = app.create_user("ada").await;
    let cookie = app.session_cookie(&ada).await;
    let res = app
        .post("/_bgh/tokens")
        .cookie(&cookie)
        .json(&json!({ "name": "deploy", "scopes": ["repo"], "expires_in_days": 5 }))
        .send()
        .await;
    res.assert_status(201);
    let token = res.json()["token"].as_str().unwrap().to_string();
    let expires_at = res.json()["expires_at"].as_str().unwrap().to_string();

    // The expiry header on API responses (GitHub's format).
    let res = app.get("/api/v3/user").token(&token).send().await;
    res.assert_status(200);
    let header = res
        .header("github-authentication-token-expiration")
        .unwrap();
    let at = chrono::DateTime::parse_from_rfc3339(&expires_at).unwrap();
    assert_eq!(header, at.format("%Y-%m-%d %H:%M:%S UTC").to_string());
    // Tokens without expiry and sessions have none.
    let res = app.get("/api/v3/user").auth(&ada).send().await;
    assert!(
        res.header("github-authentication-token-expiration")
            .is_none()
    );

    // 7-day reminder, once.
    assert_eq!(
        bgh_accounts::security::send_expiry_reminders(&app.state)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        bgh_accounts::security::send_expiry_reminders(&app.state)
            .await
            .unwrap(),
        0
    );
    let sent = mails(&app).await;
    let mail = sent
        .iter()
        .find(|m| m.subject.contains("\"deploy\" is about to expire"))
        .expect("expiry mail");
    assert_eq!(mail.to, "ada@example.com");
    assert!(mail.text.contains("/settings/tokens"));

    // 1-day reminder when it gets close.
    sqlx::query(
        "UPDATE access_tokens SET expires_at = now() + interval '20 hours' WHERE name = 'deploy'",
    )
    .execute(&app.state.db)
    .await
    .unwrap();
    assert_eq!(
        bgh_accounts::security::send_expiry_reminders(&app.state)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        bgh_accounts::security::send_expiry_reminders(&app.state)
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn site_two_factor_requirement() {
    let app = bgh_server::test_app().await;
    let admin = app.create_admin("root").await;
    let ada = app.create_user("ada").await;
    let res = app
        .patch("/_bgh/admin/settings")
        .auth(&admin)
        .json(&json!({ "auth_providers": { "require_2fa": true } }))
        .send()
        .await;
    res.assert_status(422);
    let admin_cookie = app.session_cookie(&admin).await;
    enable_totp(&app, &admin_cookie).await;
    app.patch("/_bgh/admin/settings")
        .auth(&admin)
        .json(&json!({ "auth_providers": { "require_2fa": true } }))
        .send()
        .await
        .assert_status(200);
    bgh_core::settings::invalidate(&app.state);

    let cookie = app.session_cookie(&ada).await;
    let boot = app.get("/_bgh/boot").cookie(&cookie).send().await.json();
    assert_eq!(boot["user"]["twoFactorSetupRequired"], true);
    let admin_boot = app
        .get("/_bgh/boot")
        .cookie(&admin_cookie)
        .send()
        .await
        .json();
    assert!(admin_boot["user"].get("twoFactorSetupRequired").is_none());
    // Sessions are limited to 2FA setup…
    app.get("/api/v3/user/repos")
        .cookie(&cookie)
        .send()
        .await
        .assert_status(403);
    app.post("/_bgh/tokens")
        .cookie(&cookie)
        .json(&json!({ "name": "x", "scopes": [] }))
        .send()
        .await
        .assert_status(403);
    app.get("/api/v3/user")
        .cookie(&cookie)
        .send()
        .await
        .assert_status(200);
    // …tokens are not.
    app.get("/api/v3/user/repos")
        .auth(&ada)
        .send()
        .await
        .assert_status(200);
    let status = app
        .get("/_bgh/user/two_factor")
        .cookie(&cookie)
        .send()
        .await
        .json();
    assert_eq!(status["required_by_site"], true);

    enable_totp(&app, &cookie).await;
    app.get("/api/v3/user/repos")
        .cookie(&cookie)
        .send()
        .await
        .assert_status(200);
    let boot = app.get("/_bgh/boot").cookie(&cookie).send().await.json();
    assert!(boot["user"].get("twoFactorSetupRequired").is_none());
    // And it can't be turned off while required.
    app.delete("/_bgh/user/two_factor")
        .cookie(&cookie)
        .json(&json!({ "password": ada.password }))
        .send()
        .await
        .assert_status(422);
}
