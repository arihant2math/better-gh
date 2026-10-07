//! `/user/ssh_signing_keys` and `/users/{username}/ssh_signing_keys` (P25).

use base64::Engine;
use serde_json::{Value, json};

/// A syntactically valid ed25519 public key line (the key bytes are `seed`).
fn ed25519(seed: u8, comment: &str) -> String {
    let mut blob = Vec::new();
    for part in [b"ssh-ed25519".as_slice(), &[seed; 32]] {
        blob.extend_from_slice(&(part.len() as u32).to_be_bytes());
        blob.extend_from_slice(part);
    }
    let b64 = base64::engine::general_purpose::STANDARD.encode(blob);
    format!("ssh-ed25519 {b64} {comment}").trim().to_string()
}

fn keys_of(v: &Value) -> Vec<&str> {
    let mut k: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    k.sort();
    k
}

#[tokio::test]
async fn ssh_signing_keys_crud() {
    let app = bgh_server::test_app().await;
    let ada = app.create_user("ada").await;
    let bob = app.create_user("bob").await;

    let res = app
        .post("/api/v3/user/ssh_signing_keys")
        .auth(&ada)
        .json(&json!({"key": ed25519(1, "ada@laptop")}))
        .send()
        .await;
    res.assert_status(201);
    let v = res.json();
    assert_eq!(keys_of(&v), ["created_at", "id", "key", "title"]);
    assert_eq!(
        v["title"], "ada@laptop",
        "title defaults to the key comment"
    );
    assert_eq!(v["key"], ed25519(1, ""), "stored without the comment");
    let id = v["id"].as_i64().unwrap();

    // Validation.
    for (body, field_msg) in [
        (json!({"title": "x"}), None),
        (json!({"key": "ssh-ed25519 AAAA"}), Some("key is invalid")),
        (
            json!({"key": ed25519(1, "again")}),
            Some("key is already in use"),
        ),
    ] {
        let res = app
            .post("/api/v3/user/ssh_signing_keys")
            .auth(&ada)
            .json(&body)
            .send()
            .await;
        res.assert_status(422);
        let e = &res.json()["errors"][0];
        assert_eq!(e["resource"], "SshSigningKey");
        assert_eq!(e["field"], "key");
        if let Some(m) = field_msg {
            assert!(e["message"].as_str().unwrap().contains(m), "{e}");
        } else {
            assert_eq!(e["code"], "missing_field");
        }
    }
    // The same key may also be an authentication key.
    app.post("/api/v3/user/keys")
        .auth(&ada)
        .json(&json!({"title": "auth", "key": ed25519(1, "")}))
        .send()
        .await
        .assert_status(201);

    app.post("/api/v3/user/ssh_signing_keys")
        .auth(&ada)
        .json(&json!({"title": "desktop", "key": ed25519(2, "")}))
        .send()
        .await
        .assert_status(201);

    // Lists, pagination, single key, public list.
    let res = app
        .get("/api/v3/user/ssh_signing_keys?per_page=1")
        .auth(&ada)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json().as_array().unwrap().len(), 1);
    assert!(res.header("link").unwrap().contains("rel=\"next\""));
    let res = app
        .get(&format!("/api/v3/user/ssh_signing_keys/{id}"))
        .auth(&ada)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["id"], id);
    let public = app.get("/api/v3/users/ada/ssh_signing_keys").send().await;
    public.assert_status(200);
    assert_eq!(public.json().as_array().unwrap().len(), 2);
    assert_eq!(public.json()[1]["title"], "desktop");
    app.get("/api/v3/users/nobody/ssh_signing_keys")
        .send()
        .await
        .assert_status(404);

    // Someone else's key is not found.
    app.get(&format!("/api/v3/user/ssh_signing_keys/{id}"))
        .auth(&bob)
        .send()
        .await
        .assert_status(404);
    app.delete(&format!("/api/v3/user/ssh_signing_keys/{id}"))
        .auth(&bob)
        .send()
        .await
        .assert_status(404);

    // Scopes: read < write < admin.
    let read = app.create_token(&ada, &["read:ssh_signing_key"]).await;
    let write = app.create_token(&ada, &["write:ssh_signing_key"]).await;
    let none = app.create_token(&ada, &["repo"]).await;
    app.get("/api/v3/user/ssh_signing_keys")
        .token(&none)
        .send()
        .await
        .assert_status(403);
    app.get("/api/v3/user/ssh_signing_keys")
        .token(&read)
        .send()
        .await
        .assert_status(200);
    app.post("/api/v3/user/ssh_signing_keys")
        .token(&read)
        .json(&json!({"key": ed25519(3, "")}))
        .send()
        .await
        .assert_status(403);
    app.post("/api/v3/user/ssh_signing_keys")
        .token(&write)
        .json(&json!({"key": ed25519(3, "")}))
        .send()
        .await
        .assert_status(201);
    app.delete(&format!("/api/v3/user/ssh_signing_keys/{id}"))
        .token(&write)
        .send()
        .await
        .assert_status(403);
    app.delete(&format!("/api/v3/user/ssh_signing_keys/{id}"))
        .auth(&ada)
        .send()
        .await
        .assert_status(204);
    app.get(&format!("/api/v3/user/ssh_signing_keys/{id}"))
        .auth(&ada)
        .send()
        .await
        .assert_status(404);
    app.get("/api/v3/user/ssh_signing_keys")
        .auth(&bob)
        .send()
        .await
        .assert_status(200);
    app.get("/api/v3/user/ssh_signing_keys")
        .send()
        .await
        .assert_status(401);
}
