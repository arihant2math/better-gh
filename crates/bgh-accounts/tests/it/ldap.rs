//! LDAP directory auth and sync (against the in-process fake server),
//! `password_login` enforcement and git basic-auth throttling.

use crate::common;

use bgh_accounts::ldap::fake::{FakeLdap, PEOPLE};
use bgh_accounts::ldap::sync;
use bgh_core::testing::{TestApp, TestUser};
use serde_json::{Value, json};

/// Store `auth_providers` fields through the admin settings API.
async fn set_auth(app: &TestApp, admin: &TestUser, fields: Value) {
    app.patch("/_bgh/admin/settings")
        .auth(admin)
        .json(&json!({ "auth_providers": fields }))
        .send()
        .await
        .assert_status(200);
}

async fn enable_ldap(app: &TestApp, admin: &TestUser, ldap: &FakeLdap, extra: Value) {
    let mut cfg = serde_json::to_value(ldap.settings()).unwrap();
    cfg.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    set_auth(app, admin, json!({ "ldap": cfg })).await;
}

async fn web_login(app: &TestApp, login: &str, password: &str) -> bgh_core::testing::TestResponse {
    app.post("/_bgh/session")
        .json(&json!({ "login": login, "password": password }))
        .send()
        .await
}

/// A `TestUser` handle for an account created by LDAP (with a fresh PAT).
async fn handle(app: &TestApp, login: &str, password: &str) -> TestUser {
    let id: i64 = sqlx::query_scalar("SELECT id FROM users WHERE lower(login) = lower($1)")
        .bind(login)
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    let token = bgh_core::auth::create_access_token(
        &app.state.db,
        id,
        "t",
        &["repo", "admin:org", "user", "admin:public_key"].map(String::from),
        None,
    )
    .await
    .unwrap()
    .1;
    TestUser {
        id,
        login: login.into(),
        password: password.into(),
        token,
    }
}

async fn user_row(app: &TestApp, login: &str) -> (bool, bool, Option<String>) {
    sqlx::query_as(
        "SELECT site_admin, suspended_at IS NOT NULL, name FROM users WHERE lower(login) = lower($1)",
    )
    .bind(login)
    .fetch_one(&app.state.db)
    .await
    .unwrap()
}

