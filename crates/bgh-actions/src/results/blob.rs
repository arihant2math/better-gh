//! The subset of the Azure Blob REST API that `@actions/cache` (v2) and
//! `@actions/artifact` (v2) use against the signed URLs they get from the
//! results services: Put Blob, Put Block, Put Block List, Get Blob (with
//! `Range` / `x-ms-range`) and Get Blob Properties (HEAD). The legacy cache
//! protocol downloads from the same URLs (plain GET / HEAD with ranges).
//!
//! URLs: `/_bgh/actions/blob/{kind}/{id}?se=<exp>&sp=<r|w>&sig=<hmac>`
//! ([`crate::runtime::signed_blob_url`]). Kinds:
//!
//! | kind | id | write target | read source |
//! |---|---|---|---|
//! | `cache` | cache entry id | staging file of an uncommitted entry | committed archive |
//! | `artifact-upload` | `{job}-{name hash}` | artifact staging file | — |
//! | `artifact` | artifact id | — | stored artifact zip |

use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Router, extract::Path as AxPath};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use bgh_core::AppState;
use futures::StreamExt;
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

use crate::runtime::{BLOB_PREFIX, BlobPerm, check_blob_signature};

pub const X_MS_VERSION: &str = "2024-11-04";
/// Azure's limit on a block id (decoded).
const MAX_BLOCK_ID_LEN: usize = 64;
/// Azure's limit on the number of blocks in a block list.
const MAX_BLOCKS: usize = 50_000;

pub fn routes() -> Router<AppState> {
    Router::new().route(
        &format!("{BLOB_PREFIX}/{{kind}}/{{id}}"),
        get(get_blob).put(put),
    )
}

#[derive(Debug, Default, Deserialize)]
pub struct BlobQuery {
    se: Option<String>,
    sp: Option<String>,
    sig: Option<String>,
    comp: Option<String>,
    blockid: Option<String>,
}

/// An Azure-style XML error.
pub fn azure_error(status: StatusCode, code: &str, message: &str) -> Response {
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?><Error><Code>{code}</Code><Message>{}</Message></Error>",
        xml_escape(message)
    );
    let mut resp = (status, body).into_response();
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/xml"),
    );
    if let Ok(v) = HeaderValue::from_str(code) {
        h.insert("x-ms-error-code", v);
    }
    common_headers(h);
    resp
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn common_headers(h: &mut HeaderMap) {
    h.insert("x-ms-version", HeaderValue::from_static(X_MS_VERSION));
    if let Ok(v) = HeaderValue::from_str(&uuid::Uuid::new_v4().to_string()) {
        h.insert("x-ms-request-id", v);
    }
}

fn internal(err: impl std::fmt::Display) -> Response {
    tracing::error!(%err, "blob service error");
    azure_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "InternalError",
        "The server encountered an internal error.",
    )
}

fn auth_failed() -> Response {
    azure_error(
        StatusCode::FORBIDDEN,
        "AuthenticationFailed",
        "Server failed to authenticate the request. The signature is invalid or expired.",
    )
}

fn not_found() -> Response {
    azure_error(
        StatusCode::NOT_FOUND,
        "BlobNotFound",
        "The specified blob does not exist.",
    )
}

/// Where uploads of a blob are assembled.
struct WriteTarget {
    staging: PathBuf,
    blocks: PathBuf,
    /// Upper bound of the blob size.
    max_size: u64,
}

fn artifact_staging_dir(state: &AppState) -> PathBuf {
    state
        .config
        .data_dir
        .join("actions")
        .join("artifacts")
        .join("staging")
}

/// Staging file of a results-service artifact upload.
pub fn artifact_staging_path(state: &AppState, id: &str) -> PathBuf {
    artifact_staging_dir(state).join(format!("{id}.part"))
}

pub fn artifact_blocks_dir(state: &AppState, id: &str) -> PathBuf {
    artifact_staging_dir(state).join(format!("{id}.blocks"))
}

/// Largest artifact accepted through the results service.
const MAX_ARTIFACT_SIZE: u64 = 10 << 30;

