//! SAML single sign-on against the in-process test IdP
//! (`bgh_accounts::saml::testing`): SP-initiated round trip, JIT
//! provisioning and attribute mapping, replay / tampering / expiry,
//! encrypted assertions, IdP-initiated sign-in, single logout, admin
//! endpoints and settings validation.

use crate::common::{self, cookie_from};

use bgh_accounts::saml::testing::{self as idp, ResponseOpts};
use bgh_core::testing::{TestApp, TestResponse, TestUser};
use serde_json::{Value, json};

async fn set_auth(app: &TestApp, admin: &TestUser, fields: Value) -> TestResponse {
    app.patch("/_bgh/admin/settings")
        .auth(admin)
        .json(&json!({ "auth_providers": fields }))
        .send()
        .await
}

async fn enable(app: &TestApp, admin: &TestUser, extra: Value) {
    set_auth(app, admin, idp::settings(extra))
        .await
        .assert_status(200);
}

/// Start an SP-initiated sign-in: `(request ID, RelayState)`.
async fn start(app: &TestApp, return_to: &str) -> (String, String) {
    let res = app
        .get(&format!("/_bgh/saml/login?return_to={return_to}"))
        .send()
        .await;
    res.assert_status(303);
    let loc = res.header("location").unwrap().to_string();
    assert!(loc.starts_with(idp::IDP_SSO_URL), "{loc}");
    idp::authn_request(&loc)
}

async fn post_response(app: &TestApp, saml_response: &str, relay: Option<&str>) -> TestResponse {
    let mut body = url::form_urlencoded::Serializer::new(String::new());
    body.append_pair("SAMLResponse", saml_response);
    if let Some(r) = relay {
        body.append_pair("RelayState", r);
    }
    app.post("/saml/consume")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(body.finish())
        .send()
        .await
}

fn failed(res: &TestResponse) -> bool {
    res.status() == 303
        && res.header("location").unwrap().starts_with("/login?error=")
        && res.header("set-cookie").is_none()
}

/// A full SP-initiated sign-in; returns the response of the ACS.
async fn sign_in(
    app: &TestApp,
    opts: impl FnOnce(ResponseOpts) -> ResponseOpts,
    name_id: &str,
) -> TestResponse {
    let (id, relay) = start(app, "/dashboard").await;
    let o = opts(ResponseOpts::new(&app.base_url, name_id, Some(&id)));
    post_response(app, &idp::response(&o), Some(&relay)).await
}

#[tokio::test]
async fn sp_initiated_round_trip_provisions_and_maps_attributes() {
    let app = bgh_server::test_app().await;
    let admin = app.create_admin("root").await;
    enable(&app, &admin, json!({ "admin_attribute": "administrator" })).await;

    // Public site info advertises the provider.
    let site = app.get("/_bgh/site").send().await.json();
    assert_eq!(site["saml"]["display_name"], "Test IdP");
    assert_eq!(site["saml"]["login_url"], "/_bgh/saml/login");

    // Metadata.
    let md = app.get("/saml/metadata").send().await;
    md.assert_status(200);
    assert_eq!(
        md.header("content-type"),
        Some("application/samlmetadata+xml")
    );
    let md = md.text();
    assert!(md.contains(&format!("entityID=\"{}\"", app.base_url)));
    assert!(md.contains(&format!("Location=\"{}/saml/consume\"", app.base_url)));
    assert!(md.contains("WantAssertionsSigned=\"true\""));

    // The AuthnRequest names us and our ACS.
    let res = app.get("/_bgh/saml/login?return_to=/settings").send().await;
    let loc = res.header("location").unwrap().to_string();
    let req = idp::redirect_message(&loc, "SAMLRequest").unwrap();
    assert!(req.contains(&format!(
        "AssertionConsumerServiceURL=\"{}/saml/consume\"",
        app.base_url
    )));
    assert!(req.contains(&format!("<saml:Issuer>{}</saml:Issuer>", app.base_url)));
    let (id, relay) = idp::authn_request(&loc);

    let key = common::ssh_ed25519(7);
    let o = ResponseOpts::new(&app.base_url, "mona@corp.example", Some(&id))
        .attr("full_name", &["Mona Lisa"])
        .attr("emails", &["mona@corp.example", "ml@corp.example"])
        .attr("public_keys", &[&key])
        .attr("administrator", &["true"]);
    let res = post_response(&app, &idp::response(&o), Some(&relay)).await;
    res.assert_status(303);
    assert_eq!(res.header("location"), Some("/settings"));
    let cookie = cookie_from(&res);
    let me = app.get("/api/v3/user").cookie(&cookie).send().await.json();
    assert_eq!(me["login"], "mona");
    assert_eq!(me["name"], "Mona Lisa");
    assert_eq!(me["site_admin"], true);
    let ids = app
        .get("/_bgh/user/identities")
        .cookie(&cookie)
        .send()
        .await
        .json();
    assert_eq!(ids[0]["provider"], "saml");
    assert_eq!(ids[0]["subject"], "mona@corp.example");
    let (verified, keys): (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM user_emails e WHERE e.user_id = u.id AND e.verified),
                (SELECT count(*) FROM ssh_keys k WHERE k.user_id = u.id AND k.saml_synced)
           FROM users u WHERE u.login = 'mona'",
    )
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!((verified, keys), (2, 1));

    // The same response can't be used twice.
    let res = post_response(&app, &idp::response(&o), Some(&relay)).await;
    assert!(failed(&res));

    // Second sign-in: same account; keys follow the attribute; admin revoked.
    let res = sign_in(
        &app,
        |o| o.attr("public_keys", &[]).attr("administrator", &["false"]),
        "mona@corp.example",
    )
    .await;
    res.assert_status(303);
    let me = app
        .get("/api/v3/user")
        .cookie(&cookie_from(&res))
        .send()
        .await
        .json();
    assert_eq!(me["login"], "mona");
    assert_eq!(me["site_admin"], false);
    let keys: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ssh_keys k JOIN users u ON u.id = k.user_id WHERE u.login = 'mona'",
    )
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(keys, 0);
    // Created as an admin (JIT), then demoted by the attribute.
    let audit: Vec<String> = sqlx::query_scalar(
        "SELECT action FROM audit_log WHERE action IN ('user.saml_link', 'user.promote', 'user.demote')
          ORDER BY id",
    )
    .fetch_all(&app.state.db)
    .await
    .unwrap();
    assert_eq!(audit, ["user.saml_link", "user.demote"]);
}

