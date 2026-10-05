//! Users, emails, followers, blocks, SSH/GPG keys.

mod common;

use common::*;
use serde_json::json;

#[tokio::test]
async fn authenticated_user_and_patch() {
    let app = bgh_server::test_app().await;
    let ada = app.create_user("ada").await;

    let res = app.get("/api/v3/user").auth(&ada).send().await;
    res.assert_status(200);
    let v = res.json();
    assert_simple_user(&v, "ada");
    assert_eq!(v["two_factor_authentication"], false);
    for k in [
        "name",
        "company",
        "blog",
        "location",
        "email",
        "hireable",
        "bio",
        "twitter_username",
        "public_repos",
        "public_gists",
        "followers",
        "following",
        "created_at",
        "updated_at",
        "private_gists",
        "total_private_repos",
        "owned_private_repos",
        "disk_usage",
        "collaborators",
        "plan",
    ] {
        assert!(v.get(k).is_some(), "private-user missing {k}");
    }
    assert!(res.header("x-oauth-scopes").unwrap().contains("user"));
    assert_eq!(res.header("x-ratelimit-limit"), Some("5000"));
    assert_eq!(res.header("x-ratelimit-resource"), Some("core"));

    let res = app
        .patch("/api/v3/user")
        .auth(&ada)
        .json(
            &json!({"name": "Ada Lovelace", "bio": "Analyst", "twitter_username": "@ada",
                      "hireable": true, "blog": "https://ada.dev", "email": "ada@example.com"}),
        )
        .send()
        .await;
    res.assert_status(200);
    let v = res.json();
    assert_eq!(v["name"], "Ada Lovelace");
    assert_eq!(v["twitter_username"], "ada");
    assert_eq!(v["hireable"], true);
    assert_eq!(v["email"], "ada@example.com");

    // Null clears; absent keeps.
    let v = app
        .patch("/api/v3/user")
        .auth(&ada)
        .json(&json!({"bio": null}))
        .send()
        .await
        .json();
    assert_eq!(v["bio"], serde_json::Value::Null);
    assert_eq!(v["name"], "Ada Lovelace");

    // Public email must be a verified address of the user.
    let res = app
        .patch("/api/v3/user")
        .auth(&ada)
        .json(&json!({"email": "other@example.com"}))
        .send()
        .await;
    res.assert_status(422);
    assert_eq!(res.json()["errors"][0]["field"], "email");

    // The change is recorded for sync in the user scope.
    let n: i64 =
        sqlx::query_scalar("SELECT count(*) FROM sync_actions WHERE scope = $1 AND model = 'user'")
            .bind(format!("user:{}", ada.id))
            .fetch_one(&app.state.db)
            .await
            .unwrap();
    assert!(n >= 1);

    // Scope enforcement.
    let token = app.create_token(&ada, &["repo"]).await;
    app.patch("/api/v3/user")
        .token(&token)
        .json(&json!({"name": "x"}))
        .send()
        .await
        .assert_status(403);
    app.get("/api/v3/user").send().await.assert_status(401);
}

#[tokio::test]
async fn public_users_and_listing() {
    let app = bgh_server::test_app().await;
    let ada = app.create_user("ada").await;
    let bob = app.create_user("bob").await;
    let org = app.create_org("acme", &ada).await;

    let res = app.get("/api/v3/users/ADA").send().await;
    res.assert_status(200);
    let v = res.json();
    assert_eq!(v["login"], "ada");
    assert!(
        v.get("plan").is_none(),
        "public profile has no private fields"
    );
    assert_eq!(
        app.get("/api/v3/users/acme").send().await.json()["type"],
        "Organization"
    );
    app.get("/api/v3/users/nobody")
        .send()
        .await
        .assert_status(404);
    assert_eq!(
        app.get(&format!("/api/v3/user/{}", bob.id))
            .send()
            .await
            .json()["login"],
        "bob"
    );

    let res = app.get("/api/v3/users?per_page=2").send().await;
    res.assert_status(200);
    assert_eq!(logins(&res.json()), vec!["ada", "bob"]);
    let link = res.header("link").unwrap();
    assert!(
        link.contains(&format!("since={}>; rel=\"next\"", bob.id)),
        "{link}"
    );
    let res = app
        .get(&format!("/api/v3/users?since={}", bob.id))
        .send()
        .await;
    let v = res.json();
    assert_eq!(v[0]["id"], org.id);
    assert_eq!(v[0]["type"], "Organization");
}