/// Errors are boxed: `Response` is large and these are cold paths.
async fn write_target(
    state: &AppState,
    kind: &str,
    id: &str,
) -> Result<WriteTarget, Box<Response>> {
    match kind {
        "cache" => {
            let id: i64 = id.parse().map_err(|_| not_found())?;
            let row: Option<i64> = sqlx::query_scalar(
                "SELECT repo_id FROM actions_caches WHERE id = $1 AND NOT committed",
            )
            .bind(id)
            .fetch_optional(&state.db)
            .await
            .map_err(internal)?;
            // Committed entries are immutable.
            let repo_id = row.ok_or_else(|| {
                azure_error(
                    StatusCode::CONFLICT,
                    "BlobImmutableDueToPolicy",
                    "This cache entry is not open for upload.",
                )
            })?;
            let max_size = crate::cache::size_limit(state, repo_id)
                .await
                .map_err(internal)?;
            Ok(WriteTarget {
                staging: crate::cache::staging_path(state, id),
                blocks: crate::cache::blocks_dir(state, id),
                max_size,
            })
        }
        "artifact-upload" => {
            if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
                return Err(not_found().into());
            }
            Ok(WriteTarget {
                staging: artifact_staging_path(state, id),
                blocks: artifact_blocks_dir(state, id),
                max_size: MAX_ARTIFACT_SIZE,
            })
        }
        _ => Err(not_found().into()),
    }
}

async fn read_source(state: &AppState, kind: &str, id: &str) -> Result<PathBuf, Box<Response>> {
    let id: i64 = id.parse().map_err(|_| not_found())?;
    let ok: bool = match kind {
        "cache" => {
            sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM actions_caches WHERE id = $1 AND committed)",
            )
            .bind(id)
            .fetch_one(&state.db)
            .await
        }
        "artifact" => {
            sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM actions_artifacts WHERE id = $1 AND NOT expired)",
            )
            .bind(id)
            .fetch_one(&state.db)
            .await
        }
        _ => Ok(false),
    }
    .map_err(internal)?;
    if !ok {
        return Err(not_found().into());
    }
    Ok(match kind {
        "cache" => crate::cache::archive_path(state, id),
        _ => crate::server::artifact_path(state, id),
    })
}

fn etag_of(meta: &std::fs::Metadata) -> String {
    let nanos = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!("\"0x{:X}{:X}\"", nanos, meta.len())
}

fn last_modified(meta: &std::fs::Metadata) -> String {
    let t: chrono::DateTime<chrono::Utc> = meta
        .modified()
        .map(Into::into)
        .unwrap_or_else(|_| chrono::Utc::now());
    t.format("%a, %d %b %Y %H:%M:%S GMT").to_string()
}

fn blob_headers(h: &mut HeaderMap, meta: &std::fs::Metadata) {
    common_headers(h);
    let set = |h: &mut HeaderMap, k: &'static str, v: String| {
        if let Ok(v) = HeaderValue::from_str(&v) {
            h.insert(k, v);
        }
    };
    set(h, "etag", etag_of(meta));
    set(h, "last-modified", last_modified(meta));
    h.insert("x-ms-blob-type", HeaderValue::from_static("BlockBlob"));
    h.insert("accept-ranges", HeaderValue::from_static("bytes"));
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
}

/// Parse `bytes=a-b` / `bytes=a-` / `bytes=-n` against `len`.
pub fn parse_range(spec: &str, len: u64) -> Option<Result<(u64, u64), ()>> {
    let spec = spec.trim();
    let r = spec.strip_prefix("bytes=")?;
    if r.contains(',') {
        return None; // multi-range: serve the whole blob
    }
    let (a, b) = r.split_once('-')?;
    let (a, b) = (a.trim(), b.trim());
    let range = if a.is_empty() {
        let n: u64 = b.parse().ok()?;
        if n == 0 || len == 0 {
            return Some(Err(()));
        }
        (len.saturating_sub(n), len - 1)
    } else {
        let start: u64 = a.parse().ok()?;
        let end: u64 = if b.is_empty() {
            len.saturating_sub(1)
        } else {
            b.parse::<u64>().ok()?.min(len.saturating_sub(1))
        };
        if start >= len || end < start {
            return Some(Err(()));
        }
        (start, end)
    };
    Some(Ok(range))
}

