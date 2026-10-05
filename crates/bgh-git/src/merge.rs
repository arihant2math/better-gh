//! Server-side merges without a working tree: `git merge-tree
//! --write-tree` to compute trees (and detect conflicts), `commit-tree` to
//! create commits, and old-value-checked `update-ref` to publish them.
//!
//! Also: merge bases, commit ranges, rebasing (cherry-picking with
//! `merge-tree --merge-base`), and copying refs between repositories (for
//! cross-fork pull requests).

use chrono::{DateTime, Utc};

use crate::storage::RepoStore;
use crate::write::Identity;
use crate::{GitError, GitResult, cmd, is_sha, is_valid_ref_name};

fn check_sha(s: &str) -> GitResult<()> {
    if is_sha(s) {
        Ok(())
    } else {
        Err(GitError::InvalidInput(format!("invalid object id {s:?}")))
    }
}

fn git_date(when: DateTime<Utc>, offset_minutes: i32) -> String {
    let sign = if offset_minutes < 0 { '-' } else { '+' };
    let m = offset_minutes.abs();
    format!("{} {sign}{:02}{:02}", when.timestamp(), m / 60, m % 60)
}

fn stdout_line(out: &[u8]) -> String {
    String::from_utf8_lossy(out)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string()
}

/// Best common ancestor of two commits (`None` if unrelated).
pub async fn merge_base(
    store: &RepoStore,
    repo_id: i64,
    a: &str,
    b: &str,
) -> GitResult<Option<String>> {
    check_sha(a)?;
    check_sha(b)?;
    let dir = store.git_dir(repo_id)?;
    let out = cmd::run_status(&store.git_bin, Some(&dir), &["merge-base", a, b], &[]).await?;
    match out.code {
        Some(0) => Ok(Some(stdout_line(&out.stdout))),
        Some(1) => Ok(None),
        _ => Err(GitError::Command {
            args: "merge-base".into(),
            status: format!("{:?}", out.code),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        }),
    }
}

/// Whether `ancestor` is reachable from `descendant` (`merge-base --is-ancestor`).
pub async fn is_ancestor(
    store: &RepoStore,
    repo_id: i64,
    ancestor: &str,
    descendant: &str,
) -> GitResult<bool> {
    check_sha(ancestor)?;
    check_sha(descendant)?;
    let dir = store.git_dir(repo_id)?;
    let out = cmd::run_status(
        &store.git_bin,
        Some(&dir),
        &["merge-base", "--is-ancestor", ancestor, descendant],
        &[],
    )
    .await?;
    match out.code {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(GitError::Command {
            args: "merge-base --is-ancestor".into(),
            status: format!("{:?}", out.code),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        }),
    }
}

/// Outcome of a tree-level merge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeTree {
    Clean { tree: String },
    Conflict { files: Vec<String> },
}

impl MergeTree {
    pub fn tree(&self) -> Option<&str> {
        match self {
            Self::Clean { tree } => Some(tree),
            Self::Conflict { .. } => None,
        }
    }
}

/// Merge `theirs` into `ours` at the tree level. With `merge_base`, use it
/// instead of computing one (used for cherry-picks).
pub async fn merge_tree(
    store: &RepoStore,
    repo_id: i64,
    ours: &str,
    theirs: &str,
    merge_base: Option<&str>,
) -> GitResult<MergeTree> {
    check_sha(ours)?;
    check_sha(theirs)?;
    let dir = store.git_dir(repo_id)?;
    let base_arg;
    let mut args = vec!["merge-tree", "--write-tree", "--name-only", "--no-messages"];
    if let Some(b) = merge_base {
        check_sha(b)?;
        base_arg = format!("--merge-base={b}");
        args.push(&base_arg);
    } else {
        args.push("--allow-unrelated-histories");
    }
    args.push(ours);
    args.push(theirs);
    let timer = bgh_core::observability::git_op("merge-tree");
    let out = cmd::run_status(&store.git_bin, Some(&dir), &args, &[]).await?;
    // Exit 1 is a conflicted (but successful) merge.
    timer.finish(matches!(out.code, Some(0 | 1)));
    let text = String::from_utf8_lossy(&out.stdout);
    match out.code {
        Some(0) => Ok(MergeTree::Clean {
            tree: stdout_line(&out.stdout),
        }),
        Some(1) => {
            let mut files: Vec<String> = text
                .lines()
                .skip(1)
                .take_while(|l| !l.is_empty())
                .map(str::to_string)
                .collect();
            files.dedup();
            Ok(MergeTree::Conflict { files })
        }
        _ => Err(GitError::Command {
            args: args.join(" "),
            status: format!("{:?}", out.code),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        }),
    }
}

