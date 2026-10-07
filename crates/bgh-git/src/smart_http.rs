//! Git smart-HTTP protocol (v0/v1, and v2 for fetch).
//!
//! Callers (bgh-repos) authenticate and authorize, then delegate here:
//! * `GET /info/refs?service=git-upload-pack|git-receive-pack` → [`info_refs`]
//! * `POST /git-upload-pack` → [`upload_pack`] (streams the pack back)
//! * `POST /git-receive-pack` → [`receive_pack`]: the ref update commands
//!   are parsed *before* git sees the pack and handed to an `authorize`
//!   callback (branch protection etc.). On success the applied updates are
//!   returned so the caller can enqueue post-receive work.
//!
//! Request bodies may be gzip-encoded (`Content-Encoding: gzip`). The
//! `Git-Protocol` header is forwarded as `GIT_PROTOCOL`.

use std::future::Future;
use std::pin::Pin;
use std::process::Stdio;

use async_compression::tokio::bufread::GzipDecoder;
use axum::body::Body;
use axum::response::Response;
use bytes::{Bytes, BytesMut};
use futures::TryStreamExt;
use http::{HeaderMap, HeaderValue, StatusCode, header};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio_util::io::{ReaderStream, StreamReader};

use crate::pktline::{self, Packet};
use crate::storage::RepoStore;
use crate::{GitError, GitResult, RefUpdate, cmd, is_sha};

/// Upper bound on the size of the receive-pack command section.
const MAX_COMMANDS_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Service {
    UploadPack,
    ReceivePack,
}

impl Service {
    /// Parse `git-upload-pack` / `git-receive-pack`.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "git-upload-pack" => Some(Self::UploadPack),
            "git-receive-pack" => Some(Self::ReceivePack),
            _ => None,
        }
    }

    /// `upload-pack` / `receive-pack`
    pub fn command(self) -> &'static str {
        match self {
            Self::UploadPack => "upload-pack",
            Self::ReceivePack => "receive-pack",
        }
    }

    pub fn is_write(self) -> bool {
        self == Self::ReceivePack
    }
}

/// Validated `Git-Protocol` header value (e.g. `version=2`).
pub fn git_protocol(headers: &HeaderMap) -> Option<String> {
    let v = headers.get("git-protocol")?.to_str().ok()?;
    let ok = !v.is_empty()
        && v.len() <= 256
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"=:._-".contains(&b));
    ok.then(|| v.to_string())
}

fn no_cache(resp: &mut Response) {
    let h = resp.headers_mut();
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache, max-age=0, must-revalidate"),
    );
    h.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    h.insert(
        header::EXPIRES,
        HeaderValue::from_static("Fri, 01 Jan 1980 00:00:00 GMT"),
    );
}

fn response(status: StatusCode, content_type: &'static str, body: Body) -> Response {
    let mut resp = Response::new(body);
    *resp.status_mut() = status;
    resp.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    no_cache(&mut resp);
    resp
}

/// Turn a request body into a reader, transparently gunzipping.
pub fn body_reader(headers: &HeaderMap, body: Body) -> Pin<Box<dyn AsyncRead + Send>> {
    let stream = body.into_data_stream().map_err(std::io::Error::other);
    let reader = StreamReader::new(stream);
    let gzip = headers
        .get(header::CONTENT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("gzip") || v.eq_ignore_ascii_case("x-gzip"));
    if gzip {
        Box::pin(GzipDecoder::new(reader))
    } else {
        Box::pin(reader)
    }
}

/// `GET info/refs?service=...`: the ref advertisement.
pub async fn info_refs(
    store: &RepoStore,
    repo_id: i64,
    service: Service,
    headers: &HeaderMap,
) -> GitResult<Response> {
    let dir = store.git_dir(repo_id)?;
    let proto = git_protocol(headers);
    let mut envs: Vec<(&str, &str)> = Vec::new();
    if let Some(p) = &proto {
        envs.push(("GIT_PROTOCOL", p));
    }
    let args = advertise_args(service, &dir);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = cmd::run(&store.git_bin, None, &args, &envs, None).await?;
    let v2 =
        service == Service::UploadPack && proto.as_deref().is_some_and(|p| p.contains("version=2"));
    let mut body = BytesMut::with_capacity(out.len() + 64);
    if !v2 {
        pktline::put(
            &mut body,
            format!("# service=git-{}\n", service.command()).as_bytes(),
        );
        body.extend_from_slice(pktline::FLUSH);
    }
    body.extend_from_slice(&out);
    let ct = match service {
        Service::UploadPack => "application/x-git-upload-pack-advertisement",
        Service::ReceivePack => "application/x-git-receive-pack-advertisement",
    };
    Ok(response(StatusCode::OK, ct, Body::from(body.freeze())))
}

