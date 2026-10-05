//! SCIM 2.0 provisioning: enterprise Users (create, get, filter, PATCH
//! deactivation revoking credentials, PUT, DELETE), Groups → team sync,
//! organization Users, auth and error shapes, and SAML sign-in of SCIM
//! users.

use crate::common;

use bgh_core::testing::{TestApp, TestResponse, TestUser};
use serde_json::{Value, json};

const E: &str = "/api/v3/scim/v2/enterprises/acme-corp";

async fn enable(app: &TestApp, admin: &TestUser) {
    app.patch("/_bgh/admin/settings")
        .auth(admin)
        .json(&json!({ "auth_providers": { "scim": { "enabled": true } } }))
        .send()
        .await
        .assert_status(200);
}

fn assert_scim_error(res: &TestResponse, status: u16) {
    res.assert_status(status);
    assert!(
        res.header("content-type")
            .unwrap()
            .starts_with("application/scim+json"),
        "{:?}",
        res.header("content-type")
    );
    let v = res.json();
    assert_eq!(
        v["schemas"],
        json!(["urn:ietf:params:scim:api:messages:2.0:Error"])
    );
    assert_eq!(v["status"], status);
    assert!(v["detail"].is_string());
}

fn assert_user_shape(v: &Value, base: &str) {
    assert_eq!(
        v["schemas"],
        json!(["urn:ietf:params:scim:schemas:core:2.0:User"])
    );
    let id = v["id"].as_str().unwrap();
    assert_eq!(id.len(), 36);
    for k in [
        "externalId",
        "userName",
        "displayName",
        "name",
        "emails",
        "active",
        "meta",
    ] {
        assert!(v.get(k).is_some(), "missing {k}: {v}");
    }
    assert_eq!(v["meta"]["resourceType"], "User");
    assert_eq!(v["meta"]["location"], format!("{base}/Users/{id}"));
    assert!(v["meta"]["created"].as_str().unwrap().ends_with('Z'));
}

fn new_user(user_name: &str, external_id: &str) -> Value {
    json!({
        "schemas": ["urn:ietf:params:scim:schemas:core:2.0:User"],
        "userName": user_name,
        "externalId": external_id,
        "name": { "givenName": "Mona", "familyName": "Octocat" },
        "emails": [{ "value": format!("{user_name}@corp.example"), "type": "work", "primary": true }],
        "active": true,
    })
}

async fn token_for(app: &TestApp, login: &str, scopes: &[&str]) -> String {
    let id: i64 = sqlx::query_scalar("SELECT id FROM users WHERE login = $1")
        .bind(login)
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    bgh_core::auth::create_access_token(
        &app.state.db,
        id,
        "t",
        &scopes.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        None,
    )
    .await
    .unwrap()
    .1
}

