//! Lightweight repository maintenance.
//!
//! Pushes create loose ref files, and with no auto-gc they accumulate:
//! listing many loose refs (and peeling annotated tags without the
//! packed-refs peel cache) costs a file read per ref on every listing.
//! [`pack_refs`] folds them into `packed-refs` with peeled values.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

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

// ----- fork-network-aware object maintenance --------------------------------
//
// Forks are `clone --shared` of their parent: they borrow its objects through
// `objects/info/alternates` (possibly chained: a fork of a fork borrows from
// the fork, which borrows from the root). An object that is unreachable from
// a repository's own refs may therefore still be needed by a repository that
// borrows from it ("dependent"). The rules:
//
// * repository with dependents: repack keeping unreachable objects, never
//   prune (`repack -a -d --keep-unreachable`);
// * fork without dependents: repack local objects only (`-l`); unreachable
//   objects are loosened and pruned after the grace period;
// * standalone repository: `gc --prune=<grace>`.
//
// Pruning always honours a grace period (default two weeks) so objects
// written by an in-flight push are never removed; the only exception is
// the audited, forced admin "prune now" on a repository without dependents.

/// How a repository relates to its fork network on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NetworkRole {
    /// Borrows objects through `objects/info/alternates`.
    pub has_alternates: bool,
    /// Other repositories borrow objects from this one.
    pub has_dependents: bool,
}

/// Object counts from `git count-objects -v`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ObjectStats {
    pub loose_count: i64,
    pub pack_count: i64,
    /// Bytes of loose objects + packs (KiB, as reported by git).
    pub size_kib: i64,
}

/// What [`plan`] is asked to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Task {
    /// Full collection: one pack, unreachable objects handled per role.
    Gc,
    /// Incremental: commit-graph + geometric repack (+ midx/bitmap when
    /// standalone in the object sense, i.e. no alternates).
    Incremental,
    /// Consolidate into one pack without dropping anything (unreachable
    /// objects are kept, whatever the role).
    Repack,
    /// Only `commit-graph write`.
    CommitGraph,
    /// Forced: drop unreachable objects immediately. Refused for
    /// repositories with dependents.
    PruneNow,
}

/// Errors specific to maintenance planning.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanError {
    #[error("repository has forks borrowing its objects; refusing to prune")]
    HasDependents,
}

/// Grace period argument for git (`<days>.days.ago`), at least one day.
pub fn grace_arg(days: u32) -> String {
    format!("{}.days.ago", days.max(1))
}

/// The git invocations for `task` on a repository with `role`, in order.
/// Pure (unit-tested); [`run_task`] executes them.
pub fn plan(task: Task, role: NetworkRole, grace_days: u32) -> Result<Vec<Vec<String>>, PlanError> {
    let grace = grace_arg(grace_days);
    let v = |args: &[&str]| args.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let mut out = Vec::new();
    // Local-only packing: objects borrowed from alternates stay there.
    // Bitmaps need every reachable object in the pack, impossible with
    // alternates (bare repositories write them by default).
    let local: &[&str] = if role.has_alternates {
        &["-l", "--no-write-bitmap-index"]
    } else {
        &[]
    };
    match task {
        Task::PruneNow => {
            if role.has_dependents {
                return Err(PlanError::HasDependents);
            }
            let mut repack = v(&["repack", "-a", "-d", "-q"]);
            repack.extend(local.iter().map(|s| s.to_string()));
            out.push(v(&["pack-refs", "--all", "--prune"]));
            out.push(repack);
            out.push(v(&["prune", "--expire", "now"]));
        }
        Task::Gc => {
            out.push(v(&["pack-refs", "--all", "--prune"]));
            if role.has_dependents {
                let mut repack = v(&["repack", "-a", "-d", "-q", "--keep-unreachable"]);
                repack.extend(local.iter().map(|s| s.to_string()));
                out.push(repack);
            } else if role.has_alternates {
                // `-A`: unreachable objects become loose (unless older than
                // the grace period) and `prune` removes them once expired.
                let unpack = format!("--unpack-unreachable={grace}");
                let mut repack = v(&["repack", "-A", "-d", "-q", &unpack]);
                repack.extend(local.iter().map(|s| s.to_string()));
                out.push(repack);
                out.push(v(&["prune", "--expire", &grace]));
            } else {
                out.push(vec![
                    "gc".into(),
                    "--quiet".into(),
                    format!("--prune={grace}"),
                ]);
            }
            out.push(commit_graph_args(role));
        }
        Task::Repack => {
            let mut repack = v(&["repack", "-a", "-d", "-q", "--keep-unreachable"]);
            repack.extend(local.iter().map(|s| s.to_string()));
            out.push(repack);
            out.push(commit_graph_args(role));
        }
        Task::CommitGraph => out.push(commit_graph_args(role)),
        Task::Incremental => {
            out.push(commit_graph_args(role));
            // Geometric repack rolls whole packs (reachable or not) together,
            // so it never drops objects a dependent borrows.
            let mut repack = v(&["repack", "-d", "-q", "--geometric=2"]);
            if role.has_alternates {
                repack.extend(local.iter().map(|s| s.to_string()));
            } else {
                repack.extend(v(&["--write-midx", "--write-bitmap-index"]));
            }
            out.push(repack);
        }
    }
    Ok(out)
}

