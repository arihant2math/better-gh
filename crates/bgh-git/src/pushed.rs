//! What a push adds to a repository: the new commits of one ref update and
//! the files they change, read with the quarantined objects of a push
//! (see `smart_http::QuarantineEnv`). Used to evaluate ruleset push and
//! metadata rules before the refs move.

use std::collections::HashMap;

use crate::{GitCli, GitResult, is_sha};

/// A commit a push introduces.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PushedCommit {
    pub sha: String,
    pub parents: usize,
    pub author_email: String,
    pub committer_email: String,
    pub message: String,
}

/// A file added or modified by a [`PushedCommit`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChangedFile {
    pub commit: String,
    pub path: String,
    pub blob: String,
    pub size: u64,
}

impl GitCli {
    /// Commits reachable from `new` that no existing ref reaches, newest
    /// first. `envs` exposes the quarantined objects.
    pub async fn new_commits(
        &self,
        new: &str,
        envs: &[(&str, &str)],
    ) -> GitResult<Vec<PushedCommit>> {
        if !is_sha(new) {
            return Ok(vec![]);
        }
        let out = self
            .run(
                &[
                    "log",
                    "-z",
                    "--format=%H%x1f%P%x1f%ae%x1f%ce%x1f%B",
                    new,
                    "--not",
                    "--all",
                ],
                envs,
                None,
            )
            .await?;
        let text = String::from_utf8_lossy(&out);
        Ok(text
            .split('\0')
            .filter(|r| !r.trim().is_empty())
            .filter_map(|r| {
                let mut f = r.trim_start_matches('\n').splitn(5, '\x1f');
                let sha = f.next()?.to_string();
                let parents = f.next()?.split_whitespace().count();
                Some(PushedCommit {
                    sha,
                    parents,
                    author_email: f.next()?.to_string(),
                    committer_email: f.next()?.to_string(),
                    message: f.next().unwrap_or_default().trim_end().to_string(),
                })
            })
            .collect())
    }

    /// Files added or modified by `commits` (merge commits contribute
    /// nothing of their own), with blob sizes.
    pub async fn changed_files(
        &self,
        commits: &[String],
        envs: &[(&str, &str)],
    ) -> GitResult<Vec<ChangedFile>> {
        if commits.is_empty() {
            return Ok(vec![]);
        }
        let mut input = commits.join("\n");
        input.push('\n');
        let out = self
            .run(
                &["diff-tree", "--stdin", "-r", "--root", "--no-renames", "-z"],
                envs,
                Some(input.as_bytes()),
            )
            .await?;
        let mut files = Vec::new();
        let mut commit = String::new();
        let mut it = out
            .split(|b| *b == 0)
            .map(|t| String::from_utf8_lossy(t).into_owned());
        while let Some(tok) = it.next() {
            let tok = tok.trim_matches('\n');
            if let Some(meta) = tok.strip_prefix(':') {
                let path = it.next().unwrap_or_default();
                // ":old_mode new_mode old_sha new_sha status"
                let f: Vec<&str> = meta.split(' ').collect();
                if f.len() < 5 || f[4].starts_with('D') || f[1] == "160000" {
                    continue;
                }
                files.push(ChangedFile {
                    commit: commit.clone(),
                    path,
                    blob: f[3].to_string(),
                    size: 0,
                });
            } else if is_sha(tok) {
                commit = tok.to_string();
            }
        }
        if files.is_empty() {
            return Ok(files);
        }
        let mut blobs: Vec<&str> = files.iter().map(|f| f.blob.as_str()).collect();
        blobs.sort_unstable();
        blobs.dedup();
        let mut input = blobs.join("\n");
        input.push('\n');
        let out = self
            .run(
                &["cat-file", "--batch-check=%(objectname) %(objectsize)"],
                envs,
                Some(input.as_bytes()),
            )
            .await?;
        let sizes: HashMap<String, u64> = String::from_utf8_lossy(&out)
            .lines()
            .filter_map(|l| {
                let (sha, size) = l.split_once(' ')?;
                Some((sha.to_string(), size.trim().parse().ok()?))
            })
            .collect();
        for f in &mut files {
            f.size = sizes.get(&f.blob).copied().unwrap_or(0);
        }
        Ok(files)
    }
}
