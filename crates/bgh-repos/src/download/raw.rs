//! `GET /{owner}/{repo}/raw/{ref}/{path}`

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::Response;
use bgh_core::prelude::*;
use bgh_git::{PathLookup, TreeEntryKind};
use tokio_util::io::ReaderStream;

use super::{TokenQuery, mime};
use crate::browse::{cache_control, not_modified, resolve_with};

/// Raw responses for refs (not SHAs) are cacheable this long.
const RAW_TTL_SECS: u32 = 300;

/// Never let raw content execute on our origin.
const RAW_CSP: &str = "default-src 'none'; style-src 'unsafe-inline'; sandbox";

enum Content {
    Bytes(Vec<u8>),
    Large,
}

pub async fn get(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, spec)): Path<(String, String, String)>,
    Query(q): Query<TokenQuery>,
    req: HeaderMap,
) -> ApiResult<Response> {
    let access = super::access(&state, auth.as_ref(), &owner, &repo, q.token.as_deref()).await?;
    let t = resolve_with(&state, access, Some(&spec)).await?;
    if t.path.is_empty() {
        return Err(ApiError::NotFound);
    }
    let private = t.access.repo.is_private();
    let cache = if t.immutable() {
        cache_control(private, true)
    } else {
        HeaderValue::from_str(&format!(
            "{}, max-age={RAW_TTL_SECS}",
            if private { "private" } else { "public" }
        ))
        .expect("header")
    };
    let (commit, path) = (t.commit.clone(), t.path.clone());
    let limit = state.config.max_blob_size;
    let (sha, size, content) = t
        .store(&state)
        .read(t.access.repo.id, move |r| {
            let e = match r.lookup_path(&commit, &path)? {
                PathLookup::Entry(e) if e.kind != TreeEntryKind::Commit => e,
                _ => return Err(bgh_git::GitError::NotFound(path)),
            };
            let size = r.header(&e.sha)?.map(|(_, s)| s).unwrap_or(0);
            if size > limit {
                return Ok((e.sha, size, Content::Large));
            }
            let blob = r.blob_with_limit(&e.sha, limit)?;
            Ok((e.sha, size, Content::Bytes(blob.data)))
        })
        .await?;
    let name = t.path.rsplit('/').next().unwrap_or(&t.path).to_string();
    let etag = format!("\"{sha}\"");
    if let Some(r) = not_modified(&req, &etag, cache.clone()) {
        return Ok(r);
    }

    let (body, len, binary) = match content {
        Content::Large => (
            Body::from_stream(
                bgh_git::stream::blob_stream(&t.store(&state), t.access.repo.id, &sha).await?,
            ),
            size,
            true,
        ),
        Content::Bytes(data) => match bgh_git::lfs::parse_pointer(&data) {
            Some(p) if crate::lfs::has_object(&state, t.access.repo.id, &p.oid).await? => {
                let file = crate::lfs::object_store(&state).open(&p.oid).await;
                match file {
                    Ok(file) => {
                        let len = file.metadata().await.map(|m| m.len()).unwrap_or(p.size);
                        let binary = true;
                        (
                            Body::from_stream(ReaderStream::with_capacity(file, 64 * 1024)),
                            len,
                            binary,
                        )
                    }
                    Err(_) => {
                        let binary = data.iter().take(8000).any(|&b| b == 0);
                        let len = data.len() as u64;
                        (Body::from(data), len, binary)
                    }
                }
            }
            _ => {
                let binary = data.iter().take(8000).any(|&b| b == 0);
                let len = data.len() as u64;
                (Body::from(data), len, binary)
            }
        },
    };
    let ct = mime::for_raw(&name, binary);
    let mut resp = Response::new(body);
    *resp.status_mut() = StatusCode::OK;
    let h = resp.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(ct));
    h.insert(header::CONTENT_LENGTH, HeaderValue::from(len));
    h.insert(header::CACHE_CONTROL, cache);
    h.insert(header::ETAG, HeaderValue::from_str(&etag).expect("etag"));
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(RAW_CSP),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::VARY,
        HeaderValue::from_static("Cookie, Authorization"),
    );
    Ok(resp)
}