#[tokio::test]
async fn ldap_login_on_web_and_git_provisions_accounts() {
    let app = bgh_server::test_app().await;
    let admin = app.create_admin("root").await;
    let ldap = FakeLdap::start().await;
    let key = common::ssh_ed25519(3);
    let dn = ldap.add_user(
        "carol",
        "ldap-pw",
        &[
            ("cn", "Carol Danvers"),
            ("mail", "carol@corp.example"),
            ("mail", "cd@corp.example"),
            ("sshPublicKey", &key),
        ],
    );
    enable_ldap(&app, &admin, &ldap, json!({})).await;

    // The stored bind password is redacted and kept when sent back.
    let s = app
        .get("/_bgh/admin/settings")
        .auth(&admin)
        .send()
        .await
        .json();
    assert_eq!(s["auth_providers"]["ldap"]["bind_password"], "********");
    set_auth(&app, &admin, json!({ "ldap": s["auth_providers"]["ldap"] })).await;
    assert_eq!(
        app.get("/_bgh/site").send().await.json()["ldap"],
        json!(true)
    );

    // Wrong password: 401, audited, no account.
    web_login(&app, "carol", "nope").await.assert_status(401);
    assert!(
        sqlx::query_scalar::<_, i64>("SELECT id FROM users WHERE login = 'carol'")
            .fetch_optional(&app.state.db)
            .await
            .unwrap()
            .is_none()
    );

    // First sign-in provisions the account from the directory.
    let res = web_login(&app, "carol", "ldap-pw").await;
    res.assert_status(200);
    assert_eq!(res.json()["login"], "carol");
    assert_eq!(res.json()["name"], "Carol Danvers");
    let carol = handle(&app, "carol", "ldap-pw").await;
    let emails = app
        .get("/api/v3/user/emails")
        .auth(&carol)
        .send()
        .await
        .json();
    let mut addrs: Vec<&str> = emails
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["email"].as_str().unwrap())
        .collect();
    addrs.sort();
    assert_eq!(addrs, ["carol@corp.example", "cd@corp.example"]);
    assert!(
        emails
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["verified"] == true)
    );
    let keys = app
        .get("/api/v3/user/keys")
        .auth(&carol)
        .send()
        .await
        .json();
    assert_eq!(keys.as_array().unwrap().len(), 1);
    let (subject,): (String,) = sqlx::query_as(
        "SELECT subject FROM user_identities WHERE provider = 'ldap' AND user_id = $1",
    )
    .bind(carol.id)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(subject, bgh_accounts::ldap::normalize_dn(&dn));

    // The _bgh/auth web login too (by login).
    app.post("/_bgh/auth/login")
        .json(&json!({ "login": "carol", "password": "ldap-pw" }))
        .send()
        .await
        .assert_status(200);

    // Git over HTTP: basic auth with the directory password.
    app.create_private_repo(&carol, "secret").await;
    let refs = "/carol/secret.git/info/refs?service=git-upload-pack";
    app.get(refs)
        .basic("carol", "ldap-pw")
        .send()
        .await
        .assert_status(200);
    app.get(refs)
        .basic("carol", "bad")
        .send()
        .await
        .assert_status(401);
    // The REST API never takes passwords.
    app.get("/api/v3/user")
        .basic("carol", "ldap-pw")
        .send()
        .await
        .assert_status(401);

    // Directory changes apply at the next sign-in; the key is replaced.
    let key2 = common::ssh_ed25519(4);
    ldap.set_attr(&dn, "cn", &["Captain Marvel"]);
    ldap.set_attr(&dn, "sshPublicKey", &[&key2]);
    web_login(&app, "carol", "ldap-pw").await.assert_status(200);
    assert_eq!(
        user_row(&app, "carol").await.2.as_deref(),
        Some("Captain Marvel")
    );
    let keys = app
        .get("/api/v3/user/keys")
        .auth(&carol)
        .send()
        .await
        .json();
    assert_eq!(keys.as_array().unwrap().len(), 1);
    assert!(key2.starts_with(keys[0]["key"].as_str().unwrap()));

    // Linked accounts can't use a built-in password.
    sqlx::query("UPDATE users SET password_hash = (SELECT password_hash FROM users WHERE login = 'root') WHERE id = $1")
        .bind(carol.id)
        .execute(&app.state.db)
        .await
        .unwrap();
    web_login(&app, "carol", &admin.password)
        .await
        .assert_status(401);

    // Users unknown to the directory keep their built-in password.
    let bob = app.create_user("bob").await;
    web_login(&app, "bob", &bob.password)
        .await
        .assert_status(200);

    // Without JIT provisioning, unknown directory users are refused.
    enable_ldap(&app, &admin, &ldap, json!({ "jit_provisioning": false })).await;
    ldap.add_user("dan", "dan-pw", &[("mail", "dan@corp.example")]);
    web_login(&app, "dan", "dan-pw").await.assert_status(401);

    // Directory down: built-in accounts still work, directory accounts not.
    let mut down = ldap.settings();
    down.port = 1;
    set_auth(&app, &admin, json!({ "ldap": down })).await;
    web_login(&app, "bob", &bob.password)
        .await
        .assert_status(200);
    web_login(&app, "carol", "ldap-pw").await.assert_status(401);
}

