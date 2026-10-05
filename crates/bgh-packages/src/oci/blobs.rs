//! Blobs and blob uploads.

use axum::body::Body;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::Response;
use bgh_core::AppState;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use super::{Caller, OciError, OciResult, Query, Repo, authorize, builder};
use crate::digest::Digest;
use crate::model::PackageRow;
use crate::{ops, storage};

fn parse_digest(s: &str) -> OciResult<Digest> {
    Digest::parse(s).ok_or_else(|| OciError::digest_invalid("invalid or unsupported digest"))
}

/// Size of `digest` if it is linked into `package_id`.
async fn linked_size(state: &AppState, package_id: i64, digest: &str) -> OciResult<Option<i64>> {
    Ok(sqlx::query_scalar(
        "SELECT b.size FROM package_blob_links l JOIN package_blobs b ON b.digest = l.digest
          WHERE l.package_id = $1 AND l.digest = $2",
    )
    .bind(package_id)
    .bind(digest)
    .fetch_optional(&state.db)
    .await?)
}

/// `GET`/`HEAD /v2/{name}/blobs/{digest}` (single `Range` supported).
pub async fn get_blob(
    state: &AppState,
    caller: &Caller,
    name: &str,
    digest: &str,
    headers: &HeaderMap,
    head: bool,
) -> OciResult {
    let (repo, _) = authorize(state, caller, name, "pull").await?;
    let digest = parse_digest(digest)?;
    let Some(pkg) = repo.package else {
        return Err(OciError::blob_unknown());
    };
    let Some(size) = linked_size(state, pkg.id, &digest.to_string()).await? else {
        return Err(OciError::blob_unknown());
    };
    let size = size as u64;
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(|r| parse_range(r, size));
    let (status, start, len) = match range {
        None => (StatusCode::OK, 0, size),
        Some(Some((s, e))) => (StatusCode::PARTIAL_CONTENT, s, e - s + 1),
        Some(None) => {
            return Err(OciError::new(
                StatusCode::RANGE_NOT_SATISFIABLE,
                "BLOB_UNKNOWN",
                "requested range not satisfiable",
            )
            .header("content-range", format!("bytes */{size}")));
        }
    };
    let mut b = builder(status)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::CONTENT_LENGTH, len)
        .header(header::ACCEPT_RANGES, "bytes")
        .header("docker-content-digest", digest.to_string())
        .header(header::ETAG, format!("\"{digest}\""))
        .header(
            header::CACHE_CONTROL,
            "private, max-age=31536000, immutable",
        );
    if status == StatusCode::PARTIAL_CONTENT {
        b = b.header(
            header::CONTENT_RANGE,
            format!("bytes {start}-{}/{size}", start + len - 1),
        );
    }
    if head {
        return Ok(b.body(Body::empty()).expect("response"));
    }
    let mut file = tokio::fs::File::open(storage::blob_path(state, &digest))
        .await
        .map_err(|err| {
            tracing::error!(%digest, ?err, "blob file missing");
            OciError::blob_unknown()
        })?;
    if start > 0 {
        file.seek(std::io::SeekFrom::Start(start)).await?;
    }
    let stream = tokio_util::io::ReaderStream::with_capacity(file.take(len), 64 * 1024);
    Ok(b.body(Body::from_stream(stream)).expect("response"))
}

/// `bytes=a-b`, `bytes=a-`, `bytes=-n` → inclusive `(start, end)`;
/// `None` when unsatisfiable.
fn parse_range(v: &str, size: u64) -> Option<(u64, u64)> {
    let spec = v.trim().strip_prefix("bytes=")?;
    if spec.contains(',') || size == 0 {
        return None;
    }
    let (a, b) = spec.split_once('-')?;
    let (start, end) = match (a.trim(), b.trim()) {
        ("", n) => {
            let n: u64 = n.parse().ok()?;
            (size.saturating_sub(n), size - 1)
        }
        (s, "") => (s.parse().ok()?, size - 1),
        (s, e) => (s.parse().ok()?, e.parse::<u64>().ok()?.min(size - 1)),
    };
    (start <= end && start < size).then_some((start, end))
}