/// `commit-graph write`: split (incremental) chains, except in forks whose
/// split chain would reference the parent's layers (which the parent may
/// merge away later); forks write a self-contained graph instead.
fn commit_graph_args(role: NetworkRole) -> Vec<String> {
    let mut a = vec![
        "commit-graph".to_string(),
        "write".into(),
        "--reachable".into(),
        "--changed-paths".into(),
        "--no-progress".into(),
    ];
    if !role.has_alternates {
        a.push("--split".into());
    }
    a
}

/// Execute [`plan`] in `git_dir`; returns the combined (trimmed) output.
pub async fn run_task(
    git_bin: &str,
    git_dir: &Path,
    task: Task,
    role: NetworkRole,
    grace_days: u32,
) -> GitResult<String> {
    let steps =
        plan(task, role, grace_days).map_err(|e| crate::GitError::InvalidInput(e.to_string()))?;
    let mut log = String::new();
    let res = async {
        for step in &steps {
            let args: Vec<&str> = step.iter().map(String::as_str).collect();
            let out = cmd::run_status(git_bin, Some(git_dir), &args, &[]).await?;
            let text = String::from_utf8_lossy(&out.stderr);
            log.push_str(&format!("$ git {}\n", args.join(" ")));
            if !text.trim().is_empty() {
                log.push_str(text.trim());
                log.push('\n');
            }
            if out.code != Some(0) {
                return Err(crate::GitError::Command {
                    args: args.join(" "),
                    status: format!("{:?}", out.code),
                    stderr: text.trim().to_string(),
                });
            }
        }
        Ok(())
    }
    .await;
    // Packs may have been replaced: drop cached handles either way.
    crate::cache::evict(git_dir);
    res.map(|()| log.trim().to_string())
}

/// `git count-objects -v`.
pub async fn object_stats(git_bin: &str, git_dir: &Path) -> GitResult<ObjectStats> {
    let out = cmd::run(git_bin, Some(git_dir), &["count-objects", "-v"], &[], None).await?;
    Ok(parse_count_objects(&String::from_utf8_lossy(&out)))
}

fn parse_count_objects(text: &str) -> ObjectStats {
    let mut s = ObjectStats::default();
    for line in text.lines() {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let n: i64 = v.trim().parse().unwrap_or(0);
        match k.trim() {
            "count" => s.loose_count = n,
            "packs" => s.pack_count = n,
            "size" | "size-pack" => s.size_kib += n,
            _ => {}
        }
    }
    s
}

/// Whether a `receive-pack` is in flight (git's quarantine directory
/// `objects/tmp_objdir-incoming-*` exists and is younger than an hour;
/// older ones are leftovers of killed pushes).
pub fn push_in_progress(git_dir: &Path) -> bool {
    let Ok(rd) = std::fs::read_dir(git_dir.join("objects")) else {
        return false;
    };
    let now = std::time::SystemTime::now();
    rd.flatten().any(|e| {
        let name = e.file_name();
        let name = name.to_string_lossy();
        (name.starts_with("tmp_objdir-incoming-") || name.starts_with("incoming-"))
            && e.metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| now.duration_since(t).ok())
                .is_none_or(|age| age < std::time::Duration::from_secs(3600))
    })
}

/// Git directories `git_dir` borrows objects from (direct alternates only).
pub fn alternates(git_dir: &Path) -> Vec<PathBuf> {
    let objects = git_dir.join("objects");
    let Ok(text) = std::fs::read_to_string(objects.join("info/alternates")) else {
        return vec![];
    };
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| {
            let p = PathBuf::from(l);
            let p = if p.is_relative() { objects.join(p) } else { p };
            let p = std::fs::canonicalize(&p).unwrap_or(p);
            // `<git dir>/objects` → `<git dir>`
            p.parent().map(Path::to_path_buf).unwrap_or(p)
        })
        .collect()
}

/// Whether `git_dir` has an `objects/info/alternates` file.
pub fn has_alternates(git_dir: &Path) -> bool {
    git_dir.join("objects/info/alternates").is_file()
}