/// `POST git-upload-pack`: stream the request into `git upload-pack
/// --stateless-rpc` and its output back to the client.
pub async fn upload_pack(
    store: &RepoStore,
    repo_id: i64,
    headers: &HeaderMap,
    body: Body,
) -> GitResult<Response> {
    let dir = store.git_dir(repo_id)?;
    let mut reader = body_reader(headers, body);
    let mut c = cmd::git(&store.git_bin, None);
    c.arg("upload-pack")
        .arg("--stateless-rpc")
        .arg(&dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(p) = git_protocol(headers) {
        c.env("GIT_PROTOCOL", p);
    }
    let timer = bgh_core::observability::git_op("upload-pack");
    let mut child = c.spawn()?;
    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let mut stderr = child.stderr.take().expect("stderr");

    tokio::spawn(async move {
        if let Err(err) = tokio::io::copy(&mut reader, &mut stdin).await {
            tracing::debug!(?err, "upload-pack: request body copy failed");
        }
        let _ = stdin.shutdown().await;
    });
    tokio::spawn(async move {
        let mut err = String::new();
        let _ = stderr.read_to_string(&mut err).await;
        let status = child.wait().await;
        let ok = matches!(&status, Ok(s) if s.success());
        timer.finish(ok);
        if !ok {
            tracing::debug!(?status, stderr = %err.trim(), "git upload-pack exited unsuccessfully");
        }
    });

    Ok(response(
        StatusCode::OK,
        "application/x-git-upload-pack-result",
        Body::from_stream(ReaderStream::with_capacity(stdout, 64 * 1024)),
    ))
}

/// Capabilities requested by the client in the first command line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PushCapabilities {
    pub report_status: bool,
    pub side_band_64k: bool,
    pub atomic: bool,
    pub push_options: bool,
}

/// Parsed command section of a receive-pack request.
#[derive(Debug, Clone, Default)]
pub struct PushCommands {
    pub updates: Vec<RefUpdate>,
    pub capabilities: PushCapabilities,
    /// The raw bytes consumed (re-fed to git verbatim).
    pub raw: Vec<u8>,
}

/// Parse ref update commands up to and including the flush packet.
pub async fn read_push_commands<R: AsyncRead + Unpin + ?Sized>(
    r: &mut R,
) -> GitResult<PushCommands> {
    let mut out = PushCommands::default();
    let bad = |m: &str| GitError::InvalidInput(format!("malformed push: {m}"));
    let mut first = true;
    loop {
        let pkt = pktline::read_packet(r, &mut out.raw)
            .await
            .map_err(|e| bad(&e.to_string()))?;
        if out.raw.len() > MAX_COMMANDS_BYTES {
            return Err(bad("command list too large"));
        }
        let data = match pkt {
            Packet::Flush => break,
            Packet::Data(d) => d,
            _ => return Err(bad("unexpected packet")),
        };
        let mut line = data.as_slice();
        if line.ends_with(b"\n") {
            line = &line[..line.len() - 1];
        }
        if line.starts_with(b"shallow ") {
            continue;
        }
        if line.starts_with(b"push-cert") {
            return Err(GitError::InvalidInput(
                "signed pushes are not supported".into(),
            ));
        }
        let (cmd, caps) = match line.iter().position(|&b| b == 0) {
            Some(i) => (&line[..i], Some(&line[i + 1..])),
            None => (line, None),
        };
        if first {
            if let Some(caps) = caps {
                for cap in String::from_utf8_lossy(caps).split(' ') {
                    match cap {
                        "report-status" | "report-status-v2" => {
                            out.capabilities.report_status = true
                        }
                        "side-band-64k" => out.capabilities.side_band_64k = true,
                        "atomic" => out.capabilities.atomic = true,
                        "push-options" => out.capabilities.push_options = true,
                        _ => {}
                    }
                }
            }
            first = false;
        }
        let cmd = std::str::from_utf8(cmd).map_err(|_| bad("non-utf8 command"))?;
        let mut parts = cmd.splitn(3, ' ');
        let (Some(old), Some(new), Some(refname)) = (parts.next(), parts.next(), parts.next())
        else {
            return Err(bad("command"));
        };
        if !is_sha(old) || !is_sha(new) || !refname.starts_with("refs/") {
            return Err(bad("command"));
        }
        out.updates.push(RefUpdate {
            old: old.to_ascii_lowercase(),
            new: new.to_ascii_lowercase(),
            refname: refname.to_string(),
        });
    }
    Ok(out)
}

/// Build a report-status response rejecting every update with `reason`.
///
/// A multi-line `reason` reports its first line per ref and sends the
/// remaining lines verbatim on the progress channel (e.g. GitHub's GH013
/// rule violation report); a one-line reason is sent as `error: <reason>`.
pub fn rejection_report(cmds: &PushCommands, reason: &str) -> Bytes {
    let (reason, detail) = match reason.split_once('\n') {
        Some((first, rest)) => (first, Some(rest)),
        None => (reason, None),
    };
    let reason: String = reason.replace(['\n', '\r'], " ");
    let mut inner = BytesMut::new();
    pktline::put(&mut inner, b"unpack ok\n");
    for u in &cmds.updates {
        pktline::put(
            &mut inner,
            format!("ng {} {reason}\n", u.refname).as_bytes(),
        );
    }
    inner.extend_from_slice(pktline::FLUSH);
    if !cmds.capabilities.side_band_64k {
        return if cmds.capabilities.report_status {
            inner.freeze()
        } else {
            Bytes::new()
        };
    }
    let mut out = BytesMut::new();
    match detail {
        Some(detail) => {
            for line in detail.lines() {
                pktline::put_sideband(&mut out, 2, format!("{line}\n").as_bytes());
            }
        }
        None => pktline::put_sideband(&mut out, 2, format!("error: {reason}\n").as_bytes()),
    }
    if cmds.capabilities.report_status {
        pktline::put_sideband(&mut out, 1, &inner);
    }
    out.extend_from_slice(pktline::FLUSH);
    out.freeze()
}

