//! On-disk storage: content-addressed blobs in
//! `{data_dir}/packages/blobs/{alg}/{hex[..2]}/{hex}` (one copy per digest,
//! shared by every package) and upload sessions in
//! `{data_dir}/packages/uploads/{uuid}`.

use std::path::{Path, PathBuf};

use axum::body::Body;
use bgh_core::AppState;
use futures::StreamExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::digest::{Algorithm, Digest};

/// Pg advisory lock key serializing blob file creation against the GC's
/// deletion of unreferenced blobs ("pkgblob" as ASCII).
pub const BLOB_LOCK_KEY: i64 = 0x706b_6762_6c6f_62;

pub fn root(state: &AppState) -> PathBuf {
    state.config.data_dir.join("packages")
}

pub fn blob_path(state: &AppState, digest: &Digest) -> PathBuf {
    root(state)
        .join("blobs")
        .join(digest.algorithm.name())
        .join(&digest.hex[..2])
        .join(&digest.hex)
}

pub fn upload_path(state: &AppState, id: &uuid::Uuid) -> PathBuf {
    root(state).join("uploads").join(id.to_string())
}

/// Append a request body to a file; returns the number of bytes written.
pub async fn append_body(path: &Path, body: Body) -> std::io::Result<u64> {
    if let Some(dir) = path.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await?;
    let mut stream = body.into_data_stream();
    let mut n = 0u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(std::io::Error::other)?;
        n += chunk.len() as u64;
        file.write_all(&chunk).await?;
    }
    file.flush().await?;
    file.sync_data().await?;
    Ok(n)
}

/// Hash a file with `algorithm`; returns the digest and the size.
pub async fn hash_file(path: &Path, algorithm: Algorithm) -> std::io::Result<(Digest, u64)> {
    let mut file = tokio::fs::File::open(path).await?;
    let mut hasher = algorithm.hasher();
    let mut buf = vec![0u8; 256 * 1024];
    let mut size = 0u64;
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        size += n as u64;
        hasher.update(&buf[..n]);
    }
    Ok((hasher.finish(), size))
}

/// Move a verified upload into the blob store (no-op when the digest is
/// already stored: content addressing dedupes).
pub async fn commit_blob(state: &AppState, from: &Path, digest: &Digest) -> std::io::Result<()> {
    let dest = blob_path(state, digest);
    if tokio::fs::try_exists(&dest).await? {
        let _ = tokio::fs::remove_file(from).await;
        return Ok(());
    }
    tokio::fs::create_dir_all(dest.parent().expect("blob path has a parent")).await?;
    tokio::fs::rename(from, &dest).await
}

/// Store small in-memory content (manifests are kept in the database, but
/// clients may also fetch them as blobs).
pub async fn write_blob(state: &AppState, digest: &Digest, data: &[u8]) -> std::io::Result<()> {
    let dest = blob_path(state, digest);
    if tokio::fs::try_exists(&dest).await? {
        return Ok(());
    }
    tokio::fs::create_dir_all(dest.parent().expect("blob path has a parent")).await?;
    let tmp = dest.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
    tokio::fs::write(&tmp, data).await?;
    tokio::fs::rename(&tmp, &dest).await
}

/// Read a small blob (image configs) fully, at most `limit` bytes.
pub async fn read_small(state: &AppState, digest: &Digest, limit: u64) -> Option<Vec<u8>> {
    let path = blob_path(state, digest);
    let meta = tokio::fs::metadata(&path).await.ok()?;
    if meta.len() > limit {
        return None;
    }
    tokio::fs::read(&path).await.ok()
}
