//! Write operations through git plumbing (`hash-object`, `update-index`,
//! `write-tree`, `commit-tree`, `update-ref`). All ref updates use
//! old-value checks so concurrent writers can't silently clobber each other.

use chrono::{DateTime, Utc};

use crate::storage::RepoStore;
use crate::{GitError, GitResult, ZERO_SHA, cmd, is_valid_ref_name};

/// Commit author/committer identity.
#[derive(Debug, Clone)]
pub struct Identity {
    pub name: String,
    pub email: String,
    /// Defaults to now.
    pub when: Option<DateTime<Utc>>,
}

impl Identity {
    pub fn new(name: impl Into<String>, email: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            email: email.into(),
            when: None,
        }
    }
}

/// A change to apply in [`commit_changes`].
#[derive(Debug, Clone)]
pub enum FileChange {
    /// Create or replace a file (`executable` → mode 100755).
    Write {
        path: String,
        content: Vec<u8>,
        executable: bool,
    },
    /// Delete a file.
    Delete { path: String },
}

impl FileChange {
    pub fn write(path: impl Into<String>, content: impl Into<Vec<u8>>) -> Self {
        Self::Write {
            path: path.into(),
            content: content.into(),
            executable: false,
        }
    }
}

/// Options for [`commit_changes`].
#[derive(Debug, Clone)]
pub struct CommitRequest<'a> {
    /// Branch to update (`main`, not `refs/heads/main`).
    pub branch: &'a str,
    /// Expected current tip of the branch; `None` creates the branch (and
    /// fails if it already exists).
    pub parent: Option<&'a str>,
    pub changes: &'a [FileChange],
    pub message: &'a str,
    pub author: &'a Identity,
    /// Defaults to the author.
    pub committer: Option<&'a Identity>,
}

fn ident_env(prefix: &str, id: &Identity) -> Vec<(String, String)> {
    let mut env = vec![
        (format!("GIT_{prefix}_NAME"), id.name.clone()),
        (format!("GIT_{prefix}_EMAIL"), id.email.clone()),
    ];
    if let Some(when) = id.when {
        env.push((
            format!("GIT_{prefix}_DATE"),
            format!("{} +0000", when.timestamp()),
        ));
    }
    env
}

fn validate_path(path: &str) -> GitResult<()> {
    let bad = path.is_empty()
        || path.starts_with('/')
        || path.ends_with('/')
        || path
            .split('/')
            .any(|c| c.is_empty() || c == "." || c == ".." || c == ".git")
        || path.contains('\0');
    if bad {
        Err(GitError::InvalidInput(format!("invalid path {path:?}")))
    } else {
        Ok(())
    }
}

