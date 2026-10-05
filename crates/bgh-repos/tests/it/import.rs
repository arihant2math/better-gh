//! Repository import from a URL and pull mirrors (P11). The "remote" is a
//! repository served by the test server itself, allow-listed for SSRF.

use crate::gitwork::{Work, git, ok, seeded};

use std::time::Duration;

use base64::Engine;
use bgh_core::testing::{TestApp, TestUser};
use serde_json::{Value, json};

const LFS: &str = "application/vnd.git-lfs+json";

async fn app_allowing_loopback() -> TestApp {
    TestApp::spawn_with_config(bgh_server::factory(), |c| {
        c.webhook_allowed_hosts = vec!["127.0.0.1".into()];
    })
    .await
}

fn basic(user: &TestUser) -> String {
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{}:{}", user.login, user.token))
    )
}

/// Upload an LFS object to `repo` (`owner/name`) through the batch API.
async fn lfs_upload(app: &TestApp, user: &TestUser, repo: &str, data: &[u8]) -> String {
    let oid = sha256_hex_bytes(data);
    let res = app
        .post(&format!("/{repo}.git/info/lfs/objects/batch"))
        .header("accept", LFS)
        .header("content-type", LFS)
        .header("authorization", &basic(user))
        .json(&json!({"operation": "upload", "transfers": ["basic"],
                      "objects": [{"oid": oid, "size": data.len()}]}))
        .send()
        .await;
    res.assert_status(200);
    let v: Value = serde_json::from_str(&res.text()).unwrap();
    let href = v["objects"][0]["actions"]["upload"]["href"]
        .as_str()
        .unwrap()
        .to_string();
    let path = href.strip_prefix(&app.base_url).unwrap().to_string();
    app.put(&path)
        .header("authorization", &basic(user))
        .header("content-type", "application/octet-stream")
        .body(data.to_vec())
        .send()
        .await
        .assert_status(200);
    oid
}

fn sha256_hex_bytes(data: &[u8]) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(data))
}