/// `GET` (Get Blob) / `HEAD` (Get Blob Properties).
pub async fn get_blob(
    State(state): State<AppState>,
    AxPath((kind, id)): AxPath<(String, String)>,
    Query(q): Query<BlobQuery>,
    headers: HeaderMap,
) -> Response {
    match check_blob_signature(
        &state,
        &kind,
        &id,
        BlobPerm::Read,
        q.se.as_deref(),
        q.sp.as_deref(),
        q.sig.as_deref(),
    ) {
        Ok(true) => {}
        Ok(false) => return auth_failed(),
        Err(e) => return internal(e),
    }
    let path = match read_source(&state, &kind, &id).await {
        Ok(p) => p,
        Err(r) => return *r,
    };
    let mut file = match tokio::fs::File::open(&path).await {
        Ok(f) => f,
        Err(_) => return not_found(),
    };
    let meta = match file.metadata().await {
        Ok(m) => m,
        Err(e) => return internal(e),
    };
    let len = meta.len();
    let range = headers
        .get("x-ms-range")
        .or_else(|| headers.get(header::RANGE))
        .and_then(|v| v.to_str().ok())
        .and_then(|s| parse_range(s, len));
    let (status, start, end) = match range {
        None => (StatusCode::OK, 0, len.saturating_sub(1)),
        Some(Ok((a, b))) => (StatusCode::PARTIAL_CONTENT, a, b),
        Some(Err(())) => {
            let mut r = azure_error(
                StatusCode::RANGE_NOT_SATISFIABLE,
                "InvalidRange",
                "The range specified is invalid for the current size of the resource.",
            );
            if let Ok(v) = HeaderValue::from_str(&format!("bytes */{len}")) {
                r.headers_mut().insert(header::CONTENT_RANGE, v);
            }
            return r;
        }
    };
    let count = if len == 0 { 0 } else { end - start + 1 };
    if start > 0
        && let Err(e) = file.seek(std::io::SeekFrom::Start(start)).await
    {
        return internal(e);
    }
    let stream = tokio_util::io::ReaderStream::new(file.take(count));
    let mut resp = Response::new(Body::from_stream(stream));
    *resp.status_mut() = status;
    let h = resp.headers_mut();
    blob_headers(h, &meta);
    h.insert(header::CONTENT_LENGTH, HeaderValue::from(count));
    if status == StatusCode::PARTIAL_CONTENT
        && let Ok(v) = HeaderValue::from_str(&format!("bytes {start}-{end}/{len}"))
    {
        h.insert(header::CONTENT_RANGE, v);
    }
    resp
}

/// Write a request body to `file` from its current position, refusing to
/// grow it past `max` bytes. Returns the number of bytes written.
pub async fn copy_body(
    body: Body,
    file: &mut tokio::fs::File,
    start: u64,
    max: u64,
) -> Result<u64, CopyError> {
    let mut stream = body.into_data_stream();
    let mut written = 0u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| CopyError::Body(e.to_string()))?;
        if start + written + chunk.len() as u64 > max {
            return Err(CopyError::TooLarge);
        }
        file.write_all(&chunk).await.map_err(CopyError::Io)?;
        written += chunk.len() as u64;
    }
    file.flush().await.map_err(CopyError::Io)?;
    Ok(written)
}

#[derive(Debug)]
pub enum CopyError {
    TooLarge,
    Body(String),
    Io(std::io::Error),
}

fn copy_error(e: CopyError) -> Response {
    match e {
        CopyError::TooLarge => azure_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "RequestBodyTooLarge",
            "The request body is too large and exceeds the maximum permissible limit.",
        ),
        CopyError::Body(m) => azure_error(StatusCode::BAD_REQUEST, "InvalidInput", &m),
        CopyError::Io(e) => internal(e),
    }
}

async fn create_parent(p: &Path) -> std::io::Result<()> {
    tokio::fs::create_dir_all(p.parent().expect("has parent")).await
}

fn created(meta: Option<&std::fs::Metadata>) -> Response {
    let mut resp = StatusCode::CREATED.into_response();
    let h = resp.headers_mut();
    common_headers(h);
    if let Some(meta) = meta {
        if let Ok(v) = HeaderValue::from_str(&etag_of(meta)) {
            h.insert("etag", v);
        }
        if let Ok(v) = HeaderValue::from_str(&last_modified(meta)) {
            h.insert("last-modified", v);
        }
    }
    h.insert(
        "x-ms-request-server-encrypted",
        HeaderValue::from_static("true"),
    );
    resp
}

