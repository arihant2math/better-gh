//! Legacy cache protocol (`ACTIONS_CACHE_URL` + `_apis/artifactcache/...`),
//! used by `@actions/cache` on GHES-like hosts (any host other than
//! github.com) and by the native `actions/cache` of the runner.
//!
//! | Method | Path | Body → Response |
//! |---|---|---|
//! | GET | `cache?keys=k1,k2&version=v` | → 200 `{cacheKey, scope, creationTime, archiveLocation}` or 204 |
//! | POST | `caches` | `{key, version, cacheSize}` → 201 `{cacheId}`; 409 exists; 400 too large |
//! | PATCH | `caches/{id}` | octet-stream chunk, `Content-Range: bytes a-b/*` → 204 |
//! | POST | `caches/{id}` | `{size}` → 204 (commit) |

use axum::Router;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use bgh_core::AppState;
use serde::Deserialize;
use serde_json::json;
use tokio::io::AsyncSeekExt;

use super::{CommitError, Reserved};
use crate::results::blob::{CopyError, copy_body};
use crate::runtime::{BlobPerm, RUNTIME_PREFIX, RuntimeJob, signed_blob_url};

/// Archive download URLs stay valid for an hour.
const DOWNLOAD_URL_TTL: i64 = 3600;

pub fn routes() -> Router<AppState> {
    let p = |s: &str| format!("{RUNTIME_PREFIX}_apis/artifactcache/{s}");
    Router::new()
        .route(&p("cache"), get(get_entry))
        .route(&p("caches"), post(reserve))
        .route(&p("caches/{id}"), post(commit).patch(upload_chunk))
}

/// An Azure DevOps style error body (what the toolkit logs as `message`).
fn error(status: StatusCode, type_key: &str, message: impl Into<String>) -> Response {
    (
        status,
        axum::Json(json!({
            "$id": "1",
            "innerException": null,
            "message": message.into(),
            "typeName": format!("Microsoft.Azure.DevOps.PipelineCache.WebApi.{type_key}, Microsoft.Azure.DevOps.PipelineCache.WebApi"),
            "typeKey": type_key,
            "errorCode": 0,
            "eventId": 3000,
        })),
    )
        .into_response()
}

fn internal(e: impl std::fmt::Display) -> Response {
    tracing::error!(%e, "cache service error");
    error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "InternalServerError",
        "internal error",
    )
}

#[derive(Debug, Deserialize)]
pub struct GetQuery {
    #[serde(default)]
    keys: String,
    #[serde(default)]
    version: String,
}