/// A commit author/committer with an explicit original timestamp
/// (preserved when rebasing).
#[derive(Debug, Clone)]
pub struct Person {
    pub name: String,
    pub email: String,
    pub when: DateTime<Utc>,
    pub offset_minutes: i32,
}

impl From<&crate::Signature> for Person {
    fn from(s: &crate::Signature) -> Self {
        Self {
            name: s.name.clone(),
            email: s.email.clone(),
            when: s.when,
            offset_minutes: s.offset_minutes,
        }
    }
}

impl From<&Identity> for Person {
    fn from(i: &Identity) -> Self {
        Self {
            name: i.name.clone(),
            email: i.email.clone(),
            when: i.when.unwrap_or_else(Utc::now),
            offset_minutes: 0,
        }
    }
}

fn person_env(prefix: &str, p: &Person) -> Vec<(String, String)> {
    vec![
        (format!("GIT_{prefix}_NAME"), p.name.clone()),
        (format!("GIT_{prefix}_EMAIL"), p.email.clone()),
        (
            format!("GIT_{prefix}_DATE"),
            git_date(p.when, p.offset_minutes),
        ),
    ]
}

/// Create a commit object for `tree` with `parents`.
pub async fn commit_tree(
    store: &RepoStore,
    repo_id: i64,
    tree: &str,
    parents: &[&str],
    message: &str,
    author: &Person,
    committer: &Person,
) -> GitResult<String> {
    check_sha(tree)?;
    let dir = store.git_dir(repo_id)?;
    let mut env = person_env("AUTHOR", author);
    env.extend(person_env("COMMITTER", committer));
    let env_ref: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let mut args = vec!["commit-tree", tree, "--no-gpg-sign", "-F", "-"];
    for p in parents {
        check_sha(p)?;
        args.push("-p");
        args.push(p);
    }
    let out = cmd::run(
        &store.git_bin,
        Some(&dir),
        &args,
        &env_ref,
        Some(message.as_bytes()),
    )
    .await?;
    crate::signing::sign_commit(
        &store.git_bin,
        &dir,
        store.signer.as_deref(),
        stdout_line(&out),
    )
    .await
}

/// Commits reachable from `head` but not from `base`, oldest first
/// (`git rev-list --reverse --topo-order base..head`), at most `limit`.
pub async fn rev_list(
    store: &RepoStore,
    repo_id: i64,
    base: Option<&str>,
    head: &str,
    limit: usize,
) -> GitResult<Vec<String>> {
    check_sha(head)?;
    let dir = store.git_dir(repo_id)?;
    let range = match base {
        Some(b) => {
            check_sha(b)?;
            format!("{b}..{head}")
        }
        None => head.to_string(),
    };
    let out = cmd::run(
        &store.git_bin,
        Some(&dir),
        &["rev-list", "--reverse", "--topo-order", &range],
        &[],
        None,
    )
    .await?;
    Ok(String::from_utf8_lossy(&out)
        .lines()
        .filter(|l| !l.is_empty())
        .take(limit)
        .map(str::to_string)
        .collect())
}

/// Number of commits in `base..head`.
pub async fn count_commits(
    store: &RepoStore,
    repo_id: i64,
    base: &str,
    head: &str,
) -> GitResult<i64> {
    check_sha(base)?;
    check_sha(head)?;
    let dir = store.git_dir(repo_id)?;
    let range = format!("{base}..{head}");
    let out = cmd::run(
        &store.git_bin,
        Some(&dir),
        &["rev-list", "--count", &range],
        &[],
        None,
    )
    .await?;
    Ok(stdout_line(&out).parse().unwrap_or(0))
}