/// Poll an import until it leaves `queued`/`importing`.
async fn wait_import(app: &TestApp, user: &TestUser, repo: &str) -> Value {
    for _ in 0..300 {
        app.drain_jobs().await;
        let v = app
            .get(&format!("/_bgh/repos/{repo}/import"))
            .auth(user)
            .send()
            .await
            .json();
        if !matches!(v["status"].as_str(), Some("queued" | "importing")) {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("import of {repo} did not finish");
}

/// `refname sha` lines advertised by a repository.
async fn ls_remote(app: &TestApp, user: &TestUser, owner: &str, name: &str) -> Vec<String> {
    let dir = tempfile::tempdir().unwrap();
    let out = ok(git(
        dir.path(),
        &["ls-remote", &app.git_remote(user, owner, name)],
    )
    .await);
    let mut lines: Vec<String> = out
        .stdout
        .lines()
        .filter(|l| !l.ends_with("\tHEAD"))
        .map(str::to_string)
        .collect();
    lines.sort();
    lines
}

/// Seed `alice/src` (private): main + dev branches, an annotated tag and an
/// LFS pointer whose object is uploaded. Returns the work tree and the oid.
async fn seed_source(app: &TestApp, alice: &TestUser) -> (Work, String) {
    let work = seeded(app, alice, "src", &[("README.md", "hello\n")]).await;
    app.patch("/api/v3/repos/alice/src")
        .auth(alice)
        .json(&json!({"private": true}))
        .send()
        .await
        .assert_status(200);
    let payload: Vec<u8> = (0..50_000u32).map(|i| (i % 251) as u8).collect();
    let oid = lfs_upload(app, alice, "alice/src", &payload).await;
    let pointer = format!(
        "version https://git-lfs.github.com/spec/v1\noid sha256:{oid}\nsize {}\n",
        payload.len()
    );
    work.commit(&[("asset.bin", &pointer)], "add asset").await;
    ok(work.push("main").await);
    ok(work.run(&["checkout", "-q", "-b", "dev"]).await);
    work.commit(&[("dev.txt", "dev\n")], "dev work").await;
    ok(work.push("dev").await);
    ok(work.run(&["checkout", "-q", "main"]).await);
    ok(work.run(&["tag", "-a", "v1.0", "-m", "release"]).await);
    ok(work.push("v1.0").await);
    app.drain_jobs().await;
    (work, oid)
}

#[tokio::test]
async fn imports_branches_tags_and_lfs() {
    let app = app_allowing_loopback().await;
    let alice = app.create_user("alice").await;
    let (_work, oid) = seed_source(&app, &alice).await;

    let res = app
        .post("/_bgh/imports")
        .auth(&alice)
        .json(&json!({
            "source_url": app.url("/alice/src.git"),
            "username": "alice",
            "password_or_token": alice.token,
            "name": "copy",
            "visibility": "private",
            "include_lfs": true,
        }))
        .send()
        .await;
    res.assert_status(201);
    let created = res.json();
    assert!(!res.text().contains(&alice.token), "token leaked");
    assert_eq!(created["status"], "queued");
    assert_eq!(created["mirror"], false);
    assert_eq!(created["has_credentials"], true);
    assert_eq!(created["source_url"], app.url("/alice/src.git"));
    assert_eq!(created["repository"]["full_name"], "alice/copy");
    assert_eq!(created["repository"]["private"], true);
    for k in [
        "password_or_token",
        "username",
        "credentials",
        "enc_credentials",
    ] {
        assert!(created.get(k).is_none(), "{k} present");
    }

    // Pushes are refused while the import runs.
    let done = wait_import(&app, &alice, "alice/copy").await;
    assert_eq!(done["status"], "complete", "{done}");
    assert_eq!(done["phase"], "complete");
    assert!(done["completed_at"].is_string());
    assert!(done["objects_total"].as_i64().unwrap() > 0, "{done}");
    assert_eq!(done["lfs_objects_total"], 1);
    assert_eq!(done["lfs_objects_received"], 1);

    // Refs match (branches + tags), default branch adopted.
    assert_eq!(
        ls_remote(&app, &alice, "alice", "copy").await,
        ls_remote(&app, &alice, "alice", "src").await
    );
    let repo = app
        .get("/api/v3/repos/alice/copy")
        .auth(&alice)
        .send()
        .await
        .json();
    assert_eq!(repo["default_branch"], "main");
    assert_eq!(repo["mirror_url"], Value::Null);
    assert!(repo["pushed_at"].is_string());

    // LFS object linked to the new repository.
    let linked: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM lfs_objects o JOIN repositories r ON r.id = o.repo_id
                         WHERE r.name = 'copy' AND o.oid = $1)",
    )
    .bind(&oid)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert!(linked);
    app.get("/alice/copy/raw/main/asset.bin")
        .auth(&alice)
        .send()
        .await
        .assert_status(200);

    // A clone works and it's a regular, writable repository.
    let dir = tempfile::tempdir().unwrap();
    ok(git(
        dir.path(),
        &["clone", "-q", &app.git_remote(&alice, "alice", "copy"), "c"],
    )
    .await);
    let c = dir.path().join("c");
    assert!(!c.join("dev.txt").exists() && c.join("asset.bin").exists());
    std::fs::write(c.join("new.txt"), "x").unwrap();
    ok(git(&c, &["add", "."]).await);
    ok(crate::gitwork::git_env(&c, &["commit", "-qm", "more"], &[]).await);
    ok(git(&c, &["push", "-q", "origin", "main"]).await);

    // Credentials never reach the audit log, and the import is audited.
    let audit: Vec<String> = sqlx::query_scalar("SELECT data::text FROM audit_log")
        .fetch_all(&app.state.db)
        .await
        .unwrap();
    assert!(audit.iter().all(|a| !a.contains(&alice.token)));
    let actions: Vec<String> = sqlx::query_scalar("SELECT action FROM audit_log")
        .fetch_all(&app.state.db)
        .await
        .unwrap();
    assert!(actions.iter().any(|a| a == "repo.import"));
    // Stored sealed, not in clear text.
    let stored: Vec<u8> = sqlx::query_scalar("SELECT enc_credentials FROM repo_imports")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert!(
        !String::from_utf8_lossy(&stored).contains(&alice.token),
        "credentials stored in clear text"
    );
    // A status delta was recorded for clients.
    let deltas: i64 =
        sqlx::query_scalar("SELECT count(*) FROM sync_actions WHERE model = 'repoImport'")
            .fetch_one(&app.state.db)
            .await
            .unwrap();
    assert!(deltas >= 2);
}

