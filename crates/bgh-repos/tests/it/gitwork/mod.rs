//! Shared helpers for tests that need real git history (repos API and git
//! transport tests).
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
        "GIT_SSH_COMMAND",
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

/// Author/committer date of the commit made by [`seeded`]. Fixed so tests
/// that bucket commits by time (e.g. the stats punch card) don't depend on
/// the wall clock; it is a Friday that predates every fixture date.
pub const SEED_DATE: &str = "2023-12-01T00:00:00Z";

/// Create `user/name` through the API and a local repository on `main`
/// with `files` committed and pushed. Background jobs are drained.
pub async fn seeded(app: &TestApp, user: &TestUser, name: &str, files: &[(&str, &str)]) -> Work {
    app.create_repo(user, name).await;
    let work = Work {
        dir: tempfile::tempdir().unwrap(),
        remote: app.git_remote(user, &user.login, name),
    };
    ok(git(work.dir.path(), &["init", "-q", "-b", "main"]).await);
    work.commit_as(
        files,
        "initial commit",
        "Test",
        "test@example.com",
        Some(SEED_DATE),
    )
    .await;
    ok(work.push("main").await);
    app.drain_jobs().await;
    work
}

/// A local working copy on `main` (no commits yet).
pub async fn init_work(dir: &Path) {
    ok(git(dir, &["init", "-q", "-b", "main"]).await);
}

/// Write files and commit them as `author <email>`; returns the commit SHA.
pub async fn commit_files(
    dir: &Path,
    files: &[(&str, &[u8])],
    message: &str,
    author: (&str, &str),
) -> String {
    for (path, content) in files {
        let p = dir.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }
    ok(git(dir, &["add", "-A"]).await);
    let envs = [
        ("GIT_AUTHOR_NAME", author.0),
        ("GIT_AUTHOR_EMAIL", author.1),
        ("GIT_COMMITTER_NAME", author.0),
        ("GIT_COMMITTER_EMAIL", author.1),
    ];
    ok(git_env(dir, &["commit", "-q", "-m", message], &envs).await);
    ok(git(dir, &["rev-parse", "HEAD"]).await)
        .stdout
        .trim()
        .to_string()
}

pub async fn push(
    app: &TestApp,
    user: &TestUser,
    dir: &Path,
    owner: &str,
    repo: &str,
    refs: &[&str],
) {
    let remote = app.git_remote(user, owner, repo);
    let mut args = vec!["push", "-q", remote.as_str()];
    args.extend_from_slice(refs);
    ok(git(dir, &args).await);
    app.drain_jobs().await;
}

/// Add `user` as a collaborator with `permission` (direct SQL; the
/// collaborators API lives in another package).
pub async fn add_collaborator(
    app: &TestApp,
    owner: &TestUser,
    repo: &str,
    user: &TestUser,
    permission: &str,
) {
    sqlx::query(
        "INSERT INTO collaborators (repo_id, user_id, permission)
         SELECT id, $3, $4 FROM repositories WHERE owner_id = $1 AND lower(name) = lower($2)",
    )
    .bind(owner.id)
    .bind(repo)
    .bind(user.id)
    .bind(permission)
    .execute(&app.state.db)
    .await
    .unwrap();
}

pub fn sha256_hex(data: &[u8]) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(data))
}

pub async fn repo_id(app: &TestApp, owner: &TestUser, repo: &str) -> i64 {
    bgh_core::models::db::Repository::find_by_name(&app.state.db, owner.id, repo)
        .await
        .unwrap()
        .unwrap()
        .id
}