#[tokio::test]
async fn rejects_invalid_responses() {
    let app = bgh_server::test_app().await;
    let admin = app.create_admin("root").await;
    enable(&app, &admin, json!({})).await;

    // Unsigned.
    let res = sign_in(
        &app,
        |o| ResponseOpts {
            sign_assertion: false,
            ..o
        },
        "eve",
    )
    .await;
    assert!(failed(&res));
    // Expired.
    let res = sign_in(
        &app,
        |o| ResponseOpts {
            valid_for: -600,
            ..o
        },
        "eve",
    )
    .await;
    assert!(failed(&res));
    // Wrong audience / issuer / recipient.
    let res = sign_in(
        &app,
        |o| ResponseOpts {
            audience: "https://other".into(),
            ..o
        },
        "eve",
    )
    .await;
    assert!(failed(&res));
    let res = sign_in(
        &app,
        |o| ResponseOpts {
            issuer: "https://evil".into(),
            ..o
        },
        "eve",
    )
    .await;
    assert!(failed(&res));
    let res = sign_in(
        &app,
        |o| ResponseOpts {
            acs_url: "https://other/saml/consume".into(),
            ..o
        },
        "eve",
    )
    .await;
    assert!(failed(&res));
    // Answer to another request.
    let (_, relay) = start(&app, "/").await;
    let o = ResponseOpts::new(&app.base_url, "eve", Some("_someone-else"));
    assert!(failed(
        &post_response(&app, &idp::response(&o), Some(&relay)).await
    ));
    // Tampered after signing.
    let (id, relay) = start(&app, "/").await;
    let xml = idp::response_xml(&ResponseOpts::new(&app.base_url, "eve", Some(&id)))
        .replace(">eve<", ">root<");
    let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, xml);
    assert!(failed(&post_response(&app, &b64, Some(&relay)).await));
    // Unsolicited responses are refused by default.
    let o = ResponseOpts::new(&app.base_url, "eve", None);
    assert!(failed(&post_response(&app, &idp::response(&o), None).await));
    // Garbage.
    assert!(failed(&post_response(&app, "not base64!", None).await));
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM users WHERE login IN ('eve', 'root') AND id <> $1",
    )
    .bind(admin.id)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(n, 0);

    // A signed response (instead of a signed assertion) is accepted.
    let res = sign_in(
        &app,
        |o| ResponseOpts {
            sign_assertion: false,
            sign_response: true,
            ..o
        },
        "eve",
    )
    .await;
    res.assert_status(303);
    assert_eq!(res.header("location"), Some("/dashboard"));
}

