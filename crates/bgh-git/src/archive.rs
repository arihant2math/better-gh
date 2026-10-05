//! Source archives (`git archive`), streamed and cached by commit SHA.
//!
//! The first request for `(commit, format, prefix)` streams `git archive`
//! output to the client while teeing it into a temporary file that is
//! atomically moved into the cache when git exits successfully; later
//! requests stream the cached file. An interrupted download leaves no cache
//! entry behind (the temp file is removed on drop).

use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use bytes::Bytes;
use futures::StreamExt;
use futures::stream::BoxStream;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::io::ReaderStream;

use crate::storage::RepoStore;
use crate::{GitError, GitResult, cmd, is_sha};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveFormat {
    TarGz,
    Zip,
}

impl ArchiveFormat {
    /// Parse a file name suffix: `name.tar.gz` / `name.tgz` / `name.zip`.
    /// Returns the format and the name without the suffix.
    pub fn split(name: &str) -> Option<(&str, Self)> {
        if let Some(r) = name.strip_suffix(".tar.gz") {
            Some((r, Self::TarGz))
        } else if let Some(r) = name.strip_suffix(".tgz") {
            Some((r, Self::TarGz))
        } else {
            name.strip_suffix(".zip").map(|r| (r, Self::Zip))
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::TarGz => "tar.gz",
            Self::Zip => "zip",
        }
    }

    pub fn content_type(self) -> &'static str {
        match self {
            Self::TarGz => "application/x-gzip",
            Self::Zip => "application/zip",
        }
    }

    fn git_format(self) -> &'static str {
        match self {
            Self::TarGz => "tar.gz",
            Self::Zip => "zip",
        }
    }
}

/// A streamed archive body.
pub struct Archive {
    pub stream: BoxStream<'static, io::Result<Bytes>>,
    /// Size in bytes when served from the cache.
    pub len: Option<u64>,
    /// Whether the archive was served from the cache.
    pub cached: bool,
}

/// Cache file for an archive.
pub fn cache_path(
    cache_dir: &Path,
    repo_id: i64,
    commit: &str,
    format: ArchiveFormat,
    prefix: &str,
) -> PathBuf {
    let digest = Sha256::digest(prefix.as_bytes());
    cache_dir.join(repo_id.to_string()).join(format!(
        "{commit}-{}.{}",
        hex::encode(&digest[..8]),
        format.extension()
    ))
}

/// Stream an archive of `commit` (a full commit SHA) with every path under
/// `prefix` (e.g. `repo-main/`).
pub async fn archive(
    store: &RepoStore,
    repo_id: i64,
    commit: &str,
    format: ArchiveFormat,
    prefix: &str,
    cache_dir: &Path,
) -> GitResult<Archive> {
    if !is_sha(commit) {
        return Err(GitError::InvalidInput(format!(
            "not a commit sha: {commit}"
        )));
    }
    if prefix.contains('\0') || prefix.starts_with('-') {
        return Err(GitError::InvalidInput("invalid archive prefix".into()));
    }
    let path = cache_path(cache_dir, repo_id, commit, format, prefix);
    if let Ok(file) = tokio::fs::File::open(&path).await {
        let len = file.metadata().await.ok().map(|m| m.len());
        return Ok(Archive {
            stream: ReaderStream::with_capacity(file, 64 * 1024).boxed(),
            len,
            cached: true,
        });
    }

    let dir = store.git_dir(repo_id)?;
    let mut c = cmd::git(&store.git_bin, Some(&dir));
    c.arg("archive")
        .arg(format!("--format={}", format.git_format()))
        .arg(format!("--prefix={prefix}"))
        .arg(commit)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = c.spawn()?;
    let stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let stderr_task = tokio::spawn(async move {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s).await;
        s
    });

    // Temp file next to the final location so the rename is atomic.
    let parent = path
        .parent()
        .expect("cache path has a parent")
        .to_path_buf();
    let tee = async {
        tokio::fs::create_dir_all(&parent).await?;
        let tmp = tempfile::NamedTempFile::new_in(&parent)?;
        let file = tokio::fs::File::from_std(tmp.reopen()?);
        io::Result::Ok((tmp, file))
    }
    .await
    .map_err(|err| tracing::warn!(?err, "archive cache unavailable"))
    .ok();

    struct State {
        stdout: tokio::process::ChildStdout,
        child: tokio::process::Child,
        stderr_task: Option<tokio::task::JoinHandle<String>>,
        tee: Option<(tempfile::NamedTempFile, tokio::fs::File)>,
        path: PathBuf,
        done: bool,
    }
    let state = State {
        stdout,
        child,
        stderr_task: Some(stderr_task),
        tee,
        path,
        done: false,
    };
    let stream = futures::stream::unfold(state, |mut st| async move {
        if st.done {
            return None;
        }
        let mut buf = vec![0u8; 64 * 1024];
        match st.stdout.read(&mut buf).await {
            Ok(0) => {
                st.done = true;
                let status = st.child.wait().await;
                let err = match st.stderr_task.take() {
                    Some(t) => t.await.unwrap_or_default(),
                    None => String::new(),
                };
                if !matches!(&status, Ok(s) if s.success()) {
                    tracing::warn!(?status, stderr = %err.trim(), "git archive failed");
                    return Some((Err(io::Error::other("git archive failed")), st));
                }
                if let Some((tmp, mut file)) = st.tee.take() {
                    let persisted = async {
                        file.flush().await?;
                        file.sync_all().await?;
                        drop(file);
                        tmp.persist(&st.path).map_err(|e| e.error)?;
                        io::Result::Ok(())
                    }
                    .await;
                    if let Err(err) = persisted {
                        tracing::warn!(?err, "failed to store archive in cache");
                    }
                }
                None
            }
            Ok(n) => {
                buf.truncate(n);
                if let Some((_, file)) = &mut st.tee
                    && file.write_all(&buf).await.is_err()
                {
                    st.tee = None; // stop caching, keep streaming
                }
                Some((Ok(Bytes::from(buf)), st))
            }
            Err(e) => {
                st.done = true;
                Some((Err(e), st))
            }
        }
    });
    Ok(Archive {
        stream: stream.boxed(),
        len: None,
        cached: false,
    })
}