#[tokio::test]
async fn emails_crud_and_verification() {
    let app = bgh_server::test_app().await;
    let ada = app.create_user("ada").await;

    let v = app
        .get("/api/v3/user/emails")
        .auth(&ada)
        .send()
        .await
        .json();
    assert_eq!(
        v,
        json!([{"email": "ada@example.com", "primary": true, "verified": true, "visibility": "private"}])
    );

    let res = app
        .post("/api/v3/user/emails")
        .auth(&ada)
        .json(&json!({"emails": ["ada@work.example"]}))
        .send()
        .await;
    res.assert_status(201);
    assert_eq!(
        res.json(),
        json!([{"email": "ada@work.example", "primary": false, "verified": false, "visibility": null}])
    );
    // Bare arrays work too; duplicates are rejected.
    app.post("/api/v3/user/emails")
        .auth(&ada)
        .json(&json!(["ada@work.example"]))
        .send()
        .await
        .assert_status(422);
    app.post("/api/v3/user/emails")
        .auth(&ada)
        .json(&json!(["not-an-email"]))
        .send()
        .await
        .assert_status(422);

    // Verification mail → token → verified.
    let mail = last_mail_to(&app, "ada@work.example").await;
    let token = token_after(&mail.text, "token=");
    let res = app
        .post("/_bgh/emails/verify")
        .json(&json!({"token": token}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["verified"], true);
    app.post("/_bgh/emails/verify")
        .json(&json!({"token": token}))
        .send()
        .await
        .assert_status(404);

    // Make it primary (session only), then visibility.
    let cookie = session(&app, &ada).await;
    app.put("/_bgh/user/emails/ada@work.example/primary")
        .auth(&ada)
        .send()
        .await
        .assert_status(403);
    let v = app
        .put("/_bgh/user/emails/ada@work.example/primary")
        .cookie(&cookie)
        .send()
        .await
        .json();
    assert_eq!(v[0]["email"], "ada@work.example");
    assert_eq!(v[0]["primary"], true);

    let v = app
        .patch("/api/v3/user/email/visibility")
        .auth(&ada)
        .json(&json!({"visibility": "public"}))
        .send()
        .await
        .json();
    assert_eq!(v[0]["visibility"], "public");
    assert_eq!(
        app.get("/api/v3/users/ada").send().await.json()["email"],
        "ada@work.example"
    );
    let v = app
        .get("/api/v3/user/public_emails")
        .auth(&ada)
        .send()
        .await
        .json();
    assert_eq!(v.as_array().unwrap().len(), 1);

    // Primary can't be deleted; others can.
    app.delete("/api/v3/user/emails")
        .auth(&ada)
        .json(&json!(["ada@work.example"]))
        .send()
        .await
        .assert_status(422);
    app.delete("/api/v3/user/emails")
        .auth(&ada)
        .json(&json!({"emails": ["ada@example.com"]}))
        .send()
        .await
        .assert_status(204);
    assert_eq!(
        app.get("/api/v3/user/emails")
            .auth(&ada)
            .send()
            .await
            .json()
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let token = app.create_token(&ada, &["repo"]).await;
    app.get("/api/v3/user/emails")
        .token(&token)
        .send()
        .await
        .assert_status(403);
    let token = app.create_token(&ada, &["user:email"]).await;
    app.get("/api/v3/user/emails")
        .token(&token)
        .send()
        .await
        .assert_status(200);
}

#[tokio::test]
async fn followers_and_blocks() {
    let app = bgh_server::test_app().await;
    let ada = app.create_user("ada").await;
    let bob = app.create_user("bob").await;
    let cat = app.create_user("cat").await;

    app.put("/api/v3/user/following/ada")
        .auth(&bob)
        .send()
        .await
        .assert_status(204);
    app.put("/api/v3/user/following/ada")
        .auth(&cat)
        .send()
        .await
        .assert_status(204);
    app.put("/api/v3/user/following/bob")
        .auth(&bob)
        .send()
        .await
        .assert_status(422);
    app.put("/api/v3/user/following/nobody")
        .auth(&bob)
        .send()
        .await
        .assert_status(404);

    let res = app.get("/api/v3/users/ada/followers").send().await;
    let v = res.json();
    assert_eq!(logins(&v), vec!["bob", "cat"]);
    assert_simple_user(&v[0], "bob");
    assert_eq!(
        logins(&app.get("/api/v3/users/bob/following").send().await.json()),
        vec!["ada"]
    );
    assert_eq!(
        logins(
            &app.get("/api/v3/user/followers")
                .auth(&ada)
                .send()
                .await
                .json()
        ),
        vec!["bob", "cat"]
    );
    assert_eq!(
        logins(
            &app.get("/api/v3/user/following")
                .auth(&bob)
                .send()
                .await
                .json()
        ),
        vec!["ada"]
    );
    app.get("/api/v3/user/following/ada")
        .auth(&bob)
        .send()
        .await
        .assert_status(204);
    app.get("/api/v3/user/following/cat")
        .auth(&bob)
        .send()
        .await
        .assert_status(404);
    app.get("/api/v3/users/bob/following/ada")
        .send()
        .await
        .assert_status(204);
    assert_eq!(
        app.get("/api/v3/users/ada").send().await.json()["followers"],
        2
    );

    let p = app
        .get("/api/v3/users/ada/followers?per_page=1")
        .send()
        .await;
    assert!(p.header("link").unwrap().contains("rel=\"next\""));

    // Blocking removes follows and prevents re-following.
    app.put("/api/v3/user/blocks/bob")
        .auth(&ada)
        .send()
        .await
        .assert_status(204);
    app.get("/api/v3/user/blocks/bob")
        .auth(&ada)
        .send()
        .await
        .assert_status(204);
    app.get("/api/v3/user/blocks/cat")
        .auth(&ada)
        .send()
        .await
        .assert_status(404);
    assert_eq!(
        logins(
            &app.get("/api/v3/user/blocks")
                .auth(&ada)
                .send()
                .await
                .json()
        ),
        vec!["bob"]
    );
    assert_eq!(
        logins(&app.get("/api/v3/users/ada/followers").send().await.json()),
        vec!["cat"]
    );
    app.put("/api/v3/user/following/ada")
        .auth(&bob)
        .send()
        .await
        .assert_status(403);
    app.put("/api/v3/user/blocks/ada")
        .auth(&ada)
        .send()
        .await
        .assert_status(422);
    app.delete("/api/v3/user/blocks/bob")
        .auth(&ada)
        .send()
        .await
        .assert_status(204);
    app.put("/api/v3/user/following/ada")
        .auth(&bob)
        .send()
        .await
        .assert_status(204);

    app.delete("/api/v3/user/following/ada")
        .auth(&cat)
        .send()
        .await
        .assert_status(204);
    let token = app.create_token(&cat, &["repo"]).await;
    app.put("/api/v3/user/following/ada")
        .token(&token)
        .send()
        .await
        .assert_status(403);
    let _ = cat;
}

#[tokio::test]
async fn ssh_keys() {
    let app = bgh_server::test_app().await;
    let ada = app.create_user("ada").await;
    let bob = app.create_user("bob").await;

    let key = ssh_ed25519(1);
    let res = app
        .post("/api/v3/user/keys")
        .auth(&ada)
        .json(&json!({"title": "laptop", "key": key}))
        .send()
        .await;
    res.assert_status(201);
    let v = res.json();
    let id = v["id"].as_i64().unwrap();
    assert_eq!(v["title"], "laptop");
    assert_eq!(v["key"], key.trim_end_matches(" test@example"));
    assert_eq!(v["url"], app.url(&format!("/api/v3/user/keys/{id}")));
    assert_eq!(v["verified"], true);
    assert_eq!(v["read_only"], false);
    assert!(v["created_at"].as_str().unwrap().ends_with('Z'));
    assert!(v.get("last_used").is_some());

    // Same key for another user → already in use.
    let res = app
        .post("/api/v3/user/keys")
        .auth(&bob)
        .json(&json!({"key": key}))
        .send()
        .await;
    res.assert_status(422);
    assert_eq!(res.json()["errors"][0]["message"], "key is already in use");
    let res = app
        .post("/api/v3/user/keys")
        .auth(&bob)
        .json(&json!({"key": "ssh-rsa garbage"}))
        .send()
        .await;
    res.assert_status(422);
    assert_eq!(res.json()["errors"][0]["resource"], "PublicKey");
    // Title defaults to the key comment.
    let res = app
        .post("/api/v3/user/keys")
        .auth(&bob)
        .json(&json!({"key": ssh_ed25519(2)}))
        .send()
        .await;
    assert_eq!(res.json()["title"], "test@example");

    assert_eq!(
        app.get("/api/v3/user/keys")
            .auth(&ada)
            .send()
            .await
            .json()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    app.get(&format!("/api/v3/user/keys/{id}"))
        .auth(&ada)
        .send()
        .await
        .assert_status(200);
    app.get(&format!("/api/v3/user/keys/{id}"))
        .auth(&bob)
        .send()
        .await
        .assert_status(404);
    let v = app.get("/api/v3/users/ada/keys").send().await.json();
    assert_eq!(v[0]["id"], id);
    assert!(v[0].get("title").is_none());

    let read_only = app.create_token(&ada, &["read:public_key"]).await;
    app.get("/api/v3/user/keys")
        .token(&read_only)
        .send()
        .await
        .assert_status(200);
    app.post("/api/v3/user/keys")
        .token(&read_only)
        .json(&json!({"key": ssh_ed25519(3)}))
        .send()
        .await
        .assert_status(403);
    app.delete(&format!("/api/v3/user/keys/{id}"))
        .token(&read_only)
        .send()
        .await
        .assert_status(403);
    app.delete(&format!("/api/v3/user/keys/{id}"))
        .auth(&ada)
        .send()
        .await
        .assert_status(204);
    app.delete(&format!("/api/v3/user/keys/{id}"))
        .auth(&ada)
        .send()
        .await
        .assert_status(404);
}

const GPG_KEY: &str = "-----BEGIN PGP PUBLIC KEY BLOCK-----

mDMEasNFdBYJKwYBBAHaRw8BAQdAdcVIu4pFwkkCc7pNPaa7zW5xWGgCyLNqphdF
sWpkdvi0HFRlc3QgVXNlciA8dGVzdEBleGFtcGxlLmNvbT6ImQQTFgoAQRYhBH2T
oIlAEEx1cVu3L7aDqEhQbKbbBQJqw0V0AhsDBQkB4TOABQsJCAcCAiICBhUKCQgL
AgQWAgMBAh4HAheAAAoJELaDqEhQbKbb/twA/3OZfdL37LGX3UmyBq/safBM5zf2
EKhXSfIi9zYoV220AP9CNxpG7nCX65DW3ezQttDlp8vK5vmZssxiXqDKHb6WCrQf
VGVzdCBBbHQgPEFsdC5Vc2VyQEV4YW1wbGUub3JnPoiZBBMWCgBBFiEEfZOgiUAQ
THVxW7cvtoOoSFBsptsFAmrDRXQCGwMFCQHhM4AFCwkIBwICIgIGFQoJCAsCBBYC
AwECHgcCF4AACgkQtoOoSFBsptsTNwD+O8N8hRNCB5AZXMm6plT7H0wEpoZTbIiS
9Lsm1N7DPwMA/AvPJAvFG77eh3BW2pNxCo17eVBbydmdUPZBlShn97MNuDgEasNF
dBIKKwYBBAGXVQEFAQEHQFF/9uip4j4ZaKOwCRbUDwyVUvS/JwIoLPLIoz/bbFZt
AwEIB4h4BBgWCgAgFiEEfZOgiUAQTHVxW7cvtoOoSFBsptsFAmrDRXQCGwwACgkQ
toOoSFBspttkqAD/bP60a9/0v8KjJEMuuGWaeOWvu0f13mX326SFBUbwxJIA/1a/
JDwaRZvFNw9s2wE5FcHcHym4R7A8ZmNKexZDQxEG
=qydl
-----END PGP PUBLIC KEY BLOCK-----
";

#[tokio::test]
async fn gpg_keys() {
    let app = bgh_server::test_app().await;
    let test = app.create_user("test").await; // owns test@example.com (verified)

    let res = app
        .post("/api/v3/user/gpg_keys")
        .auth(&test)
        .json(&json!({"name": "signing", "armored_public_key": GPG_KEY}))
        .send()
        .await;
    res.assert_status(201);
    let v = res.json();
    let id = v["id"].as_i64().unwrap();
    assert_eq!(v["key_id"], "B683A848506CA6DB");
    assert_eq!(v["name"], "signing");
    assert_eq!(v["primary_key_id"], serde_json::Value::Null);
    assert_eq!(v["can_sign"], true);
    assert_eq!(v["can_encrypt_comms"], false);
    assert_eq!(v["revoked"], false);
    assert!(v["expires_at"].as_str().is_some());
    assert!(
        v["raw_key"]
            .as_str()
            .unwrap()
            .contains("BEGIN PGP PUBLIC KEY BLOCK")
    );
    assert_eq!(
        v["emails"],
        json!([
            {"email": "test@example.com", "verified": true},
            {"email": "Alt.User@Example.org", "verified": false},
        ])
    );
    let sub = &v["subkeys"][0];
    assert_eq!(sub["key_id"], "D2C855506A18DA83");
    assert_eq!(sub["primary_key_id"], id);
    assert_eq!(sub["can_encrypt_storage"], true);
    assert_eq!(sub["emails"], json!([]));
    assert_eq!(sub["subkeys"], json!([]));

    app.post("/api/v3/user/gpg_keys")
        .auth(&test)
        .json(&json!({"armored_public_key": GPG_KEY}))
        .send()
        .await
        .assert_status(422);
    let res = app
        .post("/api/v3/user/gpg_keys")
        .auth(&test)
        .json(&json!({"armored_public_key": "nope"}))
        .send()
        .await;
    res.assert_status(422);
    assert_eq!(res.json()["errors"][0]["field"], "armored_public_key");

    let list = app
        .get("/api/v3/user/gpg_keys")
        .auth(&test)
        .send()
        .await
        .json();
    assert_eq!(
        list.as_array().unwrap().len(),
        1,
        "subkeys are nested, not listed"
    );
    assert_eq!(
        app.get("/api/v3/users/test/gpg_keys").send().await.json()[0]["id"],
        id
    );
    assert_eq!(
        app.get(&format!("/api/v3/user/gpg_keys/{id}"))
            .auth(&test)
            .send()
            .await
            .json()["subkeys"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let ro = app.create_token(&test, &["read:gpg_key"]).await;
    app.delete(&format!("/api/v3/user/gpg_keys/{id}"))
        .token(&ro)
        .send()
        .await
        .assert_status(403);
    app.delete(&format!("/api/v3/user/gpg_keys/{id}"))
        .auth(&test)
        .send()
        .await
        .assert_status(204);
    assert_eq!(
        app.get("/api/v3/user/gpg_keys")
            .auth(&test)
            .send()
            .await
            .json(),
        json!([])
    );
}

#[tokio::test]
async fn api_root_and_rate_limit() {
    let app = bgh_server::test_app().await;
    let ada = app.create_user("ada").await;
    let token = app.create_token(&ada, &["repo", "read:org"]).await;

    let res = app.get("/api/v3/").token(&token).send().await;
    res.assert_status(200);
    assert_eq!(res.header("x-oauth-scopes"), Some("repo, read:org"));
    assert_eq!(res.json()["current_user_url"], app.url("/api/v3/user"));
    app.get("/api/v3").send().await.assert_status(200);

    let before = app.get("/api/v3/rate_limit").token(&token).send().await;
    before.assert_status(200);
    let v = before.json();
    assert_eq!(v["resources"]["core"]["limit"], 5000);
    assert_eq!(v["rate"]["limit"], 5000);
    let used = v["rate"]["used"].as_i64().unwrap();
    assert_eq!(v["resources"]["search"]["limit"], 30);
    app.get("/api/v3/user")
        .token(&token)
        .send()
        .await
        .assert_status(200);
    let after = app
        .get("/api/v3/rate_limit")
        .token(&token)
        .send()
        .await
        .json();
    assert_eq!(
        after["rate"]["used"].as_i64().unwrap(),
        used + 1,
        "rate_limit itself isn't counted"
    );
}
