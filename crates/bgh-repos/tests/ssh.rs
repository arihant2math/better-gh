//! Git over the built-in SSH server with the real `ssh` + `git` clients.

mod common;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use bgh_core::testing::{TestApp, TestUser};
use common::*;
use serde_json::json;
use tokio_util::sync::CancellationToken;

fn have_ssh() -> bool {
    std::process::Command::new("ssh").arg("-V").output().is_ok()
        && std::process::Command::new("ssh-keygen")
            .arg("--help")
            .output()
            .is_ok()
}

struct Ssh {
    app: TestApp,
    addr: SocketAddr,
    tmp: tempfile::TempDir,
    _stop: tokio_util::sync::DropGuard,
}

async fn start() -> Ssh {
    let app = bgh_server::test_app().await;
    let stop = CancellationToken::new();
    let addr = bgh_repos::ssh::spawn(
        app.state.clone(),
        "127.0.0.1:0".parse().unwrap(),
        stop.clone(),
    )
    .await
    .unwrap();
    Ssh {
        app,
        addr,
        tmp: tempfile::tempdir().unwrap(),
        _stop: stop.drop_guard(),
    }
}

impl Ssh {
    /// Generate a key pair; returns (private key path, public key line).
    async fn keygen(&self, name: &str) -> (PathBuf, String) {
        let path = self.tmp.path().join(name);
        let out = tokio::process::Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-C", name, "-f"])
            .arg(&path)
            .output()
            .await
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let public = std::fs::read_to_string(path.with_extension("pub")).unwrap();
        (path, public.trim().to_string())
    }

    async fn user_key(&self, user: &TestUser) -> PathBuf {
        let (path, public) = self.keygen(&format!("{}_key", user.login)).await;
        let fp = bgh_repos::ssh::keys::fingerprint_openssh(&public).unwrap();
        sqlx::query(
            "INSERT INTO ssh_keys (user_id, title, key, fingerprint) VALUES ($1, 'test', $2, $3)",
        )
        .bind(user.id)
        .bind(&public)
        .bind(&fp)
        .execute(&self.app.state.db)
        .await
        .unwrap();
        path
    }

    async fn deploy_key(&self, owner: &TestUser, repo: &str, read_only: bool) -> PathBuf {
        let (path, public) = self.keygen(&format!("deploy_{repo}_{read_only}")).await;
        let fp = bgh_repos::ssh::keys::fingerprint_openssh(&public).unwrap();
        let id = repo_id(&self.app, owner, repo).await;
        sqlx::query(
            "INSERT INTO deploy_keys (repo_id, title, key, fingerprint, read_only) VALUES ($1, 'ci', $2, $3, $4)",
        )
        .bind(id)
        .bind(&public)
        .bind(&fp)
        .bind(read_only)
        .execute(&self.app.state.db)
        .await
        .unwrap();
        path
    }

    fn url(&self, owner: &str, repo: &str) -> String {
        format!("ssh://git@{}/{owner}/{repo}.git", self.addr)
    }

    fn ssh_command(key: &Path) -> String {
        format!(
            "ssh -F /dev/null -i {} -o IdentitiesOnly=yes -o StrictHostKeyChecking=no \
             -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -o BatchMode=yes",
            key.display()
        )
    }

    async fn git(&self, key: &Path, dir: &Path, args: &[&str]) -> GitOutput {
        let cmd = Self::ssh_command(key);
        git_env(dir, args, &[("GIT_SSH_COMMAND", cmd.as_str())]).await
    }

