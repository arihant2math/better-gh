//! Content-addressed blob store for attachments:
//! `{data_dir}/files/attachments/{sha[..2]}/{sha}`. Identical uploads share
//! one blob; [`crate::gc`] removes blobs no attachment references.

use std::io;
use std::path::{Path, PathBuf};

use bgh_core::AppState;
use futures::{Stream, StreamExt};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

/// Root of the attachment store.
pub fn root(state: &AppState) -> PathBuf {
    state.config.data_dir.join("files").join("attachments")
}

/// In-flight uploads (inside the store root, so the final move is a rename;
/// skipped by the garbage collector).
fn tmp_dir(state: &AppState) -> PathBuf {
    root(state).join("tmp")
}

pub fn is_digest(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Path of the blob `sha256` (validated hex digest).
pub fn blob_path(state: &AppState, sha256: &str) -> io::Result<PathBuf> {
    if !is_digest(sha256) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "bad digest"));
    }
    Ok(root(state).join(&sha256[..2]).join(sha256))
}

/// Move a spooled file into the store (idempotent).
pub async fn put(state: &AppState, sha256: &str, file: &Path) -> io::Result<()> {
    let dest = blob_path(state, sha256)?;
    if tokio::fs::try_exists(&dest).await? {
        tokio::fs::remove_file(file).await?;
        // Refresh the mtime so a concurrent GC's grace period covers the
        // row we are about to insert.
        let f = std::fs::File::options().append(true).open(&dest)?;
        f.set_modified(std::time::SystemTime::now())?;
        return Ok(());
    }
    tokio::fs::create_dir_all(dest.parent().expect("has parent")).await?;
    tokio::fs::rename(file, &dest).await
}

/// Delete a blob (missing is fine).
pub async fn delete(state: &AppState, sha256: &str) -> io::Result<()> {
    match tokio::fs::remove_file(blob_path(state, sha256)?).await {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// A body streamed to a temp file.
pub struct Spooled {
    pub path: PathBuf,
    pub sha256: String,
    pub size: u64,
    /// The first bytes, for content sniffing.
    pub head: Vec<u8>,
}

#[derive(Debug)]
pub enum SpoolError {
    TooLarge,
    Io(io::Error),
}

const HEAD_LEN: usize = 512;

/// Stream `body` into a temp file, hashing it; [`SpoolError::TooLarge`]
/// beyond `max` bytes (the partial file is removed).
pub async fn spool<S, E>(state: &AppState, body: S, max: u64) -> Result<Spooled, SpoolError>
where
    S: Stream<Item = Result<bytes::Bytes, E>>,
    E: std::fmt::Display,
{
    let dir = tmp_dir(state);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(SpoolError::Io)?;
    let path = dir.join(format!("upload-{}", bgh_core::crypto::random_token(16)));
    let result = async {
        let mut body = std::pin::pin!(body);
        let mut file = tokio::fs::File::create(&path)
            .await
            .map_err(SpoolError::Io)?;
        let mut hasher = Sha256::new();
        let mut size = 0u64;
        let mut head = Vec::new();
        while let Some(chunk) = body.next().await {
            let chunk = chunk.map_err(|e| SpoolError::Io(io::Error::other(e.to_string())))?;
            size += chunk.len() as u64;
            if size > max {
                return Err(SpoolError::TooLarge);
            }
            if head.len() < HEAD_LEN {
                let n = (HEAD_LEN - head.len()).min(chunk.len());
                head.extend_from_slice(&chunk[..n]);
            }
            hasher.update(&chunk);
            file.write_all(&chunk).await.map_err(SpoolError::Io)?;
        }
        file.flush().await.map_err(SpoolError::Io)?;
        file.sync_all().await.map_err(SpoolError::Io)?;
        Ok((hex::encode(hasher.finalize()), size, head))
    }
    .await;
    match result {
        Ok((sha256, size, head)) => Ok(Spooled {
            path,
            sha256,
            size,
            head,
        }),
        Err(e) => {
            let _ = tokio::fs::remove_file(&path).await;
            Err(e)
        }
    }
}
