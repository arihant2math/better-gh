//! Git operations through the `git` CLI that gix doesn't cover well:
//! filtered history, diffs with patches, merge bases, merges, object
//! writes (blobs, trees, commits, tags), cross-repository object access and
//! fork dissociation.
//!
//! Obtain a [`GitCli`] with [`RepoStore::cli`]. [`GitCli::with_objects_of`]
//! makes another repository's objects visible (through
//! `GIT_ALTERNATE_OBJECT_DIRECTORIES`), which is how cross-fork compares
//! and merges see both sides.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use chrono::{DateTime, Utc};
use tokio::io::AsyncWriteExt;

use crate::objects::{Commit, Tag, TreeEntryKind};
use crate::storage::RepoStore;
use crate::write::Identity;
use crate::{GitError, GitResult, ZERO_SHA, cmd, is_sha};

/// SHA of the empty tree (always resolvable by git).
pub const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// Maximum size of a single file patch returned by [`GitCli::diff`].
pub const MAX_PATCH_BYTES: usize = 1024 * 1024;

/// A `git` invocation context for one repository.
#[derive(Debug, Clone)]
pub struct GitCli {
    bin: String,
    dir: PathBuf,
    alternates: Vec<PathBuf>,
}

impl RepoStore {
    /// CLI helper for `repo_id` (404 if the repository doesn't exist).
    pub fn cli(&self, repo_id: i64) -> GitResult<GitCli> {
        Ok(GitCli {
            bin: self.git_bin.clone(),
            dir: self.git_dir(repo_id)?,
            alternates: Vec::new(),
        })
    }
}

/// Filters for [`GitCli::log`].
#[derive(Debug, Clone, Default)]
pub struct LogFilter {
    /// Starting revision (commit SHA preferred).
    pub rev: String,
    /// Commits reachable from `rev` but not from these.
    pub exclude: Vec<String>,
    pub path: Option<String>,
    /// Author name/email regex (`--author`).
    pub author: Option<String>,
    pub committer: Option<String>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub skip: usize,
    pub limit: usize,
    /// Oldest first.
    pub reverse: bool,
    /// Only first parents.
    pub first_parent: bool,
}

/// One changed file between two trees.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DiffFile {
    /// `added` | `removed` | `modified` | `renamed` | `copied` | `changed`
    pub status: String,
    pub path: String,
    pub previous_path: Option<String>,
    pub old_sha: String,
    pub new_sha: String,
    pub old_mode: String,
    pub new_mode: String,
    pub additions: u64,
    pub deletions: u64,
    pub binary: bool,
    /// Unified diff hunks (from the first `@@`), absent for binary files or
    /// patches larger than [`MAX_PATCH_BYTES`].
    pub patch: Option<String>,
}

impl DiffFile {
    /// Blob SHA to show for the file (new side, or old side when removed).
    pub fn sha(&self) -> &str {
        if self.status == "removed" {
            &self.old_sha
        } else {
            &self.new_sha
        }
    }
}

/// An entry of a recursive tree listing.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LsTreeEntry {
    pub path: String,
    pub mode: String,
    pub kind: TreeEntryKind,
    pub sha: String,
    /// Blob size (None for trees and submodules).
    pub size: Option<u64>,
}

/// A shortlog line.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Contributor {
    pub name: String,
    pub email: String,
    pub commits: u64,
}

/// Result of a three-way tree merge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeOutcome {
    Clean { tree: String },
    Conflicts { paths: Vec<String> },
}

/// Entry of [`GitCli::build_tree`].
#[derive(Debug, Clone)]
pub enum TreeEdit {
    /// Put an existing object (`blob` / `tree` / `commit` for submodules).
    Object {
        path: String,
        mode: String,
        sha: String,
    },
    /// Write `content` as a blob and add it.
    Content {
        path: String,
        mode: String,
        content: Vec<u8>,
    },
    /// Remove a path (file or directory).
    Delete { path: String },
}

fn fmt_date(when: DateTime<Utc>) -> String {
    format!("{} +0000", when.timestamp())
}

fn ident_env(prefix: &str, id: &Identity) -> Vec<(String, String)> {
    let mut env = vec![
        (format!("GIT_{prefix}_NAME"), id.name.clone()),
        (format!("GIT_{prefix}_EMAIL"), id.email.clone()),
    ];
    if let Some(when) = id.when {
        env.push((format!("GIT_{prefix}_DATE"), fmt_date(when)));
    }
    env
}

