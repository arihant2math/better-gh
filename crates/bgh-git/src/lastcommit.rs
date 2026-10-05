//! "Last commit" per directory entry, as shown next to each file in a tree
//! listing.
//!
//! For every entry of the directory `path` at `start`, find the newest
//! commit (in committer-date order, like `git log`) that introduced the
//! entry's current object: the entry has its current SHA in the commit but
//! in none of its parents. Like git's default history simplification, a
//! commit whose directory tree equals one of its parents' is skipped and
//! only that parent is followed, so unrelated history is pruned quickly.
//! Trees are memoized per walk, so each distinct tree is parsed once.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::rc::Rc;

use crate::objects::{Commit, TreeEntryKind};
use crate::read::GitRepo;
use crate::{GitError, GitResult};

/// Upper bound on commits visited per call; entries not resolved within it
/// are left out of the result.
pub const DEFAULT_MAX_COMMITS: usize = 200_000;

type Entries = Rc<HashMap<String, (String, bool)>>; // name -> (sha, is_tree)

struct Walker<'a> {
    repo: &'a GitRepo,
    trees: HashMap<String, Entries>,
    parts: Vec<String>,
}

impl Walker<'_> {
    fn entries(&mut self, tree: &str) -> GitResult<Entries> {
        if let Some(e) = self.trees.get(tree) {
            return Ok(e.clone());
        }
        let parsed: HashMap<_, _> = self
            .repo
            .tree(tree)?
            .into_iter()
            .map(|e| (e.name, (e.sha, e.kind == TreeEntryKind::Tree)))
            .collect();
        let rc = Rc::new(parsed);
        self.trees.insert(tree.to_string(), rc.clone());
        Ok(rc)
    }

    /// SHA of the directory at `self.parts` inside root tree `root`.
    fn dir(&mut self, root: &str) -> GitResult<Option<String>> {
        let mut cur = root.to_string();
        for i in 0..self.parts.len() {
            let entries = self.entries(&cur)?;
            match entries.get(&self.parts[i]) {
                Some((sha, true)) => cur = sha.clone(),
                _ => return Ok(None),
            }
        }
        Ok(Some(cur))
    }

    fn dir_entries(&mut self, root: &str) -> GitResult<Option<Entries>> {
        match self.dir(root)? {
            Some(d) => Ok(Some(self.entries(&d)?)),
            None => Ok(None),
        }
    }
}

struct Item {
    time: i64,
    seq: usize,
    commit: Rc<Commit>,
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

impl GitRepo {
    /// Last commit for each entry of directory `path` (empty = root) at
    /// commit `start` (any commit-ish). Returns entry name → commit.
    pub fn last_commits(
        &self,
        start: &str,
        path: &str,
        max_commits: usize,
    ) -> GitResult<HashMap<String, Commit>> {
        let start = self.resolve_commit(start)?;
        let mut w = Walker {
            repo: self,
            trees: HashMap::new(),
            parts: path
                .split('/')
                .filter(|p| !p.is_empty())
                .map(str::to_string)
                .collect(),
        };
        let first = Rc::new(self.commit(&start)?);
        let target = w
            .dir_entries(&first.tree)?
            .ok_or_else(|| GitError::NotFound(format!("path {path:?}")))?;
        let mut unresolved: HashMap<String, String> = target
            .iter()
            .map(|(name, (sha, _))| (name.clone(), sha.clone()))
            .collect();
        let mut out = HashMap::new();
        let mut heap = BinaryHeap::new();
        let mut seen = HashSet::new();
        let mut commits: HashMap<String, Rc<Commit>> = HashMap::new();
        let mut seq = 0usize;
        seen.insert(start.clone());
        heap.push(Item {
            time: first.committer.when.timestamp(),
            seq,
            commit: first,
        });
        let mut visited = 0usize;

        while let Some(Item { commit, .. }) = heap.pop() {
            if unresolved.is_empty() {
                break;
            }
            visited += 1;
            if visited > max_commits {
                break;
            }
            let dir_c = w.dir(&commit.tree)?;
            let mut parents = Vec::with_capacity(commit.parents.len());
            for p in &commit.parents {
                let pc = match commits.get(p) {
                    Some(c) => c.clone(),
                    None => {
                        let c = Rc::new(self.commit(p)?);
                        commits.insert(p.clone(), c.clone());
                        c
                    }
                };
                let dir_p = w.dir(&pc.tree)?;
                parents.push((pc, dir_p));
            }
            let mut push = |c: &Rc<Commit>, heap: &mut BinaryHeap<Item>| {
                if seen.insert(c.sha.clone()) {
                    seq += 1;
                    heap.push(Item {
                        time: c.committer.when.timestamp(),
                        seq,
                        commit: c.clone(),
                    });
                }
            };
            // TREESAME to a parent: nothing changed here; follow only it.
            if let Some((p, _)) = parents.iter().find(|(_, d)| *d == dir_c) {
                let p = p.clone();
                push(&p, &mut heap);
                commits.remove(&commit.sha);
                continue;
            }
            if let Some(dir_c) = &dir_c {
                let entries_c = w.entries(dir_c)?;
                let mut parent_entries = Vec::with_capacity(parents.len());
                for (_, d) in &parents {
                    parent_entries.push(match d {
                        Some(d) => Some(w.entries(d)?),
                        None => None,
                    });
                }
                unresolved.retain(|name, target_sha| {
                    let here = entries_c.get(name).is_some_and(|(s, _)| s == target_sha);
                    let introduced = here
                        && parent_entries.iter().all(|pe| {
                            !pe.as_ref()
                                .is_some_and(|e| e.get(name).is_some_and(|(s, _)| s == target_sha))
                        });
                    if introduced {
                        out.insert(name.clone(), (*commit).clone());
                    }
                    !introduced
                });
            }
            for (p, _) in &parents {
                push(p, &mut heap);
            }
            commits.remove(&commit.sha);
        }
        Ok(out)
    }

    /// Latest commit touching exactly `path` (file or directory) from
    /// `start`, if any.
    pub fn last_commit_for_path(&self, start: &str, path: &str) -> GitResult<Option<Commit>> {
        let path = path.trim_matches('/');
        if path.is_empty() {
            return Ok(Some(self.commit(&self.resolve_commit(start)?)?));
        }
        let (dir, name) = path.rsplit_once('/').unwrap_or(("", path));
        Ok(self
            .last_commits(start, dir, DEFAULT_MAX_COMMITS)?
            .remove(name))
    }
}
