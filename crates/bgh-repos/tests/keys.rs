//! Deploy keys and autolinks.

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use serde_json::json;
use sha2::{Digest, Sha256};

fn ssh_string(out: &mut Vec<u8>, s: &[u8]) {
    out.extend_from_slice(&(s.len() as u32).to_be_bytes());
    out.extend_from_slice(s);
}

/// A syntactically valid ed25519 public key (`seed` varies the key bytes)
/// and its blob.
fn ed25519(seed: u8) -> (String, Vec<u8>) {
    let mut blob = Vec::new();
    ssh_string(&mut blob, b"ssh-ed25519");
    ssh_string(&mut blob, &[seed; 32]);
    (format!("ssh-ed25519 {}", STANDARD.encode(&blob)), blob)
}

fn fingerprint(blob: &[u8]) -> String {
    format!("SHA256:{}", STANDARD_NO_PAD.encode(Sha256::digest(blob)))
}

#[tokio::test]
async fn deploy_keys_crud() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let eve = app.create_user("eve").await;
    let repo = app.create_private_repo(&alice, "secret").await;
    let repo_id = repo["id"].as_i64().unwrap();
    app.create_repo(&alice, "other").await;
    sqlx::query(
        "INSERT INTO collaborators (repo_id, user_id, permission) VALUES ($1, $2, 'write')",
    )
    .bind(repo_id)
    .bind(bob.id)
    .execute(&app.state.db)
    .await
    .unwrap();

    let (key, blob) = ed25519(1);
    let res = app
        .post("/api/v3/repos/alice/secret/keys")
        .auth(&alice)
        .json(&json!({"title": "CI", "key": format!("{key} ci@example.com\n"), "read_only": false}))
        .send()
        .await;
    res.assert_status(201);
    let v = res.json();
    let id = v["id"].as_i64().unwrap();
    assert_eq!(v["key"], key, "comment stripped");
    assert_eq!(v["title"], "CI");
    assert_eq!(v["read_only"], false);
    assert_eq!(v["verified"], true);
    assert_eq!(v["enabled"], true);
    assert_eq!(v["added_by"], "alice");
    assert!(v["last_used"].is_null());
    assert!(v["created_at"].is_string());
    assert_eq!(
        v["url"],
        app.url(&format!("/api/v3/repos/alice/secret/keys/{id}"))
    );
    let stored: String = sqlx::query_scalar("SELECT fingerprint FROM deploy_keys WHERE id = $1")
        .bind(id)
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(stored, fingerprint(&blob));

    // read_only defaults to true.
    let (key2, _) = ed25519(2);
    let res = app
        .post("/api/v3/repos/alice/secret/keys")
        .auth(&alice)
        .json(&json!({"title": "ro", "key": key2}))
        .send()
        .await;
    res.assert_status(201);
    assert_eq!(res.json()["read_only"], true);

    // Duplicate in this repo, or used as a user SSH key → 422 in use.
    let res = app
        .post("/api/v3/repos/alice/secret/keys")
        .auth(&alice)
        .json(&json!({"title": "dup", "key": key}))
        .send()
        .await;
    res.assert_status(422);
    assert_eq!(
        res.json()["errors"][0],
        json!({"resource": "PublicKey", "code": "custom", "field": "key",
               "message": "key is already in use"})
    );
    let (user_key, user_blob) = ed25519(3);
    sqlx::query(
        "INSERT INTO ssh_keys (user_id, title, key, fingerprint) VALUES ($1, 'laptop', $2, $3)",
    )
    .bind(eve.id)
    .bind(&user_key)
    .bind(fingerprint(&user_blob))
    .execute(&app.state.db)
    .await
    .unwrap();
    let res = app
        .post("/api/v3/repos/alice/secret/keys")
        .auth(&alice)
        .json(&json!({"title": "x", "key": user_key}))
        .send()
        .await;
    res.assert_status(422);
    assert_eq!(res.json()["errors"][0]["message"], "key is already in use");

    // Invalid keys.
    for bad in [
        "not a key",
        "ssh-ed25519 AAAA",
        "ssh-rsa AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl",
    ] {
        let res = app
            .post("/api/v3/repos/alice/secret/keys")
            .auth(&alice)
            .json(&json!({"title": "bad", "key": bad}))
            .send()
            .await;
        res.assert_status(422);
        assert_eq!(res.json()["errors"][0]["field"], "key");
    }
    app.post("/api/v3/repos/alice/secret/keys")
        .auth(&alice)
        .json(&json!({"title": "missing"}))
        .send()
        .await
        .assert_status(422);

    // List + pagination.
    let res = app
        .get("/api/v3/repos/alice/secret/keys")
        .auth(&alice)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json().as_array().unwrap().len(), 2);
    let res = app
        .get("/api/v3/repos/alice/secret/keys?per_page=1")
        .auth(&alice)
        .send()
        .await;
    assert_eq!(res.json()[0]["id"], id);
    assert!(res.header("link").unwrap().contains("rel=\"next\""));

    // Get one; keys of another repo are not visible here.
    let res = app
        .get(&format!("/api/v3/repos/alice/secret/keys/{id}"))
        .auth(&alice)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["title"], "CI");
    app.get(&format!("/api/v3/repos/alice/other/keys/{id}"))
        .auth(&alice)
        .send()
        .await
        .assert_status(404);
    // The same key may be a deploy key of another repository.
    app.post("/api/v3/repos/alice/other/keys")
        .auth(&alice)
        .json(&json!({"title": "CI", "key": key}))
        .send()
        .await
        .assert_status(201);

    // Permissions: writers 403, no access 404.
    app.get("/api/v3/repos/alice/secret/keys")
        .auth(&bob)
        .send()
        .await
        .assert_status(403);
    app.post("/api/v3/repos/alice/secret/keys")
        .auth(&bob)
        .json(&json!({"title": "x", "key": ed25519(9).0}))
        .send()
        .await
        .assert_status(403);
    app.get("/api/v3/repos/alice/secret/keys")
        .auth(&eve)
        .send()
        .await
        .assert_status(404);
    app.delete(&format!("/api/v3/repos/alice/secret/keys/{id}"))
        .auth(&bob)
        .send()
        .await
        .assert_status(403);

    // Delete.
    app.delete(&format!("/api/v3/repos/alice/secret/keys/{id}"))
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    app.get(&format!("/api/v3/repos/alice/secret/keys/{id}"))
        .auth(&alice)
        .send()
        .await
        .assert_status(404);
    app.delete(&format!("/api/v3/repos/alice/secret/keys/{id}"))
        .auth(&alice)
        .send()
        .await
        .assert_status(404);
}