/// Result of [`receive_pack`].
pub struct ReceivePackOutcome {
    /// Response to send to the git client.
    pub response: Response,
    /// Updates git actually applied (verified against the refs afterwards).
    /// Empty when rejected.
    pub applied: Vec<RefUpdate>,
    /// Updates the client requested.
    pub requested: Vec<RefUpdate>,
    /// Rejection reason if `authorize` refused the push.
    pub rejected: Option<String>,
}

/// Push checks that need the pushed objects. They run in a `pre-receive`
/// hook while the new objects are still quarantined
/// (`GIT_QUARANTINE_PATH`), so a rejected push leaves nothing behind.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PushPolicy {
    /// Refs (full names) that may only move fast-forward.
    pub no_force_push: Vec<String>,
    /// Refs whose newly pushed commits must not include merge commits.
    pub linear_history: Vec<String>,
    /// Set when the pusher may not create or update workflow files
    /// (`.github/workflows/**`): the rejection message, with
    /// [`WORKFLOW_PATH_PLACEHOLDER`] standing for the offending path.
    pub workflow_denied: Option<String>,
    /// Site-wide limits (size, quota, fsck) applied to every push.
    pub limits: PushLimits,
    /// Server-side check over the quarantined objects (ruleset push and
    /// metadata rules), called back from the `pre-receive` hook.
    pub object_check: Option<ObjectCheck>,
}

/// Object directories of a push in quarantine, as seen by the
/// `pre-receive` hook. Pass [`QuarantineEnv::envs`] to git to read the
/// pushed objects (plus everything the repository already has).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QuarantineEnv {
    pub object_directory: String,
    pub alternate_object_directories: String,
}

impl QuarantineEnv {
    pub fn envs(&self) -> Vec<(&'static str, &str)> {
        let mut v = Vec::new();
        if !self.object_directory.is_empty() {
            v.push(("GIT_OBJECT_DIRECTORY", self.object_directory.as_str()));
        }
        if !self.alternate_object_directories.is_empty() {
            v.push((
                "GIT_ALTERNATE_OBJECT_DIRECTORIES",
                self.alternate_object_directories.as_str(),
            ));
        }
        v
    }
}

/// Answer of an [`ObjectCheck`]: `lines` are printed to the client (as
/// `remote:` lines); `accept: false` rejects the whole push.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HookVerdict {
    pub accept: bool,
    pub lines: Vec<String>,
}

type ObjectCheckFn =
    dyn Fn(QuarantineEnv) -> futures::future::BoxFuture<'static, HookVerdict> + Send + Sync;

/// Callback run (once per push) while the pushed objects are quarantined.
#[derive(Clone)]
pub struct ObjectCheck(pub std::sync::Arc<ObjectCheckFn>);

impl ObjectCheck {
    pub fn new<F, Fut>(f: F) -> Self
    where
        F: Fn(QuarantineEnv) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = HookVerdict> + Send + 'static,
    {
        Self(std::sync::Arc::new(move |env| Box::pin(f(env))))
    }
}

impl std::fmt::Debug for ObjectCheck {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ObjectCheck")
    }
}

impl PartialEq for ObjectCheck {
    fn eq(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for ObjectCheck {}

/// Limits of [`PushPolicy`] that don't depend on the ref (site settings
/// `git.*` and storage quotas, filled in by the caller).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PushLimits {
    /// `receive.fsckObjects` override (`None`: the repository config, on).
    pub fsck: Option<bool>,
    /// Blobs larger than this (bytes) reject the push with GH001.
    pub max_blob_bytes: Option<u64>,
    /// Blobs larger than this (bytes) print a warning.
    pub warn_blob_bytes: Option<u64>,
    /// `receive.maxInputSize` (bytes of the incoming pack).
    pub max_input_bytes: Option<u64>,
    /// Remaining storage quota (KB); the quarantined objects must fit.
    pub quota_remaining_kb: Option<i64>,
    /// Message printed when the quota check fails.
    pub quota_message: Option<String>,
}

/// Placeholder for the file path in [`PushPolicy::workflow_denied`].
pub const WORKFLOW_PATH_PLACEHOLDER: &str = "@PATH@";

impl PushPolicy {
    /// Whether the policy needs no `pre-receive` hook.
    pub fn is_empty(&self) -> bool {
        self.no_force_push.is_empty()
            && self.linear_history.is_empty()
            && self.workflow_denied.is_none()
            && self.limits.max_blob_bytes.is_none()
            && self.limits.warn_blob_bytes.is_none()
            && self.limits.quota_remaining_kb.is_none()
            && self.object_check.is_none()
    }

