//! Diffs between two commits, shaped for GitHub's pull request / compare
//! "files" APIs, and helpers for mapping review comment positions.
//!
//! [`diff_files`] runs `git diff --raw --numstat -z` for the file list
//! (robust against any path bytes) and `git diff -p` for the patches, then
//! pairs them up by order. Per-file patches are the hunks only (starting at
//! the first `@@`), exactly like GitHub's `patch` field.
//!
//! [`stream_diff`] / [`stream_patch`] stream `git diff` / `git format-patch`
//! output for the `.diff` / `.patch` media types without buffering.

use std::path::Path;

use bytes::Bytes;
use futures::Stream;
use serde::{Deserialize, Serialize};
use tokio_util::io::ReaderStream;

use crate::storage::RepoStore;
use crate::{GitError, GitResult, cmd, is_sha};

/// GitHub file statuses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileStatus {
    Added,
    Removed,
    Modified,
    Renamed,
    Copied,
    Changed,
    Unchanged,
}

impl FileStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Removed => "removed",
            Self::Modified => "modified",
            Self::Renamed => "renamed",
            Self::Copied => "copied",
            Self::Changed => "changed",
            Self::Unchanged => "unchanged",
        }
    }
}

/// One changed file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileDiff {
    pub status: FileStatus,
    pub filename: String,
    pub previous_filename: Option<String>,
    /// Blob SHA before (`None` when added).
    pub old_sha: Option<String>,
    /// Blob SHA after (`None` when removed).
    pub new_sha: Option<String>,
    pub old_mode: String,
    pub new_mode: String,
    pub additions: u64,
    pub deletions: u64,
    pub binary: bool,
    /// Unified diff hunks (from the first `@@`); `None` for binary files,
    /// pure renames/mode changes, or patches over the size limit.
    pub patch: Option<String>,
    /// The patch was dropped because it exceeded the limit.
    pub patch_truncated: bool,
}

impl FileDiff {
    pub fn changes(&self) -> u64 {
        self.additions + self.deletions
    }
}

/// Limits for [`diff_files`].
#[derive(Debug, Clone, Copy)]
pub struct DiffLimits {
    /// Maximum number of files returned (GitHub: 3000).
    pub max_files: usize,
    /// Patches larger than this many bytes are omitted.
    pub max_patch_bytes: usize,
}

impl Default for DiffLimits {
    fn default() -> Self {
        Self {
            max_files: 3000,
            max_patch_bytes: 1024 * 1024,
        }
    }
}

/// Result of [`diff_files`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffResult {
    pub files: Vec<FileDiff>,
    /// Total number of changed files (may exceed `files.len()`).
    pub total_files: usize,
    pub additions: u64,
    pub deletions: u64,
}

fn check_sha(s: &str) -> GitResult<()> {
    if is_sha(s) {
        Ok(())
    } else {
        Err(GitError::InvalidInput(format!("invalid object id {s:?}")))
    }
}

struct RawEntry {
    old_mode: String,
    new_mode: String,
    old_sha: String,
    new_sha: String,
    status: char,
    path: String,
    previous: Option<String>,
    additions: u64,
    deletions: u64,
    binary: bool,
}

/// Parse `git diff --raw --numstat -z` output. With both flags git prints
/// all raw records first, then all numstat records.
fn parse_raw_numstat(out: &[u8]) -> GitResult<Vec<RawEntry>> {
    let mut fields = out.split(|&b| b == 0).peekable();
    let mut entries: Vec<RawEntry> = Vec::new();
    let bad = |m: &str| GitError::Object(format!("unexpected diff output: {m}"));
    // Raw records: ":old new osha nsha STATUS\0path\0[path2\0]"
    while let Some(f) = fields.peek() {
        if !f.starts_with(b":") {
            break;
        }
        let header = String::from_utf8_lossy(fields.next().unwrap()).to_string();
        let parts: Vec<&str> = header[1..].split(' ').collect();
        if parts.len() < 5 {
            return Err(bad("raw header"));
        }
        let status = parts[4].chars().next().ok_or_else(|| bad("status"))?;
        let p1 = String::from_utf8_lossy(fields.next().ok_or_else(|| bad("path"))?).to_string();
        let (path, previous) = if matches!(status, 'R' | 'C') {
            let p2 =
                String::from_utf8_lossy(fields.next().ok_or_else(|| bad("path2"))?).to_string();
            (p2, Some(p1))
        } else {
            (p1, None)
        };
        entries.push(RawEntry {
            old_mode: parts[0].to_string(),
            new_mode: parts[1].to_string(),
            old_sha: parts[2].to_string(),
            new_sha: parts[3].to_string(),
            status,
            path,
            previous,
            additions: 0,
            deletions: 0,
            binary: false,
        });
    }
    // Numstat records: "add\tdel\tpath\0" or, for renames, "add\tdel\t\0old\0new\0".
    let mut i = 0;
    while let Some(f) = fields.next() {
        if f.is_empty() {
            continue;
        }
        let s = String::from_utf8_lossy(f);
        let mut it = s.splitn(3, '\t');
        let a = it.next().unwrap_or("");
        let d = it.next().unwrap_or("");
        let rest = it.next().unwrap_or("");
        if rest.is_empty() {
            // rename form: two more fields
            fields.next();
            fields.next();
        }
        if let Some(e) = entries.get_mut(i) {
            if a == "-" && d == "-" {
                e.binary = true;
            } else {
                e.additions = a.parse().unwrap_or(0);
                e.deletions = d.parse().unwrap_or(0);
            }
        }
        i += 1;
    }
    Ok(entries)
}

