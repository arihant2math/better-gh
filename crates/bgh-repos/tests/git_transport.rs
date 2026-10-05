//! End-to-end git smart HTTP with the real `git` CLI against a TCP-bound app.

use std::path::Path;

use bgh_core::events::Event;
use bgh_core::testing::{TestApp, TestUser};
use serde_json::json;

struct GitOutput {
    ok: bool,
    stdout: String,
    stderr: String,
}

/// Run git with an isolated config (async: the server shares this runtime).
async fn git(dir: &Path, args: &[&str]) -> GitOutput {
    let mut c = tokio::process::Command::new("git");
    for k in [
        "http_proxy",
        "https_proxy",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "all_proxy",
    ] {
        c.env_remove(k);
    }
    let out = c
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .args(args)
        .output()
        .await
        .expect("run git");
    GitOutput {
        ok: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

#[track_caller]
fn ok(out: GitOutput) -> GitOutput {
    assert!(out.ok, "git failed:\n{}\n{}", out.stdout, out.stderr);
    out
}

/// A local working copy with one commit on `main`.
async fn local_repo(dir: &Path, file: &str, content: &str) {
    ok(git(dir, &["init", "-q", "-b", "main"]).await);
    std::fs::write(dir.join(file), content).unwrap();
    ok(git(dir, &["add", "."]).await);
    ok(git(dir, &["commit", "-q", "-m", "first commit"]).await);
}

fn anon_url(app: &TestApp, owner: &str, repo: &str) -> String {
    app.url(&format!("/{owner}/{repo}.git"))
}

fn password_url(app: &TestApp, user: &TestUser, owner: &str, repo: &str) -> String {
    format!(
        "http://{}:{}@{}/{owner}/{repo}.git",
        user.login, user.password, app.addr
    )
}

#[tokio::test]
async fn push_then_clone_roundtrip() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let repo = app.create_repo(&alice, "demo").await;
    let repo_id = repo["id"].as_i64().unwrap();
    assert!(repo["pushed_at"].is_null());

    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path().join("work");
    std::fs::create_dir(&work).unwrap();
    local_repo(&work, "hello.txt", "hello world\n").await;

    let mut events = app.state.events.subscribe();
    ok(git(
        &work,
        &[
            "push",
            "-q",
            &app.git_remote(&alice, "alice", "demo"),
            "main",
        ],
    )
    .await);

    // The post-receive job was enqueued before `git push` returned.
    assert_eq!(app.drain_jobs().await, 1);
    let res = app.get("/api/v3/repos/alice/demo").send().await;
    let v = res.json();
    assert!(v["pushed_at"].is_string(), "pushed_at set: {v}");
    assert_eq!(v["default_branch"], "main");
    let ev = events.try_recv().expect("push event");
    match &*ev {
        Event::Push(p) => {
            assert_eq!(p.repo_id, repo_id);
            assert_eq!(p.pusher_id, Some(alice.id));
            assert_eq!(p.updates.len(), 1);
            assert_eq!(p.updates[0].refname, "refs/heads/main");
            assert!(p.updates[0].is_create());
        }
        other => panic!("unexpected event {other:?}"),
    }
    let synced: i64 =
        sqlx::query_scalar("SELECT count(*) FROM sync_actions WHERE scope = $1 AND action = 'U'")
            .bind(format!("repo:{repo_id}"))
            .fetch_one(&app.state.db)
            .await
            .unwrap();
    assert_eq!(synced, 1);

    // Anonymous clone of a public repository (protocol v2 and v0).
    for (name, proto) in [("clone-v2", "2"), ("clone-v0", "0")] {
        let out = git(
            tmp.path(),
            &[
                "-c",
                &format!("protocol.version={proto}"),
                "clone",
                "-q",
                &anon_url(&app, "alice", "demo"),
                name,
            ],
        )
        .await;
        ok(out);
        let content = std::fs::read_to_string(tmp.path().join(name).join("hello.txt")).unwrap();
        assert_eq!(content, "hello world\n");
    }

    // Second push (fast-forward) from the clone, then fetch it back.
    let clone = tmp.path().join("clone-v2");
    std::fs::write(clone.join("second.txt"), "2\n").unwrap();
    ok(git(&clone, &["add", "."]).await);
    ok(git(&clone, &["commit", "-q", "-m", "second"]).await);
    ok(git(
        &clone,
        &[
            "push",
            "-q",
            &app.git_remote(&alice, "alice", "demo"),
            "main",
        ],
    )
    .await);
    ok(git(
        &work,
        &["pull", "-q", &anon_url(&app, "alice", "demo"), "main"],
    )
    .await);
    assert!(work.join("second.txt").exists());
    let head = ok(git(&work, &["rev-parse", "HEAD"]).await).stdout;
    let store = bgh_git::RepoStore::from_config(&app.state.config);
    let server_head = store
        .read(repo_id, |r| r.resolve_commit("main"))
        .await
        .unwrap();
    assert_eq!(head.trim(), server_head);
}

#[tokio::test]
async fn first_push_sets_default_branch() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    app.create_repo(&alice, "legacy").await;
    let tmp = tempfile::tempdir().unwrap();
    ok(git(tmp.path(), &["init", "-q", "-b", "trunk"]).await);
    std::fs::write(tmp.path().join("f"), "x").unwrap();
    ok(git(tmp.path(), &["add", "."]).await);
    ok(git(tmp.path(), &["commit", "-q", "-m", "c"]).await);
    ok(git(
        tmp.path(),
        &[
            "push",
            "-q",
            &app.git_remote(&alice, "alice", "legacy"),
            "trunk",
        ],
    )
    .await);
    app.drain_jobs().await;
    let v = app.get("/api/v3/repos/alice/legacy").send().await.json();
    assert_eq!(v["default_branch"], "trunk");
    // HEAD follows, so a plain clone checks out trunk.
    let out = ok(git(
        tmp.path(),
        &[
            "ls-remote",
            "--symref",
            &anon_url(&app, "alice", "legacy"),
            "HEAD",
        ],
    )
    .await);
    assert!(
        out.stdout.contains("ref: refs/heads/trunk"),
        "{}",
        out.stdout
    );
}