    /// The same policy with `limits`.
    pub fn with_limits(mut self, limits: PushLimits) -> Self {
        self.limits = limits;
        self
    }
}

/// `-c` overrides every `git receive-pack` runs with, whatever the
/// repository's config says (defense in depth for repositories whose
/// config predates [`crate::storage::CONFIG_VERSION`]).
fn receive_overrides(c: &mut tokio::process::Command) {
    for prefix in crate::storage::HIDDEN_REF_PREFIXES {
        c.arg("-c").arg(format!("receive.hideRefs={prefix}"));
    }
}

/// `-c` arguments of [`receive_overrides`] for [`cmd::run`].
fn receive_override_args() -> Vec<String> {
    crate::storage::HIDDEN_REF_PREFIXES
        .iter()
        .flat_map(|p| ["-c".to_string(), format!("receive.hideRefs={p}")])
        .collect()
}

/// Message of the hidden-ref rejection (git's own wording).
pub const HIDDEN_REF_REASON: &str = "deny updating a hidden ref";

/// The `pre-receive` hook enforcing [`PushPolicy`]. Inputs (environment):
///
/// * `BGH_QUOTA_KB` / `BGH_QUOTA_MESSAGE`: the quarantined objects must
///   fit in the remaining storage quota.
/// * `BGH_MAX_BLOB` / `BGH_WARN_BLOB`: blob size limits in bytes over the
///   newly pushed objects (`rev-list --objects` + `cat-file
///   --batch-check`), with GitHub's GH001 wording.
/// * `BGH_CHECK_DIR`: directory with the `req` / `resp` FIFOs of an
///   [`ObjectCheck`]: the hook writes its object directories to `req` and
///   reads the verdict (exit code line, then messages) from `resp`.
/// * `BGH_NO_FF_REFS` / `BGH_LINEAR_REFS`: space separated refs.
/// * `BGH_WORKFLOW_DENIED`: rejection message (with
///   [`WORKFLOW_PATH_PLACEHOLDER`]) when the pusher may not change
///   `.github/workflows/**`.
pub const PRE_RECEIVE_HOOK: &str = r#"#!/bin/sh
# Installed by Better GitHub: push checks needing the pushed objects.
z=0000000000000000000000000000000000000000
status=0
input=$(cat)
news=""
while read old new ref; do
  [ -z "$ref" ] || [ "$new" = "$z" ] || news="$news $new"
done <<EOF
$input
EOF
if [ -n "$BGH_QUOTA_KB" ] && [ -n "$GIT_QUARANTINE_PATH" ] && [ -d "$GIT_QUARANTINE_PATH" ]; then
  used=$(du -sk "$GIT_QUARANTINE_PATH" | cut -f1)
  if [ "${used:-0}" -gt "$BGH_QUOTA_KB" ]; then
    echo "error: $BGH_QUOTA_MESSAGE" >&2
    exit 1
  fi
fi
if [ -n "$BGH_MAX_BLOB$BGH_WARN_BLOB" ] && [ -n "$news" ]; then
  report=$(git rev-list --objects $news --not --all |
    git cat-file --batch-check='%(objecttype) %(objectsize) %(rest)' |
    awk -v max="$BGH_MAX_BLOB" -v warn="$BGH_WARN_BLOB" '
      $1 != "blob" { next }
      {
        path = $0; sub(/^[^ ]+ [^ ]+ ?/, "", path)
        if (max != "" && $2 + 0 > max + 0) {
          printf "error: File %s is %.2f MB; this exceeds the file size limit of %.2f MB\n", path, $2 / 1048576, max / 1048576
          big = 1
        } else if (warn != "" && $2 + 0 > warn + 0) {
          printf "warning: File %s is %.2f MB; this is larger than the recommended maximum file size of %.2f MB\n", path, $2 / 1048576, warn / 1048576
          large = 1
        }
      }
      END {
        lfs = "GH001: Large files detected. You may want to try Git Large File Storage - https://git-lfs.github.com."
        if (big) { print "error: " lfs; exit 1 }
        if (large) print "warning: " lfs
      }')
  rc=$?
  [ -n "$report" ] && echo "$report" >&2
  [ $rc -eq 0 ] || exit 1
fi
if [ -n "$BGH_CHECK_DIR" ]; then
  printf '%s\n%s\nend\n' "$GIT_OBJECT_DIRECTORY" "$GIT_ALTERNATE_OBJECT_DIRECTORIES" > "$BGH_CHECK_DIR/req"
  verdict=$(cat "$BGH_CHECK_DIR/resp")
  printf '%s\n' "$verdict" | sed '1d' >&2
  [ "$(printf '%s\n' "$verdict" | head -n 1)" = "0" ] || exit 1
fi
while read old new ref; do
  [ -z "$ref" ] && continue
  [ "$new" = "$z" ] && continue
  case " $BGH_NO_FF_REFS " in
    *" $ref "*)
      if [ "$old" != "$z" ] && ! git merge-base --is-ancestor "$old" "$new" 2>/dev/null; then
        echo "error: GH006: Protected branch update failed for $ref." >&2
        echo "error: Cannot force-push to this branch" >&2
        status=1
      fi;;
  esac
  if [ -n "$BGH_WORKFLOW_DENIED" ]; then
    if [ "$old" = "$z" ]; then
      f=$(git log --format= --name-only "$new" --not --all -- .github/workflows | sed -n '/./{p;q;}')
    else
      f=$(git log --format= --name-only "$old..$new" -- .github/workflows | sed -n '/./{p;q;}')
    fi
    if [ -n "$f" ]; then
      echo "error: ${BGH_WORKFLOW_DENIED%%@PATH@*}$f${BGH_WORKFLOW_DENIED#*@PATH@}" >&2
      status=1
    fi
  fi
  case " $BGH_LINEAR_REFS " in
    *" $ref "*)
      if [ "$old" = "$z" ]; then
        merges=$(git rev-list --min-parents=2 --max-count=1 "$new" --not --all)
      else
        merges=$(git rev-list --min-parents=2 --max-count=1 "$old..$new")
      fi
      if [ -n "$merges" ]; then
        echo "error: GH006: Protected branch update failed for $ref." >&2
        echo "error: This branch must not contain merge commits." >&2
        status=1
      fi;;
  esac