/// Split `git diff -p` output into per-file chunks (in output order).
fn split_patches(out: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut pos = 0;
    while pos < out.len() {
        if out[pos..].starts_with(b"diff --git ") {
            starts.push(pos);
        }
        match out[pos..].iter().position(|&b| b == b'\n') {
            Some(n) => pos += n + 1,
            None => break,
        }
    }
    let mut chunks = Vec::with_capacity(starts.len());
    for (k, &s) in starts.iter().enumerate() {
        let e = starts.get(k + 1).copied().unwrap_or(out.len());
        chunks.push(&out[s..e]);
    }
    chunks
}

/// Hunks of one file chunk (from the first `@@` line), without the
/// trailing newline (GitHub's `patch` has none).
fn hunks_of(chunk: &[u8]) -> Option<String> {
    let mut pos = 0;
    while pos < chunk.len() {
        if chunk[pos..].starts_with(b"@@") {
            let mut s = String::from_utf8_lossy(&chunk[pos..]).into_owned();
            if s.ends_with('\n') {
                s.pop();
            }
            return Some(s);
        }
        if chunk[pos..].starts_with(b"Binary files ") || chunk[pos..].starts_with(b"GIT binary") {
            return None;
        }
        match chunk[pos..].iter().position(|&b| b == b'\n') {
            Some(n) => pos += n + 1,
            None => break,
        }
    }
    None
}

const DIFF_FLAGS: &[&str] = &[
    "-c",
    "diff.noprefix=false",
    "-c",
    "core.quotePath=false",
    "diff",
    "--no-color",
    "--no-ext-diff",
    "--no-textconv",
    "-M",
    "--diff-algorithm=myers",
];

/// Files changed between two commits (`base` → `head`, both full SHAs).
/// For a pull request pass the merge base as `base` (three-dot diff).
pub async fn diff_files(
    store: &RepoStore,
    repo_id: i64,
    base: &str,
    head: &str,
    limits: DiffLimits,
) -> GitResult<DiffResult> {
    check_sha(base)?;
    check_sha(head)?;
    let dir = store.git_dir(repo_id)?;
    let bin = store.git_bin.as_str();

    let mut args: Vec<&str> = DIFF_FLAGS.to_vec();
    args.extend(["--raw", "--numstat", "-z", "--abbrev=40", base, head]);
    let raw = cmd::run(bin, Some(&dir), &args, &[], None).await?;
    let entries = parse_raw_numstat(&raw)?;
    let total_files = entries.len();
    let additions = entries.iter().map(|e| e.additions).sum();
    let deletions = entries.iter().map(|e| e.deletions).sum();

    let mut args: Vec<&str> = DIFF_FLAGS.to_vec();
    args.extend(["-p", "--full-index", base, head]);
    let patch_out = cmd::run(bin, Some(&dir), &args, &[], None).await?;
    let chunks = split_patches(&patch_out);

    let zero = |s: &str| s.bytes().all(|b| b == b'0');
    let mut files = Vec::with_capacity(entries.len().min(limits.max_files));
    for (i, e) in entries.into_iter().enumerate() {
        if files.len() >= limits.max_files {
            break;
        }
        let status = match e.status {
            'A' => FileStatus::Added,
            'D' => FileStatus::Removed,
            'R' => FileStatus::Renamed,
            'C' => FileStatus::Copied,
            'T' => FileStatus::Changed,
            'M' => FileStatus::Modified,
            _ => FileStatus::Changed,
        };
        let mut patch = if e.binary {
            None
        } else {
            chunks.get(i).and_then(|c| hunks_of(c))
        };
        let mut truncated = false;
        if patch
            .as_ref()
            .is_some_and(|p| p.len() > limits.max_patch_bytes)
        {
            patch = None;
            truncated = true;
        }
        files.push(FileDiff {
            status,
            filename: e.path,
            previous_filename: e.previous,
            old_sha: (!zero(&e.old_sha)).then_some(e.old_sha),
            new_sha: (!zero(&e.new_sha)).then_some(e.new_sha),
            old_mode: e.old_mode,
            new_mode: e.new_mode,
            additions: e.additions,
            deletions: e.deletions,
            binary: e.binary,
            patch,
            patch_truncated: truncated,
        });
    }
    Ok(DiffResult {
        files,
        total_files,
        additions,
        deletions,
    })
}