/// Every repository directory (`{root}/{xx}/{name}.git`) under the store.
fn repo_dirs(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(shards) = std::fs::read_dir(root) else {
        return out;
    };
    for shard in shards.flatten() {
        let Ok(repos) = std::fs::read_dir(shard.path()) else {
            continue;
        };
        out.extend(
            repos
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "git")),
        );
    }
    out
}

/// Map from a (canonical) git dir to the git dirs that borrow from it, built
/// by scanning every alternates file under `root`. One pass serves a whole
/// maintenance run.
pub fn dependents_index(root: &Path) -> HashMap<PathBuf, Vec<PathBuf>> {
    let mut map: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
    for dir in repo_dirs(root) {
        if !has_alternates(&dir) {
            continue;
        }
        for target in alternates(&dir) {
            map.entry(target).or_default().push(dir.clone());
        }
    }
    map
}

/// Repositories under `root` borrowing objects from `git_dir` (filesystem
/// check, the fallback for fork relationships the database no longer has).
pub fn dependents_on_disk(root: &Path, git_dir: &Path) -> Vec<PathBuf> {
    let key = std::fs::canonicalize(git_dir).unwrap_or_else(|_| git_dir.to_path_buf());
    dependents_index(root).remove(&key).unwrap_or_default()
}

/// Look `git_dir` up in an index from [`dependents_index`].
pub fn dependents_in<'a>(
    index: &'a HashMap<PathBuf, Vec<PathBuf>>,
    git_dir: &Path,
) -> &'a [PathBuf] {
    let key = std::fs::canonicalize(git_dir).unwrap_or_else(|_| git_dir.to_path_buf());
    index.get(&key).map(Vec::as_slice).unwrap_or(&[])
}

/// Make `git_dir` self-contained: copy every object it reaches (including
/// borrowed ones) into its own pack, drop `objects/info/alternates` and
/// verify with `git fsck --connectivity-only`. On a failed check the
/// alternates file is restored and an error returned. Idempotent.
///
/// Repositories borrowing from `git_dir` may need objects that `git_dir`
/// itself only reaches through its alternates without referencing them, so
/// they are made self-contained first (depth-first, see
/// [`dissociate_network`]).
pub async fn dissociate(git_bin: &str, git_dir: &Path) -> GitResult<()> {
    let alt = git_dir.join("objects/info/alternates");
    if !alt.is_file() {
        return Ok(());
    }
    // Keep unreachable objects: other repositories may borrow them.
    let steps: [&[&str]; 1] = [&["repack", "-a", "-d", "-q", "--keep-unreachable"]];
    for args in steps {
        cmd::run(git_bin, Some(git_dir), args, &[], None).await?;
    }
    let parked = git_dir.join("objects/info/alternates.bgh-dissociated");
    tokio::fs::rename(&alt, &parked).await?;
    crate::cache::evict(git_dir);
    let check = cmd::run_status(
        git_bin,
        Some(git_dir),
        &[
            "fsck",
            "--connectivity-only",
            "--no-progress",
            "--no-dangling",
        ],
        &[],
    )
    .await;
    match check {
        Ok(out) if out.code == Some(0) => {
            let _ = tokio::fs::remove_file(&parked).await;
            Ok(())
        }
        other => {
            tokio::fs::rename(&parked, &alt).await?;
            crate::cache::evict(git_dir);
            let stderr = match other {
                Ok(out) => String::from_utf8_lossy(&out.stderr).trim().to_string(),
                Err(e) => e.to_string(),
            };
            Err(crate::GitError::Command {
                args: "fsck --connectivity-only".into(),
                status: "dissociation check failed".into(),
                stderr,
            })
        }
    }
}

/// [`dissociate`] `git_dir` after every repository (transitively) borrowing
/// from it under `root`, deepest first.
pub async fn dissociate_network(git_bin: &str, root: &Path, git_dir: &Path) -> GitResult<()> {
    let index = dependents_index(root);
    // Iterative post-order over the dependents tree (cycle-safe).
    let start = std::fs::canonicalize(git_dir).unwrap_or_else(|_| git_dir.to_path_buf());
    let mut order = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut stack = vec![(start, false)];
    while let Some((dir, expanded)) = stack.pop() {
        if expanded {
            order.push(dir);
            continue;
        }
        if !seen.insert(dir.clone()) {
            continue;
        }
        stack.push((dir.clone(), true));
        for d in dependents_in(&index, &dir) {
            let d = std::fs::canonicalize(d).unwrap_or_else(|_| d.clone());
            stack.push((d, false));
        }
    }
    for dir in order {
        dissociate(git_bin, &dir).await?;
    }
    Ok(())
}

