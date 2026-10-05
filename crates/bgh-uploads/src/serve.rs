//! `GET /user-attachments/assets/{uuid}` and
//! `GET /user-attachments/files/{id}/{name}`.
//!
//! Attachments of a private repository need read access to it (404
//! otherwise). Every response is locked down: `nosniff`, a sandboxing CSP,
//! and `Content-Disposition: attachment` for anything that is not a raster
//! image or a video (SVG and HTML included). Videos support Range.

use std::io::SeekFrom;

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bgh_core::prelude::*;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;

use crate::model::AttachmentRow;
use crate::storage;

pub const CSP: &str = "default-src 'none'; sandbox";

pub async fn asset(
    State(state): State<AppState>,
    auth: MaybeUser,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    let uuid = uuid::Uuid::parse_str(&id).map_err(|_| ApiError::NotFound)?;
    let row: AttachmentRow = sqlx::query_as(&format!(
        "SELECT {} FROM attachments WHERE uuid = $1",
        AttachmentRow::COLUMNS
    ))
    .bind(uuid)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)?;
    serve(&state, auth.as_ref(), &headers, row).await
}

pub async fn file(
    State(state): State<AppState>,
    auth: MaybeUser,
    headers: HeaderMap,
    Path((id, name)): Path<(i64, String)>,
) -> ApiResult<Response> {
    // The name is part of the URL so ids alone can't be enumerated.
    let row: AttachmentRow = sqlx::query_as(&format!(
        "SELECT {} FROM attachments WHERE id = $1 AND name = $2",
        AttachmentRow::COLUMNS
    ))
    .bind(id)
    .bind(&name)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)?;
    serve(&state, auth.as_ref(), &headers, row).await
}

