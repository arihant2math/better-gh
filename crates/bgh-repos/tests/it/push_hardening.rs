//! Push and ref hardening (real git CLI over smart HTTP): hidden
//! `refs/pull/*`, receive-side fsck, file and push size limits, storage
//! quotas (git and LFS), and the repository config upgrade.

use crate::gitwork::*;

use bgh_core::testing::{TestApp, TestUser};
use serde_json::json;

async fn store_git_settings(app: &TestApp, value: serde_json::Value) {
    bgh_core::settings::store_section(&app.state.db, "git", &value)
        .await
        .unwrap();
    bgh_core::settings::invalidate(&app.state);
}

/// `git push` without `-q`, so remote messages show up in stderr.
async fn push_loud(work: &Work, refspec: &str) -> GitOutput {
    work.run(&["push", &work.remote, refspec]).await
}

async fn remote_refs(work: &Work) -> String {
    ok(work.run(&["ls-remote", &work.remote]).await).stdout
}

async fn branch_sha(app: &TestApp, user: &TestUser, repo: &str, branch: &str) -> Option<String> {
    let r = app
        .get(&format!(
            "/api/v3/repos/{}/{repo}/git/ref/heads/{branch}",
            user.login
        ))
        .auth(user)
        .send()
        .await;
    (r.status() == 200).then(|| r.json()["object"]["sha"].as_str().unwrap().to_string())
}

/// Deterministic incompressible bytes.
fn noise(len: usize) -> Vec<u8> {
    let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

#[tokio::test]
async fn pull_refs_are_hidden_from_pushes() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let work = seeded(&app, &alice, "hidden", &[("README.md", "hi\n")]).await;
    let head = work.head().await;
    let id = repo_id(&app, &alice, "hidden").await;
    let store = bgh_repos::store(&app.state);

    // Internal writers (bgh-pulls mirror_head / test merges) still work.
    bgh_git::merge::force_ref(&store, id, "refs/pull/1/head", &head)
        .await
        .unwrap();
    // Fetches still see them (like GitHub).
    assert!(remote_refs(&work).await.contains("refs/pull/1/head"));

    // A push that only touches refs/pull/* is refused up front.
    let other = work.commit(&[("x.txt", "x\n")], "second").await;
    let out = push_loud(&work, "HEAD:refs/pull/1/head").await;
    assert!(!out.ok);
    assert!(
        out.stderr.contains("deny updating a hidden ref"),
        "{}",
        out.stderr
    );
    let out = push_loud(&work, ":refs/pull/1/head").await;
    assert!(!out.ok);
    assert!(
        out.stderr.contains("deny updating a hidden ref"),
        "{}",
        out.stderr
    );
    let out = push_loud(&work, "HEAD:refs/bgh/internal").await;
    assert!(!out.ok);
    let refs = remote_refs(&work).await;
    assert!(
        refs.contains(&format!("{head}\trefs/pull/1/head")),
        "{refs}"
    );
    assert!(!refs.contains("refs/bgh/"), "{refs}");

    // `git push --mirror` of a clone carrying refs/pull/*: only those refs
    // are rejected, the branches go through.
    ok(work.run(&["branch", "feature"]).await);
    ok(work.run(&["update-ref", "refs/pull/2/head", &other]).await);
    let out = work.run(&["push", "--mirror", &work.remote]).await;
    assert!(!out.ok, "hidden refs are rejected");
    assert!(
        out.stderr.contains("deny updating a hidden ref"),
        "{}",
        out.stderr
    );
    let refs = remote_refs(&work).await;
    assert!(
        refs.contains(&format!("{other}\trefs/heads/main")),
        "{refs}"
    );
    assert!(
        refs.contains(&format!("{other}\trefs/heads/feature")),
        "{refs}"
    );
    assert!(!refs.contains("refs/pull/2/head"), "{refs}");
    assert!(
        refs.contains(&format!("{head}\trefs/pull/1/head")),
        "{refs}"
    );

    // And internal writers can still move them afterwards.
    bgh_git::merge::force_ref(&store, id, "refs/pull/1/head", &other)
        .await
        .unwrap();
    let refs = remote_refs(&work).await;
    assert!(
        refs.contains(&format!("{other}\trefs/pull/1/head")),
        "{refs}"
    );
}

