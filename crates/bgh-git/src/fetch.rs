//! Fetching from remote repositories (imports and pull mirrors).
//!
//! Remote access is locked down for server-side use: only `http(s)` is
//! allowed (no `file://`, `ssh`, `ext::`), redirects are not followed,
//! credentials travel in an `Authorization` header passed through the
//! environment (never in argv or the URL), and the caller can pin the host
//! to addresses it has checked (`http.curloptResolve`), so DNS can't be
//! rebound to an internal address between the check and the connection.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

use crate::cmd;
use crate::lfs::{MAX_POINTER_SIZE, Pointer, parse_pointer};
use crate::{GitError, GitResult, RefUpdate, RepoStore, ZERO_SHA};

/// Where and how to fetch from.
#[derive(Debug, Clone, Default)]
pub struct Remote {
    /// `http(s)` URL without userinfo.
    pub url: String,
    /// Full `Authorization` header value (e.g. `Basic …`).
    pub authorization: Option<String>,
    /// `host:port:addr[,addr…]` entries pinning DNS resolution.
    pub resolve: Vec<String>,
    /// Overall time limit for one git invocation.
    pub timeout: Option<Duration>,
}

impl Remote {
    fn config(&self) -> Vec<(String, String)> {
        let mut c: Vec<(String, String)> = vec![
            ("protocol.allow".into(), "never".into()),
            ("protocol.http.allow".into(), "always".into()),
            ("protocol.https.allow".into(), "always".into()),
            ("http.followRedirects".into(), "false".into()),
            ("http.lowSpeedLimit".into(), "1000".into()),
            ("http.lowSpeedTime".into(), "60".into()),
            ("credential.helper".into(), String::new()),
            ("core.askPass".into(), String::new()),
        ];
        if let Some(a) = &self.authorization {
            c.push(("http.extraHeader".into(), format!("Authorization: {a}")));
        }
        for r in &self.resolve {
            c.push(("http.curloptResolve".into(), r.clone()));
        }
        c
    }

    fn command(&self, bin: &str, git_dir: Option<&Path>) -> tokio::process::Command {
        let mut c = cmd::git(bin, git_dir);
        let config = self.config();
        c.env("GIT_CONFIG_COUNT", config.len().to_string());
        for (i, (k, v)) in config.iter().enumerate() {
            c.env(format!("GIT_CONFIG_KEY_{i}"), k);
            c.env(format!("GIT_CONFIG_VALUE_{i}"), v);
        }
        c.env("GIT_ASKPASS", "/bin/false");
        c
    }
}

/// Progress reported while fetching.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Progress {
    /// `counting`, `compressing`, `receiving`, `resolving`, ...
    pub phase: String,
    pub objects_received: i64,
    pub objects_total: i64,
    pub bytes_received: i64,
}