/// `DELETE /v2/{name}/blobs/{digest}`: unlink the blob from the package
/// (the GC removes unreferenced content).
pub async fn delete_blob(state: &AppState, caller: &Caller, name: &str, digest: &str) -> OciResult {
    let (repo, _) = authorize(state, caller, name, "delete").await?;
    let digest = parse_digest(digest)?;
    let Some(pkg) = repo.package else {
        return Err(OciError::blob_unknown());
    };
    let mut tx = state.db.begin().await?;
    let n = sqlx::query("DELETE FROM package_blob_links WHERE package_id = $1 AND digest = $2")
        .bind(pkg.id)
        .bind(digest.to_string())
        .execute(&mut *tx)
        .await?
        .rows_affected();
    if n == 0 {
        return Err(OciError::blob_unknown());
    }
    ops::recompute_size(&mut tx, pkg.id).await?;
    tx.commit().await?;
    Ok(builder(StatusCode::ACCEPTED)
        .header(header::CONTENT_LENGTH, 0)
        .body(Body::empty())
        .expect("response"))
}

async fn writable_package(state: &AppState, caller: &Caller, repo: &Repo) -> OciResult<PackageRow> {
    if let Some(p) = &repo.package {
        return Ok(p.clone());
    }
    Ok(ops::ensure_package(
        state,
        &repo.owner,
        &repo.name,
        caller.user_id(),
        caller.job_repo(),
    )
    .await?)
}

fn upload_location(name: &str, id: &uuid::Uuid) -> String {
    format!("/v2/{name}/blobs/uploads/{id}")
}

fn created(name: &str, digest: &Digest) -> Response {
    builder(StatusCode::CREATED)
        .header(header::LOCATION, format!("/v2/{name}/blobs/{digest}"))
        .header("docker-content-digest", digest.to_string())
        .header(header::CONTENT_LENGTH, 0)
        .body(Body::empty())
        .expect("response")
}

fn accepted_upload(name: &str, id: &uuid::Uuid, size: i64, status: StatusCode) -> Response {
    // `Range` is inclusive; an empty upload reports `0-0` like the
    // reference registry.
    builder(status)
        .header(header::LOCATION, upload_location(name, id))
        .header(header::RANGE, format!("0-{}", (size - 1).max(0)))
        .header("docker-upload-uuid", id.to_string())
        .header(header::CONTENT_LENGTH, 0)
        .body(Body::empty())
        .expect("response")
}

/// Link a verified blob file into the package (moves `file` into the blob
/// store). Holds the shared blob lock so the GC can't delete the content
/// between the move and the database rows.
async fn link_blob(
    state: &AppState,
    pkg: &PackageRow,
    file: &std::path::Path,
    digest: &Digest,
    size: u64,
) -> OciResult<()> {
    let mut tx = state.db.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock_shared($1)")
        .bind(storage::BLOB_LOCK_KEY)
        .execute(&mut *tx)
        .await?;
    storage::commit_blob(state, file, digest).await?;
    sqlx::query("INSERT INTO package_blobs (digest, size) VALUES ($1, $2) ON CONFLICT DO NOTHING")
        .bind(digest.to_string())
        .bind(size as i64)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT INTO package_blob_links (package_id, digest) VALUES ($1, $2)
         ON CONFLICT (package_id, digest) DO UPDATE SET created_at = now()",
    )
    .bind(pkg.id)
    .bind(digest.to_string())
    .execute(&mut *tx)
    .await?;
    ops::recompute_size(&mut tx, pkg.id).await?;
    tx.commit().await?;
    Ok(())
}

async fn quota(state: &AppState, owner_id: i64, size: u64) -> OciResult<()> {
    match ops::check_quota(state, owner_id, size).await? {
        Ok(()) => Ok(()),
        Err(msg) => Err(OciError::denied(msg)),
    }
}

