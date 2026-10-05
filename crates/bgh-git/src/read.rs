//! In-process read operations backed by gix.
//!
//! All methods are blocking. From async code, go through
//! [`crate::RepoStore::read`], which runs them on the blocking pool.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashSet};
use std::path::{Path, PathBuf};

use gix::ObjectId;
use gix::objs::Kind;

use crate::objects::{Commit, Tag, TreeEntry, TreeEntryKind, parse_tree};
use crate::{GitError, GitResult, cmd};

/// A reference and the object it points to.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RefInfo {
    /// Full name, e.g. `refs/heads/main`.
    pub name: String,
    /// Object the ref points to directly (a tag object for annotated tags).
    pub target: String,
    /// Fully peeled commit (differs from `target` for annotated tags).
    pub peeled: String,
}

impl RefInfo {
    /// Name without `refs/heads/` / `refs/tags/`.
    pub fn short_name(&self) -> &str {
        self.name
            .strip_prefix("refs/heads/")
            .or_else(|| self.name.strip_prefix("refs/tags/"))
            .unwrap_or(&self.name)
    }
}

/// Blob contents.
#[derive(Debug, Clone)]
pub struct Blob {
    pub sha: String,
    pub size: u64,
    pub data: Vec<u8>,
}

impl Blob {
    /// Heuristic binary detection (NUL byte in the first 8000 bytes, like git).
    pub fn is_binary(&self) -> bool {
        self.data.iter().take(8000).any(|&b| b == 0)
    }
}

/// Result of resolving a path inside a commit's tree.
#[derive(Debug, Clone)]
pub enum PathLookup {
    /// A directory (or the root): its tree SHA and entries.
    Tree {
        sha: String,
        entries: Vec<TreeEntry>,
    },
    /// A non-tree entry (file, symlink, submodule).
    Entry(TreeEntry),
}

pub struct GitRepo {
    repo: gix::Repository,
    path: PathBuf,
    max_blob_size: u64,
    git_bin: String,
}

fn oid(sha: &str) -> GitResult<ObjectId> {
    ObjectId::from_hex(sha.as_bytes())
        .map_err(|_| GitError::InvalidInput(format!("invalid object id {sha:?}")))
}

impl GitRepo {
    pub fn open(path: &Path, max_blob_size: u64) -> GitResult<Self> {
        let repo = gix::open_opts(path, gix::open::Options::isolated()).map_err(GitError::gix)?;
        Ok(Self {
            repo,
            path: path.to_path_buf(),
            max_blob_size,
            git_bin: "git".into(),
        })
    }

