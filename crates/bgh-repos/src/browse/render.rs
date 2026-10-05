//! `GET /_bgh/render/blob/{owner}/{repo}/{sha}?path=src/main.rs`
//! (docs/SYNC_PROTOCOL.md §10): highlighted lines of a blob, addressed by
//! blob SHA, so the response is immutable. `404` when no highlighter
//! applies (unknown language, binary, LFS pointer, too large); the client
//! then renders plain text.

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::Response;
use bgh_core::prelude::*;
use bgh_git::highlight;
use serde::{Deserialize, Serialize};

use super::{CACHE_VERSION, cache_control, cache_get, cache_put, etag_of, not_modified};

#[derive(Debug, Deserialize)]
pub struct RenderQuery {
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HighlightedBlob {
    pub language: String,
    pub lines: Vec<String>,
}

pub async fn blob(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, sha)): Path<(String, String, String)>,
    Query(q): Query<RenderQuery>,
    req: HeaderMap,
) -> ApiResult<Response> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    if !bgh_git::is_sha(&sha) {
        return Err(ApiError::NotFound);
    }
    let sha = sha.to_ascii_lowercase();
    let path = q.path.unwrap_or_default();
    let cache = cache_control(access.repo.is_private(), true);
    let etag = etag_of(&[CACHE_VERSION, "render", &sha, &path]);
    if let Some(r) = not_modified(&req, &etag, cache.clone()) {
        return Ok(r);
    }
    let body = highlighted_blob(&state, access.repo.id, &sha, &path).await?;
    let body = body.ok_or(ApiError::NotFound)?;
    let mut resp = Response::new(Body::from(
        serde_json::to_vec(&body).map_err(ApiError::internal)?,
    ));
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    h.insert(header::CACHE_CONTROL, cache);
    h.insert(header::ETAG, HeaderValue::from_str(&etag).expect("etag"));
    Ok(resp)
}

/// Max blocks per `POST /_bgh/render/code` request.
const MAX_CODE_BLOCKS: usize = 50;

#[derive(Debug, Deserialize)]
pub struct CodeBlock {
    pub lang: String,
    pub code: String,
}

#[derive(Debug, Deserialize)]
pub struct CodeRequest {
    pub blocks: Vec<CodeBlock>,
}

/// `POST /_bgh/render/code` `{blocks: [{lang, code}]}` →
/// `{blocks: [{language, lines} | null]}`: highlighting for fenced code
/// blocks rendered by the web client's Markdown renderer (P35). `null`
/// when no grammar matches the language or the block is too large; the
/// client keeps plain text. Pure function of the input (no repo access).
pub async fn code(Json(req): Json<CodeRequest>) -> ApiResult<Json<serde_json::Value>> {
    if req.blocks.len() > MAX_CODE_BLOCKS {
        return Err(ApiError::unprocessable(format!(
            "at most {MAX_CODE_BLOCKS} blocks per request"
        )));
    }
    let out = tokio::task::spawn_blocking(move || {
        req.blocks
            .into_iter()
            .map(|b| {
                if b.code.len() > highlight::MAX_HIGHLIGHT_BYTES {
                    return None;
                }
                let h = highlight::highlight_lang(&b.lang, &b.code);
                match h.language {
                    Some(language) if h.highlighted => Some(HighlightedBlob {
                        language,
                        lines: h.lines,
                    }),
                    _ => None,
                }
            })
            .collect::<Vec<_>>()
    })
    .await
    .map_err(ApiError::internal)?;
    Ok(Json(serde_json::json!({ "blocks": out })))
}

/// Highlighted lines of blob `sha` (lowercase hex) as `path`, cached in
/// Redis by blob SHA. `None` when no highlighter applies (unknown language,
/// binary, LFS pointer, too large); `NotFound` when `sha` is not a blob.
pub(crate) async fn highlighted_blob(
    state: &AppState,
    repo_id: i64,
    sha: &str,
    path: &str,
) -> ApiResult<Option<HighlightedBlob>> {
    let key = format!("render:{CACHE_VERSION}:{sha}:{path}");
    if let Some(b) = cache_get::<Option<HighlightedBlob>>(state, &key).await {
        return Ok(b);
    }
    let id = sha.to_string();
    let data = crate::store(state)
        .read(repo_id, move |r| {
            match r.header(&id)? {
                Some(("blob", size)) if size as usize <= highlight::MAX_HIGHLIGHT_BYTES => {}
                Some(("blob", _)) => return Ok(None),
                _ => return Err(bgh_git::GitError::NotFound(id)),
            }
            Ok(Some(r.blob(&id)?.data))
        })
        .await?;
    let rendered = match data {
        Some(d)
            if !d.iter().take(8000).any(|&b| b == 0)
                && bgh_git::lfs::parse_pointer(&d).is_none() =>
        {
            let text = String::from_utf8_lossy(&d).into_owned();
            let p = path.to_string();
            let h = tokio::task::spawn_blocking(move || highlight::highlight(&p, &text))
                .await
                .map_err(ApiError::internal)?;
            match h.language {
                Some(language) if h.highlighted => Some(HighlightedBlob {
                    language,
                    lines: h.lines,
                }),
                _ => None,
            }
        }
        _ => None,
    };
    cache_put(state, &key, &rendered).await;
    Ok(rendered)
}