#[tokio::test]
async fn encrypted_assertions_and_idp_initiated_sign_in() {
    let app = bgh_server::test_app().await;
    let admin = app.create_admin("root").await;
    // Encryption needs the SP key pair.
    set_auth(
        &app,
        &admin,
        idp::settings(json!({ "require_encrypted_assertions": true })),
    )
    .await
    .assert_status(422);
    enable(
        &app,
        &admin,
        json!({
            "require_encrypted_assertions": true,
            "allow_idp_initiated": true,
            "sp_certificate": idp::SP_CERT,
            "sp_private_key": idp::SP_KEY,
        }),
    )
    .await;
    let md = app.get("/saml/metadata").send().await.text();
    assert!(md.contains("<md:KeyDescriptor use=\"encryption\">"));

    // Plaintext is refused, encrypted works.
    let res = sign_in(&app, |o| o, "enc").await;
    assert!(failed(&res));
    let res = sign_in(
        &app,
        |o| ResponseOpts {
            encrypt_for: Some(idp::SP_CERT.into()),
            ..o
        },
        "enc",
    )
    .await;
    res.assert_status(303);
    let me = app
        .get("/api/v3/user")
        .cookie(&cookie_from(&res))
        .send()
        .await
        .json();
    assert_eq!(me["login"], "enc");

    // IdP-initiated (no request, RelayState = target path).
    let o = ResponseOpts {
        encrypt_for: Some(idp::SP_CERT.into()),
        ..ResponseOpts::new(&app.base_url, "enc", None)
    };
    let res = post_response(&app, &idp::response(&o), Some("/notifications")).await;
    res.assert_status(303);
    assert_eq!(res.header("location"), Some("/notifications"));
    // Off-site RelayState never becomes a redirect target.
    let o = ResponseOpts {
        encrypt_for: Some(idp::SP_CERT.into()),
        ..ResponseOpts::new(&app.base_url, "enc", None)
    };
    let res = post_response(&app, &idp::response(&o), Some("https://evil.example")).await;
    assert_eq!(res.header("location"), Some("/"));

    // The private key is write-only.
    let s = app
        .get("/_bgh/admin/settings")
        .auth(&admin)
        .send()
        .await
        .json();
    assert_eq!(s["auth_providers"]["saml"]["sp_private_key"], "********");
    set_auth(&app, &admin, json!({ "saml": s["auth_providers"]["saml"] }))
        .await
        .assert_status(200);
    let res = sign_in(
        &app,
        |o| ResponseOpts {
            encrypt_for: Some(idp::SP_CERT.into()),
            ..o
        },
        "enc",
    )
    .await;
    res.assert_status(303);
    assert!(res.header("set-cookie").is_some());
}

#[tokio::test]
async fn single_logout_ends_sessions() {
    let app = bgh_server::test_app().await;
    let admin = app.create_admin("root").await;
    enable(&app, &admin, json!({})).await;
    let res = sign_in(&app, |o| o, "slo-user").await;
    let cookie = cookie_from(&res);
    app.get("/api/v3/user")
        .cookie(&cookie)
        .send()
        .await
        .assert_status(200);

    // Unsigned logout requests are refused.
    app.get("/saml/sls?SAMLRequest=abc")
        .send()
        .await
        .assert_status(403);
    let res = app
        .get(&format!(
            "/saml/sls?{}",
            idp::logout_request_query("slo-user")
        ))
        .send()
        .await;
    res.assert_status(303);
    let loc = res.header("location").unwrap();
    assert!(loc.starts_with(idp::IDP_SLO_URL), "{loc}");
    let resp = idp::redirect_message(loc, "SAMLResponse").unwrap();
    assert!(resp.contains("InResponseTo=\"_l1\""));
    assert!(resp.contains("status:Success"));
    app.get("/api/v3/user")
        .cookie(&cookie)
        .send()
        .await
        .assert_status(401);

    // SP-initiated logout hands back the IdP logout URL.
    let res = sign_in(&app, |o| o, "slo-user").await;
    let cookie = cookie_from(&res);
    let csrf = bgh_core::auth::csrf_token(cookie.trim_start_matches("bgh_session="));
    let out = app
        .post("/_bgh/saml/logout")
        .cookie(&cookie)
        .header("x-csrf-token", &csrf)
        .send()
        .await;
    out.assert_status(200);
    let to = out.json()["redirect"].as_str().unwrap().to_string();
    assert!(to.starts_with(idp::IDP_SLO_URL));
    assert!(
        idp::redirect_message(&to, "SAMLRequest")
            .unwrap()
            .contains(">slo-user<")
    );
    app.get("/api/v3/user")
        .cookie(&cookie)
        .send()
        .await
        .assert_status(401);
}