done <<EOF
$input
EOF
exit $status
"#;

/// Write the hook into `{store.root}/.bgh-hooks` (once per process and
/// whenever it is missing or outdated); returns the hooks directory.
pub async fn ensure_hooks(store: &RepoStore) -> GitResult<std::path::PathBuf> {
    let dir = store.root.join(".bgh-hooks");
    let hook = dir.join("pre-receive");
    let current = tokio::fs::read_to_string(&hook).await.ok();
    if current.as_deref() != Some(PRE_RECEIVE_HOOK) {
        tokio::fs::create_dir_all(&dir).await?;
        let tmp = dir.join(format!("pre-receive.{}", bgh_core::crypto::random_token(8)));
        tokio::fs::write(&tmp, PRE_RECEIVE_HOOK).await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).await?;
        }
        tokio::fs::rename(&tmp, &hook).await?;
    }
    // git silently skips a hook it can't execute (lost mode bits, a
    // `noexec` mount), which would turn every object check off: repair
    // the mode, and refuse pushes rather than accept them unchecked.
    if !executable(&hook) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).await?;
        }
        if !executable(&hook) {
            tracing::error!(
                hook = %hook.display(),
                "the pre-receive hook is not executable (noexec mount?); refusing pushes"
            );
            return Err(GitError::Object(format!(
                "pre-receive hook {} is not executable",
                hook.display()
            )));
        }
    }
    Ok(dir)
}

/// Whether the current process may execute `path` (`access(X_OK)`, which
/// also reports `noexec` mounts).
fn executable(path: &std::path::Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: `c` is a valid NUL-terminated path.
    unsafe { libc::access(c.as_ptr(), libc::X_OK) == 0 }
}

/// Transport-independent result of [`receive_pack_stream`].
pub struct PushResult {
    /// Bytes to send back to the git client (report-status etc.).
    pub output: Bytes,
    pub applied: Vec<RefUpdate>,
    pub requested: Vec<RefUpdate>,
    pub rejected: Option<String>,
}

/// `POST git-receive-pack`.
///
/// `authorize` receives the requested ref updates before any data reaches
/// git; returning `Err(reason)` rejects the whole push (reported to the
/// client per ref as `ng <ref> <reason>`).
pub async fn receive_pack<F, Fut>(
    store: &RepoStore,
    repo_id: i64,
    headers: &HeaderMap,
    body: Body,
    authorize: F,
) -> GitResult<ReceivePackOutcome>
where
    F: FnOnce(Vec<RefUpdate>) -> Fut,
    Fut: Future<Output = Result<(), String>>,
{
    receive_pack_with_policy(store, repo_id, headers, body, |updates| {
        let fut = authorize(updates);
        async move { fut.await.map(|()| PushPolicy::default()) }
    })
    .await
}

/// Like [`receive_pack`], but `authorize` may also return a [`PushPolicy`]
/// that is enforced by a `pre-receive` hook once the pack is received
/// (force-push and linear-history checks).
pub async fn receive_pack_with_policy<F, Fut>(
    store: &RepoStore,
    repo_id: i64,
    headers: &HeaderMap,
    body: Body,
    authorize: F,
) -> GitResult<ReceivePackOutcome>
where
    F: FnOnce(Vec<RefUpdate>) -> Fut,
    Fut: Future<Output = Result<PushPolicy, String>>,
{
    store.git_dir(repo_id)?;
    let reader = body_reader(headers, body);
    let r = receive_pack_stream_with_policy(store, repo_id, reader, authorize).await?;
    Ok(ReceivePackOutcome {
        response: response(
            StatusCode::OK,
            "application/x-git-receive-pack-result",
            Body::from(r.output),
        ),
        applied: r.applied,
        requested: r.requested,
        rejected: r.rejected,
    })
}