#[tokio::test]
async fn restricted_group_and_admin_group() {
    let app = bgh_server::test_app().await;
    let admin = app.create_admin("root").await;
    let ldap = FakeLdap::start().await;
    let erin = ldap.add_user("erin", "pw-e", &[("mail", "erin@corp.example")]);
    let finn = ldap.add_user("finn", "pw-f", &[("mail", "finn@corp.example")]);
    let staff = ldap.add_group("staff", &[&erin, &finn]);
    let admins = ldap.add_group("admins", &[&erin]);
    enable_ldap(
        &app,
        &admin,
        &ldap,
        json!({ "admin_group": admins, "restricted_group": staff }),
    )
    .await;

    web_login(&app, "erin", "pw-e").await.assert_status(200);
    web_login(&app, "finn", "pw-f").await.assert_status(200);
    assert!(
        user_row(&app, "erin").await.0,
        "admin group grants site admin"
    );
    assert!(!user_row(&app, "finn").await.0);

    // Leaving the admin group demotes at the next sync.
    ldap.add_group("admins", &[&finn]);
    let report = sync::sync_all(&app.state).await.unwrap();
    assert_eq!(report.users, 2);
    assert!(!user_row(&app, "erin").await.0);
    assert!(user_row(&app, "finn").await.0);

    // Outside the restricted group: no sign-in.
    ldap.add_group("staff", &[&finn]);
    web_login(&app, "erin", "pw-e").await.assert_status(401);
}

#[tokio::test]
async fn sync_suspends_disabled_and_missing_users() {
    let app = bgh_server::test_app().await;
    let admin = app.create_admin("root").await;
    let ldap = FakeLdap::start().await;
    let gus = ldap.add_user("gus", "pw-g", &[("mail", "gus@corp.example")]);
    let hal = ldap.add_user("hal", "pw-h", &[("mail", "hal@corp.example")]);
    enable_ldap(&app, &admin, &ldap, json!({})).await;
    web_login(&app, "gus", "pw-g").await.assert_status(200);
    let res = web_login(&app, "hal", "pw-h").await;
    res.assert_status(200);
    let hal_cookie = common::cookie_from(&res);

    // Disabled in the directory (nsAccountLock) and deleted.
    ldap.set_attr(&gus, "nsAccountLock", &["true"]);
    ldap.remove(&hal);
    let report = sync::sync_all(&app.state).await.unwrap();
    assert_eq!(report.suspended, 2);
    assert!(user_row(&app, "gus").await.1);
    assert!(user_row(&app, "hal").await.1);
    // Suspension ends sessions and blocks sign-in.
    app.get("/api/v3/user")
        .cookie(&hal_cookie)
        .send()
        .await
        .assert_status(401);
    web_login(&app, "gus", "pw-g").await.assert_status(401);
    let audits: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE action = 'user.suspend' AND data->>'ldap' = 'true'",
    )
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(audits, 2);
    // Idempotent.
    assert_eq!(sync::sync_all(&app.state).await.unwrap().suspended, 0);

    // Re-enabled: the sync lifts its own suspension, not manual ones.
    ldap.set_attr(&gus, "nsAccountLock", &[]);
    sync::sync_all(&app.state).await.unwrap();
    assert!(!user_row(&app, "gus").await.1);
    web_login(&app, "gus", "pw-g").await.assert_status(200);
    let gus_user = handle(&app, "gus", "pw-g").await;
    app.put("/api/v3/users/gus/suspended")
        .auth(&admin)
        .json(&json!({ "reason": "manual" }))
        .send()
        .await
        .assert_status(204);
    sync::sync_all(&app.state).await.unwrap();
    assert!(user_row(&app, "gus").await.1, "manual suspension stays");
    let _ = gus_user;
}

