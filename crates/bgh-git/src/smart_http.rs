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
    let dir_s = dir.to_string_lossy().to_string();
    let out = cmd::run(
        &store.git_bin,
        None,
        &[
            service.command(),
            "--stateless-rpc",
            "--advertise-refs",
            &dir_s,
        ],
        &envs,
        None,
    )
    .await?;
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
        if !matches!(&status, Ok(s) if s.success()) {
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
pub fn rejection_report(cmds: &PushCommands, reason: &str) -> Bytes {
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
    pktline::put_sideband(&mut out, 2, format!("error: {reason}\n").as_bytes());
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
    let dir = store.git_dir(repo_id)?;
    let mut reader = body_reader(headers, body);
    let cmds = read_push_commands(&mut reader).await?;
    const CT: &str = "application/x-git-receive-pack-result";

    if let Err(reason) = authorize(cmds.updates.clone()).await {
        // Consume the pack so the client sees our report instead of EPIPE.
        let _ = tokio::io::copy(&mut reader, &mut tokio::io::sink()).await;
        return Ok(ReceivePackOutcome {
            response: response(
                StatusCode::OK,
                CT,
                Body::from(rejection_report(&cmds, &reason)),
            ),
            applied: vec![],
            requested: cmds.updates,
            rejected: Some(reason),
        });
    }

    let mut c = cmd::git(&store.git_bin, None);
    c.arg("receive-pack")
        .arg("--stateless-rpc")
        .arg(&dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
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
    let _ = writer.await;
    let out = out_task.await.unwrap_or_default();
    let err = err_task.await.unwrap_or_default();
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

    Ok(ReceivePackOutcome {
        response: response(StatusCode::OK, CT, Body::from(out)),
        applied,
        requested: cmds.updates,
        rejected: None,
    })
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

    #[test]
    fn validates_protocol_header() {
        let mut h = HeaderMap::new();
        h.insert("git-protocol", "version=2".parse().unwrap());
        assert_eq!(git_protocol(&h).as_deref(), Some("version=2"));
        h.insert("git-protocol", "version=2; rm -rf".parse().unwrap());
        assert_eq!(git_protocol(&h), None);
    }
}