/// The receive-pack request half shared by HTTP and SSH: read the ref
/// update commands from `reader`, authorize them, then feed commands and
/// pack to `git receive-pack --stateless-rpc` and collect its report.
///
/// git exits once it has read the pack (which is self-delimiting), so the
/// reader does not need to reach EOF (SSH clients keep the channel open).
pub async fn receive_pack_stream<R, F, Fut>(
    store: &RepoStore,
    repo_id: i64,
    reader: R,
    authorize: F,
) -> GitResult<PushResult>
where
    R: AsyncRead + Send + Unpin + 'static,
    F: FnOnce(Vec<RefUpdate>) -> Fut,
    Fut: Future<Output = Result<(), String>>,
{
    receive_pack_stream_with_policy(store, repo_id, reader, |updates| {
        let fut = authorize(updates);
        async move { fut.await.map(|()| PushPolicy::default()) }
    })
    .await
}

/// [`receive_pack_stream`] with a [`PushPolicy`] (see
/// [`receive_pack_with_policy`]).
pub async fn receive_pack_stream_with_policy<R, F, Fut>(
    store: &RepoStore,
    repo_id: i64,
    mut reader: R,
    authorize: F,
) -> GitResult<PushResult>
where
    R: AsyncRead + Send + Unpin + 'static,
    F: FnOnce(Vec<RefUpdate>) -> Fut,
    Fut: Future<Output = Result<PushPolicy, String>>,
{
    let dir = store.git_dir(repo_id)?;
    let cmds = read_push_commands(&mut reader).await?;
    if cmds.updates.is_empty() {
        // Nothing to update (e.g. "Everything up-to-date" over SSH).
        return Ok(PushResult {
            output: Bytes::new(),
            applied: vec![],
            requested: vec![],
            rejected: None,
        });
    }

    let policy = match authorize(cmds.updates.clone()).await {
        Ok(policy) => policy,
        Err(reason) => {
            // Consume the pack so the client sees our report instead of EPIPE
            // (until EOF, or until the client goes quiet).
            let mut buf = vec![0u8; 64 * 1024];
            let idle = std::time::Duration::from_secs(10);
            while let Ok(Ok(n)) = tokio::time::timeout(idle, reader.read(&mut buf)).await {
                if n == 0 {
                    break;
                }
            }
            return Ok(PushResult {
                output: rejection_report(&cmds, &reason),
                applied: vec![],
                requested: cmds.updates,
                rejected: Some(reason),
            });
        }
    };

    let mut c = cmd::git(&store.git_bin, None);
    receive_overrides(&mut c);
    let limits = &policy.limits;
    if let Some(fsck) = limits.fsck {
        c.arg("-c").arg(format!("receive.fsckObjects={fsck}"));
    }
    if let Some(max) = limits.max_input_bytes {
        c.arg("-c").arg(format!("receive.maxInputSize={max}"));
    }
    let mut check_task = None;
    let mut _check_dir = None;
    if let Some(check) = policy.object_check.clone() {
        let dir = tempfile::Builder::new().prefix("bgh-check-").tempdir()?;
        let (req, resp) = (dir.path().join("req"), dir.path().join("resp"));
        mkfifo(&req)?;
        mkfifo(&resp)?;
        let rx = tokio::net::unix::pipe::OpenOptions::new()
            .read_write(true)
            .open_receiver(&req)?;
        c.env("BGH_CHECK_DIR", dir.path());
        check_task = Some(tokio::spawn(serve_object_check(check, rx, resp)));
        _check_dir = Some(dir);
    }
    if !policy.is_empty() {
        let hooks = ensure_hooks(store).await?;
        let opt = |v: Option<u64>| v.map(|n| n.to_string()).unwrap_or_default();
        c.arg("-c")
            .arg(format!("core.hooksPath={}", hooks.display()))
            .env("BGH_NO_FF_REFS", policy.no_force_push.join(" "))
            .env("BGH_LINEAR_REFS", policy.linear_history.join(" "))
            .env(
                "BGH_WORKFLOW_DENIED",
                policy.workflow_denied.as_deref().unwrap_or_default(),
            )
            .env("BGH_MAX_BLOB", opt(limits.max_blob_bytes))
            .env("BGH_WARN_BLOB", opt(limits.warn_blob_bytes))
            .env(
                "BGH_QUOTA_KB",
                limits
                    .quota_remaining_kb
                    .map(|n| n.max(0).to_string())
                    .unwrap_or_default(),
            )
            .env(
                "BGH_QUOTA_MESSAGE",
                limits
                    .quota_message
                    .as_deref()
                    .unwrap_or("This push would exceed the storage quota."),
            );
    }
    c.arg("receive-pack")
        .arg("--stateless-rpc")
        .arg(&dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let timer = bgh_core::observability::git_op("receive-pack");
    let mut child = c.spawn()?;
    let mut stdin = child.stdin.take().expect("stdin");
    let mut stdout = child.stdout.take().expect("stdout");
    let mut stderr = child.stderr.take().expect("stderr");

    let prefix = cmds.raw.clone();
    let writer = tokio::spawn(async move {
        let r = async {
            stdin.write_all(&prefix).await?;
            tokio::io::copy(&mut reader, &mut stdin).await?;
            stdin.shutdown().await
        }
        .await;
        if let Err(err) = r {
            tracing::debug!(?err, "receive-pack: request body copy failed");
        }
    });
    let out_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf).await;
        buf
    });
    let err_task = tokio::spawn(async move {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s).await;
        s
    });
    let status = child.wait().await?;
    // git has read everything it needs; don't wait for the client to close.
    writer.abort();
    if let Some(t) = check_task {
        t.abort();
    }
    let out = out_task.await.unwrap_or_default();
    let err = err_task.await.unwrap_or_default();
    timer.finish(status.success());
    if !status.success() {
        tracing::warn!(%status, stderr = %err.trim(), "git receive-pack failed");
    }

    // Determine which updates were applied by reading the refs back.
    let applied = {
        let store = store.clone();
        let updates = cmds.updates.clone();
        tokio::task::spawn_blocking(move || -> GitResult<Vec<RefUpdate>> {
            let repo = store.open(repo_id)?;
            let mut applied = Vec::new();
            for u in updates {
                let current = repo.find_ref(&u.refname)?;
                let ok = if u.is_delete() {
                    current.is_none()
                } else {
                    current.is_some_and(|r| r.target == u.new)
                };
                if ok {
                    applied.push(u);
                }
            }
            Ok(applied)
        })
        .await
        .map_err(|e| GitError::Object(e.to_string()))??
    };

    Ok(PushResult {
        output: Bytes::from(out),
        applied,
        requested: cmds.updates,
        rejected: None,
    })
}