#[tokio::test]
async fn groups_attribute_syncs_mapped_teams() {
    let app = bgh_server::test_app().await;
    let admin = app.create_admin("root").await;
    let org = app.create_org("acme", &admin).await;
    enable(&app, &admin, json!({})).await;
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
    .json(&json!({ "groups": [{ "group_id": "engineering" }] }))
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
        common::logins(&v)
    };
    // (The team's creator is a member already.)
    sign_in(&app, |o| o.attr("groups", &["engineering", "other"]), "dev")
        .await
        .assert_status(303);
    assert_eq!(members().await, vec!["root", "dev"]);
    sign_in(&app, |o| o.attr("groups", &["other"]), "dev")
        .await
        .assert_status(303);
    assert_eq!(members().await, vec!["root"]);
}

#[tokio::test]
async fn admin_endpoints_and_settings_validation() {
    let app = bgh_server::test_app().await;
    let admin = app.create_admin("root").await;
    let user = app.create_user("bob").await;

    app.get("/_bgh/admin/saml")
        .auth(&user)
        .send()
        .await
        .assert_status(403);
    let info = app.get("/_bgh/admin/saml").auth(&admin).send().await.json();
    assert_eq!(info["enabled"], false);
    assert_eq!(info["entity_id"], app.base_url);
    assert_eq!(info["acs_url"], app.url("/saml/consume"));
    assert_eq!(info["metadata_url"], app.url("/saml/metadata"));
    assert_eq!(info["idp_certificates"], json!([]));

    // Login is 404 while disabled.
    app.get("/_bgh/saml/login").send().await.assert_status(404);

    // Validation.
    set_auth(&app, &admin, json!({ "saml": { "enabled": true, "idp_sso_url": "ftp://x", "idp_certificate": idp::IDP_CERT } }))
        .await
        .assert_status(422);
    set_auth(
        &app,
        &admin,
        json!({ "saml": { "enabled": true, "idp_sso_url": "https://idp.test/sso" } }),
    )
    .await
    .assert_status(422);
    // SAML counts as a sign-in method for `password_login = false`.
    set_auth(&app, &admin, json!({ "password_login": false }))
        .await
        .assert_status(422);
    enable(&app, &admin, json!({})).await;
    set_auth(&app, &admin, json!({ "password_login": false }))
        .await
        .assert_status(200);
    app.post("/_bgh/session")
        .json(&json!({ "login": "bob", "password": user.password }))
        .send()
        .await
        .assert_status(403);

    let info = app.get("/_bgh/admin/saml").auth(&admin).send().await.json();
    assert_eq!(info["enabled"], true);
    let c = &info["idp_certificates"][0];
    assert!(c["subject"].as_str().unwrap().contains("test-idp"));
    assert_eq!(c["fingerprint_sha256"].as_str().unwrap().len(), 95);
    assert_eq!(c["expired"], false);

    let kp = app
        .post("/_bgh/admin/saml/keypair")
        .auth(&admin)
        .send()
        .await;
    kp.assert_status(201);
    let kp = kp.json();
    assert!(
        kp["certificate"]
            .as_str()
            .unwrap()
            .starts_with("-----BEGIN CERTIFICATE-----")
    );
    assert!(
        kp["private_key"]
            .as_str()
            .unwrap()
            .starts_with("-----BEGIN PRIVATE KEY-----")
    );

    let der = idp::IDP_CERT
        .lines()
        .filter(|l| !l.starts_with("-----"))
        .collect::<String>();
    let md = format!(
        r#"<EntityDescriptor xmlns="urn:oasis:names:tc:SAML:2.0:metadata" entityID="https://idp.corp"><IDPSSODescriptor protocolSupportEnumeration="urn:oasis:names:tc:SAML:2.0:protocol"><KeyDescriptor use="signing"><KeyInfo xmlns="http://www.w3.org/2000/09/xmldsig#"><X509Data><X509Certificate>{der}</X509Certificate></X509Data></KeyInfo></KeyDescriptor><SingleSignOnService Binding="urn:oasis:names:tc:SAML:2.0:bindings:HTTP-Redirect" Location="https://idp.corp/sso"/></IDPSSODescriptor></EntityDescriptor>"#
    );
    let parsed = app
        .post("/_bgh/admin/saml/idp_metadata")
        .auth(&admin)
        .json(&json!({ "metadata": md }))
        .send()
        .await;
    parsed.assert_status(200);
    let parsed = parsed.json();
    assert_eq!(parsed["idp_entity_id"], "https://idp.corp");
    assert_eq!(parsed["idp_sso_url"], "https://idp.corp/sso");
    assert_eq!(parsed["idp_slo_url"], Value::Null);
    app.post("/_bgh/admin/saml/idp_metadata")
        .auth(&admin)
        .json(&json!({ "metadata": "<x/>" }))
        .send()
        .await
        .assert_status(422);
}