/// [`dissociate_network`] for every repository borrowing from `git_dir`
/// (not `git_dir` itself): used before deleting `git_dir`.
pub async fn dissociate_dependents(git_bin: &str, root: &Path, git_dir: &Path) -> GitResult<()> {
    for dep in dependents_on_disk(root, git_dir) {
        dissociate_network(git_bin, root, &dep).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const STANDALONE: NetworkRole = NetworkRole {
        has_alternates: false,
        has_dependents: false,
    };
    const PARENT: NetworkRole = NetworkRole {
        has_alternates: false,
        has_dependents: true,
    };
    const FORK: NetworkRole = NetworkRole {
        has_alternates: true,
        has_dependents: false,
    };
    const MIDDLE: NetworkRole = NetworkRole {
        has_alternates: true,
        has_dependents: true,
    };

    fn joined(task: Task, role: NetworkRole) -> Vec<String> {
        plan(task, role, 14)
            .unwrap()
            .into_iter()
            .map(|s| s.join(" "))
            .collect()
    }

    #[test]
    fn parents_never_prune() {
        for role in [PARENT, MIDDLE] {
            for task in [Task::Gc, Task::Repack, Task::Incremental, Task::CommitGraph] {
                for step in joined(task, role) {
                    assert!(!step.contains("prune "), "{step}");
                    assert!(!step.starts_with("gc"), "{step}");
                    assert!(!step.contains("-A"), "{step}");
                    if step.starts_with("repack") && step.contains("-a") {
                        assert!(step.contains("--keep-unreachable"), "{step}");
                    }
                }
            }
            assert_eq!(
                plan(Task::PruneNow, role, 14),
                Err(PlanError::HasDependents)
            );
        }
    }

    #[test]
    fn forks_repack_locally_with_grace() {
        let steps = joined(Task::Gc, FORK);
        assert!(
            steps.contains(
                &"repack -A -d -q --unpack-unreachable=14.days.ago -l --no-write-bitmap-index"
                    .to_string()
            ),
            "{steps:?}"
        );
        assert!(steps.contains(&"prune --expire 14.days.ago".to_string()));
        let inc = joined(Task::Incremental, FORK);
        assert!(
            inc.iter().any(|s| s.starts_with("repack")
                && s.contains("-l")
                && !s.contains("bitmap-index --")
                && !s.contains("--write-midx")),
            "{inc:?}"
        );
        assert!(
            inc.iter()
                .any(|s| s.starts_with("commit-graph") && !s.contains("--split"))
        );
        for s in joined(Task::Gc, MIDDLE) {
            if s.starts_with("repack") {
                assert!(s.contains("-l"), "{s}");
            }
        }
    }

    #[test]
    fn standalone_gc_has_grace_and_bitmaps() {
        assert!(
            joined(Task::Gc, STANDALONE).contains(&"gc --quiet --prune=14.days.ago".to_string())
        );
        assert_eq!(grace_arg(0), "1.days.ago");
        let inc = joined(Task::Incremental, STANDALONE);
        assert!(
            inc.iter()
                .any(|s| s.contains("--write-midx --write-bitmap-index"))
        );
        assert!(
            inc.iter()
                .any(|s| s.starts_with("commit-graph") && s.contains("--split"))
        );
        let prune = joined(Task::PruneNow, FORK);
        assert!(prune.iter().any(|s| s == "prune --expire now"));
    }

    #[test]
    fn parses_count_objects() {
        let s = parse_count_objects(
            "count: 12\nsize: 48\nin-pack: 300\npacks: 3\nsize-pack: 100\nprune-packable: 0\ngarbage: 0\nsize-garbage: 0\n",
        );
        assert_eq!(
            s,
            ObjectStats {
                loose_count: 12,
                pack_count: 3,
                size_kib: 148
            }
        );
    }

    /// Immediate pruning (the fork-corrupting `gc` flag) must never come back.
    #[test]
    fn no_immediate_prune_in_codebase() {
        let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let needle = ["prune", "=now"].concat();
        let mut hits = Vec::new();
        fn walk(p: &Path, needle: &str, hits: &mut Vec<String>) {
            let Ok(rd) = std::fs::read_dir(p) else { return };
            for e in rd.flatten() {
                let path = e.path();
                if path.is_dir() {
                    if path.file_name().is_some_and(|n| n == "target") {
                        continue;
                    }
                    walk(&path, needle, hits);
                } else if let Ok(text) = std::fs::read_to_string(&path)
                    && text.contains(needle)
                {
                    hits.push(path.display().to_string());
                }
            }
        }
        walk(crates, &needle, &mut hits);
        assert!(hits.is_empty(), "found {needle:?} in {hits:?}");
    }
}
