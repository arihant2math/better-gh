//! Git LFS: batch API, transfers, locks, accounting, GC, and the real
//! git-lfs client.

use crate::common;

use std::time::Duration;

use base64::Engine;
use bgh_core::testing::{TestApp, TestUser};
use common::*;
use serde_json::{Value, json};

const LFS: &str = "application/vnd.git-lfs+json";

fn basic(user: &TestUser) -> String {
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{}:{}", user.login, user.token))
    )
}

async fn batch(
    app: &TestApp,
    repo: &str,
    auth: Option<&str>,
    body: Value,
) -> (u16, Value, Option<String>) {
    let mut req = app
        .post(&format!("/{repo}.git/info/lfs/objects/batch"))
        .header("accept", LFS)
        .header("content-type", LFS)
        .json(&body);
    if let Some(a) = auth {
        req = req.header("authorization", a);
    }
    let res = req.send().await;
    let ct = res.header("content-type").map(str::to_string);
    (
        res.status(),
        serde_json::from_str(&res.text()).unwrap_or(Value::Null),
        ct,
    )
}

async fn upload(app: &TestApp, auth: &str, repo: &str, data: &[u8]) -> String {
    let oid = sha256_hex(data);
    let (status, v, _) = batch(
        app,
        repo,
        Some(auth),
        json!({"operation": "upload", "transfers": ["basic"], "objects": [{"oid": oid, "size": data.len()}]}),
    )
    .await;
    assert_eq!(status, 200, "{v}");
    let href = v["objects"][0]["actions"]["upload"]["href"]
        .as_str()
        .unwrap()
        .to_string();
    let path = href.strip_prefix(&app.base_url).unwrap().to_string();
    app.put(&path)
        .header("authorization", auth)
        .header("content-type", "application/octet-stream")
        .body(data.to_vec())
        .send()
        .await
        .assert_status(200);
    oid
}

#[tokio::test]
async fn batch_upload_download_verify() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let eve = app.create_user("eve").await;
    app.create_repo(&alice, "demo").await;
    app.create_repo(&eve, "other").await;
    let auth = basic(&alice);
    let data = b"large binary content".repeat(100);
    let oid = sha256_hex(&data);
    let size = data.len();

    // Anonymous upload → 401 with an LFS challenge.
    let res = app
        .post("/alice/demo.git/info/lfs/objects/batch")
        .json(&json!({"operation": "upload", "objects": [{"oid": oid, "size": size}]}))
        .send()
        .await;
    res.assert_status(401);
    assert!(res.header("lfs-authenticate").is_some());
    assert_eq!(res.header("content-type"), Some(LFS));
    assert!(res.json()["message"].is_string());
    // Reader (public repo, no write) → 403.
    let (status, _, _) = batch(
        &app,
        "alice/demo",
        Some(&basic(&eve)),
        json!({"operation": "upload", "objects": [{"oid": oid, "size": size}]}),
    )
    .await;
    assert_eq!(status, 403);

    // Upload negotiation.
    let (status, v, ct) = batch(
        &app,
        "alice/demo",
        Some(&auth),
        json!({"operation": "upload", "transfers": ["basic"], "ref": {"name": "refs/heads/main"},
               "objects": [{"oid": oid, "size": size}, {"oid": "nothex", "size": 1}],
               "hash_algo": "sha256"}),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(ct.as_deref(), Some(LFS));
    assert_eq!(v["transfer"], "basic");
    assert_eq!(v["hash_algo"], "sha256");
    let obj = &v["objects"][0];
    assert_eq!(obj["oid"], oid.as_str());
    assert_eq!(obj["authenticated"], true);
    let up = &obj["actions"]["upload"];
    assert_eq!(
        up["href"],
        app.url(&format!("/alice/demo.git/info/lfs/objects/{oid}"))
    );
    assert_eq!(up["header"]["Authorization"], auth.as_str());
    assert_eq!(up["expires_in"], 3600);
    assert_eq!(
        obj["actions"]["verify"]["href"],
        app.url(&format!("/alice/demo.git/info/lfs/objects/{oid}/verify"))
    );
    assert_eq!(v["objects"][1]["error"]["code"], 422);

    // Bad content is rejected; nothing is linked.
    let path = format!("/alice/demo.git/info/lfs/objects/{oid}");
    app.put(&path)
        .header("authorization", &auth)
        .body(b"wrong".to_vec())
        .send()
        .await
        .assert_status(422);
    app.put(&path)
        .header("authorization", &auth)
        .body(data.clone())
        .send()
        .await
        .assert_status(200);
    // Idempotent re-upload.
    app.put(&path)
        .header("authorization", &auth)
        .body(data.clone())
        .send()
        .await
        .assert_status(200);
    let lfs_size: i64 = sqlx::query_scalar("SELECT lfs_size FROM repositories WHERE name = 'demo'")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(lfs_size, size as i64, "size accounted once");

    // Verify.
    app.post(&format!("{path}/verify"))
        .header("authorization", &auth)
        .json(&json!({"oid": oid, "size": size}))
        .send()
        .await
        .assert_status(200);
    app.post(&format!("{path}/verify"))
        .header("authorization", &auth)
        .json(&json!({"oid": oid, "size": size + 1}))
        .send()
        .await
        .assert_status(422);

    // Already uploaded: no actions.
    let (_, v, _) = batch(
        &app,
        "alice/demo",
        Some(&auth),
        json!({"operation": "upload", "objects": [{"oid": oid, "size": size}]}),
    )
    .await;
    assert!(v["objects"][0]["actions"].is_null(), "{v}");

    // Download (anonymous on a public repo).
    let (status, v, _) = batch(
        &app,
        "alice/demo",
        None,
        json!({"operation": "download", "objects": [{"oid": oid, "size": size}]}),
    )
    .await;
    assert_eq!(status, 200);
    let href = v["objects"][0]["actions"]["download"]["href"]
        .as_str()
        .unwrap();
    assert!(v["objects"][0]["actions"]["download"]["header"].is_null());
    let res = app
        .get(href.strip_prefix(&app.base_url).unwrap())
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.text().as_bytes(), &data[..]);
    assert_eq!(
        res.header("content-length"),
        Some(size.to_string().as_str())
    );

    // Objects are scoped per repository even though storage is shared.
    let (_, v, _) = batch(
        &app,
        "eve/other",
        None,
        json!({"operation": "download", "objects": [{"oid": oid, "size": size}]}),
    )
    .await;
    assert_eq!(v["objects"][0]["error"]["code"], 404);
    app.get(&format!("/eve/other.git/info/lfs/objects/{oid}"))
        .send()
        .await
        .assert_status(404);

    // Unknown operation / hash algo.
    let (status, _, _) = batch(
        &app,
        "alice/demo",
        Some(&auth),
        json!({"operation": "delete", "objects": []}),
    )
    .await;
    assert_eq!(status, 422);
    let (status, _, _) = batch(
        &app,
        "alice/demo",
        Some(&auth),
        json!({"operation": "download", "objects": [], "hash_algo": "sha512"}),
    )
    .await;
    assert_eq!(status, 409);
}