/// Default branch advertised by the remote (`HEAD` symref), if any.
pub async fn remote_head(store: &RepoStore, remote: &Remote) -> GitResult<Option<String>> {
    let mut c = remote.command(&store.git_bin, None);
    c.args(["ls-remote", "--symref", &remote.url, "HEAD"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = c.spawn()?;
    let out = match remote.timeout {
        Some(t) => tokio::time::timeout(t, child.wait_with_output())
            .await
            .map_err(|_| timeout_error("ls-remote"))??,
        None => child.wait_with_output().await?,
    };
    if !out.status.success() {
        return Err(GitError::Command {
            args: "ls-remote".into(),
            status: out.status.to_string(),
            stderr: tail(&String::from_utf8_lossy(&out.stderr)),
        });
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(text.lines().find_map(|l| {
        l.strip_prefix("ref: refs/heads/")
            .and_then(|r| r.strip_suffix("\tHEAD"))
            .map(str::to_string)
    }))
}

/// Fetch all branches and tags of `remote` into `repo_id`, force-updating
/// local refs (and deleting the ones the remote no longer has when `prune`).
/// Returns the ref updates that happened.
pub async fn fetch(
    store: &RepoStore,
    repo_id: i64,
    remote: &Remote,
    prune: bool,
    cancel: &CancellationToken,
    mut on_progress: impl FnMut(&Progress),
) -> GitResult<Vec<RefUpdate>> {
    let dir = store.git_dir(repo_id)?;
    let before = list_refs(store, &dir).await?;
    let mut c = remote.command(&store.git_bin, Some(&dir));
    c.args([
        "fetch",
        "--progress",
        "--no-tags",
        "--no-write-fetch-head",
        "--force",
    ]);
    if prune {
        c.arg("--prune");
    }
    c.arg("--")
        .arg(&remote.url)
        .args(["+refs/heads/*:refs/heads/*", "+refs/tags/*:refs/tags/*"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut child = c.spawn()?;
    let mut stderr = child.stderr.take().expect("piped stderr");
    let mut log = String::new();
    let mut progress = Progress::default();
    let mut buf = [0u8; 8192];
    let mut pending = String::new();
    let deadline = remote.timeout.map(|t| tokio::time::Instant::now() + t);
    loop {
        let read = async {
            match deadline {
                Some(d) => tokio::time::timeout_at(d, stderr.read(&mut buf))
                    .await
                    .map_err(|_| timeout_error("fetch"))?
                    .map_err(GitError::from),
                None => stderr.read(&mut buf).await.map_err(GitError::from),
            }
        };
        let n = tokio::select! {
            n = read => n?,
            _ = cancel.cancelled() => {
                let _ = child.kill().await;
                return Err(GitError::Command {
                    args: "fetch".into(),
                    status: "cancelled".into(),
                    stderr: "cancelled".into(),
                });
            }
        };
        if n == 0 {
            break;
        }
        pending.push_str(&String::from_utf8_lossy(&buf[..n]));
        while let Some(pos) = pending.find(['\r', '\n']) {
            let line: String = pending.drain(..=pos).collect();
            let line = line.trim_end_matches(['\r', '\n']);
            if parse_progress(line, &mut progress) {
                on_progress(&progress);
            } else if !line.trim().is_empty() {
                log.push_str(line);
                log.push('\n');
            }
        }
    }
    log.push_str(&pending);
    let status = match deadline {
        Some(d) => tokio::time::timeout_at(d, child.wait())
            .await
            .map_err(|_| timeout_error("fetch"))??,
        None => child.wait().await?,
    };
    if !status.success() {
        return Err(GitError::Command {
            args: "fetch".into(),
            status: status.to_string(),
            stderr: tail(&log),
        });
    }
    // Small fetches don't report receiving progress.
    if progress.objects_received < progress.objects_total {
        progress.objects_received = progress.objects_total;
        on_progress(&progress);
    }
    let after = list_refs(store, &dir).await?;
    Ok(diff_refs(&before, &after))
}

/// Fetch explicit refspecs from `remote` (e.g.
/// `+refs/pull/*/head:refs/pull/*/head`, or `<sha>:refs/pull/7/head` for
/// a commit the remote serves by id), without progress or ref diffing.
/// A glob that matches nothing fetches nothing (not an error).
pub async fn fetch_refspecs(
    store: &RepoStore,
    repo_id: i64,
    remote: &Remote,
    refspecs: &[String],
) -> GitResult<()> {
    if refspecs.is_empty() {
        return Ok(());
    }
    let dir = store.git_dir(repo_id)?;
    let mut c = remote.command(&store.git_bin, Some(&dir));
    c.args(["fetch", "--no-tags", "--no-write-fetch-head", "--force"])
        .arg("--")
        .arg(&remote.url)
        .args(refspecs)
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let child = c.spawn()?;
    let out = match remote.timeout {
        Some(t) => tokio::time::timeout(t, child.wait_with_output())
            .await
            .map_err(|_| timeout_error("fetch"))??,
        None => child.wait_with_output().await?,
    };
    if !out.status.success() {
        return Err(GitError::Command {
            args: "fetch".into(),
            status: out.status.to_string(),
            stderr: tail(&String::from_utf8_lossy(&out.stderr)),
        });
    }
    Ok(())
}

fn timeout_error(what: &str) -> GitError {
    GitError::Command {
        args: what.into(),
        status: "timeout".into(),
        stderr: "timed out".into(),
    }
}

/// Last lines of git's diagnostic output (for error messages).
fn tail(s: &str) -> String {
    let lines: Vec<&str> = s.lines().filter(|l| !l.trim().is_empty()).collect();
    let start = lines.len().saturating_sub(5);
    lines[start..].join("\n")
}

/// `refname -> sha` for branches and tags.
async fn list_refs(store: &RepoStore, dir: &Path) -> GitResult<Vec<(String, String)>> {
    let out = cmd::run(
        &store.git_bin,
        Some(dir),
        &[
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            "refs/heads",
            "refs/tags",
        ],
        &[],
        None,
    )
    .await?;
    let mut refs: Vec<(String, String)> = String::from_utf8_lossy(&out)
        .lines()
        .filter_map(|l| l.split_once(' '))
        .map(|(r, s)| (r.to_string(), s.to_string()))
        .collect();
    refs.sort();
    Ok(refs)
}

fn diff_refs(before: &[(String, String)], after: &[(String, String)]) -> Vec<RefUpdate> {
    let old = |r: &str| before.iter().find(|(n, _)| n == r).map(|(_, s)| s.as_str());
    let mut out: Vec<RefUpdate> = after
        .iter()
        .filter(|(r, s)| old(r) != Some(s.as_str()))
        .map(|(r, s)| RefUpdate {
            old: old(r).unwrap_or(ZERO_SHA).to_string(),
            new: s.clone(),
            refname: r.clone(),
        })
        .collect();
    out.extend(
        before
            .iter()
            .filter(|(r, _)| !after.iter().any(|(n, _)| n == r))
            .map(|(r, s)| RefUpdate {
                old: s.clone(),
                new: ZERO_SHA.to_string(),
                refname: r.clone(),
            }),
    );
    out
}

/// Parse one line of `git fetch --progress` output into `p`.
fn parse_progress(line: &str, p: &mut Progress) -> bool {
    let line = line.trim_start_matches("remote: ").trim();
    // `Total 3 (delta 0), reused 0 (delta 0), pack-reused 0`
    if let Some(rest) = line.strip_prefix("Total ") {
        if let Some(n) = rest.split([' ', ',']).next().and_then(|n| n.parse().ok()) {
            p.objects_total = n;
        }
        return true;
    }
    let Some((name, rest)) = line.split_once(':') else {
        return false;
    };
    let phase = match name.trim() {
        "Enumerating objects" => "enumerating",
        "Counting objects" => "counting",
        "Compressing objects" => "compressing",
        "Receiving objects" | "Unpacking objects" => "receiving",
        "Resolving deltas" => "resolving",
        "Updating files" | "Checking connectivity" => "checking",
        _ => return false,
    };
    p.phase = phase.into();
    let counts = rest
        .split_once('(')
        .and_then(|(_, r)| r.split_once(')'))
        .and_then(|(r, _)| r.split_once('/'))
        .and_then(|(a, b)| Some((a.trim().parse::<i64>().ok()?, b.trim().parse::<i64>().ok()?)));
    match phase {
        // `Enumerating objects: 5, done.`
        "enumerating" => {
            if let Some(n) = rest.trim().split(',').next().and_then(|n| n.parse().ok()) {
                p.objects_total = n;
            }
        }
        "counting" => {
            if let Some((_, total)) = counts {
                p.objects_total = total;
            }
        }
        // `Receiving objects:  45% (450/1000), 1.50 MiB | 2.00 MiB/s`
        "receiving" => {
            if let Some((a, b)) = counts {
                p.objects_received = a;
                p.objects_total = b;
            }
            if let Some(size) = rest.split(", ").nth(1)
                && let Some(bytes) = parse_size(size.split('|').next().unwrap_or("").trim())
            {
                p.bytes_received = bytes;
            }
        }
        _ => {}
    }
    true
}

fn parse_size(s: &str) -> Option<i64> {
    let (num, unit) = s.split_once(' ')?;
    let n: f64 = num.parse().ok()?;
    let mult = match unit.trim() {
        "bytes" | "byte" => 1.0,
        "KiB" => 1024.0,
        "MiB" => 1024.0 * 1024.0,
        "GiB" => 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    Some((n * mult) as i64)
}

/// LFS pointers among the blobs reachable from `tips` but not from
/// `exclude` (pass the previous tips for an incremental scan).
pub async fn lfs_pointers(
    store: &RepoStore,
    repo_id: i64,
    tips: &[String],
    exclude: &[String],
) -> GitResult<Vec<Pointer>> {
    if tips.is_empty() {
        return Ok(Vec::new());
    }
    let dir = store.git_dir(repo_id)?;
    let mut input = String::new();
    for t in tips {
        input.push_str(t);
        input.push('\n');
    }
    for e in exclude.iter().filter(|e| e.as_str() != ZERO_SHA) {
        input.push('^');
        input.push_str(e);
        input.push('\n');
    }
    let objects = cmd::run(
        &store.git_bin,
        Some(&dir),
        &["rev-list", "--objects", "--stdin"],
        &[],
        Some(input.as_bytes()),
    )
    .await?;
    let mut ids = String::new();
    for line in String::from_utf8_lossy(&objects).lines() {
        if let Some(sha) = line.split(' ').next().filter(|s| !s.is_empty()) {
            ids.push_str(sha);
            ids.push('\n');
        }
    }
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let checked = cmd::run(
        &store.git_bin,
        Some(&dir),
        &[
            "cat-file",
            "--batch-check=%(objectname) %(objecttype) %(objectsize)",
        ],
        &[],
        Some(ids.as_bytes()),
    )
    .await?;
    let mut small = String::new();
    for line in String::from_utf8_lossy(&checked).lines() {
        let mut parts = line.split(' ');
        if let (Some(sha), Some("blob"), Some(size)) = (parts.next(), parts.next(), parts.next())
            && size.parse::<usize>().is_ok_and(|s| s <= MAX_POINTER_SIZE)
        {
            small.push_str(sha);
            small.push('\n');
        }
    }
    if small.is_empty() {
        return Ok(Vec::new());
    }
    let contents = cmd::run(
        &store.git_bin,
        Some(&dir),
        &["cat-file", "--batch"],
        &[],
        Some(small.as_bytes()),
    )
    .await?;
    let mut pointers: Vec<Pointer> = Vec::new();
    let mut rest: &[u8] = &contents;
    while let Some(nl) = rest.iter().position(|&b| b == b'\n') {
        let header = String::from_utf8_lossy(&rest[..nl]).to_string();
        rest = &rest[nl + 1..];
        let size: usize = match header.rsplit(' ').next().and_then(|s| s.parse().ok()) {
            Some(s) if rest.len() >= s => s,
            _ => break,
        };
        if let Some(p) = parse_pointer(&rest[..size])
            && !pointers.contains(&p)
        {
            pointers.push(p);
        }
        rest = &rest[(size + 1).min(rest.len())..];
    }
    Ok(pointers)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_lines() {
        let mut p = Progress::default();
        assert!(parse_progress(
            "Receiving objects:  45% (450/1000), 1.50 MiB | 2.00 MiB/s",
            &mut p
        ));
        assert_eq!(p.phase, "receiving");
        assert_eq!((p.objects_received, p.objects_total), (450, 1000));
        assert_eq!(p.bytes_received, 1572864);
        assert!(parse_progress(
            "Resolving deltas: 100% (3/3), done.",
            &mut p
        ));
        assert_eq!(p.phase, "resolving");
        assert_eq!(p.objects_received, 450);
        assert!(parse_progress(
            "remote: Enumerating objects: 5, done.",
            &mut p
        ));
        assert_eq!(p.objects_total, 5);
        assert!(parse_progress(
            "remote: Total 3 (delta 0), reused 0 (delta 0), pack-reused 0",
            &mut p
        ));
        assert_eq!(p.objects_total, 3);
        assert!(!parse_progress("fatal: repository not found", &mut p));
        assert!(parse_progress(
            "Receiving objects: 100% (5/5), done.",
            &mut p
        ));
        assert_eq!((p.objects_received, p.objects_total), (5, 5));
    }

    #[test]
    fn ref_diff() {
        let a = |v: &[(&str, &str)]| {
            v.iter()
                .map(|(r, s)| (r.to_string(), s.to_string()))
                .collect::<Vec<_>>()
        };
        let before = a(&[("refs/heads/a", "1"), ("refs/heads/b", "2")]);
        let after = a(&[("refs/heads/a", "1"), ("refs/heads/c", "3")]);
        let d = diff_refs(&before, &after);
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].refname, "refs/heads/c");
        assert_eq!(d[0].old, ZERO_SHA);
        assert_eq!(d[1].refname, "refs/heads/b");
        assert_eq!(d[1].new, ZERO_SHA);
    }

    #[test]
    fn config_has_no_credentials_in_url() {
        let r = Remote {
            url: "https://example.com/x.git".into(),
            authorization: Some("Basic abc".into()),
            resolve: vec!["example.com:443:1.2.3.4".into()],
            timeout: None,
        };
        let c = r.config();
        assert!(c.contains(&("http.extraHeader".into(), "Authorization: Basic abc".into())));
        assert!(c.contains(&("protocol.allow".into(), "never".into())));
        assert!(c.contains(&(
            "http.curloptResolve".into(),
            "example.com:443:1.2.3.4".into()
        )));
    }
}