fn trimmed(out: Vec<u8>) -> String {
    String::from_utf8_lossy(&out).trim().to_string()
}

fn is_valid_tree_path(path: &str) -> bool {
    !(path.is_empty()
        || path.starts_with('/')
        || path.ends_with('/')
        || path.contains('\0')
        || path.contains('\n')
        || path
            .split('/')
            .any(|c| c.is_empty() || c == "." || c == ".." || c.eq_ignore_ascii_case(".git")))
}

impl GitCli {
    /// The repository's git directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Make the objects of the repository at `git_dir` readable too.
    pub fn with_objects_of(mut self, git_dir: &Path) -> Self {
        if git_dir != self.dir {
            self.alternates.push(git_dir.join("objects"));
        }
        self
    }

    fn command(&self, envs: &[(&str, &str)]) -> tokio::process::Command {
        let mut c = cmd::git(&self.bin, Some(&self.dir));
        if !self.alternates.is_empty() {
            let joined = std::env::join_paths(&self.alternates).unwrap_or_default();
            c.env("GIT_ALTERNATE_OBJECT_DIRECTORIES", joined);
        }
        c.envs(envs.iter().copied());
        c
    }

    /// Run to completion, feeding `stdin`; returns (exit code, stdout, stderr).
    async fn exec(
        &self,
        args: &[&str],
        envs: &[(&str, &str)],
        stdin: Option<&[u8]>,
    ) -> GitResult<(Option<i32>, Vec<u8>, String)> {
        let mut c = self.command(envs);
        c.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
        if stdin.is_some() {
            c.stdin(Stdio::piped());
        }
        let mut child = c.spawn()?;
        if let Some(input) = stdin {
            let mut pipe = child.stdin.take().expect("piped stdin");
            let input = input.to_vec();
            tokio::spawn(async move {
                let _ = pipe.write_all(&input).await;
                let _ = pipe.shutdown().await;
            });
        }
        let out = child.wait_with_output().await?;
        Ok((
            out.status.code(),
            out.stdout,
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ))
    }

    /// Run and require success.
    pub async fn run(
        &self,
        args: &[&str],
        envs: &[(&str, &str)],
        stdin: Option<&[u8]>,
    ) -> GitResult<Vec<u8>> {
        let (code, out, err) = self.exec(args, envs, stdin).await?;
        if code != Some(0) {
            return Err(GitError::Command {
                args: args.join(" "),
                status: format!("{code:?}"),
                stderr: err,
            });
        }
        Ok(out)
    }

    // ----- revisions --------------------------------------------------------