/// Delete cached archives not accessed for `max_age` (by mtime) and remove
/// the cache of deleted repositories. Returns the number of files removed.
pub async fn prune_cache(cache_dir: &Path, max_age: std::time::Duration) -> io::Result<usize> {
    let dir = cache_dir.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut removed = 0;
        let Ok(repos) = std::fs::read_dir(&dir) else {
            return Ok(0);
        };
        let now = std::time::SystemTime::now();
        for repo in repos.flatten() {
            let Ok(files) = std::fs::read_dir(repo.path()) else {
                continue;
            };
            for f in files.flatten() {
                let old = f
                    .metadata()
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| now.duration_since(t).ok())
                    .is_some_and(|age| age > max_age);
                if old && std::fs::remove_file(f.path()).is_ok() {
                    removed += 1;
                }
            }
            let _ = std::fs::remove_dir(repo.path()); // only succeeds when empty
        }
        Ok(removed)
    })
    .await
    .map_err(io::Error::other)?
}

/// Trim the archive cache to at most `max_bytes`, removing the least
/// recently written files first. Returns the number of files removed.
pub async fn prune_cache_to_size(cache_dir: &Path, max_bytes: u64) -> io::Result<usize> {
    let dir = cache_dir.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut files = Vec::new();
        let Ok(repos) = std::fs::read_dir(&dir) else {
            return Ok(0);
        };
        for repo in repos.flatten() {
            let Ok(entries) = std::fs::read_dir(repo.path()) else {
                continue;
            };
            for f in entries.flatten() {
                if let Ok(m) = f.metadata()
                    && m.is_file()
                {
                    let mtime = m.modified().unwrap_or(std::time::UNIX_EPOCH);
                    files.push((mtime, m.len(), f.path()));
                }
            }
        }
        let mut total: u64 = files.iter().map(|f| f.1).sum();
        files.sort_by_key(|f| f.0);
        let mut removed = 0;
        for (_, len, path) in files {
            if total <= max_bytes {
                break;
            }
            if std::fs::remove_file(&path).is_ok() {
                total = total.saturating_sub(len);
                removed += 1;
                if let Some(parent) = path.parent() {
                    let _ = std::fs::remove_dir(parent); // only when empty
                }
            }
        }
        Ok(removed)
    })
    .await
    .map_err(io::Error::other)?
}

/// Remove every cached archive of a repository.
pub async fn purge_repo(cache_dir: &Path, repo_id: i64) -> io::Result<()> {
    match tokio::fs::remove_dir_all(cache_dir.join(repo_id.to_string())).await {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_names() {
        assert_eq!(
            ArchiveFormat::split("main.tar.gz"),
            Some(("main", ArchiveFormat::TarGz))
        );
        assert_eq!(
            ArchiveFormat::split("v1.0.zip"),
            Some(("v1.0", ArchiveFormat::Zip))
        );
        assert_eq!(ArchiveFormat::split("main"), None);
    }
}