#[tokio::test]
async fn mirror_syncs_and_is_read_only() {
    let app = app_allowing_loopback().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let (work, _) = seed_source(&app, &alice).await;
    let src_url = app.url("/alice/src.git");
    // Userinfo in the URL becomes the (sealed) credentials.
    let with_creds = src_url.replace("http://", &format!("http://alice:{}@", alice.token));
    let res = app
        .post("/_bgh/imports")
        .auth(&alice)
        .json(
            &json!({"source_url": with_creds, "name": "mirror", "mirror": true,
                      "mirror_interval_minutes": 60}),
        )
        .send()
        .await;
    res.assert_status(201);
    assert!(!res.text().contains(&alice.token));
    assert_eq!(res.json()["source_url"], src_url);
    let done = wait_import(&app, &alice, "alice/mirror").await;
    assert_eq!(done["status"], "complete", "{done}");

    // REST and GraphQL expose the mirror.
    let repo = app
        .get("/api/v3/repos/alice/mirror")
        .auth(&alice)
        .send()
        .await
        .json();
    assert_eq!(repo["mirror_url"], src_url.as_str());
    let gql = app
        .post("/api/graphql")
        .auth(&alice)
        .json(&json!({"query": "{ repository(owner: \"alice\", name: \"mirror\") { isMirror mirrorUrl } }"}))
        .send()
        .await
        .json();
    assert_eq!(gql["data"]["repository"]["isMirror"], true, "{gql}");
    assert_eq!(gql["data"]["repository"]["mirrorUrl"], src_url.as_str());
    let found = app
        .get("/api/v3/search/repositories?q=mirror:true")
        .auth(&alice)
        .send()
        .await
        .json();
    assert_eq!(found["total_count"], 1, "{found}");

    // Settings: no credentials in the response.
    let res = app
        .get("/_bgh/repos/alice/mirror/mirror")
        .auth(&alice)
        .send()
        .await;
    res.assert_status(200);
    assert!(!res.text().contains(&alice.token));
    let m = res.json();
    assert_eq!(m["url"], src_url.as_str());
    assert_eq!(m["has_credentials"], true);
    assert_eq!(m["interval_minutes"], 60);
    assert_eq!(m["last_status"], "success");
    app.get("/_bgh/repos/alice/mirror/mirror")
        .auth(&bob)
        .send()
        .await
        .assert_status(403);

    // Pushing to the mirror is rejected; so are API ref writes.
    let dir = tempfile::tempdir().unwrap();
    ok(git(
        dir.path(),
        &[
            "clone",
            "-q",
            &app.git_remote(&alice, "alice", "mirror"),
            "m",
        ],
    )
    .await);
    let mdir = dir.path().join("m");
    let m = &mdir;
    std::fs::write(m.join("x.txt"), "x").unwrap();
    ok(git(m, &["add", "."]).await);
    ok(crate::gitwork::git_env(m, &["commit", "-qm", "x"], &[]).await);
    let pushed = git(m, &["push", "origin", "main"]).await;
    assert!(!pushed.ok);
    assert!(
        pushed
            .stderr
            .contains("This repository is a mirror and is read-only"),
        "{}",
        pushed.stderr
    );
    let res = app
        .post("/api/v3/repos/alice/mirror/git/refs")
        .auth(&alice)
        .json(&json!({"ref": "refs/heads/x", "sha": "0".repeat(40)}))
        .send()
        .await;
    res.assert_status(403);
    assert_eq!(
        res.json()["message"],
        "This repository is a mirror and is read-only"
    );

    // A new upstream commit and a deleted branch arrive with the next sync.
    let new_head = work.commit(&[("later.txt", "later\n")], "later").await;
    ok(work.push("main").await);
    ok(work.run(&["push", "-q", &work.remote, ":dev"]).await);
    app.drain_jobs().await;
    let mut events = app.state.events.subscribe();
    let res = app
        .post("/_bgh/repos/alice/mirror/mirror/sync")
        .auth(&alice)
        .send()
        .await;
    res.assert_status(202);
    app.drain_jobs().await;
    let refs = ls_remote(&app, &alice, "alice", "mirror").await;
    assert!(
        refs.contains(&format!("{new_head}\trefs/heads/main")),
        "{refs:?}"
    );
    assert!(
        !refs.iter().any(|r| r.ends_with("refs/heads/dev")),
        "{refs:?}"
    );
    assert_eq!(refs, ls_remote(&app, &alice, "alice", "src").await);
    let mut saw_push = false;
    while let Ok(e) = events.try_recv() {
        if let bgh_core::events::Event::Push(p) = &*e {
            assert_eq!(p.origin.as_deref(), Some("mirror"));
            saw_push = true;
        }
    }
    assert!(saw_push);

    // Settings update and failure reporting.
    let res = app
        .patch("/_bgh/repos/alice/mirror/mirror")
        .auth(&alice)
        .json(&json!({"interval_minutes": 5}))
        .send()
        .await;
    res.assert_status(422);
    let res = app
        .patch("/_bgh/repos/alice/mirror/mirror")
        .auth(&alice)
        .json(&json!({"interval_minutes": 30, "url": app.url("/alice/gone.git")}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["interval_minutes"], 30);
    app.post("/_bgh/repos/alice/mirror/mirror/sync")
        .auth(&alice)
        .send()
        .await
        .assert_status(202);
    app.drain_jobs().await;
    let m = app
        .get("/_bgh/repos/alice/mirror/mirror")
        .auth(&alice)
        .send()
        .await
        .json();
    assert_eq!(m["last_status"], "failed", "{m}");
    assert!(m["last_error"].is_string());
    assert_eq!(m["consecutive_failures"], 1);
    // Site admins list failing mirrors.
    let root = app.create_admin("root").await;
    let res = app.get("/_bgh/admin/mirrors").auth(&root).send().await;
    res.assert_status(200);
    let list = res.json();
    assert_eq!(list.as_array().unwrap().len(), 1, "{list}");
    assert_eq!(list[0]["repository"], "alice/mirror");
    assert_eq!(list[0]["last_status"], "failed");
    assert!(!res.text().contains(&alice.token));
    app.get("/_bgh/admin/mirrors")
        .auth(&bob)
        .send()
        .await
        .assert_status(403);

    // Convert into a regular repository: pushes work again.
    app.delete("/_bgh/repos/alice/mirror/mirror")
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    let repo = app
        .get("/api/v3/repos/alice/mirror")
        .auth(&alice)
        .send()
        .await
        .json();
    assert_eq!(repo["mirror_url"], Value::Null);
    ok(git(&mdir, &["pull", "-q", "--rebase", "origin", "main"]).await);
    ok(git(&mdir, &["push", "-q", "origin", "main"]).await);
}

#[tokio::test]
async fn scheduler_enqueues_due_mirrors() {
    let app = app_allowing_loopback().await;
    let alice = app.create_user("alice").await;
    seeded(&app, &alice, "src", &[("a", "1")]).await;
    app.post("/_bgh/imports")
        .auth(&alice)
        .json(&json!({"source_url": app.url("/alice/src.git"), "name": "m", "mirror": true}))
        .send()
        .await
        .assert_status(201);
    assert_eq!(
        wait_import(&app, &alice, "alice/m").await["status"],
        "complete"
    );
    assert!(
        bgh_repos::mirrors::enqueue_due(&app.state)
            .await
            .unwrap()
            .is_empty()
    );
    sqlx::query("UPDATE repo_mirrors SET next_sync_at = now() - interval '1 minute'")
        .execute(&app.state.db)
        .await
        .unwrap();
    let due = bgh_repos::mirrors::enqueue_due(&app.state).await.unwrap();
    assert_eq!(due.len(), 1);
    // Rescheduled: not due again.
    assert!(
        bgh_repos::mirrors::enqueue_due(&app.state)
            .await
            .unwrap()
            .is_empty()
    );
    app.drain_jobs().await;
    let m = app
        .get("/_bgh/repos/alice/m/mirror")
        .auth(&alice)
        .send()
        .await
        .json();
    assert_eq!(m["last_status"], "success");
}

#[tokio::test]
async fn import_validation_ssrf_and_retry() {
    // Default policy: loopback is not reachable.
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    for url in [
        app.url("/alice/x.git"),
        "file:///etc/passwd".to_string(),
        "ssh://example.com/x.git".to_string(),
        "http://169.254.169.254/latest".to_string(),
        "not a url".to_string(),
    ] {
        let res = app
            .post("/_bgh/imports")
            .auth(&alice)
            .json(&json!({"source_url": url, "name": "x"}))
            .send()
            .await;
        res.assert_status(422);
        assert_eq!(res.json()["errors"][0]["field"], "source_url");
    }
    app.post("/_bgh/imports")
        .json(&json!({"source_url": "https://example.com/x.git"}))
        .send()
        .await
        .assert_status(401);
    drop(app);

    let app = app_allowing_loopback().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    seed_source(&app, &alice).await;
    // Wrong credentials: the import fails with git's message.
    let res = app
        .post("/_bgh/imports")
        .auth(&bob)
        .json(&json!({"source_url": app.url("/alice/src.git"),
                      "password_or_token": "bghp_wrong"}))
        .send()
        .await;
    res.assert_status(201);
    assert_eq!(res.json()["repository"]["full_name"], "bob/src");
    let failed = wait_import(&app, &bob, "bob/src").await;
    assert_eq!(failed["status"], "failed", "{failed}");
    assert!(failed["error"].as_str().unwrap().len() > 3);
    // Duplicate names are rejected like repository creation.
    app.post("/_bgh/imports")
        .auth(&bob)
        .json(&json!({"source_url": app.url("/alice/src.git")}))
        .send()
        .await
        .assert_status(422);
    // Cancel only applies to running imports; others can't retry.
    app.post("/_bgh/repos/bob/src/import/cancel")
        .auth(&bob)
        .send()
        .await
        .assert_status(422);
    app.post("/_bgh/repos/bob/src/import/retry")
        .auth(&alice)
        .send()
        .await
        .assert_status(403);
    // Retry with working credentials (alice's token can read alice/src).
    let res = app
        .post("/_bgh/repos/bob/src/import/retry")
        .auth(&bob)
        .json(&json!({"username": "alice", "password_or_token": alice.token}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["status"], "queued");
    let done = wait_import(&app, &bob, "bob/src").await;
    assert_eq!(done["status"], "complete", "{done}");
    assert_eq!(done["attempts"], 2);
    assert_eq!(
        ls_remote(&app, &bob, "bob", "src").await,
        ls_remote(&app, &alice, "alice", "src").await
    );
}

#[tokio::test]
async fn cancel_and_stale_imports() {
    let app = app_allowing_loopback().await;
    let alice = app.create_user("alice").await;
    seeded(&app, &alice, "src", &[("a", "1")]).await;
    app.post("/_bgh/imports")
        .auth(&alice)
        .json(&json!({"source_url": app.url("/alice/src.git"), "name": "c"}))
        .send()
        .await
        .assert_status(201);
    // Cancel before the job runs: the job then does nothing.
    let res = app
        .post("/_bgh/repos/alice/c/import/cancel")
        .auth(&alice)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["status"], "cancelled");
    app.drain_jobs().await;
    let v = wait_import(&app, &alice, "alice/c").await;
    assert_eq!(v["status"], "cancelled");
    assert!(ls_remote(&app, &alice, "alice", "c").await.is_empty());

    // An import whose process died is failed by the sweeper.
    sqlx::query(
        "UPDATE repo_imports SET status = 'importing', updated_at = now() - interval '1 hour'",
    )
    .execute(&app.state.db)
    .await
    .unwrap();
    // …and pushes are refused while it's "running".
    let w = Work {
        dir: tempfile::tempdir().unwrap(),
        remote: app.git_remote(&alice, "alice", "c"),
    };
    crate::gitwork::init_work(w.dir.path()).await;
    w.commit(&[("f", "1")], "c").await;
    let pushed = w.push("main").await;
    assert!(!pushed.ok);
    assert!(
        pushed.stderr.contains("being imported"),
        "{}",
        pushed.stderr
    );
    assert_eq!(bgh_repos::import::sweep_stale(&app.state).await.unwrap(), 1);
    let v = app
        .get("/_bgh/repos/alice/c/import")
        .auth(&alice)
        .send()
        .await
        .json();
    assert_eq!(v["status"], "failed");
    ok(w.push("main").await);
}