    /// Resolve `rev` to a commit SHA (peeling tags), or `None`.
    pub async fn resolve_commit(&self, rev: &str) -> GitResult<Option<String>> {
        if rev.is_empty() || rev.starts_with('-') || rev.contains('\0') {
            return Ok(None);
        }
        let spec = format!("{rev}^{{commit}}");
        let (code, out, _) = self
            .exec(
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    "--end-of-options",
                    &spec,
                ],
                &[],
                None,
            )
            .await?;
        Ok((code == Some(0))
            .then(|| trimmed(out))
            .filter(|s| is_sha(s)))
    }

    /// Object type (`commit`, `tree`, `blob`, `tag`) if `sha` exists.
    pub async fn object_type(&self, sha: &str) -> GitResult<Option<String>> {
        if !is_sha(sha) {
            return Ok(None);
        }
        let (code, out, _) = self.exec(&["cat-file", "-t", sha], &[], None).await?;
        Ok((code == Some(0)).then(|| trimmed(out)))
    }

    /// Whether `ancestor` is reachable from `descendant`.
    pub async fn is_ancestor(&self, ancestor: &str, descendant: &str) -> GitResult<bool> {
        let (code, _, err) = self
            .exec(
                &["merge-base", "--is-ancestor", ancestor, descendant],
                &[],
                None,
            )
            .await?;
        match code {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(GitError::Command {
                args: "merge-base --is-ancestor".into(),
                status: format!("{code:?}"),
                stderr: err,
            }),
        }
    }

    /// Best common ancestor of two commits.
    pub async fn merge_base(&self, a: &str, b: &str) -> GitResult<Option<String>> {
        let (code, out, _) = self.exec(&["merge-base", a, b], &[], None).await?;
        Ok((code == Some(0))
            .then(|| trimmed(out))
            .filter(|s| is_sha(s)))
    }

    /// `(ahead, behind)`: commits in `head` not in `base`, and vice versa.
    pub async fn ahead_behind(&self, base: &str, head: &str) -> GitResult<(u64, u64)> {
        let range = format!("{base}...{head}");
        let out = trimmed(
            self.run(&["rev-list", "--left-right", "--count", &range], &[], None)
                .await?,
        );
        let mut it = out
            .split_whitespace()
            .map(|n| n.parse::<u64>().unwrap_or(0));
        let behind = it.next().unwrap_or(0);
        let ahead = it.next().unwrap_or(0);
        Ok((ahead, behind))
    }

    /// Commit SHAs matching `filter`, newest first unless `reverse`.
    pub async fn log(&self, f: &LogFilter) -> GitResult<Vec<String>> {
        let mut args: Vec<String> = vec!["log".into(), "--format=%H".into()];
        if f.limit > 0 && !f.reverse {
            args.push(format!("--max-count={}", f.limit));
            if f.skip > 0 {
                args.push(format!("--skip={}", f.skip));
            }
        }
        if f.reverse {
            args.push("--reverse".into());
        }
        if f.first_parent {
            args.push("--first-parent".into());
        }
        if let Some(a) = &f.author {
            args.push(format!("--author={}", regex_escape(a)));
        }
        if let Some(c) = &f.committer {
            args.push(format!("--committer={}", regex_escape(c)));
        }
        if let Some(s) = f.since {
            args.push(format!("--since={}", s.timestamp()));
        }
        if let Some(u) = f.until {
            args.push(format!("--until={}", u.timestamp()));
        }
        args.push("--end-of-options".into());
        args.push(f.rev.clone());
        for e in &f.exclude {
            args.push(format!("^{e}"));
        }
        args.push("--".into());
        if let Some(p) = f.path.as_deref().filter(|p| !p.is_empty()) {
            args.push(p.to_string());
        }
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = self.run(&refs, &[], None).await?;
        let mut shas: Vec<String> = String::from_utf8_lossy(&out)
            .lines()
            .filter(|l| is_sha(l))
            .map(str::to_string)
            .collect();
        if f.reverse && f.limit > 0 {
            shas = shas.into_iter().skip(f.skip).take(f.limit).collect();
        }
        Ok(shas)
    }

    /// Count commits in `rev` not reachable from `exclude`.
    pub async fn count(&self, rev: &str, exclude: Option<&str>) -> GitResult<u64> {
        let neg = exclude.map(|e| format!("^{e}"));
        let mut args = vec!["rev-list", "--count", rev];
        if let Some(n) = &neg {
            args.push(n);
        }
        Ok(trimmed(self.run(&args, &[], None).await?)
            .parse()
            .unwrap_or(0))
    }

    /// Read raw objects in one `git cat-file --batch`. Missing objects are
    /// skipped. Returns `(sha, type, data)` in input order.
    pub async fn cat_objects(&self, shas: &[String]) -> GitResult<Vec<(String, String, Vec<u8>)>> {
        if shas.is_empty() {
            return Ok(vec![]);
        }
        let mut input = Vec::with_capacity(shas.len() * 41);
        for s in shas {
            input.extend_from_slice(s.as_bytes());
            input.push(b'\n');
        }
        let out = self
            .run(&["cat-file", "--batch"], &[], Some(&input))
            .await?;
        let mut res = Vec::with_capacity(shas.len());
        let mut i = 0;
        while i < out.len() {
            let nl = match out[i..].iter().position(|&b| b == b'\n') {
                Some(n) => i + n,
                None => break,
            };
            let header = String::from_utf8_lossy(&out[i..nl]).to_string();
            i = nl + 1;
            let parts: Vec<&str> = header.split(' ').collect();
            if parts.len() != 3 {
                continue; // "<sha> missing"
            }
            let size: usize = parts[2]
                .parse()
                .map_err(|_| GitError::Object("bad cat-file header".into()))?;
            if i + size > out.len() {
                return Err(GitError::Object("truncated cat-file output".into()));
            }
            res.push((
                parts[0].to_string(),
                parts[1].to_string(),
                out[i..i + size].to_vec(),
            ));
            i += size + 1;
        }
        Ok(res)
    }

    /// Parse many commits at once (input order, missing ones skipped).
    pub async fn commits(&self, shas: &[String]) -> GitResult<Vec<Commit>> {
        self.cat_objects(shas)
            .await?
            .into_iter()
            .filter(|(_, kind, _)| kind == "commit")
            .map(|(sha, _, data)| Commit::parse(&sha, &data))
            .collect()
    }

    /// One commit (404 if missing).
    pub async fn commit(&self, sha: &str) -> GitResult<Commit> {
        self.commits(&[sha.to_string()])
            .await?
            .pop()
            .ok_or_else(|| GitError::NotFound(format!("commit {sha}")))
    }

    /// One annotated tag (404 if missing or not a tag).
    pub async fn tag(&self, sha: &str) -> GitResult<Tag> {
        let mut objs = self.cat_objects(&[sha.to_string()]).await?;
        match objs.pop() {
            Some((sha, kind, data)) if kind == "tag" => Tag::parse(&sha, &data),
            _ => Err(GitError::NotFound(format!("tag {sha}"))),
        }
    }

    // ----- diffs ------------------------------------------------------------

    /// Changed files between `from` (None = empty tree) and `to`, with
    /// rename detection, line counts and per-file patches.
    pub async fn diff(&self, from: Option<&str>, to: &str) -> GitResult<Vec<DiffFile>> {
        let from = from.unwrap_or(EMPTY_TREE);
        let raw = self
            .run(
                &[
                    "diff",
                    "--raw",
                    "--numstat",
                    "-z",
                    "-M",
                    "--no-abbrev",
                    "--no-ext-diff",
                    "--no-textconv",
                    from,
                    to,
                    "--",
                ],
                &[],
                None,
            )
            .await?;
        let mut files = parse_raw_numstat(&raw)?;
        if files.is_empty() {
            return Ok(files);
        }
        let patch = self
            .run(
                &[
                    "diff",
                    "-M",
                    "--no-color",
                    "--no-ext-diff",
                    "--no-textconv",
                    "--full-index",
                    from,
                    to,
                    "--",
                ],
                &[],
                None,
            )
            .await?;
        let patches = split_patches(&String::from_utf8_lossy(&patch));
        if patches.len() == files.len() {
            for (f, p) in files.iter_mut().zip(patches) {
                if !f.binary && p.as_ref().is_some_and(|p| p.len() <= MAX_PATCH_BYTES) {
                    f.patch = p;
                }
            }
        }
        Ok(files)
    }

    /// Full unified diff text (`application/vnd.github.diff`).
    pub async fn diff_text(&self, from: Option<&str>, to: &str) -> GitResult<Vec<u8>> {
        self.run(
            &[
                "diff",
                "-M",
                "--no-color",
                "--no-ext-diff",
                "--no-textconv",
                "--full-index",
                "--binary",
                from.unwrap_or(EMPTY_TREE),
                to,
                "--",
            ],
            &[],
            None,
        )
        .await
    }

    /// Mailbox-format patches for the commits of `base..head` (or just
    /// `head` when `base` is None), oldest first.
    pub async fn format_patch(&self, base: Option<&str>, head: &str) -> GitResult<Vec<u8>> {
        let range = match base {
            Some(b) => format!("{b}..{head}"),
            None => format!("-1 {head}"),
        };
        let mut args = vec![
            "format-patch",
            "--stdout",
            "--no-color",
            "--no-signature",
            "--full-index",
            "--binary",
        ];
        let parts: Vec<&str> = range.split(' ').collect();
        args.extend(parts);
        self.run(&args, &[], None).await
    }

    // ----- trees ------------------------------------------------------------

    /// List a tree (`recursive` includes sub-trees and their contents).
    pub async fn ls_tree(&self, tree: &str, recursive: bool) -> GitResult<Vec<LsTreeEntry>> {
        let mut args = vec!["ls-tree", "-l", "-z", "--full-tree"];
        if recursive {
            args.push("-r");
            args.push("-t");
        }
        args.push(tree);
        let out = self.run(&args, &[], None).await?;
        let mut entries = Vec::new();
        for rec in out.split(|&b| b == 0).filter(|r| !r.is_empty()) {
            let tab = rec
                .iter()
                .position(|&b| b == b'\t')
                .ok_or_else(|| GitError::Object("malformed ls-tree".into()))?;
            let head = String::from_utf8_lossy(&rec[..tab]);
            let path = String::from_utf8_lossy(&rec[tab + 1..]).into_owned();
            let mut it = head.split_whitespace();
            let (Some(mode), Some(_kind), Some(sha), Some(size)) =
                (it.next(), it.next(), it.next(), it.next())
            else {
                return Err(GitError::Object("malformed ls-tree".into()));
            };
            let mode = format!("{mode:0>6}");
            let kind = match mode.as_str() {
                "040000" => TreeEntryKind::Tree,
                "160000" => TreeEntryKind::Commit,
                "120000" => TreeEntryKind::Symlink,
                "100755" => TreeEntryKind::Executable,
                _ => TreeEntryKind::Blob,
            };
            entries.push(LsTreeEntry {
                path,
                mode,
                kind,
                sha: sha.to_string(),
                size: size.parse().ok(),
            });
        }
        Ok(entries)
    }

    /// Commit counts per author (`git shortlog -sne`) on `rev`, most
    /// commits first.
    pub async fn shortlog(&self, rev: &str) -> GitResult<Vec<Contributor>> {
        let out = self
            .run(&["shortlog", "-sne", "--no-merges", rev, "--"], &[], None)
            .await?;
        let mut res = Vec::new();
        for line in String::from_utf8_lossy(&out).lines() {
            let line = line.trim();
            let Some((count, rest)) = line.split_once('\t') else {
                continue;
            };
            let (name, email) = match (rest.rfind('<'), rest.rfind('>')) {
                (Some(lt), Some(gt)) if gt > lt => {
                    (rest[..lt].trim().to_string(), rest[lt + 1..gt].to_string())
                }
                _ => (rest.trim().to_string(), String::new()),
            };
            res.push(Contributor {
                name,
                email,
                commits: count.trim().parse().unwrap_or(0),
            });
        }
        res.sort_by(|a, b| b.commits.cmp(&a.commits).then(a.email.cmp(&b.email)));
        Ok(res)
    }

    // ----- writes -----------------------------------------------------------

    /// Write a blob; returns its SHA.
    pub async fn write_blob(&self, data: &[u8]) -> GitResult<String> {
        Ok(trimmed(
            self.run(&["hash-object", "-w", "--stdin"], &[], Some(data))
                .await?,
        ))
    }

    /// Build a tree from `base` (None = empty) with `edits` applied, writing
    /// only the changed trees (`git mktree`). Directories left empty are
    /// dropped, like git does.
    pub async fn build_tree(&self, base: Option<&str>, edits: &[TreeEdit]) -> GitResult<String> {
        let mut root = EditDir::Unloaded(base.unwrap_or(EMPTY_TREE).to_string());
        for e in edits {
            let path = match e {
                TreeEdit::Object { path, .. }
                | TreeEdit::Content { path, .. }
                | TreeEdit::Delete { path } => path,
            };
            if !is_valid_tree_path(path) {
                return Err(GitError::InvalidInput(format!("invalid path {path:?}")));
            }
            let parts: Vec<&str> = path.split('/').collect();
            let (name, dirs) = parts.split_last().expect("non-empty path");
            let leaf = match e {
                TreeEdit::Delete { .. } => None,
                TreeEdit::Object { mode, sha, .. } => {
                    let mode = format!("{mode:0>6}");
                    if mode == "040000" {
                        Some(EditNode::Dir(EditDir::Unloaded(sha.clone())))
                    } else {
                        Some(EditNode::Leaf {
                            mode,
                            sha: sha.clone(),
                        })
                    }
                }
                TreeEdit::Content { mode, content, .. } => Some(EditNode::Leaf {
                    mode: format!("{mode:0>6}"),
                    sha: self.write_blob(content).await?,
                }),
            };
            let mut dir = &mut root;
            for d in dirs {
                let entries = dir.load(self).await?;
                let node = entries
                    .entry(d.to_string())
                    .or_insert_with(|| EditNode::Dir(EditDir::Loaded(Default::default())));
                if !matches!(node, EditNode::Dir(_)) {
                    // A file where a directory is needed: replace it.
                    *node = EditNode::Dir(EditDir::Loaded(Default::default()));
                }
                let EditNode::Dir(next) = node else {
                    unreachable!()
                };
                dir = next;
            }
            let entries = dir.load(self).await?;
            match leaf {
                Some(node) => {
                    entries.insert(name.to_string(), node);
                }
                None => {
                    entries.remove(*name);
                }
            }
        }
        Ok(root
            .write(self)
            .await?
            .unwrap_or_else(|| EMPTY_TREE.to_string()))
    }

    /// Create a commit object; returns its SHA.
    pub async fn commit_tree(
        &self,
        tree: &str,
        parents: &[String],
        message: &str,
        author: &Identity,
        committer: &Identity,
    ) -> GitResult<String> {
        let mut env = ident_env("AUTHOR", author);
        env.extend(ident_env("COMMITTER", committer));
        let env_ref: Vec<(&str, &str)> =
            env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let mut args = vec!["commit-tree", tree, "--no-gpg-sign", "-F", "-"];
        for p in parents {
            args.push("-p");
            args.push(p);
        }
        Ok(trimmed(
            self.run(&args, &env_ref, Some(message.as_bytes())).await?,
        ))
    }

    /// Create an annotated tag object; returns its SHA.
    pub async fn write_tag(
        &self,
        name: &str,
        message: &str,
        object: &str,
        object_type: &str,
        tagger: &Identity,
    ) -> GitResult<String> {
        if name.is_empty() || name.contains(['\n', '\0']) {
            return Err(GitError::InvalidInput(format!("invalid tag name {name:?}")));
        }
        let when = tagger.when.unwrap_or_else(Utc::now);
        let mut body = format!(
            "object {object}\ntype {object_type}\ntag {name}\ntagger {} <{}> {}\n\n",
            tagger.name.replace(['<', '>', '\n'], ""),
            tagger.email.replace(['<', '>', '\n'], ""),
            fmt_date(when)
        );
        body.push_str(message);
        if !message.is_empty() && !message.ends_with('\n') {
            body.push('\n');
        }
        Ok(trimmed(
            self.run(&["mktag"], &[], Some(body.as_bytes())).await?,
        ))
    }

    /// Three-way merge of two commits (`git merge-tree --write-tree`).
    pub async fn merge_trees(&self, ours: &str, theirs: &str) -> GitResult<MergeOutcome> {
        let (code, out, err) = self
            .exec(
                &[
                    "merge-tree",
                    "--write-tree",
                    "--name-only",
                    "-z",
                    "--no-messages",
                    "--allow-unrelated-histories",
                    ours,
                    theirs,
                ],
                &[],
                None,
            )
            .await?;
        let mut parts = out.split(|&b| b == 0).filter(|p| !p.is_empty());
        let tree = parts
            .next()
            .map(|t| String::from_utf8_lossy(t).trim().to_string())
            .unwrap_or_default();
        match code {
            Some(0) if is_sha(&tree) => Ok(MergeOutcome::Clean { tree }),
            Some(1) => {
                let mut paths: Vec<String> = parts
                    .map(|p| String::from_utf8_lossy(p).into_owned())
                    .collect();
                paths.dedup();
                Ok(MergeOutcome::Conflicts { paths })
            }
            _ => Err(GitError::Command {
                args: "merge-tree".into(),
                status: format!("{code:?}"),
                stderr: err,
            }),
        }
    }

    /// Update `refname` from `old` (None = must not exist) to `new`.
    pub async fn update_ref(&self, refname: &str, new: &str, old: Option<&str>) -> GitResult<()> {
        self.run(
            &["update-ref", refname, new, old.unwrap_or(ZERO_SHA)],
            &[],
            None,
        )
        .await
        .map_err(|e| match e {
            GitError::Command { stderr, .. } => {
                GitError::InvalidInput(format!("could not update {refname}: {stderr}"))
            }
            other => other,
        })?;
        Ok(())
    }

    /// Copy the objects reachable from `rev` in the repository at
    /// `src_dir` into this one (no refs are kept).
    pub async fn fetch_objects(&self, src_dir: &Path, rev: &str) -> GitResult<()> {
        let tmp = format!("refs/bgh-tmp/{}", bgh_core::crypto::random_token(12));
        let spec = format!("+{rev}:{tmp}");
        let src = src_dir.to_string_lossy().to_string();
        let res = self
            .run(
                &[
                    "fetch",
                    "--quiet",
                    "--no-tags",
                    "--no-write-fetch-head",
                    &src,
                    &spec,
                ],
                &[],
                None,
            )
            .await;
        let _ = self.run(&["update-ref", "-d", &tmp], &[], None).await;
        res.map(|_| ())
    }

    /// Repack everything reachable (including objects borrowed from
    /// alternates) into this repository and drop `objects/info/alternates`,
    /// making it self-contained. Idempotent.
    pub async fn dissociate(&self) -> GitResult<()> {
        let alternates = self.dir.join("objects/info/alternates");
        let has_alternates = tokio::fs::try_exists(&alternates).await.unwrap_or(false);
        if !has_alternates && self.alternates.is_empty() {
            return Ok(());
        }
        self.run(&["repack", "-a", "-d", "-q"], &[], None).await?;
        match tokio::fs::remove_file(&alternates).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// Repositories (git dirs) this one borrows objects from.
    pub async fn alternates(&self) -> Vec<PathBuf> {
        let file = self.dir.join("objects/info/alternates");
        let Ok(text) = tokio::fs::read_to_string(&file).await else {
            return vec![];
        };
        text.lines()
            .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
            .map(|l| {
                let p = PathBuf::from(l.trim());
                let p = if p.is_relative() {
                    self.dir.join("objects").join(p)
                } else {
                    p
                };
                p.parent().map(Path::to_path_buf).unwrap_or(p)
            })
            .collect()
    }
}

