//! Spawning `git` with an environment isolated from the host's config.

use std::path::Path;
use std::process::Stdio;

use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::{GitError, GitResult};

/// Environment variables that must not leak into server-side git processes.
const SCRUB: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
    "GIT_CEILING_DIRECTORIES",
    "GIT_PROTOCOL",
    "GIT_CONFIG",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
];

/// A `git` command with `--git-dir` (when given) and an isolated
/// configuration (no system/global config, no prompts, no signing).
pub(crate) fn git(bin: &str, git_dir: Option<&Path>) -> Command {
    let mut c = Command::new(bin);
    for k in SCRUB {
        c.env_remove(k);
    }
    c.env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .kill_on_drop(true);
    if let Some(dir) = git_dir {
        c.arg("--git-dir").arg(dir);
    }
    c
}

/// Blocking variant of [`run`] (no stdin) for use inside `spawn_blocking`.
pub(crate) fn run_blocking(bin: &str, git_dir: &Path, args: &[&str]) -> GitResult<Vec<u8>> {
    let mut c = std::process::Command::new(bin);
    for k in SCRUB {
        c.env_remove(k);
    }
    let out = c
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .arg("--git-dir")
        .arg(git_dir)
        .args(args)
        .stdin(Stdio::null())
        .output()?;
    if !out.status.success() {
        return Err(GitError::Command {
            args: args.join(" "),
            status: out.status.to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    Ok(out.stdout)
}

/// Run git to completion, optionally feeding `stdin`; returns stdout.
pub(crate) async fn run(
    bin: &str,
    git_dir: Option<&Path>,
    args: &[&str],
    envs: &[(&str, &str)],
    stdin: Option<&[u8]>,
) -> GitResult<Vec<u8>> {
    let mut c = git(bin, git_dir);
    c.args(args)
        .envs(envs.iter().copied())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if stdin.is_some() {
        c.stdin(Stdio::piped());
    }
    let mut child = c.spawn()?;
    if let Some(input) = stdin {
        let mut pipe = child.stdin.take().expect("piped stdin");
        let input = input.to_vec();
        tokio::spawn(async move {
            let _ = pipe.write_all(&input).await;
            let _ = pipe.shutdown().await;
        });
    }
    let out = child.wait_with_output().await?;
    if !out.status.success() {
        return Err(GitError::Command {
            args: args.join(" "),
            status: out.status.to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    Ok(out.stdout)
}

/// Raw result of a git invocation that may legitimately exit non-zero
/// (e.g. `merge-tree` exits 1 on conflicts).
pub(crate) struct RawOutput {
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Run git to completion and return its exit code and output without
/// treating a non-zero exit as an error.
pub(crate) async fn run_status(
    bin: &str,
    git_dir: Option<&Path>,
    args: &[&str],
    envs: &[(&str, &str)],
) -> GitResult<RawOutput> {
    let mut c = git(bin, git_dir);
    c.args(args)
        .envs(envs.iter().copied())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = c.spawn()?.wait_with_output().await?;
    Ok(RawOutput {
        code: out.status.code(),
        stdout: out.stdout,
        stderr: out.stderr,
    })
}

/// Spawn git with piped stdout for streaming its output.
pub(crate) fn spawn_stdout(
    bin: &str,
    git_dir: &Path,
    args: &[&str],
) -> GitResult<tokio::process::Child> {
    let mut c = git(bin, Some(git_dir));
    c.args(args).stdout(Stdio::piped()).stderr(Stdio::null());
    Ok(c.spawn()?)
}