#[tokio::test]
async fn transport_authorization() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    app.create_private_repo(&alice, "secret").await;
    app.create_repo(&alice, "public").await;
    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path().join("work");
    std::fs::create_dir(&work).unwrap();
    local_repo(&work, "a.txt", "a").await;

    // Owner pushes with a password (Basic) to the private repo.
    ok(git(
        &work,
        &[
            "push",
            "-q",
            &password_url(&app, &alice, "alice", "secret"),
            "main",
        ],
    )
    .await);

    // Anonymous clone of a private repo is challenged (and fails without a prompt).
    let out = git(
        tmp.path(),
        &["clone", "-q", &anon_url(&app, "alice", "secret"), "anon"],
    )
    .await;
    assert!(!out.ok);
    let res = app
        .get("/alice/secret.git/info/refs?service=git-upload-pack")
        .send()
        .await;
    res.assert_status(401);
    assert_eq!(
        res.header("www-authenticate"),
        Some("Basic realm=\"Better GitHub\"")
    );

    // Bob has no access: 404 even with valid credentials.
    let out = git(
        tmp.path(),
        &[
            "clone",
            "-q",
            &app.git_remote(&bob, "alice", "secret"),
            "bob",
        ],
    )
    .await;
    assert!(!out.ok);
    app.get("/alice/secret.git/info/refs?service=git-upload-pack")
        .basic("bob", &bob.token)
        .send()
        .await
        .assert_status(404);

    // Bob can read the public repo but not push to it.
    let out = git(
        &work,
        &[
            "push",
            "-q",
            &app.git_remote(&bob, "alice", "public"),
            "main",
        ],
    )
    .await;
    assert!(!out.ok);
    assert!(out.stderr.contains("403"), "{}", out.stderr);
    // Anonymous push is challenged.
    app.get("/alice/public.git/info/refs?service=git-receive-pack")
        .send()
        .await
        .assert_status(401);
    // Wrong password is challenged too.
    app.get("/alice/public.git/info/refs?service=git-upload-pack")
        .basic("alice", "wrong")
        .send()
        .await
        .assert_status(401);
    // Dumb protocol is not supported.
    app.get("/alice/public.git/info/refs")
        .send()
        .await
        .assert_status(403);

    // Archived repositories reject pushes.
    sqlx::query("UPDATE repositories SET archived = true WHERE name = 'secret'")
        .execute(&app.state.db)
        .await
        .unwrap();
    let out = git(
        &work,
        &[
            "push",
            "-q",
            &app.git_remote(&alice, "alice", "secret"),
            "main:other",
        ],
    )
    .await;
    assert!(!out.ok);
    assert_eq!(
        app.drain_jobs().await,
        1,
        "only the first successful push enqueued work"
    );
}

#[tokio::test]
async fn branch_protection_rejects_direct_pushes() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let carol = app.create_user("carol").await;
    let repo = app.create_repo(&alice, "guarded").await;
    let repo_id = repo["id"].as_i64().unwrap();
    sqlx::query(
        "INSERT INTO collaborators (repo_id, user_id, permission) VALUES ($1, $2, 'write')",
    )
    .bind(repo_id)
    .bind(carol.id)
    .execute(&app.state.db)
    .await
    .unwrap();

    let tmp = tempfile::tempdir().unwrap();
    local_repo(tmp.path(), "a.txt", "a").await;
    ok(git(
        tmp.path(),
        &[
            "push",
            "-q",
            &app.git_remote(&carol, "alice", "guarded"),
            "main",
        ],
    )
    .await);

    sqlx::query(
        "INSERT INTO branch_protections (repo_id, pattern, required_pull_request_reviews)
         VALUES ($1, 'main', $2)",
    )
    .bind(repo_id)
    .bind(json!({"required_approving_review_count": 1}))
    .execute(&app.state.db)
    .await
    .unwrap();

    std::fs::write(tmp.path().join("b.txt"), "b").unwrap();
    ok(git(tmp.path(), &["add", "."]).await);
    ok(git(tmp.path(), &["commit", "-q", "-m", "b"]).await);
    let out = git(
        tmp.path(),
        &["push", &app.git_remote(&carol, "alice", "guarded"), "main"],
    )
    .await;
    assert!(!out.ok, "direct push to protected branch must fail");
    assert!(
        out.stderr.contains("must be made through a pull request"),
        "{}",
        out.stderr
    );

    // Other branches are fine; the admin bypasses (enforce_admins = false).
    ok(git(
        tmp.path(),
        &[
            "push",
            "-q",
            &app.git_remote(&carol, "alice", "guarded"),
            "main:feature",
        ],
    )
    .await);
    ok(git(
        tmp.path(),
        &[
            "push",
            "-q",
            &app.git_remote(&alice, "alice", "guarded"),
            "main",
        ],
    )
    .await);

    // Deleting the protected branch is refused even for admins.
    let out = git(
        tmp.path(),
        &["push", &app.git_remote(&alice, "alice", "guarded"), ":main"],
    )
    .await;
    assert!(!out.ok);
    assert!(
        out.stderr.to_lowercase().contains("cannot delete"),
        "{}",
        out.stderr
    );
}