    /// Run a raw ssh command; returns (exit code, stdout, stderr).
    async fn ssh(&self, key: &Path, command: Option<&str>) -> (i32, String, String) {
        let mut c = tokio::process::Command::new("ssh");
        c.args(["-F", "/dev/null", "-i"]).arg(key).args([
            "-o",
            "IdentitiesOnly=yes",
            "-o",
            "StrictHostKeyChecking=no",
            "-o",
            "UserKnownHostsFile=/dev/null",
            "-o",
            "LogLevel=ERROR",
            "-o",
            "BatchMode=yes",
            "-p",
            &self.addr.port().to_string(),
            "git@127.0.0.1",
        ]);
        if let Some(cmd) = command {
            c.arg(cmd);
        }
        let out = c.stdin(std::process::Stdio::null()).output().await.unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

async fn work_repo(dir: &Path) -> String {
    std::fs::create_dir_all(dir).unwrap();
    init_work(dir).await;
    commit_files(
        dir,
        &[("hello.txt", b"hello over ssh\n")],
        "first",
        ("A", "a@example.com"),
    )
    .await
}

#[tokio::test]
async fn push_and_clone_over_ssh() {
    if !have_ssh() {
        eprintln!("ssh not installed; skipping");
        return;
    }
    let s = start().await;
    let alice = s.app.create_user("alice").await;
    s.app.create_repo(&alice, "demo").await;
    let key = s.user_key(&alice).await;
    let w = s.tmp.path().join("work");
    let c1 = work_repo(&w).await;

    let mut events = s.app.state.events.subscribe();
    ok(s.git(&key, &w, &["push", &s.url("alice", "demo"), "main"])
        .await);
    assert!(s.app.drain_jobs().await >= 1, "post-receive enqueued");
    let v = s.app.get("/api/v3/repos/alice/demo").send().await.json();
    assert!(v["pushed_at"].is_string());
    match &*events.try_recv().expect("push event") {
        bgh_core::events::Event::Push(p) => {
            assert_eq!(p.pusher_id, Some(alice.id));
            assert_eq!(p.updates[0].new, c1);
        }
        other => panic!("unexpected {other:?}"),
    }

    // Clone (protocol v2 via GIT_PROTOCOL env) and fetch new commits.
    let clone = s.tmp.path().join("clone");
    ok(s.git(
        &key,
        s.tmp.path(),
        &[
            "-c",
            "protocol.version=2",
            "clone",
            "-q",
            &s.url("alice", "demo"),
            clone.to_str().unwrap(),
        ],
    )
    .await);
    assert_eq!(
        std::fs::read_to_string(clone.join("hello.txt")).unwrap(),
        "hello over ssh\n"
    );
    let c2 = commit_files(&w, &[("two.txt", b"2\n")], "second", ("A", "a@example.com")).await;
    ok(
        s.git(&key, &w, &["push", "-q", &s.url("alice", "demo"), "main"])
            .await,
    );
    ok(s.git(&key, &clone, &["pull", "-q", "--ff-only"]).await);
    let head = ok(git(&clone, &["rev-parse", "HEAD"]).await).stdout;
    assert_eq!(head.trim(), c2);
    // Protocol v0 too.
    let out = ok(s
        .git(
            &key,
            s.tmp.path(),
            &[
                "-c",
                "protocol.version=0",
                "ls-remote",
                &s.url("alice", "demo"),
            ],
        )
        .await);
    assert!(
        out.stdout.contains(&c2) && out.stdout.contains("refs/heads/main"),
        "{}",
        out.stdout
    );

    // Branch deletion and a no-op push.
    ok(s.git(
        &key,
        &w,
        &["push", "-q", &s.url("alice", "demo"), "main:topic"],
    )
    .await);
    ok(
        s.git(&key, &w, &["push", "-q", &s.url("alice", "demo"), ":topic"])
            .await,
    );
    let out = ok(s
        .git(&key, &w, &["push", &s.url("alice", "demo"), "main"])
        .await);
    assert!(out.stderr.contains("up-to-date"), "{}", out.stderr);
    let out = ok(s
        .git(&key, s.tmp.path(), &["ls-remote", &s.url("alice", "demo")])
        .await);
    assert!(!out.stdout.contains("topic"));

    // Key usage is recorded.
    let used: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT last_used_at FROM ssh_keys WHERE user_id = $1")
            .bind(alice.id)
            .fetch_one(&s.app.state.db)
            .await
            .unwrap();
    assert!(used.is_some());
}

#[tokio::test]
async fn permissions_over_ssh() {
    if !have_ssh() {
        return;
    }
    let s = start().await;
    let alice = s.app.create_user("alice").await;
    let eve = s.app.create_user("eve").await;
    let bob = s.app.create_user("bob").await;
    s.app.create_repo(&alice, "demo").await;
    s.app.create_private_repo(&alice, "secret").await;
    add_collaborator(&s.app, &alice, "secret", &bob, "read").await;
    let alice_key = s.user_key(&alice).await;
    let eve_key = s.user_key(&eve).await;
    let bob_key = s.user_key(&bob).await;
    let w = s.tmp.path().join("work");
    work_repo(&w).await;
    ok(s.git(
        &alice_key,
        &w,
        &["push", "-q", &s.url("alice", "demo"), "main"],
    )
    .await);
    ok(s.git(
        &alice_key,
        &w,
        &["push", "-q", &s.url("alice", "secret"), "main"],
    )
    .await);

    // Public: anyone with a key can read, only writers push.
    let c = s.tmp.path().join("eve");
    ok(s.git(
        &eve_key,
        s.tmp.path(),
        &["clone", "-q", &s.url("alice", "demo"), c.to_str().unwrap()],
    )
    .await);
    let out = s
        .git(&eve_key, &w, &["push", &s.url("alice", "demo"), "main:eve"])
        .await;
    assert!(!out.ok);
    assert!(
        out.stderr
            .contains("Permission to alice/demo.git denied to eve"),
        "{}",
        out.stderr
    );

    // Private: invisible to eve, readable but not writable for bob.
    let out = s
        .git(
            &eve_key,
            s.tmp.path(),
            &["ls-remote", &s.url("alice", "secret")],
        )
        .await;
    assert!(!out.ok);
    assert!(
        out.stderr.contains("Repository not found"),
        "{}",
        out.stderr
    );
    ok(s.git(
        &bob_key,
        s.tmp.path(),
        &["ls-remote", &s.url("alice", "secret")],
    )
    .await);
    let out = s
        .git(
            &bob_key,
            &w,
            &["push", &s.url("alice", "secret"), "main:bob"],
        )
        .await;
    assert!(out.stderr.contains("denied to bob"), "{}", out.stderr);
    // Missing repository.
    let out = s
        .git(
            &alice_key,
            s.tmp.path(),
            &["ls-remote", &s.url("alice", "nope")],
        )
        .await;
    assert!(out.stderr.contains("Repository not found"));

    // Unknown keys can't authenticate.
    let (stranger, _) = s.keygen("stranger").await;
    let out = s
        .git(
            &stranger,
            s.tmp.path(),
            &["ls-remote", &s.url("alice", "demo")],
        )
        .await;
    assert!(!out.ok);
    assert!(out.stderr.contains("Permission denied"), "{}", out.stderr);

    // Suspended users are rejected.
    sqlx::query("UPDATE users SET suspended_at = now() WHERE id = $1")
        .bind(eve.id)
        .execute(&s.app.state.db)
        .await
        .unwrap();
    let out = s
        .git(
            &eve_key,
            s.tmp.path(),
            &["ls-remote", &s.url("alice", "demo")],
        )
        .await;
    assert!(out.stderr.contains("Permission denied"), "{}", out.stderr);

    // Storage quotas apply to SSH pushes like HTTP (bgh_core::settings).
    s.app.create_repo(&alice, "big").await;
    sqlx::query("INSERT INTO storage_quotas (owner_id, max_repo_size_mb) VALUES ($1, 1)")
        .bind(alice.id)
        .execute(&s.app.state.db)
        .await
        .unwrap();
    sqlx::query("UPDATE repositories SET size = 4096 WHERE name = 'big'")
        .execute(&s.app.state.db)
        .await
        .unwrap();
    let out = s
        .git(&alice_key, &w, &["push", &s.url("alice", "big"), "main"])
        .await;
    assert!(!out.ok);
    assert!(out.stderr.contains("size limit"), "{}", out.stderr);
    sqlx::query("DELETE FROM storage_quotas WHERE owner_id = $1")
        .bind(alice.id)
        .execute(&s.app.state.db)
        .await
        .unwrap();

    // Disabled repositories are blocked (except for site admins).
    sqlx::query("UPDATE repositories SET disabled = true WHERE name = 'secret'")
        .execute(&s.app.state.db)
        .await
        .unwrap();
    let out = s
        .git(
            &bob_key,
            s.tmp.path(),
            &["ls-remote", &s.url("alice", "secret")],
        )
        .await;
    assert!(
        out.stderr.contains("Repository access blocked"),
        "{}",
        out.stderr
    );
    let root = s.app.create_admin("root").await;
    let root_key = s.user_key(&root).await;
    ok(s.git(
        &root_key,
        s.tmp.path(),
        &["ls-remote", &s.url("alice", "secret")],
    )
    .await);

    // Archived repositories are read-only.
    sqlx::query("UPDATE repositories SET archived = true WHERE name = 'demo'")
        .execute(&s.app.state.db)
        .await
        .unwrap();
    let out = s
        .git(&alice_key, &w, &["push", &s.url("alice", "demo"), "main:x"])
        .await;
    assert!(out.stderr.contains("archived"), "{}", out.stderr);

    // Shell access and unsupported commands.
    let (code, _, err) = s.ssh(&alice_key, None).await;
    assert_eq!(code, 1);
    assert!(err.contains("Hi alice!"), "{err}");
    let (code, _, err) = s.ssh(&alice_key, Some("rm -rf /")).await;
    assert_ne!(code, 0);
    assert!(err.contains("Invalid command"), "{err}");
}

#[tokio::test]
async fn deploy_keys_and_branch_protection() {
    if !have_ssh() {
        return;
    }
    let s = start().await;
    let alice = s.app.create_user("alice").await;
    s.app.create_private_repo(&alice, "app").await;
    s.app.create_private_repo(&alice, "other").await;
    let alice_key = s.user_key(&alice).await;
    let ro = s.deploy_key(&alice, "app", true).await;
    let rw = s.deploy_key(&alice, "other", false).await;
    let w = s.tmp.path().join("work");
    work_repo(&w).await;
    ok(s.git(
        &alice_key,
        &w,
        &["push", "-q", &s.url("alice", "app"), "main"],
    )
    .await);
    s.app.drain_jobs().await;

    // Read-only deploy key: clone yes, push no, other repos invisible.
    ok(
        s.git(&ro, s.tmp.path(), &["ls-remote", &s.url("alice", "app")])
            .await,
    );
    let out = s
        .git(&ro, &w, &["push", &s.url("alice", "app"), "main:x"])
        .await;
    assert!(out.stderr.contains("read only"), "{}", out.stderr);
    let out = s
        .git(&ro, s.tmp.path(), &["ls-remote", &s.url("alice", "other")])
        .await;
    assert!(out.stderr.contains("Repository not found"));

    // Read-write deploy key pushes (no user attributed).
    let mut events = s.app.state.events.subscribe();
    ok(
        s.git(&rw, &w, &["push", "-q", &s.url("alice", "other"), "main"])
            .await,
    );
    assert!(s.app.drain_jobs().await >= 1);
    match &*events.try_recv().unwrap() {
        bgh_core::events::Event::Push(p) => assert_eq!(p.pusher_id, None),
        other => panic!("{other:?}"),
    }

    // Branch protection applies to SSH pushes.
    let id = repo_id(&s.app, &alice, "other").await;
    sqlx::query(
        "INSERT INTO branch_protections (repo_id, pattern, required_pull_request_reviews, enforce_admins)
         VALUES ($1, 'main', '{}', true)",
    )
    .bind(id)
    .execute(&s.app.state.db)
    .await
    .unwrap();
    commit_files(&w, &[("new.txt", b"n\n")], "more", ("A", "a@example.com")).await;
    let out = s
        .git(&alice_key, &w, &["push", &s.url("alice", "other"), "main"])
        .await;
    assert!(!out.ok);
    assert!(
        out.stderr.contains("protected branch hook declined"),
        "{}",
        out.stderr
    );
    // Other branches are fine.
    ok(s.git(
        &rw,
        &w,
        &["push", "-q", &s.url("alice", "other"), "main:feature"],
    )
    .await);
}

#[tokio::test]
async fn lfs_over_ssh() {
    if !have_ssh() {
        return;
    }
    let s = start().await;
    let alice = s.app.create_user("alice").await;
    s.app.create_private_repo(&alice, "media").await;
    let key = s.user_key(&alice).await;

    // Raw git-lfs-authenticate.
    let (code, out, err) = s
        .ssh(&key, Some("git-lfs-authenticate alice/media.git upload"))
        .await;
    assert_eq!(code, 0, "{err}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["href"], s.app.url("/alice/media.git/info/lfs"));
    assert_eq!(v["expires_in"], 3600);
    let auth = v["header"]["Authorization"].as_str().unwrap().to_string();
    assert!(auth.starts_with("RemoteAuth "));
    let res = s
        .app
        .post("/alice/media.git/info/lfs/objects/batch")
        .header("authorization", &auth)
        .json(&json!({"operation": "upload", "objects": []}))
        .send()
        .await;
    res.assert_status(200);
    let (code, _, err) = s
        .ssh(&key, Some("git-lfs-authenticate alice/media.git destroy"))
        .await;
    assert_ne!(code, 0);
    assert!(err.contains("Usage"), "{err}");

    if std::process::Command::new("git-lfs")
        .arg("version")
        .output()
        .is_err()
    {
        return;
    }
    // git-lfs over an ssh remote: tries git-lfs-transfer, falls back to
    // git-lfs-authenticate + HTTP.
    let w = s.tmp.path().join("work");
    std::fs::create_dir_all(&w).unwrap();
    init_work(&w).await;
    ok(git(&w, &["lfs", "install", "--local"]).await);
    ok(git(&w, &["lfs", "track", "*.bin"]).await);
    let payload = vec![42u8; 50_000];
    commit_files(&w, &[("big.bin", &payload)], "lfs", ("A", "a@example.com")).await;
    ok(git(&w, &["config", "lfs.locksverify", "false"]).await);
    ok(
        s.git(&key, &w, &["push", "-q", &s.url("alice", "media"), "main"])
            .await,
    );
    let stored: i64 = sqlx::query_scalar("SELECT count(*) FROM lfs_objects")
        .fetch_one(&s.app.state.db)
        .await
        .unwrap();
    assert_eq!(stored, 1);
    let c = s.tmp.path().join("clone");
    ok(s.git(
        &key,
        s.tmp.path(),
        &["clone", "-q", &s.url("alice", "media"), c.to_str().unwrap()],
    )
    .await);
    ok(git(&c, &["lfs", "install", "--local"]).await);
    ok(s.git(&key, &c, &["lfs", "pull"]).await);
    assert_eq!(std::fs::read(c.join("big.bin")).unwrap(), payload);
}

#[tokio::test]
async fn host_key_is_persistent() {
    let s = start().await;
    let path = bgh_repos::ssh::host_key_path(&s.app.state);
    let first = std::fs::read_to_string(&path).unwrap();
    assert!(first.contains("OPENSSH PRIVATE KEY"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let stop = CancellationToken::new();
    bgh_repos::ssh::spawn(
        s.app.state.clone(),
        "127.0.0.1:0".parse().unwrap(),
        stop.clone(),
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), first);
    stop.cancel();
}