    /// Override the git binary used for CLI fallbacks.
    pub fn with_git_bin(mut self, bin: &str) -> Self {
        self.git_bin = bin.into();
        self
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Access the underlying gix repository for operations not wrapped here.
    pub fn gix(&self) -> &gix::Repository {
        &self.repo
    }

    // ----- refs -----------------------------------------------------------

    /// All refs under `prefix` (e.g. `refs/heads/`), sorted by name.
    pub fn refs(&self, prefix: &str) -> GitResult<Vec<RefInfo>> {
        let platform = self.repo.references().map_err(GitError::gix)?;
        let iter = platform.prefixed(prefix).map_err(GitError::gix)?;
        let mut out = Vec::new();
        for r in iter {
            let mut r = r.map_err(GitError::gix)?;
            let name = r.name().as_bstr().to_string();
            let target = match r.target() {
                gix::refs::TargetRef::Object(id) => id.to_hex().to_string(),
                gix::refs::TargetRef::Symbolic(_) => continue,
            };
            let peeled = r
                .peel_to_id()
                .map(|id| id.detach().to_string())
                .unwrap_or_else(|_| target.clone());
            out.push(RefInfo {
                name,
                target,
                peeled,
            });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    pub fn branches(&self) -> GitResult<Vec<RefInfo>> {
        self.refs("refs/heads/")
    }

    pub fn tags(&self) -> GitResult<Vec<RefInfo>> {
        self.refs("refs/tags/")
    }

    /// A single ref by full name, if it exists.
    pub fn find_ref(&self, name: &str) -> GitResult<Option<RefInfo>> {
        let r = self.repo.try_find_reference(name).map_err(GitError::gix)?;
        let Some(mut r) = r else { return Ok(None) };
        let target = match r.target() {
            gix::refs::TargetRef::Object(id) => id.to_hex().to_string(),
            gix::refs::TargetRef::Symbolic(_) => String::new(),
        };
        let peeled = r.peel_to_id().map_err(GitError::gix)?.detach().to_string();
        Ok(Some(RefInfo {
            name: r.name().as_bstr().to_string(),
            target: if target.is_empty() {
                peeled.clone()
            } else {
                target
            },
            peeled,
        }))
    }

    /// Branch HEAD points to (even if it doesn't exist yet), e.g. `main`.
    pub fn head_branch(&self) -> GitResult<Option<String>> {
        Ok(self
            .repo
            .head_name()
            .map_err(GitError::gix)?
            .map(|n| n.as_bstr().to_string())
            .and_then(|n| n.strip_prefix("refs/heads/").map(str::to_string)))
    }

    /// True if the repository has no branches or tags.
    pub fn is_empty(&self) -> GitResult<bool> {
        Ok(self.refs("refs/heads/")?.is_empty() && self.refs("refs/tags/")?.is_empty())
    }

    // ----- objects ----------------------------------------------------------

    /// Resolve a revision (branch, tag, SHA, `HEAD~2`, ...) to an object id.
    /// Returns `None` if it doesn't resolve.
    pub fn resolve(&self, rev: &str) -> GitResult<Option<String>> {
        if rev.is_empty() || rev.starts_with('-') {
            return Ok(None);
        }
        Ok(self
            .repo
            .rev_parse_single(rev)
            .ok()
            .map(|id| id.detach().to_string()))
    }

    /// Resolve a revision and peel it to a commit SHA.
    pub fn resolve_commit(&self, rev: &str) -> GitResult<String> {
        let mut sha = self
            .resolve(rev)?
            .ok_or_else(|| GitError::NotFound(format!("revision {rev:?}")))?;
        for _ in 0..16 {
            let (kind, data) = self.raw(&sha)?;
            match kind {
                Kind::Commit => return Ok(sha),
                Kind::Tag => sha = Tag::parse(&sha, &data)?.object,
                _ => return Err(GitError::NotFound(format!("{rev:?} is not a commit"))),
            }
        }
        Err(GitError::NotFound(format!("{rev:?} is not a commit")))
    }

    /// Object type name and size, if the object exists.
    pub fn header(&self, sha: &str) -> GitResult<Option<(&'static str, u64)>> {
        let h = self
            .repo
            .try_find_header(oid(sha)?)
            .map_err(GitError::gix)?;
        Ok(h.map(|h| {
            let kind = match h.kind() {
                Kind::Blob => "blob",
                Kind::Tree => "tree",
                Kind::Commit => "commit",
                Kind::Tag => "tag",
            };
            (kind, h.size())
        }))
    }

    fn raw(&self, sha: &str) -> GitResult<(Kind, Vec<u8>)> {
        let obj = self
            .repo
            .try_find_object(oid(sha)?)
            .map_err(GitError::gix)?
            .ok_or_else(|| GitError::NotFound(format!("object {sha}")))?;
        Ok((obj.kind, obj.data.clone()))
    }

    fn raw_of_kind(&self, sha: &str, kind: Kind) -> GitResult<Vec<u8>> {
        let (k, data) = self.raw(sha)?;
        if k != kind {
            return Err(GitError::NotFound(format!("{sha} is not a {kind}")));
        }
        Ok(data)
    }

    pub fn commit(&self, sha: &str) -> GitResult<Commit> {
        Commit::parse(sha, &self.raw_of_kind(sha, Kind::Commit)?)
    }

    pub fn tag(&self, sha: &str) -> GitResult<Tag> {
        Tag::parse(sha, &self.raw_of_kind(sha, Kind::Tag)?)
    }

    pub fn tree(&self, sha: &str) -> GitResult<Vec<TreeEntry>> {
        parse_tree(&self.raw_of_kind(sha, Kind::Tree)?)
    }

    /// Read a blob, refusing blobs larger than the configured limit.
    pub fn blob(&self, sha: &str) -> GitResult<Blob> {
        self.blob_with_limit(sha, self.max_blob_size)
    }

    pub fn blob_with_limit(&self, sha: &str, limit: u64) -> GitResult<Blob> {
        let (kind, size) = self
            .header(sha)?
            .ok_or_else(|| GitError::NotFound(format!("blob {sha}")))?;
        if kind != "blob" {
            return Err(GitError::NotFound(format!("{sha} is not a blob")));
        }
        if size > limit {
            return Err(GitError::TooLarge { size, limit });
        }
        let data = self.raw_of_kind(sha, Kind::Blob)?;
        Ok(Blob {
            sha: sha.to_string(),
            size,
            data,
        })
    }

    /// Resolve `path` (slash-separated, empty = root) within `commitish`.
    pub fn lookup_path(&self, commitish: &str, path: &str) -> GitResult<PathLookup> {
        let commit = self.commit(&self.resolve_commit(commitish)?)?;
        let mut tree_sha = commit.tree;
        let mut entries = self.tree(&tree_sha)?;
        let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
        for (i, part) in parts.iter().enumerate() {
            let entry = entries
                .iter()
                .find(|e| e.name == *part)
                .cloned()
                .ok_or_else(|| GitError::NotFound(format!("path {path:?}")))?;
            let last = i + 1 == parts.len();
            if entry.kind == TreeEntryKind::Tree {
                tree_sha = entry.sha.clone();
                entries = self.tree(&tree_sha)?;
            } else if last {
                return Ok(PathLookup::Entry(entry));
            } else {
                return Err(GitError::NotFound(format!("path {path:?}")));
            }
        }
        Ok(PathLookup::Tree {
            sha: tree_sha,
            entries,
        })
    }

    // ----- history ------------------------------------------------------------

    /// Commits reachable from `rev`, newest first (by committer date, like
    /// `git log`), optionally limited to those touching `path`.
    pub fn log(
        &self,
        rev: &str,
        path: Option<&str>,
        skip: usize,
        limit: usize,
    ) -> GitResult<Vec<Commit>> {
        let start = self.resolve_commit(rev)?;
        if let Some(path) = path.filter(|p| !p.is_empty()) {
            // Path-limited history needs tree diffs + simplification: use git.
            let skip_arg = format!("--skip={skip}");
            let max_arg = format!("--max-count={limit}");
            let out = cmd::run_blocking(
                &self.git_bin,
                &self.path,
                &[
                    "log",
                    "--format=%H",
                    &skip_arg,
                    &max_arg,
                    &start,
                    "--",
                    path,
                ],
            )?;
            return String::from_utf8_lossy(&out)
                .lines()
                .filter(|l| !l.is_empty())
                .map(|sha| self.commit(sha))
                .collect();
        }

        struct Item {
            time: i64,
            seq: usize,
            commit: Commit,
        }
        impl PartialEq for Item {
            fn eq(&self, o: &Self) -> bool {
                self.cmp(o) == Ordering::Equal
            }
        }
        impl Eq for Item {}
        impl PartialOrd for Item {
            fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
                Some(self.cmp(o))
            }
        }
        impl Ord for Item {
            fn cmp(&self, o: &Self) -> Ordering {
                self.time.cmp(&o.time).then(o.seq.cmp(&self.seq))
            }
        }

        let mut heap = BinaryHeap::new();
        let mut seen = HashSet::new();
        let mut seq = 0;
        let first = self.commit(&start)?;
        seen.insert(start);
        heap.push(Item {
            time: first.committer.when.timestamp(),
            seq,
            commit: first,
        });
        let mut out = Vec::new();
        let mut skipped = 0;
        while let Some(Item { commit, .. }) = heap.pop() {
            for p in &commit.parents {
                if seen.insert(p.clone()) {
                    seq += 1;
                    let pc = self.commit(p)?;
                    heap.push(Item {
                        time: pc.committer.when.timestamp(),
                        seq,
                        commit: pc,
                    });
                }
            }
            if skipped < skip {
                skipped += 1;
                continue;
            }
            out.push(commit);
            if out.len() >= limit {
                break;
            }
        }
        Ok(out)
    }

    /// Whether `ancestor` is reachable from `descendant`.
    pub fn is_ancestor(&self, ancestor: &str, descendant: &str) -> GitResult<bool> {
        let mut stack = vec![descendant.to_string()];
        let mut seen = HashSet::new();
        while let Some(sha) = stack.pop() {
            if sha == ancestor {
                return Ok(true);
            }
            if !seen.insert(sha.clone()) {
                continue;
            }
            stack.extend(self.commit(&sha)?.parents);
        }
        Ok(false)
    }
}