enum EditNode {
    Leaf { mode: String, sha: String },
    Dir(EditDir),
}

enum EditDir {
    Unloaded(String),
    Loaded(std::collections::BTreeMap<String, EditNode>),
}

impl EditDir {
    async fn load(
        &mut self,
        git: &GitCli,
    ) -> GitResult<&mut std::collections::BTreeMap<String, EditNode>> {
        if let EditDir::Unloaded(sha) = self {
            let mut map = std::collections::BTreeMap::new();
            if sha != EMPTY_TREE {
                for e in git.ls_tree(sha, false).await? {
                    let node = if e.kind == TreeEntryKind::Tree {
                        EditNode::Dir(EditDir::Unloaded(e.sha))
                    } else {
                        EditNode::Leaf {
                            mode: e.mode,
                            sha: e.sha,
                        }
                    };
                    map.insert(e.path, node);
                }
            }
            *self = EditDir::Loaded(map);
        }
        match self {
            EditDir::Loaded(map) => Ok(map),
            EditDir::Unloaded(_) => unreachable!(),
        }
    }

    /// Write this directory; `None` when it ends up empty.
    fn write<'a>(
        &'a self,
        git: &'a GitCli,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = GitResult<Option<String>>> + Send + 'a>>
    {
        Box::pin(async move {
            let map = match self {
                EditDir::Unloaded(sha) => return Ok(Some(sha.clone())),
                EditDir::Loaded(map) => map,
            };
            let mut input = Vec::new();
            for (name, node) in map {
                let (mode, kind, sha) = match node {
                    EditNode::Leaf { mode, sha } => {
                        let kind = if mode == "160000" { "commit" } else { "blob" };
                        (mode.clone(), kind, sha.clone())
                    }
                    EditNode::Dir(d) => match d.write(git).await? {
                        Some(sha) if sha != EMPTY_TREE => ("040000".to_string(), "tree", sha),
                        _ => continue,
                    },
                };
                input.extend_from_slice(format!("{mode} {kind} {sha}\t{name}").as_bytes());
                input.push(0);
            }
            if input.is_empty() {
                return Ok(None);
            }
            let out = git.run(&["mktree", "-z"], &[], Some(&input)).await?;
            Ok(Some(trimmed(out)))
        })
    }
}