/// `POST /v2/{name}/blobs/uploads/`: start an upload, upload monolithically
/// (`?digest=`), or mount from another repository (`?mount=&from=`).
pub async fn start_upload(
    state: &AppState,
    caller: &Caller,
    name: &str,
    query: &Query,
    body: Body,
) -> OciResult {
    let (repo, _) = authorize(state, caller, name, "push").await?;

    if let Some(mount) = query.get("mount") {
        let digest = parse_digest(mount)?;
        let source = match query.get("from") {
            Some(from) if from != name => match authorize(state, caller, from, "pull").await {
                Ok((src, _)) => src.package,
                Err(_) => None,
            },
            // Without `from`, only content already in this package mounts.
            _ => repo.package.clone(),
        };
        if let Some(src) = source
            && let Some(size) = linked_size(state, src.id, &digest.to_string()).await?
        {
            let pkg = writable_package(state, caller, &repo).await?;
            if src.id != pkg.id {
                quota(state, repo.owner.id, size as u64).await?;
            }
            let mut tx = state.db.begin().await?;
            sqlx::query(
                "INSERT INTO package_blob_links (package_id, digest) VALUES ($1, $2)
                 ON CONFLICT (package_id, digest) DO UPDATE SET created_at = now()",
            )
            .bind(pkg.id)
            .bind(digest.to_string())
            .execute(&mut *tx)
            .await?;
            ops::recompute_size(&mut tx, pkg.id).await?;
            tx.commit().await?;
            return Ok(created(name, &digest));
        }
        // Not mountable: fall back to a regular upload session.
    }

    let pkg = writable_package(state, caller, &repo).await?;
    let id = uuid::Uuid::new_v4();
    let path = storage::upload_path(state, &id);

    if let Some(d) = query.get("digest") {
        let digest = parse_digest(d)?;
        let size = storage::append_body(&path, body).await?;
        let (actual, _) = storage::hash_file(&path, digest.algorithm).await?;
        if actual != digest {
            let _ = tokio::fs::remove_file(&path).await;
            return Err(OciError::digest_invalid(
                "provided digest did not match uploaded content",
            ));
        }
        if let Err(e) = quota(state, repo.owner.id, size).await {
            let _ = tokio::fs::remove_file(&path).await;
            return Err(e);
        }
        link_blob(state, &pkg, &path, &digest, size).await?;
        return Ok(created(name, &digest));
    }

    // Some clients send the first chunk with the POST.
    let size = storage::append_body(&path, body).await? as i64;
    sqlx::query(
        "INSERT INTO package_uploads (id, package_id, user_id, size) VALUES ($1, $2, $3, $4)",
    )
    .bind(id)
    .bind(pkg.id)
    .bind(caller.user_id())
    .bind(size)
    .execute(&state.db)
    .await?;
    Ok(accepted_upload(name, &id, size, StatusCode::ACCEPTED))
}

async fn load_upload(
    state: &AppState,
    repo: &Repo,
    id: uuid::Uuid,
) -> OciResult<(PackageRow, i64)> {
    let Some(pkg) = &repo.package else {
        return Err(OciError::upload_unknown());
    };
    let size: Option<i64> =
        sqlx::query_scalar("SELECT size FROM package_uploads WHERE id = $1 AND package_id = $2")
            .bind(id)
            .bind(pkg.id)
            .fetch_optional(&state.db)
            .await?;
    match size {
        Some(s) => Ok((pkg.clone(), s)),
        None => Err(OciError::upload_unknown()),
    }
}

/// `Content-Range: <start>-<end>` must continue at the current offset.
fn check_content_range(
    headers: &HeaderMap,
    offset: i64,
    name: &str,
    id: &uuid::Uuid,
) -> OciResult<()> {
    let Some(v) = headers
        .get(header::CONTENT_RANGE)
        .and_then(|v| v.to_str().ok())
    else {
        return Ok(());
    };
    let v = v
        .trim()
        .trim_start_matches("bytes ")
        .trim_start_matches("bytes=");
    let start = v
        .split(['-', '/'])
        .next()
        .and_then(|s| s.trim().parse::<i64>().ok());
    if start == Some(offset) {
        return Ok(());
    }
    Err(OciError::new(
        StatusCode::RANGE_NOT_SATISFIABLE,
        "BLOB_UPLOAD_INVALID",
        "content range does not continue the upload",
    )
    .header("location", upload_location(name, id))
    .header("range", format!("0-{}", (offset - 1).max(0)))
    .header("docker-upload-uuid", id.to_string()))
}