/// Create a commit applying `changes` on top of `parent` and advance
/// `refs/heads/{branch}` from `parent` to it. Returns the new commit SHA.
pub async fn commit_changes(
    store: &RepoStore,
    repo_id: i64,
    req: CommitRequest<'_>,
) -> GitResult<String> {
    if !is_valid_ref_name(req.branch) {
        return Err(GitError::InvalidInput(format!(
            "invalid branch name {:?}",
            req.branch
        )));
    }
    for c in req.changes {
        match c {
            FileChange::Write { path, .. } | FileChange::Delete { path } => validate_path(path)?,
        }
    }
    let dir = store.git_dir(repo_id)?;
    let bin = store.git_bin.as_str();
    let index = dir.join(format!("bgh-index-{}", bgh_core::crypto::random_token(12)));
    let index_s = index.to_string_lossy().to_string();
    let idx_env = [("GIT_INDEX_FILE", index_s.as_str())];

    let result = async {
        match req.parent {
            Some(p) => cmd::run(bin, Some(&dir), &["read-tree", p], &idx_env, None).await?,
            None => cmd::run(bin, Some(&dir), &["read-tree", "--empty"], &idx_env, None).await?,
        };
        for change in req.changes {
            match change {
                FileChange::Write {
                    path,
                    content,
                    executable,
                } => {
                    let sha = cmd::run(
                        bin,
                        Some(&dir),
                        &["hash-object", "-w", "--stdin"],
                        &[],
                        Some(content),
                    )
                    .await?;
                    let sha = String::from_utf8_lossy(&sha).trim().to_string();
                    let mode = if *executable { "100755" } else { "100644" };
                    let info = format!("{mode},{sha},{path}");
                    cmd::run(
                        bin,
                        Some(&dir),
                        &["update-index", "--add", "--cacheinfo", &info],
                        &idx_env,
                        None,
                    )
                    .await?;
                }
                FileChange::Delete { path } => {
                    // `--force-remove` needs a work tree; mode 0 via
                    // `--index-info` removes the entry in bare repos.
                    let info = format!("0 {ZERO_SHA}\t{path}\n");
                    cmd::run(
                        bin,
                        Some(&dir),
                        &["update-index", "--index-info"],
                        &idx_env,
                        Some(info.as_bytes()),
                    )
                    .await?;
                }
            }
        }
        let tree = cmd::run(bin, Some(&dir), &["write-tree"], &idx_env, None).await?;
        let tree = String::from_utf8_lossy(&tree).trim().to_string();

        let mut env: Vec<(String, String)> = ident_env("AUTHOR", req.author);
        env.extend(ident_env("COMMITTER", req.committer.unwrap_or(req.author)));
        let env_ref: Vec<(&str, &str)> =
            env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let mut args = vec!["commit-tree", tree.as_str(), "--no-gpg-sign", "-F", "-"];
        if let Some(p) = req.parent {
            args.push("-p");
            args.push(p);
        }
        let commit = cmd::run(
            bin,
            Some(&dir),
            &args,
            &env_ref,
            Some(req.message.as_bytes()),
        )
        .await?;
        let commit = String::from_utf8_lossy(&commit).trim().to_string();

        let refname = format!("refs/heads/{}", req.branch);
        let old = req.parent.unwrap_or(ZERO_SHA);
        cmd::run(
            bin,
            Some(&dir),
            &["update-ref", &refname, &commit, old],
            &[],
            None,
        )
        .await?;
        Ok(commit)
    }
    .await;
    let _ = tokio::fs::remove_file(&index).await;
    result
}

/// Point HEAD at `refs/heads/{branch}` (the default branch).
pub async fn set_head(store: &RepoStore, repo_id: i64, branch: &str) -> GitResult<()> {
    if !is_valid_ref_name(branch) {
        return Err(GitError::InvalidInput(format!(
            "invalid branch name {branch:?}"
        )));
    }
    let dir = store.git_dir(repo_id)?;
    let target = format!("refs/heads/{branch}");
    cmd::run(
        &store.git_bin,
        Some(&dir),
        &["symbolic-ref", "HEAD", &target],
        &[],
        None,
    )
    .await?;
    Ok(())
}

/// Update `refname` to `new`, requiring its current value to be `old`
/// (`None` = must not exist).
pub async fn update_ref(
    store: &RepoStore,
    repo_id: i64,
    refname: &str,
    new: &str,
    old: Option<&str>,
) -> GitResult<()> {
    if !refname.starts_with("refs/") || !is_valid_ref_name(refname) {
        return Err(GitError::InvalidInput(format!("invalid ref {refname:?}")));
    }
    let dir = store.git_dir(repo_id)?;
    cmd::run(
        &store.git_bin,
        Some(&dir),
        &["update-ref", refname, new, old.unwrap_or(ZERO_SHA)],
        &[],
        None,
    )
    .await?;
    Ok(())
}

/// Delete `refname`, requiring its current value to be `old` when given.
pub async fn delete_ref(
    store: &RepoStore,
    repo_id: i64,
    refname: &str,
    old: Option<&str>,
) -> GitResult<()> {
    if !refname.starts_with("refs/") || !is_valid_ref_name(refname) {
        return Err(GitError::InvalidInput(format!("invalid ref {refname:?}")));
    }
    let dir = store.git_dir(repo_id)?;
    let mut args = vec!["update-ref", "-d", refname];
    if let Some(o) = old {
        args.push(o);
    }
    cmd::run(&store.git_bin, Some(&dir), &args, &[], None).await?;
    Ok(())
}
