//! On-disk repository storage.

use std::path::{Path, PathBuf};

use bgh_core::config::Config;

use crate::cmd;
use crate::read::GitRepo;
use crate::{GitError, GitResult};

/// Settings appended to every repository's `config`.
const REPO_CONFIG: &str = "\
[core]
\tlogAllRefUpdates = false
[receive]
\tadvertisePushOptions = true
\tautogc = false
\tunpackLimit = 100
[uploadpack]
\tallowFilter = true
\tallowReachableSHA1InWant = true
[gc]
\tauto = 0
";

/// Where repositories live and how to reach the `git` binary.
#[derive(Debug, Clone)]
pub struct RepoStore {
    /// `{data_dir}/repos`
    pub root: PathBuf,
    pub git_bin: String,
    /// Blob size limit for API reads (bytes).
    pub max_blob_size: u64,
}

impl RepoStore {
    pub fn new(root: impl Into<PathBuf>, git_bin: impl Into<String>) -> Self {
        Self {
            root: root.into(),
            git_bin: git_bin.into(),
            max_blob_size: 10 * 1024 * 1024,
        }
    }

    pub fn from_config(config: &Config) -> Self {
        Self {
            root: config.repos_dir(),
            git_bin: config.git_bin.clone(),
            max_blob_size: config.max_blob_size,
        }
    }

    /// `{root}/{id % 256 as 2-hex}/{id}.git`
    pub fn path(&self, repo_id: i64) -> PathBuf {
        self.root
            .join(format!("{:02x}", repo_id.rem_euclid(256)))
            .join(format!("{repo_id}.git"))
    }

    pub fn exists(&self, repo_id: i64) -> bool {
        self.path(repo_id).join("HEAD").is_file()
    }

    async fn configure(&self, path: &Path) -> GitResult<()> {
        let cfg = path.join("config");
        let mut existing = tokio::fs::read_to_string(&cfg).await.unwrap_or_default();
        existing.push_str(REPO_CONFIG);
        tokio::fs::write(&cfg, existing).await?;
        Ok(())
    }

    /// Create an empty bare repository whose HEAD points at `default_branch`.
    pub async fn init(&self, repo_id: i64, default_branch: &str) -> GitResult<PathBuf> {
        if !crate::is_valid_ref_name(default_branch) {
            return Err(GitError::InvalidInput(format!(
                "invalid branch name {default_branch:?}"
            )));
        }
        let path = self.path(repo_id);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let p = path.to_string_lossy().to_string();
        let branch = format!("--initial-branch={default_branch}");
        cmd::run(
            &self.git_bin,
            None,
            &["init", "--bare", "--quiet", "--template=", &branch, &p],
            &[],
            None,
        )
        .await?;
        self.configure(&path).await?;
        Ok(path)
    }

    /// Create `dst_id` as a fork of `src_id`: a bare clone sharing objects
    /// through `objects/info/alternates`. Copies all branches and tags.
    ///
    /// Note: before deleting a repository that has forks, forks must be
    /// made self-contained (`git repack -a -d` + remove alternates).
    pub async fn fork(&self, src_id: i64, dst_id: i64) -> GitResult<PathBuf> {
        let src = self.path(src_id);
        if !self.exists(src_id) {
            return Err(GitError::NotFound(format!("repository {src_id}")));
        }
        let dst = self.path(dst_id);
        if let Some(parent) = dst.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let (s, d) = (
            src.to_string_lossy().to_string(),
            dst.to_string_lossy().to_string(),
        );
        cmd::run(
            &self.git_bin,
            None,
            &[
                "clone",
                "--bare",
                "--shared",
                "--quiet",
                "--template=",
                &s,
                &d,
            ],
            &[],
            None,
        )
        .await?;
        cmd::run(
            &self.git_bin,
            Some(&dst),
            &["remote", "remove", "origin"],
            &[],
            None,
        )
        .await?;
        self.configure(&dst).await?;
        Ok(dst)
    }

    /// Remove a repository from disk (idempotent).
    pub async fn delete(&self, repo_id: i64) -> GitResult<()> {
        crate::cache::evict(&self.path(repo_id));
        match tokio::fs::remove_dir_all(self.path(repo_id)).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// Open for in-process reads (blocking; see [`Self::read`]).
    pub fn open(&self, repo_id: i64) -> GitResult<GitRepo> {
        if !self.exists(repo_id) {
            return Err(GitError::NotFound(format!("repository {repo_id}")));
        }
        Ok(
            GitRepo::open_cached(&self.path(repo_id), self.max_blob_size)?
                .with_git_bin(&self.git_bin),
        )
    }

    /// Run a blocking read against a repository on the blocking thread pool.
    ///
    /// ```ignore
    /// let branches = store.read(repo.id, |r| r.branches()).await?;
    /// ```
    pub async fn read<T, F>(&self, repo_id: i64, f: F) -> GitResult<T>
    where
        T: Send + 'static,
        F: FnOnce(&GitRepo) -> GitResult<T> + Send + 'static,
    {
        let store = self.clone();
        tokio::task::spawn_blocking(move || {
            let repo = store.open(repo_id)?;
            f(&repo)
        })
        .await
        .map_err(|e| GitError::Object(format!("blocking task failed: {e}")))?
    }

    /// Size of the repository on disk in KB (objects only).
    pub async fn disk_size_kb(&self, repo_id: i64) -> GitResult<i64> {
        let objects = self.path(repo_id).join("objects");
        tokio::task::spawn_blocking(move || {
            fn walk(p: &Path) -> u64 {
                let Ok(rd) = std::fs::read_dir(p) else {
                    return 0;
                };
                rd.flatten()
                    .map(|e| match e.file_type() {
                        Ok(t) if t.is_dir() => walk(&e.path()),
                        Ok(_) => e.metadata().map(|m| m.len()).unwrap_or(0),
                        Err(_) => 0,
                    })
                    .sum()
            }
            (walk(&objects) / 1024) as i64
        })
        .await
        .map_err(|e| GitError::Object(e.to_string()))
    }

    /// Path helper for transport code.
    pub fn git_dir(&self, repo_id: i64) -> GitResult<PathBuf> {
        if self.exists(repo_id) {
            Ok(self.path(repo_id))
        } else {
            Err(GitError::NotFound(format!("repository {repo_id}")))
        }
    }
}
