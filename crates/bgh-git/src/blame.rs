//! Blame via `git blame --incremental`, parsed as it streams.
//!
//! Incremental output reports ranges in the order git attributes them
//! (not by line number), so callers can render progressively; [`blame`]
//! collects everything into a [`Blame`] sorted by line.

use std::collections::{BTreeMap, HashSet};
use std::process::Stdio;

use futures::Stream;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

use crate::storage::RepoStore;
use crate::{GitError, GitResult, cmd, is_sha};

/// Commit metadata referenced by blame ranges.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlameCommit {
    pub sha: String,
    pub author_name: String,
    pub author_email: String,
    /// Unix seconds.
    pub author_time: i64,
    pub author_tz: String,
    pub committer_name: String,
    pub committer_email: String,
    pub committer_time: i64,
    pub summary: String,
    /// Parent commit and path the lines came from (for "blame prior").
    pub previous: Option<BlamePrevious>,
    /// True for the boundary (root) commit.
    pub boundary: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlamePrevious {
    pub sha: String,
    pub path: String,
}

/// `count` lines starting at `line` (1-based, in the blamed file) come from
/// `sha`, where they started at `orig_line` in `orig_path`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlameRange {
    pub sha: String,
    pub line: u32,
    pub count: u32,
    pub orig_line: u32,
    pub orig_path: String,
}

/// One parsed incremental chunk: a range, plus the commit's metadata the
/// first time the commit appears.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlameChunk {
    pub range: BlameRange,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<BlameCommit>,
}

/// Complete blame of a file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Blame {
    /// Sorted by `line`.
    pub ranges: Vec<BlameRange>,
    pub commits: BTreeMap<String, BlameCommit>,
}

impl Blame {
    fn push(&mut self, chunk: BlameChunk) {
        if let Some(c) = chunk.commit {
            self.commits.insert(c.sha.clone(), c);
        }
        self.ranges.push(chunk.range);
    }

    fn finish(mut self) -> Self {
        self.ranges.sort_by_key(|r| r.line);
        self
    }
}

/// Incremental-format line parser.
#[derive(Default)]
pub struct Parser {
    range: Option<BlameRange>,
    commit: Option<BlameCommit>,
    seen: HashSet<String>,
}

impl Parser {
    /// Feed one line (without the newline). Returns a chunk when an entry
    /// is complete (at its `filename` line).
    pub fn line(&mut self, line: &str) -> GitResult<Option<BlameChunk>> {
        let bad = || GitError::Object(format!("unexpected blame output: {line:?}"));
        if self.range.is_none() {
            let mut it = line.split(' ');
            let (Some(sha), Some(orig), Some(fin), Some(count)) =
                (it.next(), it.next(), it.next(), it.next())
            else {
                return Err(bad());
            };
            if !is_sha(sha) {
                return Err(bad());
            }
            let num = |s: &str| s.parse::<u32>().map_err(|_| bad());
            self.range = Some(BlameRange {
                sha: sha.to_string(),
                orig_line: num(orig)?,
                line: num(fin)?,
                count: num(count)?,
                orig_path: String::new(),
            });
            if self.seen.insert(sha.to_string()) {
                self.commit = Some(BlameCommit {
                    sha: sha.to_string(),
                    ..Default::default()
                });
            }
            return Ok(None);
        }
        let (key, value) = line.split_once(' ').unwrap_or((line, ""));
        if key == "filename" {
            let mut range = self.range.take().expect("checked above");
            range.orig_path = value.to_string();
            return Ok(Some(BlameChunk {
                range,
                commit: self.commit.take(),
            }));
        }
        let Some(c) = self.commit.as_mut() else {
            return Ok(None); // details of an already reported commit
        };
        let mail = |v: &str| v.trim_start_matches('<').trim_end_matches('>').to_string();
        match key {
            "author" => c.author_name = value.to_string(),
            "author-mail" => c.author_email = mail(value),
            "author-time" => c.author_time = value.parse().unwrap_or(0),
            "author-tz" => c.author_tz = value.to_string(),
            "committer" => c.committer_name = value.to_string(),
            "committer-mail" => c.committer_email = mail(value),
            "committer-time" => c.committer_time = value.parse().unwrap_or(0),
            "summary" => c.summary = value.to_string(),
            "boundary" => c.boundary = true,
            "previous" => {
                if let Some((sha, path)) = value.split_once(' ') {
                    c.previous = Some(BlamePrevious {
                        sha: sha.to_string(),
                        path: path.to_string(),
                    });
                }
            }
            _ => {}
        }
        Ok(None)
    }
}

