//! Git LFS: pointer files and the content-addressed object store.
//!
//! Objects live at `{data_dir}/lfs/{oid[0..2]}/{oid[2..4]}/{oid}` and are
//! shared by every repository (deduplicated by content); which repository
//! may read an object is tracked in the database by the caller.

use std::io;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

/// First line of every LFS pointer file.
pub const POINTER_VERSION: &str = "version https://git-lfs.github.com/spec/v1";
/// Pointer files are tiny; anything larger is regular content.
pub const MAX_POINTER_SIZE: usize = 1024;

/// A parsed LFS pointer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pointer {
    /// SHA-256 hex of the object.
    pub oid: String,
    pub size: u64,
}

/// Parse an LFS pointer file (`version`, `oid sha256:<hex>`, `size <n>`).
pub fn parse_pointer(data: &[u8]) -> Option<Pointer> {
    if data.len() > MAX_POINTER_SIZE || !data.starts_with(POINTER_VERSION.as_bytes()) {
        return None;
    }
    let text = std::str::from_utf8(data).ok()?;
    let mut oid = None;
    let mut size = None;
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("oid sha256:") {
            oid = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("size ") {
            size = v.trim().parse().ok();
        }
    }
    let oid = oid.filter(|o| is_valid_oid(o))?;
    Some(Pointer { oid, size: size? })
}

/// Whether `oid` is a lowercase 64-char SHA-256 hex digest.
pub fn is_valid_oid(oid: &str) -> bool {
    oid.len() == 64 && oid.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

#[derive(Debug, thiserror::Error)]
pub enum PutError {
    #[error("size mismatch: expected {expected}, got {actual}")]
    Size { expected: u64, actual: u64 },
    #[error("content does not match oid")]
    Hash,
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Content-addressed LFS object storage.
#[derive(Debug, Clone)]
pub struct LfsStore {
    pub root: PathBuf,
}

impl LfsStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// `{data_dir}/lfs`
    pub fn from_data_dir(data_dir: &Path) -> Self {
        Self::new(data_dir.join("lfs"))
    }

    /// Path of an object. `oid` must be valid (see [`is_valid_oid`]).
    pub fn path(&self, oid: &str) -> PathBuf {
        debug_assert!(is_valid_oid(oid));
        self.root.join(&oid[0..2]).join(&oid[2..4]).join(oid)
    }

    /// Size of a stored object, if present.
    pub async fn size(&self, oid: &str) -> Option<u64> {
        if !is_valid_oid(oid) {
            return None;
        }
        tokio::fs::metadata(self.path(oid))
            .await
            .ok()
            .map(|m| m.len())
    }

    pub async fn open(&self, oid: &str) -> io::Result<tokio::fs::File> {
        if !is_valid_oid(oid) {
            return Err(io::ErrorKind::NotFound.into());
        }
        tokio::fs::File::open(self.path(oid)).await
    }

    /// Stream `reader` into the store, verifying size and SHA-256. Objects
    /// that already exist are verified and discarded (idempotent uploads).
    pub async fn put<R: AsyncRead + Unpin>(
        &self,
        oid: &str,
        expected_size: u64,
        reader: &mut R,
    ) -> Result<u64, PutError> {
        if !is_valid_oid(oid) {
            return Err(PutError::Hash);
        }
        let tmp_dir = self.root.join("tmp");
        tokio::fs::create_dir_all(&tmp_dir).await?;
        let tmp = tempfile::NamedTempFile::new_in(&tmp_dir)?;
        let mut file = tokio::fs::File::from_std(tmp.reopen()?);
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 256 * 1024];
        let mut total: u64 = 0;
        loop {
            let n = reader.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            total += n as u64;
            if total > expected_size {
                return Err(PutError::Size {
                    expected: expected_size,
                    actual: total,
                });
            }
            hasher.update(&buf[..n]);
            file.write_all(&buf[..n]).await?;
        }
        if total != expected_size {
            return Err(PutError::Size {
                expected: expected_size,
                actual: total,
            });
        }
        if hex::encode(hasher.finalize()) != oid {
            return Err(PutError::Hash);
        }
        file.sync_all().await?;
        drop(file);
        let dest = self.path(oid);
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let dest2 = dest.clone();
        tokio::task::spawn_blocking(move || tmp.persist(dest2).map_err(|e| e.error))
            .await
            .map_err(io::Error::other)??;
        Ok(total)
    }

    /// Remove an object (when no repository references it any more).
    pub async fn remove(&self, oid: &str) -> io::Result<()> {
        if !is_valid_oid(oid) {
            return Ok(());
        }
        match tokio::fs::remove_file(self.path(oid)).await {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OID: &str = "4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393";

    #[test]
    fn parses_pointers() {
        let p = format!("{POINTER_VERSION}\noid sha256:{OID}\nsize 12345\n");
        assert_eq!(
            parse_pointer(p.as_bytes()),
            Some(Pointer {
                oid: OID.into(),
                size: 12345
            })
        );
        assert_eq!(parse_pointer(b"hello"), None);
        let bad = format!("{POINTER_VERSION}\noid sha256:xyz\nsize 1\n");
        assert_eq!(parse_pointer(bad.as_bytes()), None);
    }

    #[tokio::test]
    async fn stores_and_verifies() {
        let dir = tempfile::tempdir().unwrap();
        let store = LfsStore::new(dir.path());
        let data = b"hello lfs\n";
        let oid = hex::encode(Sha256::digest(data));
        assert_eq!(store.put(&oid, 10, &mut &data[..]).await.unwrap(), 10);
        assert_eq!(store.size(&oid).await, Some(10));
        assert!(matches!(
            store.put(&oid, 9, &mut &data[..]).await,
            Err(PutError::Size { .. })
        ));
        assert!(matches!(
            store.put(OID, 10, &mut &data[..]).await,
            Err(PutError::Hash)
        ));
        assert_eq!(store.size(OID).await, None);
    }
}