#[tokio::test]
async fn refs_api_rejects_hidden_refs() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let work = seeded(&app, &alice, "api", &[("README.md", "hi\n")]).await;
    let head = work.head().await;
    let id = repo_id(&app, &alice, "api").await;
    bgh_git::merge::force_ref(&bgh_repos::store(&app.state), id, "refs/pull/1/head", &head)
        .await
        .unwrap();

    let r = app
        .post("/api/v3/repos/alice/api/git/refs")
        .auth(&alice)
        .json(&json!({"ref": "refs/pull/7/head", "sha": head}))
        .send()
        .await;
    r.assert_status(422);
    let body = r.json();
    assert!(
        body["message"].as_str().unwrap().contains("hidden ref"),
        "{body}"
    );
    assert!(body["documentation_url"].is_string(), "{body}");

    let r = app
        .patch("/api/v3/repos/alice/api/git/refs/pull/1/head")
        .auth(&alice)
        .json(&json!({"sha": head, "force": true}))
        .send()
        .await;
    r.assert_status(422);
    app.delete("/api/v3/repos/alice/api/git/refs/pull/1/head")
        .auth(&alice)
        .send()
        .await
        .assert_status(422);
    app.post("/api/v3/repos/alice/api/git/refs")
        .auth(&alice)
        .json(&json!({"ref": "refs/bgh/x", "sha": head}))
        .send()
        .await
        .assert_status(422);
    // Reading them works.
    app.get("/api/v3/repos/alice/api/git/ref/pull/1/head")
        .auth(&alice)
        .send()
        .await
        .assert_status(200);
}

#[tokio::test]
async fn fsck_rejects_malicious_gitmodules() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let work = seeded(&app, &alice, "fsck", &[("README.md", "hi\n")]).await;
    let before = branch_sha(&app, &alice, "fsck", "main").await;

    work.commit(
        &[(
            ".gitmodules",
            "[submodule \"evil\"]\n\tpath = evil\n\turl = --upload-pack=touch /tmp/pwned\n",
        )],
        "option injection",
    )
    .await;
    let out = push_loud(&work, "main").await;
    assert!(!out.ok, "malicious url must be rejected");
    assert!(out.stderr.contains("gitmodulesUrl"), "{}", out.stderr);
    assert_eq!(branch_sha(&app, &alice, "fsck", "main").await, before);

    ok(work.run(&["reset", "-q", "--hard", "HEAD~1"]).await);
    work.commit(
        &[(
            ".gitmodules",
            "[submodule \"../../hooks\"]\n\tpath = x\n\turl = https://example.com/x.git\n",
        )],
        "path traversal",
    )
    .await;
    let out = push_loud(&work, "main").await;
    assert!(!out.ok, "path traversal must be rejected");
    assert!(out.stderr.contains("gitmodulesName"), "{}", out.stderr);
    assert_eq!(branch_sha(&app, &alice, "fsck", "main").await, before);

    // The site setting turns the check off.
    store_git_settings(&app, json!({"fsck_on_push": false})).await;
    ok(push_loud(&work, "main").await);
}

