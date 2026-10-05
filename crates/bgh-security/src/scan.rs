//! Scanning git objects: which blobs a set of commits introduces (with the
//! commit and path of each introduction), read in bounded batches through
//! one `git cat-file --batch` per batch, matched with an [`Engine`].
//!
//! Used for push protection (quarantined objects, `envs` from
//! `QuarantineEnv`), incremental scans after a push and history backfills.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use bgh_git::{ChangedFile, GitCli, GitResult};

use crate::patterns::{Engine, Finding};

/// Bytes of blob content read per `cat-file --batch` call.
const BATCH_BYTES: u64 = 32 << 20;

/// Commits per `diff-tree --stdin` call during history scans.
const COMMIT_CHUNK: usize = 2000;

/// A secret at one place a blob was introduced.
#[derive(Debug, Clone)]
pub struct Hit {
    pub file: ChangedFile,
    pub finding: Finding,
}

#[derive(Debug, Default)]
pub struct Outcome {
    pub hits: Vec<Hit>,
    pub blobs_scanned: i64,
    pub bytes_scanned: i64,
    /// The deadline passed before every blob was scanned.
    pub timed_out: bool,
}

/// Commits listed by `git rev-list <args>` (oldest last).
pub async fn rev_list(
    cli: &GitCli,
    args: &[&str],
    envs: &[(&str, &str)],
) -> GitResult<Vec<String>> {
    let mut full = vec!["rev-list"];
    full.extend_from_slice(args);
    let out = cli.run(&full, envs, None).await?;
    Ok(String::from_utf8_lossy(&out)
        .lines()
        .filter(|l| bgh_git::is_sha(l))
        .map(str::to_string)
        .collect())
}

/// Scan the blobs `commits` add or modify. Blobs larger than `max_blob`
/// bytes are skipped; each distinct blob is read and matched once.
pub async fn scan_commits(
    cli: &GitCli,
    commits: &[String],
    envs: &[(&str, &str)],
    engine: Arc<Engine>,
    max_blob: u64,
    deadline: Option<Instant>,
) -> GitResult<Outcome> {
    let mut out = Outcome::default();
    if engine.is_empty() {
        return Ok(out);
    }
    for chunk in commits.chunks(COMMIT_CHUNK) {
        let files = cli.changed_files(chunk, envs).await?;
        let part = scan_files(cli, files, envs, engine.clone(), max_blob, deadline).await?;
        out.hits.extend(part.hits);
        out.blobs_scanned += part.blobs_scanned;
        out.bytes_scanned += part.bytes_scanned;
        if part.timed_out {
            out.timed_out = true;
            break;
        }
    }
    Ok(out)
}

/// Scan the blobs of `files` (introductions from [`GitCli::changed_files`]).
pub async fn scan_files(
    cli: &GitCli,
    files: Vec<ChangedFile>,
    envs: &[(&str, &str)],
    engine: Arc<Engine>,
    max_blob: u64,
    deadline: Option<Instant>,
) -> GitResult<Outcome> {
    let mut out = Outcome::default();
    // blob -> introductions
    let mut by_blob: HashMap<String, Vec<ChangedFile>> = HashMap::new();
    let mut order: Vec<(String, u64)> = Vec::new();
    for f in files {
        if f.size == 0 || f.size > max_blob {
            continue;
        }
        let e = by_blob.entry(f.blob.clone()).or_default();
        if e.is_empty() {
            order.push((f.blob.clone(), f.size));
        }
        e.push(f);
    }
    let mut batch: Vec<String> = Vec::new();
    let mut batch_bytes = 0u64;
    let mut i = 0;
    while i <= order.len() {
        let flush = i == order.len() || batch_bytes + order[i].1 > BATCH_BYTES;
        if flush && !batch.is_empty() {
            if deadline.is_some_and(|d| Instant::now() >= d) {
                out.timed_out = true;
                return Ok(out);
            }
            let found = scan_batch(cli, &batch, envs, engine.clone()).await?;
            out.blobs_scanned += found.blobs;
            out.bytes_scanned += found.bytes;
            for (blob, findings) in found.findings {
                let Some(intros) = by_blob.get(&blob) else {
                    continue;
                };
                for finding in findings {
                    for file in intros {
                        out.hits.push(Hit {
                            file: file.clone(),
                            finding: finding.clone(),
                        });
                    }
                }
            }
            batch.clear();
            batch_bytes = 0;
        }
        if i < order.len() {
            batch.push(order[i].0.clone());
            batch_bytes += order[i].1;
        }
        i += 1;
    }
    Ok(out)
}

struct BatchResult {
    findings: Vec<(String, Vec<Finding>)>,
    blobs: i64,
    bytes: i64,
}

/// Read `blobs` with one `cat-file --batch` and match them (on the
/// blocking pool: matching is CPU bound).
async fn scan_batch(
    cli: &GitCli,
    blobs: &[String],
    envs: &[(&str, &str)],
    engine: Arc<Engine>,
) -> GitResult<BatchResult> {
    let mut input = Vec::with_capacity(blobs.len() * 41);
    for b in blobs {
        input.extend_from_slice(b.as_bytes());
        input.push(b'\n');
    }
    let raw = cli
        .run(&["cat-file", "--batch"], envs, Some(&input))
        .await?;
    let res = tokio::task::spawn_blocking(move || {
        let mut findings = Vec::new();
        let (mut blobs, mut bytes) = (0i64, 0i64);
        for (sha, kind, data) in parse_batch(&raw) {
            if kind != b"blob" {
                continue;
            }
            blobs += 1;
            bytes += data.len() as i64;
            let f = engine.scan(data);
            if !f.is_empty() {
                findings.push((sha.to_string(), f));
            }
        }
        BatchResult {
            findings,
            blobs,
            bytes,
        }
    })
    .await
    .map_err(|e| bgh_git::GitError::Object(e.to_string()))?;
    Ok(res)
}

/// `(sha, type, content)` records of `cat-file --batch` output (missing
/// objects skipped).
fn parse_batch(out: &[u8]) -> Vec<(&str, &[u8], &[u8])> {
    let mut res = Vec::new();
    let mut i = 0;
    while i < out.len() {
        let Some(nl) = out[i..].iter().position(|&b| b == b'\n').map(|n| i + n) else {
            break;
        };
        let header = &out[i..nl];
        i = nl + 1;
        let mut parts = header.split(|b| *b == b' ');
        let (Some(sha), Some(kind), Some(size)) = (parts.next(), parts.next(), parts.next()) else {
            continue; // "<sha> missing"
        };
        let Some(size) = std::str::from_utf8(size)
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
        else {
            break;
        };
        if i + size > out.len() {
            break;
        }
        if let Ok(sha) = std::str::from_utf8(sha) {
            res.push((sha, kind, &out[i..i + size]));
        }
        i += size + 1;
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_batch_output() {
        let out = b"aaa blob 3\nabc\nbbb missing\nccc blob 0\n\n";
        let v = parse_batch(out);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0], ("aaa", &b"blob"[..], &b"abc"[..]));
        assert_eq!(v[1].2, b"");
    }
}