fn child_stream(
    mut child: tokio::process::Child,
) -> impl Stream<Item = std::io::Result<Bytes>> + Send + 'static {
    let stdout = child.stdout.take().expect("piped stdout");
    let stream = ReaderStream::new(stdout);
    // Keep the child alive (and reaped) while the body streams. Response
    // bodies may be polled again after the end (e.g. by the compression
    // layer); `Unfold` panics on that, so the stream is fused.
    futures::StreamExt::fuse(futures::stream::unfold(
        (stream, Some(child)),
        |(mut stream, mut child)| async move {
            use futures::StreamExt;
            match stream.next().await {
                Some(item) => Some((item, (stream, child))),
                None => {
                    if let Some(mut c) = child.take() {
                        let _ = c.wait().await;
                    }
                    None
                }
            }
        },
    ))
}

fn spawn(dir: &Path, bin: &str, args: &[&str]) -> GitResult<tokio::process::Child> {
    cmd::spawn_stdout(bin, dir, args)
}

/// Stream the unified diff `base..head` (the `.diff` media type).
pub fn stream_diff(
    store: &RepoStore,
    repo_id: i64,
    base: &str,
    head: &str,
) -> GitResult<impl Stream<Item = std::io::Result<Bytes>> + Send + 'static> {
    check_sha(base)?;
    check_sha(head)?;
    let dir = store.git_dir(repo_id)?;
    let mut args: Vec<&str> = DIFF_FLAGS.to_vec();
    args.extend(["-p", "--full-index", base, head]);
    Ok(child_stream(spawn(&dir, &store.git_bin, &args)?))
}

/// Stream `git format-patch` output for the commits in `base..head`
/// (the `.patch` media type).
pub fn stream_patch(
    store: &RepoStore,
    repo_id: i64,
    base: &str,
    head: &str,
) -> GitResult<impl Stream<Item = std::io::Result<Bytes>> + Send + 'static> {
    check_sha(base)?;
    check_sha(head)?;
    let dir = store.git_dir(repo_id)?;
    let range = format!("{base}..{head}");
    let args = [
        "-c",
        "core.quotePath=false",
        "format-patch",
        "--stdout",
        "--no-color",
        "--no-signature",
        "-M",
        range.as_str(),
    ];
    Ok(child_stream(spawn(&dir, &store.git_bin, &args)?))
}

// ---------------------------------------------------------------------------
// Patch line model (review comment positions)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Hunk,
    Context,
    Add,
    Delete,
    /// `\ No newline at end of file`
    NoNewline,
}

/// A line of a file patch with its diff position (GitHub's `position`:
/// the first `@@` header is 0, every following line increments it,
/// including later hunk headers).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchLine {
    pub kind: LineKind,
    pub position: u32,
    /// Line number in the old file (context and deletions).
    pub old: Option<u32>,
    /// Line number in the new file (context and additions).
    pub new: Option<u32>,
    /// Index of the hunk header line this line belongs to (in `lines`).
    pub hunk_start: usize,
}

fn parse_hunk_header(line: &str) -> Option<(u32, u32)> {
    // @@ -a,b +c,d @@ ...
    let rest = line.strip_prefix("@@ -")?;
    let (old, rest) = rest.split_once(' ')?;
    let new = rest.strip_prefix('+')?.split(' ').next()?;
    let start = |s: &str| s.split(',').next()?.parse::<u32>().ok();
    Some((start(old)?, start(new)?))
}