fn mkfifo(path: &std::path::Path) -> GitResult<()> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|e| GitError::Object(e.to_string()))?;
    // SAFETY: `c` is a valid NUL-terminated path.
    if unsafe { libc::mkfifo(c.as_ptr(), 0o600) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

/// Answer the `pre-receive` hook's request on the [`ObjectCheck`] FIFOs:
/// read its object directories from `rx`, run the check, write the verdict
/// to `resp` (which the hook opens for reading after its request).
async fn serve_object_check(
    check: ObjectCheck,
    mut rx: tokio::net::unix::pipe::Receiver,
    resp: std::path::PathBuf,
) {
    use futures::FutureExt;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match rx.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
        if buf.ends_with(b"\nend\n") || buf.len() > 1 << 20 {
            break;
        }
    }
    let text = String::from_utf8_lossy(&buf);
    let mut lines = text.lines();
    let env = QuarantineEnv {
        object_directory: lines.next().unwrap_or_default().to_string(),
        alternate_object_directories: lines.next().unwrap_or_default().to_string(),
    };
    let verdict = std::panic::AssertUnwindSafe((check.0)(env))
        .catch_unwind()
        .await
        .unwrap_or_else(|_| HookVerdict {
            accept: false,
            lines: vec!["error: internal error evaluating repository rules".into()],
        });
    let mut out = format!("{}\n", if verdict.accept { 0 } else { 1 });
    for l in &verdict.lines {
        out.push_str(&l.replace(['\r', '\n'], " "));
        out.push('\n');
    }
    // The hook opens `resp` right after writing its request.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let mut tx = loop {
        match tokio::net::unix::pipe::OpenOptions::new().open_sender(&resp) {
            Ok(tx) => break tx,
            Err(e)
                if e.raw_os_error() == Some(libc::ENXIO)
                    && std::time::Instant::now() < deadline =>
            {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            Err(e) => {
                tracing::warn!(error = %e, "pre-receive: cannot answer the rules check");
                return;
            }
        }
    };
    if let Err(e) = tx.write_all(out.as_bytes()).await {
        tracing::warn!(error = %e, "pre-receive: writing the rules verdict failed");
    }
}

/// Raw ref advertisement of `git <service> --advertise-refs` (no smart-HTTP
/// `# service=` header), as sent first on an SSH connection.
pub async fn advertise_refs(
    store: &RepoStore,
    repo_id: i64,
    service: Service,
    protocol: Option<&str>,
) -> GitResult<Vec<u8>> {
    let dir = store.git_dir(repo_id)?;
    let mut envs: Vec<(&str, &str)> = Vec::new();
    if let Some(p) = protocol {
        envs.push(("GIT_PROTOCOL", p));
    }
    let args = advertise_args(service, &dir);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    cmd::run(&store.git_bin, None, &args, &envs, None).await
}

/// `git [-c ...] <service> --stateless-rpc --advertise-refs <dir>`
/// (receive-pack hides the server-only refs, see [`receive_overrides`]).
fn advertise_args(service: Service, dir: &std::path::Path) -> Vec<String> {
    let mut args = match service {
        Service::ReceivePack => receive_override_args(),
        Service::UploadPack => Vec::new(),
    };
    args.extend([
        service.command().to_string(),
        "--stateless-rpc".into(),
        "--advertise-refs".into(),
        dir.to_string_lossy().into_owned(),
    ]);
    args
}

