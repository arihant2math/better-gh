//! Basic transfer: object upload, download and verify.

use axum::body::{Body, Bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bgh_core::prelude::*;
use bgh_git::lfs::{PutError, is_valid_oid};
use futures::TryStreamExt;
use serde::Deserialize;
use tokio_util::io::{ReaderStream, StreamReader};

use super::{LfsResult, authorize, has_object, object_store};

/// Uploads without `Content-Length` may be at most this large.
pub const MAX_UNSIZED_UPLOAD: u64 = 5 * 1024 * 1024 * 1024;

/// `GET /{owner}/{repo}/info/lfs/objects/{oid}`
pub async fn download(
    State(state): State<AppState>,
    Path((owner, repo, oid)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> LfsResult<Response> {
    let lfs = authorize(&state, &headers, &owner, &repo, false).await?;
    if !is_valid_oid(&oid) || !has_object(&state, lfs.access.repo.id, &oid).await? {
        return Err(ApiError::NotFound.into());
    }
    let file = object_store(&state)
        .open(&oid)
        .await
        .map_err(|_| ApiError::NotFound)?;
    let len = file.metadata().await.map_err(ApiError::internal)?.len();
    let mut resp = Response::new(Body::from_stream(ReaderStream::with_capacity(
        file,
        256 * 1024,
    )));
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    h.insert(header::CONTENT_LENGTH, HeaderValue::from(len));
    Ok(resp)
}

/// `PUT /{owner}/{repo}/info/lfs/objects/{oid}`: stream to disk, verify
/// SHA-256 and size, link to the repository and account its size.
pub async fn upload(
    State(state): State<AppState>,
    Path((owner, repo, oid)): Path<(String, String, String)>,
    req: Request,
) -> LfsResult<Response> {
    let (parts, body) = req.into_parts();
    let lfs = authorize(&state, &parts.headers, &owner, &repo, true).await?;
    if !is_valid_oid(&oid) {
        return Err(ApiError::unprocessable("Invalid object id").into());
    }
    let declared: Option<u64> = parts
        .headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok());
    let stream = body.into_data_stream().map_err(std::io::Error::other);
    let mut reader = StreamReader::new(stream);
    let store = object_store(&state);
    let size = match declared {
        Some(n) => store.put(&oid, n, &mut reader).await,
        None => {
            // Unknown length: hash-verify only, bounded size.
            put_unsized(&store, &oid, &mut reader).await
        }
    };
    let size = match size {
        Ok(n) => n,
        Err(PutError::Hash) => {
            return Err(ApiError::unprocessable("Object contents do not match the oid").into());
        }
        Err(PutError::Size { .. }) => {
            return Err(ApiError::unprocessable("Object size does not match").into());
        }
        Err(PutError::Io(e)) => return Err(ApiError::internal(e).into()),
    };
    link(
        &state,
        lfs.access.repo.id,
        &oid,
        size as i64,
        lfs.user.as_ref().map(|u| u.user.id),
    )
    .await?;
    Ok(StatusCode::OK.into_response())
}

async fn put_unsized<R: tokio::io::AsyncRead + Unpin>(
    store: &bgh_git::lfs::LfsStore,
    oid: &str,
    reader: &mut R,
) -> Result<u64, PutError> {
    // Buffer into a temp file first to learn the size, then verify.
    let dir = store.root.join("tmp");
    tokio::fs::create_dir_all(&dir).await?;
    let tmp = tempfile::NamedTempFile::new_in(&dir)?;
    let mut file = tokio::fs::File::from_std(tmp.reopen()?);
    let mut limited = tokio::io::AsyncReadExt::take(reader, MAX_UNSIZED_UPLOAD + 1);
    let n = tokio::io::copy(&mut limited, &mut file).await?;
    if n > MAX_UNSIZED_UPLOAD {
        return Err(PutError::Size {
            expected: MAX_UNSIZED_UPLOAD,
            actual: n,
        });
    }
    drop(file);
    let mut f = tokio::fs::File::open(tmp.path()).await?;
    store.put(oid, n, &mut f).await
}

/// Link an uploaded object to a repository (idempotent) and update
/// `repositories.lfs_size`.
pub async fn link(
    state: &AppState,
    repo_id: i64,
    oid: &str,
    size: i64,
    uploader_id: Option<i64>,
) -> ApiResult<()> {
    sqlx::query(
        "WITH ins AS (
             INSERT INTO lfs_objects (repo_id, oid, size, uploader_id)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (repo_id, oid) DO NOTHING
             RETURNING size)
         UPDATE repositories
            SET lfs_size = lfs_size + (SELECT coalesce(sum(size), 0) FROM ins)
          WHERE id = $1",
    )
    .bind(repo_id)
    .bind(oid)
    .bind(size)
    .bind(uploader_id)
    .execute(&state.db)
    .await?;
    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct VerifyRequest {
    pub oid: String,
    pub size: i64,
}

/// `POST /{owner}/{repo}/info/lfs/objects/{oid}/verify`
pub async fn verify(
    State(state): State<AppState>,
    Path((owner, repo, oid)): Path<(String, String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> LfsResult<Response> {
    let lfs = authorize(&state, &headers, &owner, &repo, true).await?;
    let req: VerifyRequest = serde_json::from_slice(&body)
        .map_err(|_| ApiError::bad_request("Problems parsing JSON"))?;
    if req.oid != oid || !is_valid_oid(&oid) {
        return Err(ApiError::unprocessable("Invalid object id").into());
    }
    let size: Option<i64> =
        sqlx::query_scalar("SELECT size FROM lfs_objects WHERE repo_id = $1 AND oid = $2")
            .bind(lfs.access.repo.id)
            .bind(&oid)
            .fetch_optional(&state.db)
            .await?;
    match size {
        Some(s) if s == req.size => Ok(super::lfs_json(StatusCode::OK, &serde_json::json!({}))),
        Some(_) => Err(ApiError::unprocessable("Object size does not match").into()),
        None => Err(ApiError::NotFound.into()),
    }
}
