//! Shared helpers for tests that need real git history.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use bgh_core::testing::{TestApp, TestUser};

pub struct GitOutput {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Run git with an isolated config (async: the server shares this runtime).
pub async fn git_env(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> GitOutput {
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
        .envs(env.iter().copied())
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

pub async fn git(dir: &Path, args: &[&str]) -> GitOutput {
    git_env(dir, args, &[]).await
}

#[track_caller]
pub fn ok(out: GitOutput) -> GitOutput {
    assert!(out.ok, "git failed:\n{}\n{}", out.stdout, out.stderr);
    out
}

/// A local working copy pushing to `owner/repo` as `user`.
pub struct Work {
    pub dir: tempfile::TempDir,
    pub remote: String,
}

impl Work {
    pub fn path(&self) -> PathBuf {
        self.dir.path().to_path_buf()
    }

    /// Write files and commit (author `name <email>` at `date` if given).
    pub async fn commit(&self, files: &[(&str, &str)], msg: &str) -> String {
        self.commit_as(files, msg, "Test", "test@example.com", None)
            .await
    }

    pub async fn commit_as(
        &self,
        files: &[(&str, &str)],
        msg: &str,
        name: &str,
        email: &str,
        date: Option<&str>,
    ) -> String {
        for (path, content) in files {
            let p = self.dir.path().join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, content).unwrap();
        }
        ok(git(self.dir.path(), &["add", "-A"]).await);
        let mut env = vec![("GIT_AUTHOR_NAME", name), ("GIT_AUTHOR_EMAIL", email)];
        if let Some(d) = date {
            env.push(("GIT_AUTHOR_DATE", d));
            env.push(("GIT_COMMITTER_DATE", d));
        }
        ok(git_env(
            self.dir.path(),
            &["commit", "-q", "--allow-empty", "-m", msg],
            &env,
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

    pub async fn push(&self, refspec: &str) -> GitOutput {
        git(self.dir.path(), &["push", "-q", &self.remote, refspec]).await
    }
}

/// Create `user/name` through the API and a local repository on `main`
/// with `files` committed and pushed. Background jobs are drained.
pub async fn seeded(app: &TestApp, user: &TestUser, name: &str, files: &[(&str, &str)]) -> Work {
    app.create_repo(user, name).await;
    let work = Work {
        dir: tempfile::tempdir().unwrap(),
        remote: app.git_remote(user, &user.login, name),
    };
    ok(git(work.dir.path(), &["init", "-q", "-b", "main"]).await);
    work.commit(files, "initial commit").await;
    ok(work.push("main").await);
    app.drain_jobs().await;
    work
}
