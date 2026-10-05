//! Wiki smart HTTP with the real `git` CLI (`/{owner}/{repo}.wiki.git`).

use std::path::Path;

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

#[track_caller]
fn fails(out: GitOutput, needle: &str) {
    assert!(!out.ok, "git unexpectedly succeeded:\n{}", out.stdout);
    assert!(
        out.stderr.contains(needle),
        "expected {needle:?} in stderr:\n{}",
        out.stderr
    );
}

fn remote(app: &TestApp, user: &TestUser, repo: &str) -> String {
    app.git_remote(user, &user.login, &format!("{repo}.wiki"))
}

fn remote_as(app: &TestApp, user: &TestUser, owner: &str, repo: &str) -> String {
    app.git_remote(user, owner, &format!("{repo}.wiki"))
}

fn anon(app: &TestApp, owner: &str, repo: &str) -> String {
    app.url(&format!("/{owner}/{repo}.wiki.git"))
}

async fn commit_file(dir: &Path, file: &str, content: &str, msg: &str) {
    std::fs::write(dir.join(file), content).unwrap();
    ok(git(dir, &["add", "."]).await);
    ok(git(dir, &["commit", "-q", "-m", msg]).await);
}

#[tokio::test]
async fn push_creates_wiki_and_clone_roundtrip() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let repo = app.create_repo(&alice, "demo").await;
    let repo_id = repo["id"].as_i64().unwrap();
    let tmp = tempfile::tempdir().unwrap();

    // Cloning a wiki that doesn't exist yet → 404.
    fails(
        git(
            tmp.path(),
            &["clone", "-q", &anon(&app, "alice", "demo"), "none"],
        )
        .await,
        "not found",
    );

    // First push creates the wiki.
    let work = tmp.path().join("work");
    std::fs::create_dir(&work).unwrap();
    ok(git(&work, &["init", "-q", "-b", "master"]).await);
    commit_file(
        &work,
        "Home.md",
        "# Pushed home\n\nSee [[Notes]].\n",
        "init",
    )
    .await;
    commit_file(&work, "Notes.txt", "<plain> text\n", "notes").await;
    ok(git(
        &work,
        &["push", "-q", &remote(&app, &alice, "demo"), "master"],
    )
    .await);

    let v = app.get("/_bgh/repos/alice/demo/wiki").send().await.json();
    assert_eq!(v["exists"], true);
    assert_eq!(v["pages"].as_array().unwrap().len(), 2, "{v}");
    let home = app
        .get("/_bgh/repos/alice/demo/wiki/pages/Home")
        .send()
        .await
        .json();
    assert_eq!(home["commit"]["message"], "init");
    assert_eq!(home["commit"]["author"]["name"], "Test");
    assert!(home["commit"]["author"]["login"].is_null());
    assert!(
        home["html"]
            .as_str()
            .unwrap()
            .contains(r#"class="wiki-link" href="/alice/demo/wiki/Notes""#)
    );
    let notes = app
        .get("/_bgh/repos/alice/demo/wiki/pages/Notes")
        .send()
        .await
        .json();
    assert_eq!(notes["format"], "txt");
    assert_eq!(notes["html"], "<pre>&lt;plain&gt; text\n</pre>");

    // A web edit shows up in an anonymous clone (both URL spellings).
    app.put("/_bgh/repos/alice/demo/wiki/pages/Home")
        .auth(&alice)
        .json(&json!({"body": "edited on the web\n"}))
        .send()
        .await
        .assert_status(200);
    for (name, url) in [
        ("c1", anon(&app, "alice", "demo")),
        ("c2", app.url("/alice/demo.wiki")),
    ] {
        ok(git(tmp.path(), &["clone", "-q", &url, name]).await);
        let content = std::fs::read_to_string(tmp.path().join(name).join("Home.md")).unwrap();
        assert_eq!(content, "edited on the web\n");
    }
    let log = ok(git(&tmp.path().join("c1"), &["log", "-1", "--format=%an <%ae>"]).await);
    assert_eq!(log.stdout.trim(), "alice <alice@example.com>");

    // The main repository is unaffected (still empty).
    let store = bgh_git::RepoStore::from_config(&app.state.config);
    assert!(store.read(repo_id, |r| r.is_empty()).await.unwrap());

    // Pushes: anonymous → auth required; reader → 403; anyone-can-edit → ok.
    let clone = tmp.path().join("c1");
    commit_file(&clone, "Bob.md", "bob\n", "bob page").await;
    fails(
        git(
            &clone,
            &["push", "-q", &anon(&app, "alice", "demo"), "master"],
        )
        .await,
        "terminal prompts disabled",
    );
    fails(
        git(
            &clone,
            &[
                "push",
                "-q",
                &remote_as(&app, &bob, "alice", "demo"),
                "master",
            ],
        )
        .await,
        "403",
    );
    app.patch("/_bgh/repos/alice/demo/wiki/settings")
        .auth(&alice)
        .json(&json!({"anyoneCanEdit": true}))
        .send()
        .await
        .assert_status(200);
    ok(git(
        &clone,
        &[
            "push",
            "-q",
            &remote_as(&app, &bob, "alice", "demo"),
            "master",
        ],
    )
    .await);
    app.get("/_bgh/repos/alice/demo/wiki/pages/Bob")
        .send()
        .await
        .assert_status(200);

    // Password auth works too.
    let pw = format!(
        "http://alice:{}@{}/alice/demo.wiki.git",
        alice.password, app.addr
    );
    ok(git(tmp.path(), &["ls-remote", &pw]).await);

    // Archived → push rejected; has_wiki = false → 404.
    sqlx::query("UPDATE repositories SET archived = true WHERE id = $1")
        .bind(repo_id)
        .execute(&app.state.db)
        .await
        .unwrap();
    commit_file(&clone, "More.md", "more\n", "more").await;
    fails(
        git(
            &clone,
            &["push", "-q", &remote(&app, &alice, "demo"), "master"],
        )
        .await,
        "403",
    );
    sqlx::query("UPDATE repositories SET has_wiki = false, archived = false WHERE id = $1")
        .bind(repo_id)
        .execute(&app.state.db)
        .await
        .unwrap();
    fails(
        git(
            tmp.path(),
            &["clone", "-q", &anon(&app, "alice", "demo"), "c3"],
        )
        .await,
        "not found",
    );
    fails(
        git(
            &clone,
            &["push", "-q", &remote(&app, &alice, "demo"), "master"],
        )
        .await,
        "not found",
    );
}

