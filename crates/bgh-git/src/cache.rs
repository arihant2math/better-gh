//! Process-wide LRU cache of opened gix repository handles.
//!
//! Opening a repository (discovering config, refs and the object database)
//! costs far more than a typical read, so [`crate::RepoStore::open`] keeps
//! up to [`CAPACITY`] `gix::ThreadSafeRepository` handles keyed by path and
//! hands out cheap thread-local clones. gix refreshes its view of packs on
//! object lookup misses and re-reads refs from disk, so handles stay valid
//! across pushes; [`evict`] drops a handle after deletion or maintenance
//! (repack/gc) that removes pack files.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

/// Maximum number of cached repository handles.
pub const CAPACITY: usize = 256;

struct Entry {
    repo: gix::ThreadSafeRepository,
    last_used: u64,
}

#[derive(Default)]
struct Lru {
    map: HashMap<PathBuf, Entry>,
    tick: u64,
}

static CACHE: LazyLock<Mutex<Lru>> = LazyLock::new(Default::default);

/// Cached handle for `path`, opening (and caching) it on a miss.
pub(crate) fn get_or_open(path: &Path) -> crate::GitResult<gix::Repository> {
    {
        let mut lru = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        lru.tick += 1;
        let tick = lru.tick;
        if let Some(e) = lru.map.get_mut(path) {
            e.last_used = tick;
            return Ok(e.repo.to_thread_local());
        }
    }
    // Open outside the lock; a concurrent opener of the same path is harmless.
    let repo = gix::ThreadSafeRepository::open_opts(path, gix::open::Options::isolated())
        .map_err(crate::GitError::gix)?;
    let local = repo.to_thread_local();
    let mut lru = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if lru.map.len() >= CAPACITY
        && !lru.map.contains_key(path)
        && let Some(oldest) = lru
            .map
            .iter()
            .min_by_key(|(_, e)| e.last_used)
            .map(|(p, _)| p.clone())
    {
        lru.map.remove(&oldest);
    }
    let tick = lru.tick;
    lru.map.insert(
        path.to_path_buf(),
        Entry {
            repo,
            last_used: tick,
        },
    );
    Ok(local)
}

/// Drop the cached handle for `path` (after deletion, repack, gc).
pub fn evict(path: &Path) {
    CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .map
        .remove(path);
}

/// Number of cached handles (for tests and metrics).
pub fn len() -> usize {
    CACHE.lock().unwrap_or_else(|e| e.into_inner()).map.len()
}