async fn get_entry(
    State(state): State<AppState>,
    rj: RuntimeJob,
    Query(q): Query<GetQuery>,
) -> Response {
    let keys: Vec<String> = q
        .keys
        .split(',')
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .map(String::from)
        .collect();
    if keys.is_empty() || keys.len() > super::MAX_KEYS || q.version.is_empty() {
        return error(
            StatusCode::BAD_REQUEST,
            "ArgumentException",
            "keys (1 to 10) and version are required",
        );
    }
    if let Some(msg) = keys.iter().find_map(|k| super::validate_key(k).err()) {
        return error(StatusCode::BAD_REQUEST, "ArgumentException", msg);
    }
    let found = match super::lookup(&state, rj.repo.id, &rj.read_scopes(), &keys, &q.version).await
    {
        Ok(f) => f,
        Err(e) => return internal(e),
    };
    let Some(entry) = found else {
        return StatusCode::NO_CONTENT.into_response();
    };
    let url = match signed_blob_url(
        &state,
        "cache",
        &entry.id.to_string(),
        BlobPerm::Read,
        DOWNLOAD_URL_TTL,
    ) {
        Ok(u) => u,
        Err(e) => return internal(e),
    };
    axum::Json(json!({
        "scope": entry.git_ref,
        "cacheKey": entry.key,
        "cacheVersion": entry.version,
        "creationTime": bgh_core::time::Timestamp(entry.created_at),
        "archiveLocation": url,
    }))
    .into_response()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReserveRequest {
    #[serde(default)]
    key: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    cache_size: Option<i64>,
}

async fn reserve(
    State(state): State<AppState>,
    rj: RuntimeJob,
    body: axum::body::Bytes,
) -> Response {
    let Ok(req) = serde_json::from_slice::<ReserveRequest>(&body) else {
        return error(
            StatusCode::BAD_REQUEST,
            "ArgumentException",
            "invalid JSON body",
        );
    };
    if let Err(msg) = super::validate_key(&req.key) {
        return error(StatusCode::BAD_REQUEST, "ArgumentException", msg);
    }
    if req.version.is_empty() {
        return error(
            StatusCode::BAD_REQUEST,
            "ArgumentException",
            "version is required",
        );
    }
    match super::reserve(&state, &rj, &req.key, &req.version, req.cache_size).await {
        Ok(Reserved::Ok(row)) => {
            (StatusCode::CREATED, axum::Json(json!({"cacheId": row.id}))).into_response()
        }
        Ok(Reserved::Exists) => error(
            StatusCode::CONFLICT,
            "ArtifactCacheAlreadyExistsException",
            format!(
                "Cache already exists. Scope: {}, Key: {}, Version: {}",
                rj.write_scope(),
                req.key,
                req.version
            ),
        ),
        Ok(Reserved::TooLarge { limit }) => error(
            StatusCode::BAD_REQUEST,
            "ArgumentException",
            format!(
                "Cache size of ~{} MB ({} B) is over the {} MB limit of this repository, not saving cache.",
                req.cache_size.unwrap_or(0) >> 20,
                req.cache_size.unwrap_or(0),
                limit >> 20
            ),
        ),
        Err(e) => internal(e),
    }
}

/// `bytes a-b/*` → (a, b).
fn content_range(headers: &HeaderMap) -> Option<(u64, u64)> {
    let v = headers.get(header::CONTENT_RANGE)?.to_str().ok()?;
    let r = v.trim().strip_prefix("bytes ")?;
    let (range, _) = r.split_once('/')?;
    let (a, b) = range.split_once('-')?;
    let (a, b): (u64, u64) = (a.trim().parse().ok()?, b.trim().parse().ok()?);
    (b >= a).then_some((a, b))
}

async fn upload_chunk(
    State(state): State<AppState>,
    rj: RuntimeJob,
    Path(id): Path<i64>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let entry = match super::reserved_by(&state, &rj, id).await {
        Ok(Some(e)) => e,
        Ok(None) => {
            return error(
                StatusCode::NOT_FOUND,
                "ArtifactCacheNotFoundException",
                format!("Cache entry {id} is not reserved by this job."),
            );
        }
        Err(e) => return internal(e),
    };
    let Some((start, end)) = content_range(&headers) else {
        return error(
            StatusCode::BAD_REQUEST,
            "ArgumentException",
            "Content-Range: bytes <start>-<end>/* is required",
        );
    };
    let limit = match super::size_limit(&state, entry.repo_id).await {
        Ok(l) => l,
        Err(e) => return internal(e),
    };
    if end >= limit {
        return error(
            StatusCode::BAD_REQUEST,
            "ArgumentException",
            "Cache size is over the limit of this repository.",
        );
    }
    let path = super::staging_path(&state, id);
    let result: Result<u64, CopyError> = async {
        tokio::fs::create_dir_all(super::dir(&state))
            .await
            .map_err(CopyError::Io)?;
        let mut f = tokio::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .await
            .map_err(CopyError::Io)?;
        f.seek(std::io::SeekFrom::Start(start))
            .await
            .map_err(CopyError::Io)?;
        copy_body(body, &mut f, start, end + 1).await
    }
    .await;
    match result {
        Ok(n) if n == end - start + 1 => StatusCode::NO_CONTENT.into_response(),
        Ok(n) => error(
            StatusCode::BAD_REQUEST,
            "ArgumentException",
            format!(
                "Content-Range declares {} bytes but {n} were sent",
                end - start + 1
            ),
        ),
        Err(CopyError::TooLarge) => error(
            StatusCode::BAD_REQUEST,
            "ArgumentException",
            "The chunk is longer than its Content-Range.",
        ),
        Err(CopyError::Body(m)) => error(StatusCode::BAD_REQUEST, "ArgumentException", m),
        Err(CopyError::Io(e)) => internal(e),
    }
}

#[derive(Debug, Deserialize)]
pub struct CommitRequest {
    #[serde(default)]
    size: Option<i64>,
}

async fn commit(
    State(state): State<AppState>,
    rj: RuntimeJob,
    Path(id): Path<i64>,
    body: axum::body::Bytes,
) -> Response {
    let req: CommitRequest = serde_json::from_slice(&body).unwrap_or(CommitRequest { size: None });
    let entry = match super::reserved_by(&state, &rj, id).await {
        Ok(Some(e)) => e,
        Ok(None) => {
            return error(
                StatusCode::NOT_FOUND,
                "ArtifactCacheNotFoundException",
                format!("Cache entry {id} is not reserved by this job."),
            );
        }
        Err(e) => return internal(e),
    };
    match super::commit(&state, &entry, req.size).await {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(CommitError::Other(e)) => internal(e),
        Err(e) => error(StatusCode::BAD_REQUEST, "ArgumentException", e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_ranges() {
        let mut h = HeaderMap::new();
        h.insert(header::CONTENT_RANGE, "bytes 0-1023/*".parse().unwrap());
        assert_eq!(content_range(&h), Some((0, 1023)));
        h.insert(header::CONTENT_RANGE, "bytes 10-5/*".parse().unwrap());
        assert_eq!(content_range(&h), None);
        h.insert(header::CONTENT_RANGE, "items 0-1/*".parse().unwrap());
        assert_eq!(content_range(&h), None);
    }
}
