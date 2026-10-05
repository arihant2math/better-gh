//! Shared helpers for git-transport integration tests.
#![allow(dead_code)]

use std::path::Path;

use bgh_core::testing::{TestApp, TestUser};

pub struct GitOutput {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Run git with an isolated config (async: the server shares this runtime).
pub async fn git_env(dir: &Path, args: &[&str], envs: &[(&str, &str)]) -> GitOutput {
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
        .envs(envs.iter().copied())
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