async fn append(state: &AppState, id: uuid::Uuid, body: Body) -> OciResult<i64> {
    let path = storage::upload_path(state, &id);
    storage::append_body(&path, body).await?;
    let size = tokio::fs::metadata(&path).await?.len() as i64;
    sqlx::query("UPDATE package_uploads SET size = $2, updated_at = now() WHERE id = $1")
        .bind(id)
        .bind(size)
        .execute(&state.db)
        .await?;
    Ok(size)
}

/// `PATCH /v2/{name}/blobs/uploads/{id}`: append a chunk.
pub async fn patch_upload(
    state: &AppState,
    caller: &Caller,
    name: &str,
    id: uuid::Uuid,
    headers: &HeaderMap,
    body: Body,
) -> OciResult {
    let (repo, _) = authorize(state, caller, name, "push").await?;
    let (_, offset) = load_upload(state, &repo, id).await?;
    check_content_range(headers, offset, name, &id)?;
    let size = append(state, id, body).await?;
    Ok(accepted_upload(name, &id, size, StatusCode::ACCEPTED))
}

/// `PUT /v2/{name}/blobs/uploads/{id}?digest=`: final chunk + verify.
pub async fn finish_upload(
    state: &AppState,
    caller: &Caller,
    name: &str,
    id: uuid::Uuid,
    query: &Query,
    headers: &HeaderMap,
    body: Body,
) -> OciResult {
    let (repo, _) = authorize(state, caller, name, "push").await?;
    let (pkg, offset) = load_upload(state, &repo, id).await?;
    let digest = parse_digest(query.get("digest").unwrap_or(""))?;
    check_content_range(headers, offset, name, &id)?;
    let size = append(state, id, body).await? as u64;
    let path = storage::upload_path(state, &id);
    let (actual, _) = storage::hash_file(&path, digest.algorithm).await?;
    if actual != digest {
        return Err(OciError::digest_invalid(
            "provided digest did not match uploaded content",
        ));
    }
    quota(state, repo.owner.id, size).await?;
    link_blob(state, &pkg, &path, &digest, size).await?;
    sqlx::query("DELETE FROM package_uploads WHERE id = $1")
        .bind(id)
        .execute(&state.db)
        .await?;
    Ok(created(name, &digest))
}

/// `GET /v2/{name}/blobs/uploads/{id}`: upload progress.
pub async fn upload_status(
    state: &AppState,
    caller: &Caller,
    name: &str,
    id: uuid::Uuid,
) -> OciResult {
    let (repo, _) = authorize(state, caller, name, "push").await?;
    let (_, size) = load_upload(state, &repo, id).await?;
    Ok(accepted_upload(name, &id, size, StatusCode::NO_CONTENT))
}

/// `DELETE /v2/{name}/blobs/uploads/{id}`: cancel.
pub async fn cancel_upload(
    state: &AppState,
    caller: &Caller,
    name: &str,
    id: uuid::Uuid,
) -> OciResult {
    let (repo, _) = authorize(state, caller, name, "push").await?;
    load_upload(state, &repo, id).await?;
    sqlx::query("DELETE FROM package_uploads WHERE id = $1")
        .bind(id)
        .execute(&state.db)
        .await?;
    let _ = tokio::fs::remove_file(storage::upload_path(state, &id)).await;
    Ok(builder(StatusCode::NO_CONTENT)
        .body(Body::empty())
        .expect("response"))
}

#[cfg(test)]
mod tests {
    use super::parse_range;

    #[test]
    fn ranges() {
        assert_eq!(parse_range("bytes=0-9", 100), Some((0, 9)));
        assert_eq!(parse_range("bytes=90-", 100), Some((90, 99)));
        assert_eq!(parse_range("bytes=-10", 100), Some((90, 99)));
        assert_eq!(parse_range("bytes=50-500", 100), Some((50, 99)));
        assert_eq!(parse_range("bytes=100-", 100), None);
        assert_eq!(parse_range("bytes=5-1", 100), None);
        assert_eq!(parse_range("items=0-1", 100), None);
    }
}