#[tokio::test]
async fn private_repos_and_remote_auth_tokens() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    app.create_private_repo(&alice, "secret").await;
    let id = repo_id(&app, &alice, "secret").await;
    let body = json!({"operation": "download", "objects": []});
    let (status, _, _) = batch(&app, "alice/secret", None, body.clone()).await;
    assert_eq!(status, 401);
    let (status, _, _) = batch(
        &app,
        "alice/secret",
        Some("Basic Ym9ndXM6Ym9ndXM="),
        body.clone(),
    )
    .await;
    assert_eq!(status, 401);
    // Password auth works like git over HTTP.
    let pw = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("alice:{}", alice.password))
    );
    let (status, _, _) = batch(&app, "alice/secret", Some(&pw), body.clone()).await;
    assert_eq!(status, 200);

    // Tokens issued by git-lfs-authenticate (SSH).
    let token = bgh_repos::lfs::issue_grant(
        &app.state,
        &bgh_repos::lfs::LfsGrant {
            repo_id: id,
            user_id: Some(alice.id),
            deploy_key_id: None,
            write: true,
        },
    )
    .await
    .unwrap();
    let ra = format!("RemoteAuth {token}");
    let data = b"via ssh token";
    let oid = upload(&app, &ra, "alice/secret", data).await;
    let (_, v, _) = batch(
        &app,
        "alice/secret",
        Some(&ra),
        json!({"operation": "download", "objects": [{"oid": oid, "size": data.len()}]}),
    )
    .await;
    assert_eq!(
        v["objects"][0]["actions"]["download"]["header"]["Authorization"],
        ra.as_str()
    );
    let (status, _, _) = batch(&app, "alice/secret", Some("RemoteAuth nope"), body.clone()).await;
    assert_eq!(status, 401);

    // A deploy-key grant that is read-only can't upload.
    let ro = bgh_repos::lfs::issue_grant(
        &app.state,
        &bgh_repos::lfs::LfsGrant {
            repo_id: id,
            user_id: None,
            deploy_key_id: Some(1),
            write: false,
        },
    )
    .await
    .unwrap();
    let (status, _, _) = batch(
        &app,
        "alice/secret",
        Some(&format!("RemoteAuth {ro}")),
        json!({"operation": "upload", "objects": [{"oid": oid, "size": data.len()}]}),
    )
    .await;
    assert_eq!(status, 403);
    // Tokens are bound to their repository.
    app.create_repo(&alice, "other").await;
    let (status, _, _) = batch(&app, "alice/other", Some(&ra), body).await;
    assert_eq!(status, 404);
}