#[tokio::test]
async fn private_wiki_and_non_master_push() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    app.create_private_repo(&alice, "secret").await;
    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path().join("work");
    std::fs::create_dir(&work).unwrap();
    ok(git(&work, &["init", "-q", "-b", "main"]).await);
    commit_file(&work, "Home.md", "secret home\n", "init").await;

    // Pushing only `main` still makes the pages visible (HEAD follows).
    ok(git(
        &work,
        &["push", "-q", &remote(&app, &alice, "secret"), "main"],
    )
    .await);
    let v = app
        .get("/_bgh/repos/alice/secret/wiki/pages/Home")
        .auth(&alice)
        .send()
        .await;
    v.assert_status(200);
    assert_eq!(v.json()["raw"], "secret home\n");
    // Web edits then continue on that branch.
    app.put("/_bgh/repos/alice/secret/wiki/pages/Home")
        .auth(&alice)
        .json(&json!({"body": "v2\n"}))
        .send()
        .await
        .assert_status(200);
    ok(git(
        &work,
        &["pull", "-q", &remote(&app, &alice, "secret"), "main"],
    )
    .await);
    assert_eq!(
        std::fs::read_to_string(work.join("Home.md")).unwrap(),
        "v2\n"
    );

    // Anonymous: credential challenge; others: not found.
    fails(
        git(
            tmp.path(),
            &["clone", "-q", &anon(&app, "alice", "secret"), "a"],
        )
        .await,
        "terminal prompts disabled",
    );
    fails(
        git(
            tmp.path(),
            &[
                "clone",
                "-q",
                &remote_as(&app, &bob, "alice", "secret"),
                "b",
            ],
        )
        .await,
        "not found",
    );
    ok(git(
        tmp.path(),
        &["clone", "-q", &remote(&app, &alice, "secret"), "c"],
    )
    .await);
}