/// 404 unless the caller may read the attachment. Returns whether it is
/// public (cacheable by shared caches; never in private mode, where every
/// download needs sign-in).
async fn check_access(
    state: &AppState,
    auth: Option<&AuthContext>,
    row: &AttachmentRow,
) -> ApiResult<bool> {
    let shareable = !bgh_core::privacy::private_mode(state).await?;
    let Some(repo_id) = row.repo_id else {
        return Ok(shareable);
    };
    let repo = db::Repository::find(&state.db, repo_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let owner = db::User::find(&state.db, repo.owner_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let public = shareable && !repo.is_private();
    RepoAccess::for_repo(state, auth, repo, owner)
        .await
        .map_err(|_| ApiError::NotFound)?;
    Ok(public)
}

/// A single `bytes=` range resolved against `len`: `Ok(None)` without a
/// (usable) Range header, `Err(())` when unsatisfiable.
fn parse_range(headers: &HeaderMap, len: u64) -> Result<Option<(u64, u64)>, ()> {
    let Some(spec) = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().strip_prefix("bytes="))
    else {
        return Ok(None);
    };
    // Multiple ranges: serve the whole body (allowed by RFC 9110).
    if spec.contains(',') {
        return Ok(None);
    }
    let Some((start, end)) = spec.trim().split_once('-') else {
        return Ok(None);
    };
    let (start, end) = match (start.trim(), end.trim()) {
        ("", "") => return Ok(None),
        ("", n) => {
            let n: u64 = n.parse().map_err(|_| ())?;
            if n == 0 || len == 0 {
                return Err(());
            }
            (len.saturating_sub(n), len - 1)
        }
        (s, e) => {
            let s: u64 = s.parse().map_err(|_| ())?;
            let e: u64 = if e.is_empty() {
                len.saturating_sub(1)
            } else {
                e.parse::<u64>().map_err(|_| ())?.min(len.saturating_sub(1))
            };
            if s >= len || s > e {
                return Err(());
            }
            (s, e)
        }
    };
    Ok(Some((start, end)))
}

async fn serve(
    state: &AppState,
    auth: Option<&AuthContext>,
    headers: &HeaderMap,
    row: AttachmentRow,
) -> ApiResult<Response> {
    let public = check_access(state, auth, &row).await?;
    let path = storage::blob_path(state, &row.sha256).map_err(ApiError::internal)?;
    let mut file = match tokio::fs::File::open(&path).await {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(ApiError::NotFound),
        Err(e) => return Err(ApiError::internal(e)),
    };
    let len = file.metadata().await.map_err(ApiError::internal)?.len();
    let etag = format!("\"{}\"", row.sha256);

    let mut out = HeaderMap::new();
    let kind = row.kind();
    let disposition = format!(
        "{}; filename=\"{}\"; filename*=UTF-8''{}",
        if kind.inline() {
            "inline"
        } else {
            "attachment"
        },
        row.name
            .chars()
            .map(|c| if c.is_ascii() && !matches!(c, '"' | '\\') {
                c
            } else {
                '_'
            })
            .collect::<String>(),
        bgh_core::urls::encode_segment(&row.name)
    );
    let set = |out: &mut HeaderMap, k: header::HeaderName, v: &str| {
        if let Ok(v) = HeaderValue::from_str(v) {
            out.insert(k, v);
        }
    };
    set(&mut out, header::CONTENT_TYPE, &row.content_type);
    set(&mut out, header::CONTENT_DISPOSITION, &disposition);
    set(&mut out, header::X_CONTENT_TYPE_OPTIONS, "nosniff");
    set(&mut out, header::CONTENT_SECURITY_POLICY, CSP);
    set(&mut out, header::ETAG, &etag);
    set(&mut out, header::ACCEPT_RANGES, "bytes");
    set(
        &mut out,
        header::CACHE_CONTROL,
        if public {
            "public, max-age=31536000, immutable"
        } else {
            "private, max-age=3600"
        },
    );

    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|t| t.trim() == etag || t.trim() == "*"))
    {
        return Ok((StatusCode::NOT_MODIFIED, out).into_response());
    }

    match parse_range(headers, len) {
        Err(()) => {
            set(&mut out, header::CONTENT_RANGE, &format!("bytes */{len}"));
            Ok((StatusCode::RANGE_NOT_SATISFIABLE, out).into_response())
        }
        Ok(Some((start, end))) => {
            file.seek(SeekFrom::Start(start))
                .await
                .map_err(ApiError::internal)?;
            let n = end - start + 1;
            set(
                &mut out,
                header::CONTENT_RANGE,
                &format!("bytes {start}-{end}/{len}"),
            );
            out.insert(header::CONTENT_LENGTH, HeaderValue::from(n));
            let body = Body::from_stream(ReaderStream::with_capacity(file.take(n), 64 * 1024));
            Ok((StatusCode::PARTIAL_CONTENT, out, body).into_response())
        }
        Ok(None) => {
            out.insert(header::CONTENT_LENGTH, HeaderValue::from(len));
            let body = Body::from_stream(ReaderStream::with_capacity(file, 64 * 1024));
            Ok((StatusCode::OK, out, body).into_response())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range(v: &str, len: u64) -> Result<Option<(u64, u64)>, ()> {
        let mut h = HeaderMap::new();
        h.insert(header::RANGE, HeaderValue::from_str(v).unwrap());
        parse_range(&h, len)
    }

    #[test]
    fn ranges() {
        assert_eq!(range("bytes=0-9", 100), Ok(Some((0, 9))));
        assert_eq!(range("bytes=90-", 100), Ok(Some((90, 99))));
        assert_eq!(range("bytes=-10", 100), Ok(Some((90, 99))));
        assert_eq!(range("bytes=50-500", 100), Ok(Some((50, 99))));
        assert_eq!(range("bytes=100-", 100), Err(()));
        assert_eq!(range("bytes=0-1,5-6", 100), Ok(None));
        assert_eq!(range("items=0-1", 100), Ok(None));
    }
}
