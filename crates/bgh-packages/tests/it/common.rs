//! Registry test helpers.

use bgh_core::testing::{TestApp, TestUser};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub const OCI_MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";
pub const OCI_INDEX: &str = "application/vnd.oci.image.index.v1+json";

pub fn sha256(data: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(data)))
}

/// `Authorization` value: Basic `login:token`.
pub fn basic(login: &str, token: &str) -> String {
    use base64::Engine;
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{login}:{token}"))
    )
}

/// Exchange credentials for a registry JWT; returns `Bearer <jwt>`.
pub async fn bearer(app: &TestApp, auth: Option<&str>, scope: &str) -> String {
    let mut req = app.get(&format!("/v2/token?service=x&scope={scope}"));
    if let Some(a) = auth {
        req = req.header("authorization", a);
    }
    let res = req.send().await;
    res.assert_status(200);
    format!("Bearer {}", res.json()["token"].as_str().unwrap())
}

/// Full-access JWT for `user` on `name` (user's default token has
/// write:packages; add delete:packages).
pub async fn user_bearer(app: &TestApp, user: &TestUser, name: &str) -> String {
    let token = app
        .create_token(user, &["write:packages", "delete:packages", "repo"])
        .await;
    bearer(
        app,
        Some(&basic(&user.login, &token)),
        &format!("repository:{name}:pull,push,delete"),
    )
    .await
}

/// Monolithic blob upload; returns the digest.
pub async fn push_blob(app: &TestApp, auth: &str, name: &str, data: &[u8]) -> String {
    let digest = sha256(data);
    let res = app
        .post(&format!("/v2/{name}/blobs/uploads/?digest={digest}"))
        .header("authorization", auth)
        .header("content-type", "application/octet-stream")
        .body(data.to_vec())
        .send()
        .await;
    res.assert_status(201);
    digest
}

/// Push config + layers + manifest under `reference`; returns
/// `(manifest digest, manifest bytes)`.
pub async fn push_image(
    app: &TestApp,
    auth: &str,
    name: &str,
    reference: &str,
    config: Value,
    layers: &[&[u8]],
) -> (String, Vec<u8>) {
    let config = serde_json::to_vec(&config).unwrap();
    let config_digest = push_blob(app, auth, name, &config).await;
    let mut descs = Vec::new();
    for l in layers {
        let d = push_blob(app, auth, name, l).await;
        descs.push(json!({
            "mediaType": "application/vnd.oci.image.layer.v1.tar+gzip",
            "digest": d, "size": l.len(),
        }));
    }
    let manifest = serde_json::to_vec(&json!({
        "schemaVersion": 2,
        "mediaType": OCI_MANIFEST,
        "config": {
            "mediaType": "application/vnd.oci.image.config.v1+json",
            "digest": config_digest, "size": config.len(),
        },
        "layers": descs,
    }))
    .unwrap();
    let digest = put_manifest(app, auth, name, reference, OCI_MANIFEST, &manifest).await;
    (digest, manifest)
}

pub async fn put_manifest(
    app: &TestApp,
    auth: &str,
    name: &str,
    reference: &str,
    media_type: &str,
    body: &[u8],
) -> String {
    let res = app
        .put(&format!("/v2/{name}/manifests/{reference}"))
        .header("authorization", auth)
        .header("content-type", media_type)
        .body(body.to_vec())
        .send()
        .await;
    assert_eq!(res.status(), 201, "{}", res.text());
    let digest = sha256(body);
    assert_eq!(res.header("docker-content-digest"), Some(digest.as_str()));
    digest
}

pub fn config(os: &str, arch: &str) -> Value {
    json!({ "architecture": arch, "os": os, "rootfs": { "type": "layers", "diff_ids": [] } })
}