#[tokio::test]
async fn enterprise_users_conformance() {
    let app = bgh_server::test_app().await;
    let admin = app.create_admin("root").await;
    let other = app.create_user("bob").await;
    let scim = app.create_token(&admin, &["scim:enterprise"]).await;
    let base = app.url(E);

    // Disabled → 404; then auth rules.
    assert_scim_error(
        &app.get(&format!("{E}/Users")).token(&scim).send().await,
        404,
    );
    enable(&app, &admin).await;
    assert_scim_error(&app.get(&format!("{E}/Users")).send().await, 401);
    assert_scim_error(
        &app.get(&format!("{E}/Users")).auth(&other).send().await,
        403,
    );
    let wrong = app.create_token(&admin, &["site_admin"]).await;
    assert_scim_error(
        &app.get(&format!("{E}/Users")).token(&wrong).send().await,
        403,
    );
    // The scope is reserved for site admins.
    let cookie = common::session(&app, &other).await;
    let csrf = bgh_core::auth::csrf_token(cookie.trim_start_matches("bgh_session="));
    app.post("/_bgh/tokens")
        .cookie(&cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "name": "x", "scopes": ["scim:enterprise"] }))
        .send()
        .await
        .assert_status(422);

    // Create.
    let res = app
        .post(&format!("{E}/Users"))
        .token(&scim)
        .header("content-type", "application/scim+json")
        .body(new_user("octo", "e-1").to_string())
        .send()
        .await;
    res.assert_status(201);
    assert!(
        res.header("content-type")
            .unwrap()
            .starts_with("application/scim+json")
    );
    let u = res.json();
    assert_user_shape(&u, &base);
    assert_eq!(res.header("location"), u["meta"]["location"].as_str());
    assert_eq!(u["userName"], "octo");
    assert_eq!(u["externalId"], "e-1");
    assert_eq!(u["active"], true);
    assert_eq!(u["roles"], json!([]));
    let id = u["id"].as_str().unwrap().to_string();
    let account = app.get("/api/v3/users/octo").send().await.json();
    assert_eq!(account["name"], "Mona Octocat");
    assert_eq!(account["site_admin"], false);

    // Uniqueness.
    let dup = app
        .post(&format!("{E}/Users"))
        .token(&scim)
        .json(&new_user("OCTO", "e-9"))
        .send()
        .await;
    assert_scim_error(&dup, 409);
    assert_eq!(dup.json()["scimType"], "uniqueness");
    let dup = app
        .post(&format!("{E}/Users"))
        .token(&scim)
        .json(&new_user("other", "e-1"))
        .send()
        .await;
    assert_scim_error(&dup, 409);
    let bad = app
        .post(&format!("{E}/Users"))
        .token(&scim)
        .json(&json!({ "emails": [] }))
        .send()
        .await;
    assert_scim_error(&bad, 400);
    assert_eq!(bad.json()["scimType"], "invalidValue");

    // Get, filter, paginate.
    let got = app
        .get(&format!("{E}/Users/{id}"))
        .token(&scim)
        .send()
        .await;
    got.assert_status(200);
    assert_eq!(got.json(), u);
    assert_scim_error(
        &app.get(&format!("{E}/Users/nope"))
            .token(&scim)
            .send()
            .await,
        404,
    );
    app.post(&format!("{E}/Users"))
        .token(&scim)
        .json(&new_user("hubot", "e-2"))
        .send()
        .await
        .assert_status(201);
    let list = app
        .get(&format!(
            "{E}/Users?filter={}",
            urlenc(r#"userName eq "OCTO""#)
        ))
        .token(&scim)
        .send()
        .await
        .json();
    assert_eq!(
        list["schemas"],
        json!(["urn:ietf:params:scim:api:messages:2.0:ListResponse"])
    );
    assert_eq!(list["totalResults"], 1);
    assert_eq!(list["startIndex"], 1);
    assert_eq!(list["itemsPerPage"], 1);
    assert_eq!(list["Resources"][0]["id"], id.as_str());
    let list = app
        .get(&format!(
            "{E}/Users?filter={}",
            urlenc(r#"externalId eq "e-2""#)
        ))
        .token(&scim)
        .send()
        .await
        .json();
    assert_eq!(list["Resources"][0]["userName"], "hubot");
    let page = app
        .get(&format!("{E}/Users?startIndex=2&count=1"))
        .token(&scim)
        .send()
        .await
        .json();
    assert_eq!(page["totalResults"], 2);
    assert_eq!(page["startIndex"], 2);
    assert_eq!(page["Resources"][0]["userName"], "hubot");
    let bad = app
        .get(&format!(
            "{E}/Users?filter={}",
            urlenc(r#"userName co "o""#)
        ))
        .token(&scim)
        .send()
        .await;
    assert_scim_error(&bad, 400);
    assert_eq!(bad.json()["scimType"], "invalidFilter");

    // Credentials the deprovisioning must revoke.
    let pat = token_for(&app, "octo", &["repo", "user"]).await;
    app.get("/api/v3/user")
        .token(&pat)
        .send()
        .await
        .assert_status(200);
    let user_id: i64 = sqlx::query_scalar("SELECT id FROM users WHERE login = 'octo'")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO ssh_keys (user_id, title, key, fingerprint) VALUES ($1, 'k', 'ssh-ed25519 AAAA', 'SHA256:x')")
        .bind(user_id)
        .execute(&app.state.db)
        .await
        .unwrap();
    let session = bgh_core::auth::create_session(&app.state, user_id, None, None)
        .await
        .unwrap();

    // PATCH active=false (Azure AD style) suspends and revokes.
    let res = app
        .patch(&format!("{E}/Users/{id}"))
        .token(&scim)
        .json(&json!({
            "schemas": ["urn:ietf:params:scim:api:messages:2.0:PatchOp"],
            "Operations": [{ "op": "Replace", "path": "active", "value": "False" }],
        }))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["active"], false);
    let (suspended, tokens, keys, sessions): (bool, i64, i64, i64) = sqlx::query_as(
        "SELECT u.suspended_at IS NOT NULL,
                (SELECT count(*) FROM access_tokens WHERE user_id = u.id),
                (SELECT count(*) FROM ssh_keys WHERE user_id = u.id),
                (SELECT count(*) FROM sessions WHERE user_id = u.id)
           FROM users u WHERE u.id = $1",
    )
    .bind(user_id)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!((suspended, tokens, keys, sessions), (true, 0, 0, 0));
    app.get("/api/v3/user")
        .token(&pat)
        .send()
        .await
        .assert_status(401);
    app.get("/api/v3/user")
        .cookie(&format!("bgh_session={session}"))
        .send()
        .await
        .assert_status(401);
    let audit: Value = sqlx::query_scalar(
        "SELECT data FROM audit_log WHERE action = 'user.suspend' ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(audit["scim"], true);
    assert_eq!(audit["revoked_tokens"], 1);

    // Reactivate; then PUT replaces the resource.
    let res = app
        .patch(&format!("{E}/Users/{id}"))
        .token(&scim)
        .json(&json!({ "Operations": [{ "op": "replace", "value": { "active": true } }] }))
        .send()
        .await;
    assert_eq!(res.json()["active"], true);
    let suspended: bool =
        sqlx::query_scalar("SELECT suspended_at IS NOT NULL FROM users WHERE id = $1")
            .bind(user_id)
            .fetch_one(&app.state.db)
            .await
            .unwrap();
    assert!(!suspended);
    let mut put = new_user("octo", "e-1");
    put["displayName"] = json!("The Octocat");
    put["roles"] = json!([{ "value": "enterprise_owner", "primary": true }]);
    let res = app
        .put(&format!("{E}/Users/{id}"))
        .token(&scim)
        .json(&put)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["displayName"], "The Octocat");
    assert_eq!(res.json()["roles"][0]["value"], "enterprise_owner");
    let account = app.get("/api/v3/users/octo").send().await.json();
    assert_eq!(account["name"], "The Octocat");
    assert_eq!(account["site_admin"], true);

    // DELETE deprovisions and forgets the SCIM user.
    app.delete(&format!("{E}/Users/{id}"))
        .token(&scim)
        .send()
        .await
        .assert_status(204);
    assert_scim_error(
        &app.get(&format!("{E}/Users/{id}"))
            .token(&scim)
            .send()
            .await,
        404,
    );
    let (suspended, admin_now): (bool, bool) =
        sqlx::query_as("SELECT suspended_at IS NOT NULL, site_admin FROM users WHERE id = $1")
            .bind(user_id)
            .fetch_one(&app.state.db)
            .await
            .unwrap();
    assert!(suspended);
    assert!(!admin_now);
}

fn urlenc(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

#[tokio::test]
async fn groups_sync_mapped_teams() {
    let app = bgh_server::test_app().await;
    let admin = app.create_admin("root").await;
    let org = app.create_org("acme", &admin).await;
    enable(&app, &admin).await;
    let scim = app.create_token(&admin, &["scim:enterprise"]).await;
    app.post(&format!("/api/v3/orgs/{}/teams", org.login))
        .auth(&admin)
        .json(&json!({ "name": "Eng" }))
        .send()
        .await
        .assert_status(201);
    app.patch(&format!(
        "/api/v3/orgs/{}/teams/eng/team-sync/group-mappings",
        org.login
    ))
    .auth(&admin)
    .json(&json!({ "groups": [{ "group_id": "Engineering" }] }))
    .send()
    .await
    .assert_status(200);
    let members = || async {
        let v = app
            .get(&format!("/api/v3/orgs/{}/teams/eng/members", org.login))
            .auth(&admin)
            .send()
            .await
            .json();
        let mut l = common::logins(&v);
        l.sort();
        l
    };
    let mut ids = Vec::new();
    for (name, ext) in [("ann", "x1"), ("ben", "x2")] {
        let u = app
            .post(&format!("{E}/Users"))
            .token(&scim)
            .json(&new_user(name, ext))
            .send()
            .await
            .json();
        ids.push(u["id"].as_str().unwrap().to_string());
    }

    let res = app
        .post(&format!("{E}/Groups"))
        .token(&scim)
        .json(&json!({
            "schemas": ["urn:ietf:params:scim:schemas:core:2.0:Group"],
            "displayName": "engineering",
            "externalId": "g-1",
            "members": [{ "value": ids[0] }],
        }))
        .send()
        .await;
    res.assert_status(201);
    let g = res.json();
    assert_eq!(
        g["schemas"],
        json!(["urn:ietf:params:scim:schemas:core:2.0:Group"])
    );
    assert_eq!(g["meta"]["resourceType"], "Group");
    assert_eq!(g["members"][0]["value"], ids[0].as_str());
    assert_eq!(g["members"][0]["display"], "ann");
    let gid = g["id"].as_str().unwrap().to_string();
    assert_eq!(members().await, vec!["ann"]);
    // ann joined the organization too.
    app.get(&format!("/api/v3/orgs/{}/members/ann", org.login))
        .auth(&admin)
        .send()
        .await
        .assert_status(204);

    let dup = app
        .post(&format!("{E}/Groups"))
        .token(&scim)
        .json(&json!({ "displayName": "Engineering" }))
        .send()
        .await;
    assert_scim_error(&dup, 409);

    // PATCH add, then remove by filter path.
    let patch = |ops: Value| {
        let app = &app;
        let scim = scim.clone();
        let gid = gid.clone();
        async move {
            let res = app
                .patch(&format!("{E}/Groups/{gid}"))
                .token(&scim)
                .json(&json!({ "schemas": ["urn:ietf:params:scim:api:messages:2.0:PatchOp"], "Operations": ops }))
                .send()
                .await;
            res.assert_status(200);
            res.json()
        }
    };
    let g =
        patch(json!([{ "op": "add", "path": "members", "value": [{ "value": ids[1] }] }])).await;
    assert_eq!(g["members"].as_array().unwrap().len(), 2);
    assert_eq!(members().await, vec!["ann", "ben"]);
    patch(json!([{ "op": "remove", "path": format!("members[value eq \"{}\"]", ids[0]) }])).await;
    assert_eq!(members().await, vec!["ben"]);

    // Filter groups.
    let list = app
        .get(&format!(
            "{E}/Groups?filter={}",
            urlenc(r#"displayName eq "ENGINEERING""#)
        ))
        .token(&scim)
        .send()
        .await
        .json();
    assert_eq!(list["totalResults"], 1);
    assert_eq!(list["Resources"][0]["id"], gid.as_str());

    // Deactivating a member drops them from the team.
    app.patch(&format!("{E}/Users/{}", ids[1]))
        .token(&scim)
        .json(&json!({ "Operations": [{ "op": "replace", "path": "active", "value": false }] }))
        .send()
        .await
        .assert_status(200);
    assert!(members().await.is_empty());
    app.patch(&format!("{E}/Users/{}", ids[1]))
        .token(&scim)
        .json(&json!({ "Operations": [{ "op": "replace", "path": "active", "value": true }] }))
        .send()
        .await
        .assert_status(200);
    assert_eq!(members().await, vec!["ben"]);

    // Renaming the group away from the mapping, then deleting it.
    patch(json!([{ "op": "replace", "path": "displayName", "value": "Platform" }])).await;
    app.delete(&format!("{E}/Groups/{gid}"))
        .token(&scim)
        .send()
        .await
        .assert_status(204);
    assert_scim_error(
        &app.get(&format!("{E}/Groups/{gid}"))
            .token(&scim)
            .send()
            .await,
        404,
    );
}

#[tokio::test]
async fn organization_users_manage_membership() {
    let app = bgh_server::test_app().await;
    let admin = app.create_admin("root").await;
    let owner = app.create_user("owner").await;
    let member = app.create_user("member").await;
    let bob = app.create_user("bob").await;
    let org = app.create_org("acme", &owner).await;
    app.add_org_member(&org, &member, "member").await;
    enable(&app, &admin).await;
    let o = format!("/api/v3/scim/v2/organizations/{}", org.login);
    let token = app.create_token(&owner, &["admin:org"]).await;

    assert_scim_error(
        &app.get(&format!("{o}/Users")).auth(&member).send().await,
        403,
    );
    assert_scim_error(&app.get(&format!("{o}/Users")).auth(&bob).send().await, 404);
    let read_only = app.create_token(&owner, &["read:org"]).await;
    assert_scim_error(
        &app.get(&format!("{o}/Users"))
            .token(&read_only)
            .send()
            .await,
        403,
    );

    let res = app
        .post(&format!("{o}/Users"))
        .token(&token)
        .json(&new_user("bob", "b-1"))
        .send()
        .await;
    res.assert_status(201);
    let u = res.json();
    assert_user_shape(&u, &app.url(&o));
    assert!(u.get("roles").is_none());
    let id = u["id"].as_str().unwrap().to_string();
    app.get(&format!("/api/v3/orgs/{}/members/bob", org.login))
        .auth(&owner)
        .send()
        .await
        .assert_status(204);
    // Org SCIM users are per organization.
    let list = app
        .get(&format!("{o}/Users"))
        .token(&token)
        .send()
        .await
        .json();
    assert_eq!(list["totalResults"], 1);

    // Deactivation removes the membership but leaves the account alone.
    app.patch(&format!("{o}/Users/{id}"))
        .token(&token)
        .json(&json!({ "Operations": [{ "op": "replace", "path": "active", "value": false }] }))
        .send()
        .await
        .assert_status(200);
    app.get(&format!("/api/v3/orgs/{}/members/bob", org.login))
        .auth(&owner)
        .send()
        .await
        .assert_status(404);
    app.get("/api/v3/user")
        .auth(&bob)
        .send()
        .await
        .assert_status(200);
    app.delete(&format!("{o}/Users/{id}"))
        .token(&token)
        .send()
        .await
        .assert_status(204);

    // Org owners can't create accounts when sign-up is restricted.
    app.patch("/_bgh/admin/settings")
        .auth(&admin)
        .json(&json!({ "signup": { "policy": "closed" } }))
        .send()
        .await
        .assert_status(200);
    let res = app
        .post(&format!("{o}/Users"))
        .token(&token)
        .json(&new_user("stranger", "s-1"))
        .send()
        .await;
    assert_scim_error(&res, 403);
}

#[tokio::test]
async fn saml_sign_in_uses_scim_accounts() {
    use bgh_accounts::saml::testing::{self as idp, ResponseOpts};

    let app = bgh_server::test_app().await;
    let admin = app.create_admin("root").await;
    enable(&app, &admin).await;
    app.patch("/_bgh/admin/settings")
        .auth(&admin)
        .json(&json!({ "auth_providers": idp::settings(json!({ "jit_provisioning": false })) }))
        .send()
        .await
        .assert_status(200);
    let scim = app.create_token(&admin, &["scim:enterprise"]).await;
    let u = app
        .post(&format!("{E}/Users"))
        .token(&scim)
        .json(&new_user("mona.lisa@corp.example", "m-1"))
        .send()
        .await
        .json();
    let id = u["id"].as_str().unwrap().to_string();

    let sign_in = || async {
        let res = app.get("/_bgh/saml/login").send().await;
        let (rid, relay) = idp::authn_request(res.header("location").unwrap());
        let o = ResponseOpts::new(&app.base_url, "mona.lisa@corp.example", Some(&rid));
        let mut body = url::form_urlencoded::Serializer::new(String::new());
        body.append_pair("SAMLResponse", &idp::response(&o));
        body.append_pair("RelayState", &relay);
        app.post("/saml/consume")
            .header("content-type", "application/x-www-form-urlencoded")
            .body(body.finish())
            .send()
            .await
    };
    let res = sign_in().await;
    res.assert_status(303);
    let me = app
        .get("/api/v3/user")
        .cookie(&common::cookie_from(&res))
        .send()
        .await
        .json();
    assert_eq!(me["login"], "mona-lisa");

    // Deprovisioned in SCIM → SAML sign-in refused.
    app.patch(&format!("{E}/Users/{id}"))
        .token(&scim)
        .json(&json!({ "Operations": [{ "op": "replace", "path": "active", "value": false }] }))
        .send()
        .await
        .assert_status(200);
    let res = sign_in().await;
    assert!(res.header("location").unwrap().starts_with("/login?error="));
    assert!(res.header("set-cookie").is_none());
}