/// Decode an Azure block id (base64) to a file name.
fn block_file(blocks: &Path, block_id: &str) -> Option<PathBuf> {
    let raw = STANDARD.decode(block_id.trim()).ok()?;
    if raw.is_empty() || raw.len() > MAX_BLOCK_ID_LEN {
        return None;
    }
    Some(blocks.join(hex::encode(raw)))
}

/// `PUT`: Put Blob (no `comp`), Put Block (`comp=block`) or Put Block List
/// (`comp=blocklist`).
pub async fn put(
    State(state): State<AppState>,
    AxPath((kind, id)): AxPath<(String, String)>,
    Query(q): Query<BlobQuery>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    match check_blob_signature(
        &state,
        &kind,
        &id,
        BlobPerm::Write,
        q.se.as_deref(),
        q.sp.as_deref(),
        q.sig.as_deref(),
    ) {
        Ok(true) => {}
        Ok(false) => return auth_failed(),
        Err(e) => return internal(e),
    }
    let target = match write_target(&state, &kind, &id).await {
        Ok(t) => t,
        Err(r) => return *r,
    };
    match q.comp.as_deref() {
        None => put_blob(&target, &headers, body).await,
        Some("block") => put_block(&target, q.blockid.as_deref(), body).await,
        Some("blocklist") => put_block_list(&target, body).await,
        Some(_) => azure_error(
            StatusCode::BAD_REQUEST,
            "InvalidQueryParameterValue",
            "Value for one of the query parameters specified in the request URI is invalid.",
        ),
    }
}

async fn put_blob(target: &WriteTarget, headers: &HeaderMap, body: Body) -> Response {
    match headers.get("x-ms-blob-type").and_then(|v| v.to_str().ok()) {
        Some("BlockBlob") => {}
        Some(_) => {
            return azure_error(
                StatusCode::BAD_REQUEST,
                "InvalidHeaderValue",
                "Only block blobs are supported.",
            );
        }
        None => {
            return azure_error(
                StatusCode::BAD_REQUEST,
                "MissingRequiredHeader",
                "An HTTP header that's mandatory for this request is not specified: x-ms-blob-type.",
            );
        }
    }
    if let Err(e) = create_parent(&target.staging).await {
        return internal(e);
    }
    let tmp = target
        .staging
        .with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
    let mut file = match tokio::fs::File::create(&tmp).await {
        Ok(f) => f,
        Err(e) => return internal(e),
    };
    if let Err(e) = copy_body(body, &mut file, 0, target.max_size).await {
        drop(file);
        let _ = tokio::fs::remove_file(&tmp).await;
        return copy_error(e);
    }
    drop(file);
    if let Err(e) = tokio::fs::rename(&tmp, &target.staging).await {
        return internal(e);
    }
    let _ = tokio::fs::remove_dir_all(&target.blocks).await;
    created(tokio::fs::metadata(&target.staging).await.ok().as_ref())
}

async fn put_block(target: &WriteTarget, block_id: Option<&str>, body: Body) -> Response {
    let Some(path) = block_id.and_then(|b| block_file(&target.blocks, b)) else {
        return azure_error(
            StatusCode::BAD_REQUEST,
            "InvalidQueryParameterValue",
            "Value for one of the query parameters specified in the request URI is invalid: blockid.",
        );
    };
    if let Err(e) = tokio::fs::create_dir_all(&target.blocks).await {
        return internal(e);
    }
    let tmp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
    let mut file = match tokio::fs::File::create(&tmp).await {
        Ok(f) => f,
        Err(e) => return internal(e),
    };
    // A single block can't exceed the blob limit; the list is checked again.
    if let Err(e) = copy_body(body, &mut file, 0, target.max_size).await {
        drop(file);
        let _ = tokio::fs::remove_file(&tmp).await;
        return copy_error(e);
    }
    drop(file);
    if let Err(e) = tokio::fs::rename(&tmp, &path).await {
        return internal(e);
    }
    created(None)
}