#[tokio::test]
async fn autolinks_crud() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    app.create_repo(&alice, "hello").await;
    app.create_private_repo(&alice, "secret").await;

    let res = app
        .post("/api/v3/repos/alice/hello/autolinks")
        .auth(&alice)
        .json(&json!({"key_prefix": "JIRA-", "url_template": "https://jira.example.com/browse/JIRA-<num>"}))
        .send()
        .await;
    res.assert_status(201);
    let v = res.json();
    let id = v["id"].as_i64().unwrap();
    assert_eq!(
        v,
        json!({"id": id, "key_prefix": "JIRA-",
               "url_template": "https://jira.example.com/browse/JIRA-<num>",
               "is_alphanumeric": true})
    );
    let res = app
        .post("/api/v3/repos/alice/hello/autolinks")
        .auth(&alice)
        .json(
            &json!({"key_prefix": "TICKET-", "url_template": "https://t.example.com/<num>",
                      "is_alphanumeric": false}),
        )
        .send()
        .await;
    res.assert_status(201);
    assert_eq!(res.json()["is_alphanumeric"], false);

    // Validation.
    let res = app
        .post("/api/v3/repos/alice/hello/autolinks")
        .auth(&alice)
        .json(&json!({"key_prefix": "jira-", "url_template": "https://x/<num>"}))
        .send()
        .await;
    res.assert_status(422);
    assert_eq!(res.json()["errors"][0]["code"], "already_exists");
    assert_eq!(res.json()["errors"][0]["field"], "key_prefix");
    let res = app
        .post("/api/v3/repos/alice/hello/autolinks")
        .auth(&alice)
        .json(&json!({"key_prefix": "X-", "url_template": "https://x/"}))
        .send()
        .await;
    res.assert_status(422);
    assert_eq!(res.json()["errors"][0]["field"], "url_template");
    app.post("/api/v3/repos/alice/hello/autolinks")
        .auth(&alice)
        .json(&json!({"url_template": "https://x/<num>"}))
        .send()
        .await
        .assert_status(422);

    // List / get.
    let res = app
        .get("/api/v3/repos/alice/hello/autolinks")
        .auth(&alice)
        .send()
        .await;
    res.assert_status(200);
    let prefixes: Vec<String> = res
        .json()
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["key_prefix"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(prefixes, vec!["JIRA-", "TICKET-"]);
    let res = app
        .get(&format!("/api/v3/repos/alice/hello/autolinks/{id}"))
        .auth(&alice)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["key_prefix"], "JIRA-");

    // Non-admins: 403 on public repos, 404 on private ones.
    app.get("/api/v3/repos/alice/hello/autolinks")
        .auth(&bob)
        .send()
        .await
        .assert_status(403);
    app.post("/api/v3/repos/alice/hello/autolinks")
        .auth(&bob)
        .json(&json!({"key_prefix": "B-", "url_template": "https://b/<num>"}))
        .send()
        .await
        .assert_status(403);
    app.get("/api/v3/repos/alice/secret/autolinks")
        .auth(&bob)
        .send()
        .await
        .assert_status(404);

    // Delete.
    app.delete(&format!("/api/v3/repos/alice/hello/autolinks/{id}"))
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    app.get(&format!("/api/v3/repos/alice/hello/autolinks/{id}"))
        .auth(&alice)
        .send()
        .await
        .assert_status(404);
    app.delete(&format!("/api/v3/repos/alice/hello/autolinks/{id}"))
        .auth(&alice)
        .send()
        .await
        .assert_status(404);
}