#[tokio::test]
async fn locks_api() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let eve = app.create_user("eve").await;
    app.create_repo(&alice, "demo").await;
    add_collaborator(&app, &alice, "demo", &bob, "write").await;
    let base = "/alice/demo.git/info/lfs/locks";

    let res = app
        .post(base)
        .header("authorization", &basic(&alice))
        .json(&json!({"path": "assets/a.psd", "ref": {"name": "refs/heads/main"}}))
        .send()
        .await;
    res.assert_status(201);
    assert_eq!(res.header("content-type"), Some(LFS));
    let lock = res.json()["lock"].clone();
    assert_eq!(lock["path"], "assets/a.psd");
    assert_eq!(lock["owner"]["name"], "alice");
    assert!(lock["locked_at"].as_str().unwrap().ends_with('Z'));
    let id = lock["id"].as_str().unwrap().to_string();

    let res = app
        .post(base)
        .header("authorization", &basic(&bob))
        .json(&json!({"path": "assets/a.psd"}))
        .send()
        .await;
    res.assert_status(409);
    assert_eq!(res.json()["lock"]["id"], id.as_str());

    app.post(base)
        .header("authorization", &basic(&eve))
        .json(&json!({"path": "x"}))
        .send()
        .await
        .assert_status(403);

    app.post(base)
        .header("authorization", &basic(&bob))
        .json(&json!({"path": "b.bin"}))
        .send()
        .await
        .assert_status(201);

    // List (read access suffices, even anonymous on a public repo).
    let v = app.get(base).send().await.json();
    assert_eq!(v["locks"].as_array().unwrap().len(), 2);
    let v = app
        .get(&format!("{base}?path=assets/a.psd"))
        .send()
        .await
        .json();
    assert_eq!(v["locks"].as_array().unwrap().len(), 1);
    let v = app.get(&format!("{base}?limit=1")).send().await.json();
    assert_eq!(v["locks"].as_array().unwrap().len(), 1);
    let next = v["next_cursor"].as_str().unwrap().to_string();
    let v = app
        .get(&format!("{base}?limit=1&cursor={next}"))
        .send()
        .await
        .json();
    assert_eq!(v["locks"][0]["path"], "b.bin");
    assert!(v["next_cursor"].is_null());
    let v = app.get(&format!("{base}?id={id}")).send().await.json();
    assert_eq!(v["locks"][0]["id"], id.as_str());

    // Verify: ours / theirs.
    let v = app
        .post(&format!("{base}/verify"))
        .header("authorization", &basic(&bob))
        .json(&json!({"ref": {"name": "refs/heads/main"}}))
        .send()
        .await
        .json();
    assert_eq!(v["ours"][0]["path"], "b.bin");
    assert_eq!(v["theirs"][0]["path"], "assets/a.psd");

    // Unlock: others' locks need force + admin.
    let unlock = format!("{base}/{id}/unlock");
    app.post(&unlock)
        .header("authorization", &basic(&bob))
        .json(&json!({}))
        .send()
        .await
        .assert_status(403);
    app.post(&unlock)
        .header("authorization", &basic(&bob))
        .json(&json!({"force": true}))
        .send()
        .await
        .assert_status(403);
    let res = app
        .post(&unlock)
        .header("authorization", &basic(&alice))
        .json(&json!({"force": false}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["lock"]["id"], id.as_str());
    app.post(&unlock)
        .header("authorization", &basic(&alice))
        .send()
        .await
        .assert_status(404);
}

