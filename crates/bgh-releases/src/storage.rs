//! Release asset storage.
//!
//! Assets are content-addressed by their SHA-256: identical uploads share
//! one stored blob, and a blob is deleted only when no asset references it
//! anymore. [`AssetStorage`] abstracts the backend; [`DiskStorage`]
//! (`{data_dir}/files/release-assets/{sha[..2]}/{sha}`) is the default. An
//! S3-compatible backend can be installed at startup with
//! `state.with_extension(SharedStorage(Arc::new(...)))`.

use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use bgh_core::AppState;
use futures::future::BoxFuture;
use futures::{Stream, StreamExt};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncWriteExt};

pub type Reader = Pin<Box<dyn AsyncRead + Send>>;

/// A storage backend for asset blobs keyed by hex SHA-256.
pub trait AssetStorage: Send + Sync + 'static {
    /// Move a fully written, hashed local file into the store under `sha256`.
    /// Must be idempotent (the blob may already exist).
    fn put_file<'a>(&'a self, sha256: &'a str, file: &'a Path) -> BoxFuture<'a, io::Result<()>>;
    /// Open a blob for streaming; `None` if missing.
    fn open<'a>(&'a self, sha256: &'a str) -> BoxFuture<'a, io::Result<Option<(Reader, u64)>>>;
    /// Delete a blob (missing blobs are not an error).
    fn delete<'a>(&'a self, sha256: &'a str) -> BoxFuture<'a, io::Result<()>>;
}

/// Installed storage backend (state extension).
#[derive(Clone)]
pub struct SharedStorage(pub Arc<dyn AssetStorage>);

/// The configured storage: the installed extension, or disk under the data dir.
pub fn storage(state: &AppState) -> Arc<dyn AssetStorage> {
    match state.extension::<SharedStorage>() {
        Some(s) => s.0.clone(),
        None => Arc::new(DiskStorage::new(
            state.config.data_dir.join("files").join("release-assets"),
        )),
    }
}

/// Directory for in-flight uploads (same filesystem as the disk store, so
/// the final move is a rename).
pub fn tmp_dir(state: &AppState) -> PathBuf {
    state.config.data_dir.join("files").join("tmp")
}

pub struct DiskStorage {
    root: PathBuf,
}

impl DiskStorage {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn path(&self, sha256: &str) -> io::Result<PathBuf> {
        if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "bad digest"));
        }
        Ok(self.root.join(&sha256[..2]).join(sha256))
    }
}

impl AssetStorage for DiskStorage {
    fn put_file<'a>(&'a self, sha256: &'a str, file: &'a Path) -> BoxFuture<'a, io::Result<()>> {
        Box::pin(async move {
            let dest = self.path(sha256)?;
            if tokio::fs::try_exists(&dest).await? {
                tokio::fs::remove_file(file).await?;
                return Ok(());
            }
            tokio::fs::create_dir_all(dest.parent().expect("has parent")).await?;
            match tokio::fs::rename(file, &dest).await {
                Ok(()) => Ok(()),
                // Different filesystem: copy then remove.
                Err(_) => {
                    tokio::fs::copy(file, &dest).await?;
                    tokio::fs::remove_file(file).await
                }
            }
        })
    }

    fn open<'a>(&'a self, sha256: &'a str) -> BoxFuture<'a, io::Result<Option<(Reader, u64)>>> {
        Box::pin(async move {
            let path = self.path(sha256)?;
            match tokio::fs::File::open(&path).await {
                Ok(f) => {
                    let len = f.metadata().await?.len();
                    Ok(Some((Box::pin(f) as Reader, len)))
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(e),
            }
        })
    }

    fn delete<'a>(&'a self, sha256: &'a str) -> BoxFuture<'a, io::Result<()>> {
        Box::pin(async move {
            match tokio::fs::remove_file(self.path(sha256)?).await {
                Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            }
        })
    }
}

/// Result of spooling an upload body to disk.
pub struct Spooled {
    pub path: PathBuf,
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug)]
pub enum SpoolError {
    TooLarge,
    Io(io::Error),
}

/// Stream `body` into a temp file under `dir`, hashing as it goes. Fails
/// with [`SpoolError::TooLarge`] beyond `max` bytes (the file is removed).
pub async fn spool<S, E>(dir: &Path, mut body: S, max: u64) -> Result<Spooled, SpoolError>
where
    S: Stream<Item = Result<bytes::Bytes, E>> + Unpin,
    E: std::fmt::Display,
{
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(SpoolError::Io)?;
    let path = dir.join(format!("upload-{}", bgh_core::crypto::random_token(16)));
    let result = async {
        let mut file = tokio::fs::File::create(&path)
            .await
            .map_err(SpoolError::Io)?;
        let mut hasher = Sha256::new();
        let mut size = 0u64;
        while let Some(chunk) = body.next().await {
            let chunk = chunk.map_err(|e| SpoolError::Io(io::Error::other(e.to_string())))?;
            size += chunk.len() as u64;
            if size > max {
                return Err(SpoolError::TooLarge);
            }
            hasher.update(&chunk);
            file.write_all(&chunk).await.map_err(SpoolError::Io)?;
        }
        file.flush().await.map_err(SpoolError::Io)?;
        file.sync_all().await.map_err(SpoolError::Io)?;
        Ok((hex::encode(hasher.finalize()), size))
    }
    .await;
    match result {
        Ok((sha256, size)) => Ok(Spooled { path, sha256, size }),
        Err(e) => {
            let _ = tokio::fs::remove_file(&path).await;
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    #[tokio::test]
    async fn disk_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = DiskStorage::new(dir.path().join("store"));
        let body = futures::stream::iter(vec![
            Ok::<_, io::Error>(bytes::Bytes::from_static(b"hello ")),
            Ok(bytes::Bytes::from_static(b"world")),
        ]);
        let sp = spool(&dir.path().join("tmp"), body, 100).await.unwrap();
        assert_eq!(sp.size, 11);
        assert_eq!(
            sp.sha256,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
        store.put_file(&sp.sha256, &sp.path).await.unwrap();
        let (mut r, len) = store.open(&sp.sha256).await.unwrap().unwrap();
        assert_eq!(len, 11);
        let mut s = String::new();
        r.read_to_string(&mut s).await.unwrap();
        assert_eq!(s, "hello world");
        store.delete(&sp.sha256).await.unwrap();
        assert!(store.open(&sp.sha256).await.unwrap().is_none());

        let big =
            futures::stream::iter(vec![Ok::<_, io::Error>(bytes::Bytes::from(vec![0u8; 20]))]);
        assert!(matches!(
            spool(&dir.path().join("tmp"), big, 10).await,
            Err(SpoolError::TooLarge)
        ));
    }
}