/// Block ids of a `<BlockList>` body (`Latest`, `Uncommitted`, `Committed`).
pub fn parse_block_list(xml: &str) -> Option<Vec<String>> {
    let start = xml.find("<BlockList")?;
    let mut rest = &xml[start..];
    rest = &rest[rest.find('>')? + 1..];
    let mut ids = Vec::new();
    loop {
        let open = rest.find('<')?;
        rest = &rest[open + 1..];
        if rest.starts_with("/BlockList") {
            return Some(ids);
        }
        let close = rest.find('>')?;
        let tag = rest[..close].trim().to_string();
        if !matches!(tag.as_str(), "Latest" | "Uncommitted" | "Committed") {
            return None;
        }
        rest = &rest[close + 1..];
        let end_tag = format!("</{tag}>");
        let end = rest.find(&end_tag)?;
        ids.push(rest[..end].trim().to_string());
        rest = &rest[end + end_tag.len()..];
    }
}

async fn put_block_list(target: &WriteTarget, body: Body) -> Response {
    let bytes = match axum::body::to_bytes(body, 4 << 20).await {
        Ok(b) => b,
        Err(_) => {
            return azure_error(
                StatusCode::BAD_REQUEST,
                "InvalidXmlDocument",
                "XML specified is not syntactically valid.",
            );
        }
    };
    let Some(ids) = std::str::from_utf8(&bytes).ok().and_then(parse_block_list) else {
        return azure_error(
            StatusCode::BAD_REQUEST,
            "InvalidXmlDocument",
            "XML specified is not syntactically valid.",
        );
    };
    if ids.len() > MAX_BLOCKS {
        return azure_error(
            StatusCode::BAD_REQUEST,
            "BlockListTooLong",
            "The block list may not contain more than 50,000 blocks.",
        );
    }
    let mut files = Vec::with_capacity(ids.len());
    for id in &ids {
        match block_file(&target.blocks, id) {
            Some(p) if tokio::fs::try_exists(&p).await.unwrap_or(false) => files.push(p),
            _ => {
                return azure_error(
                    StatusCode::BAD_REQUEST,
                    "InvalidBlockList",
                    "The specified block list is invalid.",
                );
            }
        }
    }
    if let Err(e) = create_parent(&target.staging).await {
        return internal(e);
    }
    let tmp = target
        .staging
        .with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
    let result: std::io::Result<u64> = async {
        let mut out = tokio::fs::File::create(&tmp).await?;
        let mut total = 0u64;
        for f in &files {
            let mut src = tokio::fs::File::open(f).await?;
            total += tokio::io::copy(&mut src, &mut out).await?;
            if total > target.max_size {
                return Err(std::io::Error::other("too large"));
            }
        }
        out.flush().await?;
        Ok(total)
    }
    .await;
    if let Err(e) = result {
        let _ = tokio::fs::remove_file(&tmp).await;
        return if e.to_string() == "too large" {
            copy_error(CopyError::TooLarge)
        } else {
            internal(e)
        };
    }
    if let Err(e) = tokio::fs::rename(&tmp, &target.staging).await {
        return internal(e);
    }
    let _ = tokio::fs::remove_dir_all(&target.blocks).await;
    created(tokio::fs::metadata(&target.staging).await.ok().as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges() {
        assert_eq!(parse_range("bytes=0-9", 100), Some(Ok((0, 9))));
        assert_eq!(parse_range("bytes=90-", 100), Some(Ok((90, 99))));
        assert_eq!(parse_range("bytes=-10", 100), Some(Ok((90, 99))));
        assert_eq!(parse_range("bytes=50-500", 100), Some(Ok((50, 99))));
        assert_eq!(parse_range("bytes=100-", 100), Some(Err(())));
        assert_eq!(parse_range("bytes=0-1,5-6", 100), None);
        assert_eq!(parse_range("items=0-1", 100), None);
    }

    #[test]
    fn block_lists() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<BlockList><Latest>AAAA</Latest><Uncommitted>AAAB</Uncommitted>
<Committed>AAAC</Committed></BlockList>"#;
        assert_eq!(
            parse_block_list(xml),
            Some(vec!["AAAA".into(), "AAAB".into(), "AAAC".into()])
        );
        assert_eq!(parse_block_list("<BlockList></BlockList>"), Some(vec![]));
        assert_eq!(
            parse_block_list("<BlockList><Bogus>x</Bogus></BlockList>"),
            None
        );
        assert_eq!(parse_block_list("nope"), None);
    }

    #[test]
    fn block_ids() {
        let dir = Path::new("/b");
        assert_eq!(block_file(dir, "AAEC"), Some(PathBuf::from("/b/000102")));
        assert_eq!(block_file(dir, "!!"), None);
        assert_eq!(block_file(dir, ""), None);
    }
}