/// Outcome of [`rebase`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RebaseResult {
    /// New tip after replaying every commit.
    Done { head: String },
    /// Commit that failed to apply.
    Conflict { commit: String },
    /// The range contains merge commits.
    HasMerges,
}

/// Replay `base..head` onto `onto` (like `git rebase`, preserving authors
/// and messages, using `committer` as committer). Nothing is written to
/// refs; the caller publishes the result.
pub async fn rebase(
    store: &RepoStore,
    repo_id: i64,
    onto: &str,
    base: &str,
    head: &str,
    committer: &Identity,
) -> GitResult<RebaseResult> {
    let commits = rev_list(store, repo_id, Some(base), head, usize::MAX).await?;
    let repo_commits = {
        let shas = commits.clone();
        store
            .read(repo_id, move |r| {
                shas.iter()
                    .map(|s| r.commit(s))
                    .collect::<GitResult<Vec<_>>>()
            })
            .await?
    };
    if repo_commits.iter().any(|c| c.parents.len() > 1) {
        return Ok(RebaseResult::HasMerges);
    }
    let committer = Person::from(committer);
    let mut tip = onto.to_string();
    for c in &repo_commits {
        let parent = c.parents.first().map(String::as_str);
        let tree = match parent {
            Some(p) => match merge_tree(store, repo_id, &tip, &c.sha, Some(p)).await? {
                MergeTree::Clean { tree } => tree,
                MergeTree::Conflict { .. } => {
                    return Ok(RebaseResult::Conflict {
                        commit: c.sha.clone(),
                    });
                }
            },
            None => c.tree.clone(),
        };
        tip = commit_tree(
            store,
            repo_id,
            &tree,
            &[&tip],
            &c.message,
            &Person::from(&c.author),
            &Person {
                when: committer.when,
                ..committer.clone()
            },
        )
        .await?;
    }
    Ok(RebaseResult::Done { head: tip })
}

/// Copy `src_ref` of repository `src_id` into `dst_ref` of `dst_id`
/// (force-updated), transferring missing objects. Returns the new value of
/// `dst_ref`. Used to mirror a fork's head branch into the base repository
/// as `refs/pull/{n}/head`.
pub async fn fetch_ref(
    store: &RepoStore,
    dst_id: i64,
    src_id: i64,
    src_ref: &str,
    dst_ref: &str,
) -> GitResult<String> {
    for r in [src_ref, dst_ref] {
        if !r.starts_with("refs/") || !is_valid_ref_name(r) {
            return Err(GitError::InvalidInput(format!("invalid ref {r:?}")));
        }
    }
    let dst = store.git_dir(dst_id)?;
    let src = store.git_dir(src_id)?;
    let src_s = src.to_string_lossy().to_string();
    let spec = format!("+{src_ref}:{dst_ref}");
    cmd::run(
        &store.git_bin,
        Some(&dst),
        &[
            "fetch",
            "--quiet",
            "--no-tags",
            "--no-write-fetch-head",
            "--no-auto-gc",
            &src_s,
            &spec,
        ],
        &[],
        None,
    )
    .await?;
    let out = cmd::run(
        &store.git_bin,
        Some(&dst),
        &["rev-parse", "--verify", dst_ref],
        &[],
        None,
    )
    .await?;
    Ok(stdout_line(&out))
}

/// Push commit `sha` from repository `src_id` to `dst_ref` of `dst_id`,
/// requiring `dst_ref` to currently be `expected_old` (lease). Used to
/// update a fork's head branch (update-branch with maintainer edits).
pub async fn push_ref(
    store: &RepoStore,
    src_id: i64,
    dst_id: i64,
    sha: &str,
    dst_ref: &str,
    expected_old: &str,
) -> GitResult<()> {
    check_sha(sha)?;
    check_sha(expected_old)?;
    if !dst_ref.starts_with("refs/") || !is_valid_ref_name(dst_ref) {
        return Err(GitError::InvalidInput(format!("invalid ref {dst_ref:?}")));
    }
    let src = store.git_dir(src_id)?;
    let dst = store.git_dir(dst_id)?;
    let dst_s = dst.to_string_lossy().to_string();
    // Make the object reachable by name for the transfer.
    let tmp = format!("refs/bgh-tmp/{}", bgh_core::crypto::random_token(12));
    cmd::run(
        &store.git_bin,
        Some(&src),
        &["update-ref", &tmp, sha],
        &[],
        None,
    )
    .await?;
    let spec = format!("{tmp}:{dst_ref}");
    let lease = format!("--force-with-lease={dst_ref}:{expected_old}");
    let res = cmd::run(
        &store.git_bin,
        Some(&src),
        &["push", "--quiet", "--no-verify", &lease, &dst_s, &spec],
        &[],
        None,
    )
    .await;
    let _ = cmd::run(
        &store.git_bin,
        Some(&src),
        &["update-ref", "-d", &tmp],
        &[],
        None,
    )
    .await;
    res.map(|_| ())
}