/// Escape regex metacharacters for `--author` / `--committer`.
fn regex_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if "\\.^$|?*+()[]{}".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn status_name(code: &str) -> &'static str {
    match code.as_bytes().first() {
        Some(b'A') => "added",
        Some(b'D') => "removed",
        Some(b'R') => "renamed",
        Some(b'C') => "copied",
        Some(b'T') => "changed",
        _ => "modified",
    }
}

/// Parse `git diff --raw --numstat -z` output.
fn parse_raw_numstat(raw: &[u8]) -> GitResult<Vec<DiffFile>> {
    let bad = || GitError::Object("malformed diff output".into());
    let tokens: Vec<String> = raw
        .split(|&b| b == 0)
        .map(|t| String::from_utf8_lossy(t).into_owned())
        .collect();
    let mut files: Vec<DiffFile> = Vec::new();
    let mut i = 0;
    let mut stat_idx = 0;
    while i < tokens.len() {
        let tok = &tokens[i];
        if tok.is_empty() {
            i += 1;
            continue;
        }
        if let Some(header) = tok.strip_prefix(':') {
            let f: Vec<&str> = header.split(' ').collect();
            if f.len() < 5 {
                return Err(bad());
            }
            let status = status_name(f[4]);
            let two_paths = matches!(f[4].as_bytes().first(), Some(b'R' | b'C'));
            let p1 = tokens.get(i + 1).ok_or_else(bad)?.clone();
            let (path, previous_path) = if two_paths {
                (tokens.get(i + 2).ok_or_else(bad)?.clone(), Some(p1))
            } else {
                (p1, None)
            };
            i += if two_paths { 3 } else { 2 };
            files.push(DiffFile {
                status: status.to_string(),
                path,
                previous_path,
                old_sha: f[2].to_string(),
                new_sha: f[3].to_string(),
                old_mode: f[0].to_string(),
                new_mode: f[1].to_string(),
                additions: 0,
                deletions: 0,
                binary: false,
                patch: None,
            });
            continue;
        }
        // numstat: "add\tdel\tpath" or "add\tdel\t" + old + new (renames).
        let mut parts = tok.splitn(3, '\t');
        let (Some(a), Some(d), Some(p)) = (parts.next(), parts.next(), parts.next()) else {
            return Err(bad());
        };
        i += if p.is_empty() { 3 } else { 1 };
        if let Some(f) = files.get_mut(stat_idx) {
            if a == "-" || d == "-" {
                f.binary = true;
            } else {
                f.additions = a.parse().unwrap_or(0);
                f.deletions = d.parse().unwrap_or(0);
            }
        }
        stat_idx += 1;
    }
    Ok(files)
}