#[tokio::test]
async fn team_mapping_adds_and_removes_members() {
    let app = bgh_server::test_app().await;
    let admin = app.create_admin("root").await;
    let ldap = FakeLdap::start().await;
    let ivy = ldap.add_user("ivy", "pw-i", &[("mail", "ivy@corp.example")]);
    let jon = ldap.add_user("jon", "pw-j", &[("mail", "jon@corp.example")]);
    let devs = ldap.add_group("devs", &[&ivy]);
    enable_ldap(&app, &admin, &ldap, json!({})).await;
    for (u, p) in [("ivy", "pw-i"), ("jon", "pw-j")] {
        web_login(&app, u, p).await.assert_status(200);
    }
    let org = app.create_org("acme", &admin).await;
    let team = app
        .post(&format!("/api/v3/orgs/{}/teams", org.login))
        .auth(&admin)
        .json(&json!({ "name": "Developers" }))
        .send()
        .await
        .json();
    let team_id = team["id"].as_i64().unwrap();
    // The creator is a maintainer; LDAP manages membership from now on.

    let res = app
        .patch(&format!("/api/v3/admin/ldap/teams/{team_id}/mapping"))
        .auth(&admin)
        .json(&json!({ "ldap_dn": devs.to_uppercase() }))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["slug"], "developers");
    assert_eq!(
        res.json()["ldap_dn"],
        bgh_accounts::ldap::normalize_dn(&devs)
    );
    app.post(&format!("/api/v3/admin/ldap/teams/{team_id}/sync"))
        .auth(&admin)
        .send()
        .await
        .assert_status(201);
    app.drain_jobs().await;
    let members = || async {
        let v = app
            .get("/api/v3/orgs/acme/teams/developers/members")
            .auth(&admin)
            .send()
            .await
            .json();
        let mut l: Vec<String> = v
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["login"].as_str().unwrap().to_string())
            .collect();
        l.sort();
        l
    };
    assert_eq!(members().await, ["ivy"]);
    // Ivy joined the organization as a member.
    app.get("/api/v3/orgs/acme/members/ivy")
        .auth(&admin)
        .send()
        .await
        .assert_status(204);

    // Group changes: jon in, ivy out (full sync).
    ldap.add_group("devs", &[&jon]);
    let report = sync::sync_all(&app.state).await.unwrap();
    assert_eq!(
        (report.team_members_added, report.team_members_removed),
        (1, 1)
    );
    assert_eq!(members().await, ["jon"]);

    // At sign-in a user's mapped teams follow the directory too.
    ldap.add_group("devs", &[&jon, &ivy]);
    web_login(&app, "ivy", "pw-i").await.assert_status(200);
    assert_eq!(members().await, ["ivy", "jon"]);

    // Removing the mapping stops the sync.
    app.patch(&format!("/api/v3/admin/ldap/teams/{team_id}/mapping"))
        .auth(&admin)
        .json(&json!({ "ldap_dn": "" }))
        .send()
        .await
        .assert_status(200);
    ldap.add_group("devs", &[]);
    sync::sync_all(&app.state).await.unwrap();
    assert_eq!(members().await, ["ivy", "jon"]);
}

#[tokio::test]
async fn ghes_user_mapping_and_sync() {
    let app = bgh_server::test_app().await;
    let admin = app.create_admin("root").await;
    let ldap = FakeLdap::start().await;
    let dn = ldap.add_user("kim", "pw-k", &[("cn", "Kim Directory")]);
    enable_ldap(&app, &admin, &ldap, json!({})).await;
    let kim = app.create_user("kim2").await;

    let nobody = app.create_user("plain").await;
    app.patch("/api/v3/admin/ldap/users/kim2/mapping")
        .auth(&nobody)
        .json(&json!({ "ldap_dn": dn }))
        .send()
        .await
        .assert_status(403);
    app.patch("/api/v3/admin/ldap/users/kim2/mapping")
        .auth(&admin)
        .json(&json!({}))
        .send()
        .await
        .assert_status(422);
    let res = app
        .patch("/api/v3/admin/ldap/users/kim2/mapping")
        .auth(&admin)
        .json(&json!({ "ldap_dn": dn }))
        .send()
        .await;
    res.assert_status(200);
    let v = res.json();
    common::assert_simple_user(&v, "kim2");
    assert_eq!(v["ldap_dn"], format!("uid=kim,{PEOPLE}"));

    let res = app
        .post("/api/v3/admin/ldap/users/kim2/sync")
        .auth(&admin)
        .send()
        .await;
    res.assert_status(201);
    assert_eq!(res.json(), json!({ "status": "queued" }));
    app.drain_jobs().await;
    assert_eq!(
        user_row(&app, "kim2").await.2.as_deref(),
        Some("Kim Directory")
    );
    // Mapped: kim2 signs in with the directory account's password.
    web_login(&app, "kim", "pw-k").await.assert_status(200);
    web_login(&app, "kim2", &kim.password)
        .await
        .assert_status(401);
    app.post("/api/v3/admin/ldap/users/nobody-here/sync")
        .auth(&admin)
        .send()
        .await
        .assert_status(404);
    app.post("/api/v3/admin/ldap/teams/999999/sync")
        .auth(&admin)
        .send()
        .await
        .assert_status(404);
}

