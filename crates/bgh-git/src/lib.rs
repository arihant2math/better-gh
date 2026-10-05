//! Git storage and protocol machinery.
//!
//! * [`RepoStore`]: on-disk layout (`{data_dir}/repos/{id % 256:02x}/{id}.git`),
//!   init / fork / delete.
//! * [`GitRepo`]: fast in-process reads via `gix` (refs, commits, trees,
//!   blobs, log). gix is blocking: use [`RepoStore::read`] from async code.
//! * [`write`]: write plumbing through the `git` CLI (commits, refs, HEAD).
//! * [`smart_http`]: smart-HTTP transport (info/refs, upload-pack,
//!   receive-pack with pre-receive authorization and post-receive results).
//!
//! This crate knows nothing about users or permissions; `bgh-repos` mounts
//! the HTTP routes and performs authorization.

pub mod archive;
pub mod blame;
pub mod cache;
mod cmd;
pub mod highlight;
pub mod lastcommit;
pub mod lfs;
pub mod objects;
pub mod pktline;
pub mod read;
pub mod smart_http;
pub mod storage;
pub mod stream;
pub mod write;

pub use bgh_core::events::{RefUpdate, ZERO_SHA};
pub use objects::{Commit, Signature, Tag, TreeEntry, TreeEntryKind};
pub use read::{Blob, GitRepo, PathLookup, RefInfo};
pub use storage::RepoStore;

use bgh_core::error::{ApiError, FieldError};

#[derive(Debug, thiserror::Error)]
pub enum GitError {
    /// Missing repository, ref, object or path.
    #[error("not found: {0}")]
    NotFound(String),
    /// Bad user input (invalid ref name, malformed SHA, ...).
    #[error("invalid input: {0}")]
    InvalidInput(String),
    /// Object larger than the allowed limit.
    #[error("object too large ({size} bytes > {limit})")]
    TooLarge { size: u64, limit: u64 },
    #[error("git object error: {0}")]
    Object(String),
    #[error("`git {args}` failed ({status}): {stderr}")]
    Command {
        args: String,
        status: String,
        stderr: String,
    },
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type GitResult<T> = Result<T, GitError>;

impl GitError {
    pub(crate) fn gix(e: impl std::fmt::Display) -> Self {
        Self::Object(e.to_string())
    }
}

impl From<GitError> for ApiError {
    fn from(e: GitError) -> Self {
        match e {
            GitError::NotFound(_) => ApiError::NotFound,
            GitError::InvalidInput(msg) => {
                ApiError::invalid_field(FieldError::custom("Git", "ref", msg))
            }
            GitError::TooLarge { .. } => ApiError::forbidden(
                "This API returns blobs up to the configured size limit. The requested blob is too large to fetch via the API.",
            ),
            other => ApiError::internal(other),
        }
    }
}

/// Whether `s` is a full 40-char lowercase/uppercase hex SHA-1.
pub fn is_sha(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Validate a ref / branch name roughly per `git check-ref-format`.
pub fn is_valid_ref_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && !name.starts_with('/')
        && !name.ends_with('/')
        && !name.ends_with('.')
        && !name.ends_with(".lock")
        && !name.contains("..")
        && !name.contains("//")
        && !name.contains("@{")
        && name != "@"
        && !name.split('/').any(|c| c.is_empty() || c.starts_with('.'))
        && !name
            .bytes()
            .any(|b| b < 0x20 || b == 0x7f || b" ~^:?*[\\".contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ref_names() {
        assert!(is_valid_ref_name("main"));
        assert!(is_valid_ref_name("feature/x-1"));
        assert!(!is_valid_ref_name("a..b"));
        assert!(!is_valid_ref_name("a b"));
        assert!(!is_valid_ref_name(".hidden"));
        assert!(!is_valid_ref_name("x.lock"));
        assert!(!is_valid_ref_name("a//b"));
        assert!(is_sha(&"a".repeat(40)));
        assert!(!is_sha("abc"));
    }
}