/// Parse a file patch (hunks only) into positioned lines.
pub fn parse_patch(patch: &str) -> Vec<PatchLine> {
    let mut out = Vec::new();
    let (mut old, mut new) = (0u32, 0u32);
    let mut hunk_start = 0usize;
    let mut position = 0u32;
    let mut first = true;
    for line in patch.split('\n') {
        if line.starts_with("@@") {
            if !first {
                position += 1;
            }
            first = false;
            if let Some((o, n)) = parse_hunk_header(line) {
                old = o;
                new = n;
            }
            hunk_start = out.len();
            out.push(PatchLine {
                kind: LineKind::Hunk,
                position,
                old: None,
                new: None,
                hunk_start,
            });
            continue;
        }
        if first {
            continue;
        }
        position += 1;
        let (kind, o, n) = match line.as_bytes().first() {
            Some(b'+') => {
                new += 1;
                (LineKind::Add, None, Some(new - 1))
            }
            Some(b'-') => {
                old += 1;
                (LineKind::Delete, Some(old - 1), None)
            }
            Some(b'\\') => (LineKind::NoNewline, None, None),
            _ => {
                old += 1;
                new += 1;
                (LineKind::Context, Some(old - 1), Some(new - 1))
            }
        };
        out.push(PatchLine {
            kind,
            position,
            old: o,
            new: n,
            hunk_start,
        });
    }
    out
}

/// Which side of the diff a comment applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

/// Find the patch line for `line` on `side` (RIGHT = new file line numbers,
/// LEFT = old file line numbers). Context lines match either side.
pub fn find_line(lines: &[PatchLine], side: Side, line: u32) -> Option<&PatchLine> {
    lines.iter().find(|l| match side {
        Side::Right => l.new == Some(line) && l.kind != LineKind::Delete,
        Side::Left => l.old == Some(line) && l.kind != LineKind::Add,
    })
}

/// Find the patch line at a diff `position`.
pub fn find_position(lines: &[PatchLine], position: u32) -> Option<&PatchLine> {
    lines
        .iter()
        .find(|l| l.position == position && l.kind != LineKind::Hunk)
}

/// GitHub's `diff_hunk` for a comment on `target`: the enclosing hunk from
/// its `@@` header through the target line.
pub fn diff_hunk(patch: &str, lines: &[PatchLine], target: &PatchLine) -> String {
    let text: Vec<&str> = patch.split('\n').collect();
    // `lines` and `text` align once the leading non-hunk lines are skipped.
    let skip = text.iter().position(|l| l.starts_with("@@")).unwrap_or(0);
    let idx = lines
        .iter()
        .position(|l| std::ptr::eq(l, target))
        .unwrap_or(target.hunk_start);
    text[skip + target.hunk_start..=(skip + idx).min(text.len() - 1)].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const PATCH: &str = "@@ -1,3 +1,4 @@\n a\n-b\n+B\n+B2\n c\n@@ -10,2 +11,2 @@ fn x\n x\n-y\n+Y";

    #[test]
    fn positions() {
        let lines = parse_patch(PATCH);
        assert_eq!(lines[0].position, 0);
        let b2 = find_line(&lines, Side::Right, 3).unwrap();
        assert_eq!(b2.position, 4);
        assert_eq!(b2.kind, LineKind::Add);
        let del = find_line(&lines, Side::Left, 2).unwrap();
        assert_eq!(del.position, 2);
        // second hunk header counts as a line
        let y = find_line(&lines, Side::Right, 12).unwrap();
        assert_eq!(y.position, 9);
        assert_eq!(find_position(&lines, 9).unwrap().new, Some(12));
        assert_eq!(
            diff_hunk(PATCH, &lines, b2),
            "@@ -1,3 +1,4 @@\n a\n-b\n+B\n+B2"
        );
        assert_eq!(
            diff_hunk(PATCH, &lines, y),
            "@@ -10,2 +11,2 @@ fn x\n x\n-y\n+Y"
        );
        assert!(find_line(&lines, Side::Right, 7).is_none());
    }

    #[test]
    fn raw_numstat() {
        let out = b":100644 100644 aaaa bbbb M\0f.txt\0:000000 100644 0000 cccc A\0new.txt\0:100644 100644 dddd eeee R090\0old.txt\0moved.txt\x002\t1\tf.txt\x003\t0\tnew.txt\x001\t1\t\0old.txt\0moved.txt\0";
        let e = parse_raw_numstat(out).unwrap();
        assert_eq!(e.len(), 3);
        assert_eq!(e[0].additions, 2);
        assert_eq!(e[1].status, 'A');
        assert_eq!(e[2].path, "moved.txt");
        assert_eq!(e[2].previous.as_deref(), Some("old.txt"));
        assert_eq!(e[2].deletions, 1);
    }
}
