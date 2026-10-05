//! Git working copies pushing over smart HTTP, and secret scanning setup.
#![allow(dead_code)]

use std::path::Path;

use bgh_core::testing::{TestApp, TestUser};
use serde_json::{Value, json};

/// A realistic (not "EXAMPLE") AWS access key id.
pub const AWS_KEY: &str = "AKIAQ3EGRXPZ7K2LMN4D";
pub const AWS_KEY_2: &str = "AKIAZ7WQ4MNPLK2ERT5X";

pub struct GitOutput {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Run git with an isolated config (async: the server shares this runtime).
pub async fn git(dir: &Path, args: &[&str]) -> GitOutput {
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
pub fn ok(out: GitOutput) -> GitOutput {
    assert!(out.ok, "git failed:\n{}\n{}", out.stdout, out.stderr);
    out
}

pub struct Work {
    pub dir: tempfile::TempDir,
    pub remote: String,
}

impl Work {
    pub async fn commit(&self, files: &[(&str, &str)], msg: &str) -> String {
        for (path, content) in files {
            let p = self.dir.path().join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, content).unwrap();
        }
        ok(git(self.dir.path(), &["add", "-A"]).await);
        ok(git(
            self.dir.path(),
            &["commit", "-q", "--allow-empty", "-m", msg],
        )
        .await);
        self.head().await
    }

    pub async fn head(&self) -> String {
        ok(git(self.dir.path(), &["rev-parse", "HEAD"]).await)
            .stdout
            .trim()
            .to_string()
    }

    pub async fn run(&self, args: &[&str]) -> GitOutput {
        git(self.dir.path(), args).await
    }

    /// `git push` without `-q`, so remote messages show up in stderr.
    pub async fn push(&self, refspec: &str) -> GitOutput {
        git(self.dir.path(), &["push", &self.remote, refspec]).await
    }

    /// The same working copy pushing as another user.
    pub fn remote_as(&self, app: &TestApp, user: &TestUser, owner: &str, repo: &str) -> String {
        app.git_remote(user, owner, repo)
    }
}

/// Create `owner/name` (a user's or `org`'s) through the API and a local
/// repository on `main` with `files` committed and pushed.
pub async fn seeded_in(
    app: &TestApp,
    user: &TestUser,
    org: Option<&str>,
    name: &str,
    files: &[(&str, &str)],
) -> Work {
    app.create_repo_with(user, org, json!({ "name": name }))
        .await;
    let owner = org.unwrap_or(&user.login);
    let work = Work {
        dir: tempfile::tempdir().unwrap(),
        remote: app.git_remote(user, owner, name),
    };
    ok(git(work.dir.path(), &["init", "-q", "-b", "main"]).await);
    work.commit(files, "initial commit").await;
    ok(work.push("main").await);
    settle(app).await;
    work
}

pub async fn seeded(app: &TestApp, user: &TestUser, name: &str, files: &[(&str, &str)]) -> Work {
    seeded_in(app, user, None, name, files).await
}

/// Run jobs and listeners until nothing is left (post-receive job → push
/// event → scan job).
pub async fn settle(app: &TestApp) {
    for _ in 0..3 {
        app.drain_jobs().await;
        app.settle_events().await;
    }
    app.drain_jobs().await;
}

/// Turn secret scanning (and optionally push protection) on.
pub async fn enable(app: &TestApp, user: &TestUser, full_name: &str, push_protection: bool) {
    let pp = if push_protection {
        "enabled"
    } else {
        "disabled"
    };
    let res = app
        .patch(&format!("/api/v3/repos/{full_name}"))
        .auth(user)
        .json(&json!({"security_and_analysis": {
            "secret_scanning": {"status": "enabled"},
            "secret_scanning_push_protection": {"status": pp},
        }}))
        .send()
        .await;
    res.assert_status(200);
    settle(app).await;
}

pub async fn alerts(app: &TestApp, user: &TestUser, full_name: &str, query: &str) -> Vec<Value> {
    let res = app
        .get(&format!(
            "/api/v3/repos/{full_name}/secret-scanning/alerts{query}"
        ))
        .auth(user)
        .send()
        .await;
    res.assert_status(200);
    res.json().as_array().unwrap().clone()
}

pub async fn branch_sha(app: &TestApp, user: &TestUser, full_name: &str, branch: &str) -> String {
    let r = app
        .get(&format!("/api/v3/repos/{full_name}/git/ref/heads/{branch}"))
        .auth(user)
        .send()
        .await;
    r.assert_status(200);
    r.json()["object"]["sha"].as_str().unwrap().to_string()
}

/// The placeholder id of the first unblock URL in a push rejection.
pub fn placeholder(stderr: &str) -> String {
    let i = stderr
        .find("/unblock-secret/")
        .unwrap_or_else(|| panic!("no unblock URL in:\n{stderr}"));
    stderr[i + "/unblock-secret/".len()..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric())
        .collect()
}