#[tokio::test]
async fn large_files_are_rejected_with_gh001() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let work = seeded(&app, &alice, "big", &[("README.md", "hi\n")]).await;
    let before = branch_sha(&app, &alice, "big", "main").await;

    // 60 MB (zeros: the size check is on the uncompressed blob) → warning.
    std::fs::write(work.path().join("medium.bin"), vec![0u8; 60 * 1024 * 1024]).unwrap();
    work.commit(&[], "medium file").await;
    let out = ok(push_loud(&work, "main").await);
    assert!(
        out.stderr
            .contains("warning: File medium.bin is 60.00 MB; this is larger than"),
        "{}",
        out.stderr
    );
    assert!(
        out.stderr.contains("warning: GH001: Large files detected."),
        "{}",
        out.stderr
    );
    let medium = work.head().await;
    assert_eq!(
        branch_sha(&app, &alice, "big", "main").await.as_deref(),
        Some(medium.as_str())
    );
    assert_ne!(before.as_deref(), Some(medium.as_str()));

    // 101 MB → rejected, nothing changes.
    std::fs::create_dir_all(work.path().join("dir")).unwrap();
    std::fs::write(
        work.path().join("dir/huge.bin"),
        vec![0u8; 101 * 1024 * 1024],
    )
    .unwrap();
    work.commit(&[], "huge file").await;
    let out = push_loud(&work, "main").await;
    assert!(!out.ok);
    assert!(
        out.stderr
            .contains("GH001: Large files detected. You may want to try Git Large File Storage"),
        "{}",
        out.stderr
    );
    assert!(
        out.stderr.contains("error: File dir/huge.bin is 101.00 MB"),
        "{}",
        out.stderr
    );
    assert_eq!(
        branch_sha(&app, &alice, "big", "main").await.as_deref(),
        Some(medium.as_str())
    );

    // The limit is a site setting.
    store_git_settings(
        &app,
        json!({"max_object_size_mb": 200, "warn_object_size_mb": 150}),
    )
    .await;
    let out = ok(push_loud(&work, "main").await);
    assert!(!out.stderr.contains("GH001"), "{}", out.stderr);
}

#[tokio::test]
async fn push_size_limit() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let work = seeded(&app, &alice, "pack", &[("README.md", "hi\n")]).await;
    store_git_settings(&app, json!({"max_push_size_mb": 1})).await;
    let before = branch_sha(&app, &alice, "pack", "main").await;
    std::fs::write(work.path().join("noise.bin"), noise(3 * 1024 * 1024)).unwrap();
    work.commit(&[], "noise").await;
    let out = push_loud(&work, "main").await;
    assert!(!out.ok, "push larger than receive.maxInputSize is rejected");
    // git rejects the pack and hangs up while the client may still be
    // uploading it; under load the client then reports the broken pipe
    // instead of git's message. Either way nothing was accepted.
    let disconnected =
        out.stderr.contains("unexpected disconnect") || out.stderr.contains("hung up unexpectedly");
    assert!(
        out.stderr.contains("pack exceeds maximum allowed size") || disconnected,
        "{}",
        out.stderr
    );
    assert_eq!(branch_sha(&app, &alice, "pack", "main").await, before);
}