/// Like [`crate::write::update_ref`] but returns `Ok(false)` instead of an
/// error when the old-value check fails (ref moved concurrently).
pub async fn compare_and_swap_ref(
    store: &RepoStore,
    repo_id: i64,
    refname: &str,
    new: &str,
    old: &str,
) -> GitResult<bool> {
    check_sha(new)?;
    check_sha(old)?;
    if !refname.starts_with("refs/") || !is_valid_ref_name(refname) {
        return Err(GitError::InvalidInput(format!("invalid ref {refname:?}")));
    }
    let dir = store.git_dir(repo_id)?;
    let out = cmd::run_status(
        &store.git_bin,
        Some(&dir),
        &["update-ref", refname, new, old],
        &[],
    )
    .await?;
    Ok(out.code == Some(0))
}

/// Force-set `refname` to `new` (internal refs such as `refs/pull/*`).
pub async fn force_ref(store: &RepoStore, repo_id: i64, refname: &str, new: &str) -> GitResult<()> {
    check_sha(new)?;
    if !refname.starts_with("refs/") || !is_valid_ref_name(refname) {
        return Err(GitError::InvalidInput(format!("invalid ref {refname:?}")));
    }
    let dir = store.git_dir(repo_id)?;
    cmd::run(
        &store.git_bin,
        Some(&dir),
        &["update-ref", refname, new],
        &[],
        None,
    )
    .await?;
    Ok(())
}

/// Delete an internal ref (`refs/pull/*`); missing refs are fine.
pub async fn remove_ref(store: &RepoStore, repo_id: i64, refname: &str) -> GitResult<()> {
    if !refname.starts_with("refs/") || !is_valid_ref_name(refname) {
        return Err(GitError::InvalidInput(format!("invalid ref {refname:?}")));
    }
    let dir = store.git_dir(repo_id)?;
    cmd::run_status(
        &store.git_bin,
        Some(&dir),
        &["update-ref", "-d", refname],
        &[],
    )
    .await?;
    Ok(())
}

/// The test merge commit of a pull request: `head` merged into `base`
/// (parents base, head), committed by `committer` dated like the head commit
/// so the same inputs always give the same sha. Points `refname`
/// (`refs/pull/{n}/merge`) at it, or deletes `refname` and returns `None`
/// when the merge conflicts.
pub async fn test_merge(
    store: &RepoStore,
    repo_id: i64,
    refname: &str,
    base: &str,
    head: &str,
    committer: &Identity,
) -> GitResult<Option<String>> {
    match merge_tree(store, repo_id, base, head, None).await? {
        MergeTree::Clean { tree } => {
            let head_owned = head.to_string();
            let when = store
                .read(repo_id, move |r| Ok(r.commit(&head_owned)?.committer.when))
                .await?;
            let mut person = Person::from(committer);
            person.when = when;
            let msg = format!("Merge {head} into {base}");
            // Unsigned: the test merge must be reproducible (bgh-pulls and
            // Actions compute the same sha), and a signature is not.
            let unsigned = RepoStore {
                signer: None,
                ..store.clone()
            };
            let sha = commit_tree(
                &unsigned,
                repo_id,
                &tree,
                &[base, head],
                &msg,
                &person,
                &person,
            )
            .await?;
            force_ref(store, repo_id, refname, &sha).await?;
            Ok(Some(sha))
        }
        MergeTree::Conflict { .. } => {
            remove_ref(store, repo_id, refname).await?;
            Ok(None)
        }
    }
}