#[tokio::test]
async fn password_login_disabled_is_enforced() {
    let app = bgh_server::test_app().await;
    let admin = app.create_admin("root").await;
    let bob = app.create_user("bob").await;
    app.create_private_repo(&bob, "code").await;
    let ldap = FakeLdap::start().await;
    ldap.add_user("lee", "pw-l", &[("mail", "lee@corp.example")]);
    enable_ldap(&app, &admin, &ldap, json!({})).await;
    set_auth(&app, &admin, json!({ "password_login": false })).await;
    assert_eq!(
        app.get("/_bgh/site").send().await.json()["password_login"],
        json!(false)
    );

    // Built-in passwords: refused on web and git (also when wrong, so the
    // refusal is no password oracle), not counted as failures.
    let res = web_login(&app, "bob", &bob.password).await;
    res.assert_status(403);
    assert!(res.json()["message"].as_str().unwrap().contains("disabled"));
    web_login(&app, "bob", "wrong").await.assert_status(403);
    app.post("/_bgh/auth/login")
        .json(&json!({ "login": "bob", "password": bob.password }))
        .send()
        .await
        .assert_status(403);
    let refs = "/bob/code.git/info/refs?service=git-upload-pack";
    let res = app.get(refs).basic("bob", &bob.password).send().await;
    res.assert_status(403);
    // Git clients get the message as text (shown as `remote: ...`).
    assert!(res.text().contains("personal access token"));
    // Tokens keep working (git basic with a PAT, API).
    app.get(refs)
        .basic("bob", &bob.token)
        .send()
        .await
        .assert_status(200);
    app.get("/api/v3/user")
        .auth(&bob)
        .send()
        .await
        .assert_status(200);
    // Directory passwords are not built-in passwords.
    web_login(&app, "lee", "pw-l").await.assert_status(200);

    // Site admins only when exempt (break-glass).
    web_login(&app, "root", &admin.password)
        .await
        .assert_status(403);
    set_auth(&app, &admin, json!({ "password_login_admin_exempt": true })).await;
    web_login(&app, "root", &admin.password)
        .await
        .assert_status(200);
    web_login(&app, "bob", &bob.password)
        .await
        .assert_status(403);

    // At least one sign-in method must stay enabled.
    app.patch("/_bgh/admin/settings")
        .auth(&admin)
        .json(&json!({ "auth_providers": { "ldap": { "enabled": false } } }))
        .send()
        .await
        .assert_status(422);
}

#[tokio::test]
async fn git_basic_auth_failures_are_throttled_and_audited() {
    let app = bgh_server::test_app().await;
    let mia = app.create_user("mia").await;
    app.create_private_repo(&mia, "code").await;
    let refs = "/mia/code.git/info/refs?service=git-upload-pack";
    let mut statuses = Vec::new();
    for i in 0..20 {
        let res = app
            .get(refs)
            .basic("mia", &format!("guess-{i}"))
            .send()
            .await;
        statuses.push(res.status());
    }
    assert_eq!(&statuses[..10], &[401; 10]);
    assert_eq!(&statuses[10..], &[429; 10]);
    // Locked out: even the right password (git and web share the throttle).
    app.get(refs)
        .basic("mia", &mia.password)
        .send()
        .await
        .assert_status(429);
    web_login(&app, "mia", &mia.password)
        .await
        .assert_status(429);
    // Tokens are unaffected.
    app.get(refs)
        .basic("mia", &mia.token)
        .send()
        .await
        .assert_status(200);
    let rows: Vec<(Option<i64>, Value)> = sqlx::query_as(
        "SELECT actor_id, data FROM audit_log WHERE action = 'user.failed_login' ORDER BY id",
    )
    .fetch_all(&app.state.db)
    .await
    .unwrap();
    assert_eq!(rows.len(), 10);
    for (_, data) in &rows {
        assert_eq!(data["transport"], "git");
        assert_eq!(data["login"], "mia");
    }
}