#[tokio::test]
async fn quota_is_checked_against_the_incoming_push_and_lfs() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let work = seeded(&app, &alice, "quota", &[("README.md", "hi\n")]).await;
    let before = branch_sha(&app, &alice, "quota", "main").await;
    sqlx::query("INSERT INTO storage_quotas (owner_id, max_repo_size_mb) VALUES ($1, 2)")
        .bind(alice.id)
        .execute(&app.state.db)
        .await
        .unwrap();

    // The repository is under quota, but this one push would overshoot it.
    std::fs::write(work.path().join("noise.bin"), noise(3 * 1024 * 1024)).unwrap();
    work.commit(&[], "noise").await;
    let out = push_loud(&work, "main").await;
    assert!(!out.ok, "over-quota push must be rejected");
    assert!(
        out.stderr
            .contains("This push would put the repository over its size limit (2 MB)."),
        "{}",
        out.stderr
    );
    assert_eq!(branch_sha(&app, &alice, "quota", "main").await, before);

    // LFS counts too: a batch upload that doesn't fit gets 507.
    let oid = "a".repeat(64);
    let r = app
        .post("/alice/quota.git/info/lfs/objects/batch")
        .basic("alice", &alice.password)
        .header("accept", "application/vnd.git-lfs+json")
        .header("content-type", "application/vnd.git-lfs+json")
        .json(&json!({
            "operation": "upload",
            "transfers": ["basic"],
            "objects": [{"oid": oid, "size": 3 * 1024 * 1024}],
        }))
        .send()
        .await;
    r.assert_status(507);
    assert_eq!(
        r.header("content-type"),
        Some("application/vnd.git-lfs+json")
    );
    assert!(
        r.json()["message"].as_str().unwrap().contains("size limit"),
        "{}",
        r.text()
    );
    // ...and so does a direct upload.
    let r = app
        .put(&format!("/alice/quota.git/info/lfs/objects/{oid}"))
        .basic("alice", &alice.password)
        .header("content-type", "application/octet-stream")
        .header("content-length", &(3 * 1024 * 1024).to_string())
        .body(vec![0u8; 3 * 1024 * 1024])
        .send()
        .await;
    r.assert_status(507);

    // Small LFS objects still fit.
    let small = b"small object".to_vec();
    let r = app
        .post("/alice/quota.git/info/lfs/objects/batch")
        .basic("alice", &alice.password)
        .json(&json!({
            "operation": "upload",
            "objects": [{"oid": sha256_hex(&small), "size": small.len()}],
        }))
        .send()
        .await;
    r.assert_status(200);

    // Once LFS storage alone exceeds the limit, git pushes are refused too.
    sqlx::query("UPDATE repositories SET lfs_size = 3 * 1024 * 1024 WHERE owner_id = $1")
        .bind(alice.id)
        .execute(&app.state.db)
        .await
        .unwrap();
    ok(work.run(&["reset", "-q", "--hard", "HEAD~1"]).await);
    work.commit(&[("small.txt", "small\n")], "small").await;
    let out = push_loud(&work, "main").await;
    assert!(!out.ok);
    assert!(out.stderr.contains("403"), "{}", out.stderr);
    assert_eq!(branch_sha(&app, &alice, "quota", "main").await, before);
}

#[tokio::test]
async fn existing_repositories_get_the_new_config() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    seeded(&app, &alice, "old", &[("README.md", "hi\n")]).await;
    let path = bgh_repos::store(&app.state).path(repo_id(&app, &alice, "old").await);
    // A config as written before hidden refs and fsck.
    let legacy = "[core]\n\trepositoryformatversion = 0\n\tfilemode = true\n\tbare = true\n\
                  [core]\n\tlogAllRefUpdates = false\n[receive]\n\tadvertisePushOptions = true\n\
                  \tautogc = false\n\tunpackLimit = 100\n[uploadpack]\n\tallowFilter = true\n\
                  \tallowReachableSHA1InWant = true\n[gc]\n\tauto = 0\n";
    std::fs::write(path.join("config"), legacy).unwrap();

    let (rewritten, failed) = bgh_repos::maintenance::upgrade_repo_configs(&app.state)
        .await
        .unwrap();
    assert!(rewritten >= 1 && failed == 0);
    let get = |key: &'static str| {
        let path = path.clone();
        async move { ok(git(&path, &["config", "--get-all", key]).await).stdout }
    };
    assert_eq!(get("receive.hideRefs").await, "refs/pull/\nrefs/bgh/\n");
    assert_eq!(get("receive.fsckObjects").await, "true\n");
    assert_eq!(get("receive.fsck.badTimezone").await, "ignore\n");
    assert_eq!(get("bgh.configVersion").await, "2\n");
    assert_eq!(get("core.bare").await, "true\n");
    // Idempotent.
    let text = std::fs::read_to_string(path.join("config")).unwrap();
    bgh_repos::maintenance::upgrade_repo_configs(&app.state)
        .await
        .unwrap();
    assert_eq!(std::fs::read_to_string(path.join("config")).unwrap(), text);
}