/// Validate a `GIT_PROTOCOL` value received from a client (SSH env request).
pub fn valid_protocol(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 256
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"=:._-".contains(&b))
}

/// Run `git upload-pack` (full duplex, not stateless) between `input` and
/// `output`, as for git over SSH. Returns git's exit code.
pub async fn upload_pack_duplex<R, W, E>(
    store: &RepoStore,
    repo_id: i64,
    protocol: Option<&str>,
    mut input: R,
    mut output: W,
    mut errors: E,
) -> GitResult<i32>
where
    R: AsyncRead + Send + Unpin + 'static,
    W: tokio::io::AsyncWrite + Send + Unpin,
    E: tokio::io::AsyncWrite + Send + Unpin,
{
    let dir = store.git_dir(repo_id)?;
    let mut c = cmd::git(&store.git_bin, None);
    c.arg("upload-pack")
        .arg(&dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(p) = protocol.filter(|p| valid_protocol(p)) {
        c.env("GIT_PROTOCOL", p);
    }
    let timer = bgh_core::observability::git_op("upload-pack");
    let mut child = c.spawn()?;
    let mut stdin = child.stdin.take().expect("stdin");
    let mut stdout = child.stdout.take().expect("stdout");
    let mut stderr = child.stderr.take().expect("stderr");
    let feeder = tokio::spawn(async move {
        let _ = tokio::io::copy(&mut input, &mut stdin).await;
        let _ = stdin.shutdown().await;
    });
    let (out, err) = tokio::join!(
        tokio::io::copy(&mut stdout, &mut output),
        tokio::io::copy(&mut stderr, &mut errors)
    );
    let _ = output.flush().await;
    let _ = errors.flush().await;
    let status = child.wait().await?;
    timer.finish(status.success());
    feeder.abort();
    if let Err(e) = out.and(err) {
        tracing::debug!(?e, "upload-pack: copying output failed");
    }
    Ok(status.code().unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkt(s: &str) -> Vec<u8> {
        pktline::encode(s.as_bytes()).to_vec()
    }

    #[tokio::test]
    async fn parses_commands() {
        let a = "a".repeat(40);
        let z = "0".repeat(40);
        let mut body = pkt(&format!(
            "{z} {a} refs/heads/main\0report-status side-band-64k agent=git/2\n"
        ));
        body.extend(pkt(&format!("{a} {z} refs/heads/old\n")));
        body.extend_from_slice(b"0000PACKDATA");
        let mut r = &body[..];
        let cmds = read_push_commands(&mut r).await.unwrap();
        assert_eq!(cmds.updates.len(), 2);
        assert!(cmds.updates[0].is_create());
        assert!(cmds.updates[1].is_delete());
        assert_eq!(cmds.updates[0].refname, "refs/heads/main");
        assert!(cmds.capabilities.report_status && cmds.capabilities.side_band_64k);
        assert_eq!(r, b"PACKDATA");
        assert_eq!(cmds.raw.len(), body.len() - 8);

        let report = rejection_report(&cmds, "protected branch");
        let text = String::from_utf8_lossy(&report);
        assert!(text.contains("ng refs/heads/main protected branch"));
        assert!(text.ends_with("0000"));
    }

    #[tokio::test]
    async fn rejects_garbage() {
        let body = pkt("not a command\n");
        let mut r = &body[..];
        assert!(read_push_commands(&mut r).await.is_err());
    }

    #[tokio::test]
    async fn gunzips_request_bodies() {
        use async_compression::tokio::bufread::GzipEncoder;
        let mut gz = Vec::new();
        GzipEncoder::new(&b"0000PACK"[..])
            .read_to_end(&mut gz)
            .await
            .unwrap();
        let mut h = HeaderMap::new();
        h.insert(header::CONTENT_ENCODING, "gzip".parse().unwrap());
        let mut out = Vec::new();
        body_reader(&h, Body::from(gz))
            .read_to_end(&mut out)
            .await
            .unwrap();
        assert_eq!(out, b"0000PACK");
    }

    #[tokio::test]
    async fn hook_mode_is_repaired() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let store = RepoStore::new(tmp.path(), "git");
        let dir = ensure_hooks(&store).await.unwrap();
        let hook = dir.join("pre-receive");
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(!executable(&hook) || nix_root());
        ensure_hooks(&store).await.unwrap();
        assert!(executable(&hook));
    }

    /// root may execute anything with any x bit; `access` is still exact
    /// for 0o644 files, but keep the check robust.
    fn nix_root() -> bool {
        // SAFETY: plain syscall.
        unsafe { libc::geteuid() == 0 }
    }

    #[test]
    fn validates_protocol_header() {
        let mut h = HeaderMap::new();
        h.insert("git-protocol", "version=2".parse().unwrap());
        assert_eq!(git_protocol(&h).as_deref(), Some("version=2"));
        h.insert("git-protocol", "version=2; rm -rf".parse().unwrap());
        assert_eq!(git_protocol(&h), None);
    }
}