/// Split a multi-file unified diff into per-file hunks (from the first
/// `@@` line). `None` for files without hunks (binary, mode-only, empty).
fn split_patches(text: &str) -> Vec<Option<String>> {
    let mut out = Vec::new();
    let mut current: Option<(bool, String)> = None;
    for line in text.split_inclusive('\n') {
        if line.starts_with("diff --git ") {
            if let Some((_, hunks)) = current.take() {
                out.push(finish_patch(hunks));
            }
            current = Some((false, String::new()));
            continue;
        }
        if let Some((in_hunks, buf)) = current.as_mut() {
            if !*in_hunks && line.starts_with("@@") {
                *in_hunks = true;
            }
            if *in_hunks {
                buf.push_str(line);
            }
        }
    }
    if let Some((_, hunks)) = current {
        out.push(finish_patch(hunks));
    }
    out
}

fn finish_patch(mut hunks: String) -> Option<String> {
    if hunks.is_empty() {
        return None;
    }
    if hunks.ends_with('\n') {
        hunks.pop();
    }
    Some(hunks)
}

/// Group a flat recursive listing by directory (helper for callers that
/// need per-directory views).
pub fn by_parent(entries: &[LsTreeEntry]) -> HashMap<String, Vec<&LsTreeEntry>> {
    let mut map: HashMap<String, Vec<&LsTreeEntry>> = HashMap::new();
    for e in entries {
        let parent = e.path.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
        map.entry(parent.to_string()).or_default().push(e);
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_raw_and_numstat() {
        let a = "a".repeat(40);
        let b = "b".repeat(40);
        let z = "0".repeat(40);
        let raw = format!(
            ":100644 100644 {a} {b} M\0src/x.rs\0:000000 100644 {z} {b} A\0new.txt\0\
             :100644 100644 {a} {a} R100\0old.md\0moved.md\0:100644 100644 {a} {b} M\0img.png\0\
             3\t1\tsrc/x.rs\0002\t0\tnew.txt\0000\t0\t\0old.md\0moved.md\0-\t-\timg.png\0"
        );
        let files = parse_raw_numstat(raw.as_bytes()).unwrap();
        assert_eq!(files.len(), 4);
        assert_eq!(files[0].status, "modified");
        assert_eq!((files[0].additions, files[0].deletions), (3, 1));
        assert_eq!(files[1].status, "added");
        assert_eq!(files[2].status, "renamed");
        assert_eq!(files[2].previous_path.as_deref(), Some("old.md"));
        assert_eq!(files[2].path, "moved.md");
        assert!(files[3].binary);
    }

    #[test]
    fn splits_patches() {
        let text = "diff --git a/x b/x\nindex 1..2 100644\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n\
diff --git a/bin b/bin\nBinary files differ\n";
        let p = split_patches(text);
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].as_deref(), Some("@@ -1 +1 @@\n-a\n+b"));
        assert_eq!(p[1], None);
    }

    #[test]
    fn escapes_regex() {
        assert_eq!(regex_escape("a.b+c@x.com"), "a\\.b\\+c@x\\.com");
    }
}