fn validate(commit: &str, path: &str) -> GitResult<()> {
    if !is_sha(commit) {
        return Err(GitError::InvalidInput(format!(
            "not a commit sha: {commit}"
        )));
    }
    if path.is_empty() || path.contains('\0') {
        return Err(GitError::InvalidInput("invalid path".into()));
    }
    Ok(())
}

/// Stream blame chunks for `path` at `commit` (a full SHA) as git computes
/// them.
pub async fn blame_stream(
    store: &RepoStore,
    repo_id: i64,
    commit: &str,
    path: &str,
) -> GitResult<impl Stream<Item = GitResult<BlameChunk>> + Send + 'static> {
    validate(commit, path)?;
    let dir = store.git_dir(repo_id)?;
    let mut c = cmd::git(&store.git_bin, Some(&dir));
    c.args(["blame", "--incremental", commit, "--", path])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = c.spawn()?;
    let stdout = BufReader::new(child.stdout.take().expect("piped stdout"));
    let mut stderr = child.stderr.take().expect("piped stderr");
    let err_task = tokio::spawn(async move {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s).await;
        s
    });
    struct St {
        lines: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
        child: tokio::process::Child,
        err_task: Option<tokio::task::JoinHandle<String>>,
        parser: Parser,
        done: bool,
    }
    let st = St {
        lines: stdout.lines(),
        child,
        err_task: Some(err_task),
        parser: Parser::default(),
        done: false,
    };
    Ok(futures::stream::unfold(st, |mut st| async move {
        if st.done {
            return None;
        }
        loop {
            match st.lines.next_line().await {
                Ok(Some(line)) => match st.parser.line(&line) {
                    Ok(Some(chunk)) => return Some((Ok(chunk), st)),
                    Ok(None) => continue,
                    Err(e) => {
                        st.done = true;
                        return Some((Err(e), st));
                    }
                },
                Ok(None) => {
                    st.done = true;
                    let status = st.child.wait().await;
                    let err = match st.err_task.take() {
                        Some(t) => t.await.unwrap_or_default(),
                        None => String::new(),
                    };
                    return match status {
                        Ok(s) if s.success() => None,
                        _ if err.contains("no such path") || err.contains("no such ref") => {
                            Some((Err(GitError::NotFound(err.trim().to_string())), st))
                        }
                        _ => Some((
                            Err(GitError::Command {
                                args: "blame --incremental".into(),
                                status: format!("{status:?}"),
                                stderr: err.trim().to_string(),
                            }),
                            st,
                        )),
                    };
                }
                Err(e) => {
                    st.done = true;
                    return Some((Err(e.into()), st));
                }
            }
        }
    }))
}

/// Full blame of `path` at `commit`, ranges sorted by line.
pub async fn blame(store: &RepoStore, repo_id: i64, commit: &str, path: &str) -> GitResult<Blame> {
    use futures::StreamExt;
    let stream = blame_stream(store, repo_id, commit, path).await?;
    futures::pin_mut!(stream);
    let mut out = Blame::default();
    while let Some(chunk) = stream.next().await {
        out.push(chunk?);
    }
    Ok(out.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_incremental_output() {
        let a = "a".repeat(40);
        let b = "b".repeat(40);
        let out = format!(
            "{a} 1 1 2\nauthor Alice\nauthor-mail <alice@example.com>\nauthor-time 1700000000\n\
author-tz +0100\ncommitter Alice\ncommitter-mail <alice@example.com>\ncommitter-time 1700000001\n\
committer-tz +0100\nsummary First\nboundary\nfilename f.txt\n\
{b} 3 3 1\nauthor Bob\nauthor-mail <bob@example.com>\nauthor-time 1700000100\nauthor-tz +0000\n\
committer Bob\ncommitter-mail <bob@example.com>\ncommitter-time 1700000100\ncommitter-tz +0000\n\
summary Second\nprevious {a} f.txt\nfilename f.txt\n\
{a} 3 4 1\nfilename f.txt\n"
        );
        let mut p = Parser::default();
        let mut blame = Blame::default();
        for line in out.lines() {
            if let Some(c) = p.line(line).unwrap() {
                blame.push(c);
            }
        }
        let blame = blame.finish();
        assert_eq!(blame.ranges.len(), 3);
        assert_eq!(blame.ranges[2].line, 4);
        assert_eq!(blame.ranges[2].sha, a);
        assert_eq!(blame.commits.len(), 2);
        let ca = &blame.commits[&a];
        assert_eq!(ca.author_email, "alice@example.com");
        assert!(ca.boundary);
        assert_eq!(blame.commits[&b].previous.as_ref().unwrap().sha, a);
        assert!(p.line("garbage").is_err());
    }
}
