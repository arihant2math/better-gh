//! Lightweight repository maintenance.
//!
//! Pushes create loose ref files, and with no auto-gc they accumulate:
//! listing many loose refs (and peeling annotated tags without the
//! packed-refs peel cache) costs a file read per ref on every listing.
//! [`pack_refs`] folds them into `packed-refs` with peeled values.

use std::path::Path;

use crate::storage::RepoStore;
use crate::{GitResult, cmd};

/// Number of loose ref files under `refs/` (stops counting at `limit`).
pub fn loose_ref_count(git_dir: &Path, limit: usize) -> usize {
    fn walk(p: &Path, n: &mut usize, limit: usize) {
        let Ok(rd) = std::fs::read_dir(p) else {
            return;
        };
        for e in rd.flatten() {
            if *n >= limit {
                return;
            }
            match e.file_type() {
                Ok(t) if t.is_dir() => walk(&e.path(), n, limit),
                Ok(t) if t.is_file() => *n += 1,
                _ => {}
            }
        }
    }
    let mut n = 0;
    walk(&git_dir.join("refs"), &mut n, limit);
    n
}

/// `git pack-refs --all --prune` (safe to run concurrently with pushes:
/// git takes the ref locks).
pub async fn pack_refs(store: &RepoStore, repo_id: i64) -> GitResult<()> {
    let dir = store.git_dir(repo_id)?;
    cmd::run(
        &store.git_bin,
        Some(&dir),
        &["pack-refs", "--all", "--prune"],
        &[],
        None,
    )
    .await?;
    Ok(())
}