#[tokio::test]
async fn raw_resolves_pointers_and_gc() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    app.create_repo(&alice, "demo").await;
    let data = vec![7u8; 5000];
    let oid = upload(&app, &basic(&alice), "alice/demo", &data).await;

    let tmp = tempfile::tempdir().unwrap();
    let w = tmp.path();
    init_work(w).await;
    let pointer = format!(
        "version https://git-lfs.github.com/spec/v1\noid sha256:{oid}\nsize {}\n",
        data.len()
    );
    let missing = format!(
        "version https://git-lfs.github.com/spec/v1\noid sha256:{}\nsize 3\n",
        "0".repeat(64)
    );
    commit_files(
        w,
        &[
            ("data.bin", pointer.as_bytes()),
            ("gone.bin", missing.as_bytes()),
        ],
        "lfs",
        ("A", "a@example.com"),
    )
    .await;
    push(&app, &alice, w, "alice", "demo", &["main"]).await;

    let res = app.get("/alice/demo/raw/main/data.bin").send().await;
    res.assert_status(200);
    assert_eq!(res.text().as_bytes(), &data[..]);
    assert_eq!(res.header("content-type"), Some("application/octet-stream"));
    // Unknown objects fall back to the pointer text.
    let res = app.get("/alice/demo/raw/main/gone.bin").send().await;
    assert_eq!(res.text(), missing);

    let v = app
        .get("/_bgh/repos/alice/demo/blob/main/data.bin")
        .send()
        .await
        .json();
    assert_eq!(v["lfs"]["oid"], oid.as_str());
    assert_eq!(v["lfs"]["size"], 5000);
    assert_eq!(v["lfs"]["stored"], true);
    let v = app
        .get("/_bgh/repos/alice/demo/blob/main/gone.bin")
        .send()
        .await
        .json();
    assert_eq!(v["lfs"]["stored"], false);

    // Deleting the repository unlinks objects; GC removes the files.
    let store = bgh_repos::lfs::object_store(&app.state);
    assert!(store.size(&oid).await.is_some());
    app.delete("/api/v3/repos/alice/demo")
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    // The listener enqueues `repos.lfs_gc`; with the default grace period
    // the fresh object survives, a zero grace collects it.
    tokio::time::sleep(Duration::from_millis(200)).await;
    app.drain_jobs().await;
    assert!(
        store.size(&oid).await.is_some(),
        "grace period protects new objects"
    );
    // Kept while the repository is restorable, collected once purged.
    assert_eq!(
        bgh_repos::lfs::gc::collect(&app.state, Duration::ZERO)
            .await
            .unwrap(),
        0
    );
    app.purge_deleted_repos().await;
    let removed = bgh_repos::lfs::gc::collect(&app.state, Duration::ZERO)
        .await
        .unwrap();
    assert_eq!(removed, 1);
    assert!(store.size(&oid).await.is_none());
}

/// End-to-end with the real git-lfs client over HTTP: push, clone + pull,
/// and lock commands.
#[tokio::test]
async fn git_lfs_client_roundtrip() {
    if std::process::Command::new("git-lfs")
        .arg("version")
        .output()
        .is_err()
    {
        eprintln!("git-lfs not installed; skipping");
        return;
    }
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    app.create_repo(&alice, "media").await;
    let remote = app.git_remote(&alice, "alice", "media");

    let tmp = tempfile::tempdir().unwrap();
    let w = tmp.path().join("work");
    std::fs::create_dir(&w).unwrap();
    init_work(&w).await;
    ok(git(&w, &["lfs", "install", "--local"]).await);
    ok(git(&w, &["lfs", "track", "*.bin"]).await);
    let payload: Vec<u8> = (0..200_000u32).map(|i| (i * 7 % 251) as u8).collect();
    commit_files(
        &w,
        &[("asset.bin", &payload), ("readme.txt", b"hi\n")],
        "add asset",
        ("A", "a@example.com"),
    )
    .await;
    ok(git(&w, &["config", "lfs.locksverify", "false"]).await);
    ok(git(&w, &["push", remote.as_str(), "main"]).await);
    app.drain_jobs().await;
    let oid = sha256_hex(&payload);
    let lfs_size: i64 =
        sqlx::query_scalar("SELECT lfs_size FROM repositories WHERE name = 'media'")
            .fetch_one(&app.state.db)
            .await
            .unwrap();
    assert_eq!(lfs_size, payload.len() as i64);
    // The git tree holds a pointer; raw serves the content.
    let res = app.get("/alice/media/raw/main/asset.bin").send().await;
    res.assert_status(200);
    assert_eq!(
        res.header("content-length"),
        Some(payload.len().to_string().as_str())
    );
    let blob = app
        .get("/_bgh/repos/alice/media/blob/main/asset.bin")
        .send()
        .await
        .json();
    assert_eq!(blob["lfs"]["oid"], oid.as_str());

    // Fresh clone + pull.
    let c = tmp.path().join("clone");
    ok(git(
        tmp.path(),
        &["clone", "-q", remote.as_str(), c.to_str().unwrap()],
    )
    .await);
    ok(git(&c, &["lfs", "install", "--local"]).await);
    ok(git(&c, &["lfs", "pull"]).await);
    assert_eq!(std::fs::read(c.join("asset.bin")).unwrap(), payload);

    // Locks via the CLI.
    ok(git(
        &w,
        &[
            "config",
            "lfs.url",
            &format!("{}/info/lfs", remote.trim_end_matches('/')),
        ],
    )
    .await);
    let out = ok(git(&w, &["lfs", "lock", "asset.bin"]).await);
    assert!(out.stdout.contains("Locked"), "{}", out.stdout);
    let out = ok(git(&w, &["lfs", "locks"]).await);
    assert!(
        out.stdout.contains("asset.bin") && out.stdout.contains("alice"),
        "{}",
        out.stdout
    );
    ok(git(&w, &["lfs", "unlock", "asset.bin"]).await);
    let out = ok(git(&w, &["lfs", "locks"]).await);
    assert!(!out.stdout.contains("asset.bin"), "{}", out.stdout);
}
