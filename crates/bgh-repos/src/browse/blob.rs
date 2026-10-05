//! `GET /_bgh/repos/{owner}/{repo}/blob/{ref}/{path}`: file view with
//! server-side highlighting (cached in Redis by blob SHA + grammar),
//! image/binary/LFS detection, large-file truncation and rendered Markdown.

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Response;
use bgh_core::prelude::*;
use bgh_core::urls::encode_path;
use bgh_git::highlight::{self, Highlighted};
use bgh_git::{PathLookup, TreeEntryKind};
use serde::Serialize;

use super::readme::{is_markdown, render_document};
use super::{CACHE_VERSION, cache_get, cache_put, json_response, precheck, resolve};
use crate::download::mime;

/// Text larger than this is cut (at a line boundary) for display.
pub const DISPLAY_LIMIT: usize = 1024 * 1024;

#[derive(Debug, Clone, Serialize)]
pub struct LfsInfo {
    pub oid: String,
    pub size: u64,
    /// Whether the object was uploaded to this repository.
    pub stored: bool,
}

#[derive(Debug, Serialize)]
pub struct BlobView {
    #[serde(rename = "ref")]
    pub refname: String,
    pub commit: String,
    pub path: String,
    pub name: String,
    /// Blob SHA (submodule: the commit SHA).
    pub sha: String,
    /// `file` | `symlink` | `submodule`
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub mode: String,
    pub size: u64,
    pub binary: bool,
    pub image: bool,
    /// Content type for rendering (images) / downloads.
    pub mime: &'static str,
    pub lfs: Option<LfsInfo>,
    /// Larger than the server's blob limit: no content is returned.
    pub too_large: bool,
    /// Only the first [`DISPLAY_LIMIT`] bytes are included.
    pub truncated: bool,
    pub language: Option<String>,
    pub highlighted: bool,
    /// Number of lines in `lines` (after truncation).
    pub line_count: usize,
    /// HTML per line (class-based highlighting, see `/_bgh/highlight.css`);
    /// `null` for binary / too large / LFS content.
    pub lines: Option<Vec<String>>,
    /// Rendered HTML for Markdown files.
    pub rendered: Option<String>,
    pub symlink_target: Option<String>,
    /// Raw download URL pinned to the commit.
    pub raw_url: String,
}

enum Content {
    Submodule,
    TooLarge,
    Data { bytes: Vec<u8>, truncated: bool },
}

pub async fn get(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, spec)): Path<(String, String, String)>,
    req: HeaderMap,
) -> ApiResult<Response> {
    let t = resolve(&state, auth.as_ref(), &owner, &repo, Some(&spec)).await?;
    if t.path.is_empty() {
        return Err(ApiError::NotFound);
    }
    let key = format!("blob:{}:{}", t.commit, t.path);
    if let Some(r) = precheck(&req, &t, &key) {
        return Ok(r);
    }
    let (commit, path) = (t.commit.clone(), t.path.clone());
    let limit = state.config.max_blob_size;
    let (entry, size, content) = t
        .store(&state)
        .read(t.access.repo.id, move |r| {
            let PathLookup::Entry(e) = r.lookup_path(&commit, &path)? else {
                return Err(bgh_git::GitError::NotFound(format!("blob {path:?}")));
            };
            if e.kind == TreeEntryKind::Commit {
                return Ok((e, 0, Content::Submodule));
            }
            let size = r.header(&e.sha)?.map(|(_, s)| s).unwrap_or(0);
            if size > limit {
                return Ok((e, size, Content::TooLarge));
            }
            let mut bytes = r.blob_with_limit(&e.sha, limit)?.data;
            let truncated = bytes.len() > DISPLAY_LIMIT;
            if truncated {
                let cut = bytes[..DISPLAY_LIMIT]
                    .iter()
                    .rposition(|&b| b == b'\n')
                    .map_or(DISPLAY_LIMIT, |i| i + 1);
                bytes.truncate(cut);
            }
            Ok((e, size, Content::Data { bytes, truncated }))
        })
        .await?;

    let name = t.path.rsplit('/').next().unwrap_or(&t.path).to_string();
    let mime_type = mime::for_path(&name);
    let mut view = BlobView {
        refname: t.refname.clone(),
        commit: t.commit.clone(),
        path: t.path.clone(),
        name: name.clone(),
        sha: entry.sha.clone(),
        kind: entry.kind.content_type(),
        mode: entry.mode.clone(),
        size,
        binary: false,
        image: mime::is_image(mime_type),
        mime: mime_type,
        lfs: None,
        too_large: false,
        truncated: false,
        language: None,
        highlighted: false,
        line_count: 0,
        lines: None,
        rendered: None,
        symlink_target: None,
        raw_url: state.urls.html(&format!(
            "/{}/{}/raw/{}/{}",
            t.access.owner.login,
            t.access.repo.name,
            t.commit,
            encode_path(&t.path)
        )),
    };
    match content {
        Content::Submodule => {}
        Content::TooLarge => view.too_large = true,
        Content::Data { bytes, truncated } => {
            view.truncated = truncated;
            if entry.kind == TreeEntryKind::Symlink {
                view.symlink_target = Some(String::from_utf8_lossy(&bytes).into_owned());
            } else if let Some(p) = bgh_git::lfs::parse_pointer(&bytes) {
                let stored = crate::lfs::has_object(&state, t.access.repo.id, &p.oid).await?;
                view.lfs = Some(LfsInfo {
                    oid: p.oid,
                    size: p.size,
                    stored,
                });
            } else if bytes.iter().take(8000).any(|&b| b == 0) {
                view.binary = true;
            } else if !view.image || mime_type == "image/svg+xml" {
                let text = String::from_utf8_lossy(&bytes).into_owned();
                let h = highlighted(&state, &entry.sha, &t.path, &text, truncated).await;
                view.language = h.language;
                view.highlighted = h.highlighted;
                view.line_count = h.lines.len();
                view.lines = Some(h.lines);
                if is_markdown(&name) && !truncated {
                    let dir = t.path.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
                    let mkey = format!(
                        "md:{CACHE_VERSION}:{}:{}/{}:{}:{}",
                        entry.sha, t.access.owner.login, t.access.repo.name, t.refname, t.path
                    );
                    let html = match cache_get::<String>(&state, &mkey).await {
                        Some(h) => h,
                        None => {
                            let h = render_document(
                                &state,
                                &t.access.owner.login,
                                &t.access.repo.name,
                                &t.refname,
                                dir,
                                &name,
                                &text,
                            );
                            cache_put(&state, &mkey, &h).await;
                            h
                        }
                    };
                    view.rendered = Some(html);
                }
            }
        }
    }
    json_response(&req, &t, &key, &view)
}

/// Highlight with a Redis cache keyed by blob SHA and grammar.
pub async fn highlighted(
    state: &AppState,
    blob_sha: &str,
    path: &str,
    text: &str,
    truncated: bool,
) -> Highlighted {
    let lang = highlight::find_syntax(path, text)
        .map(|s| s.name.clone())
        .unwrap_or_else(|| "plain".into());
    let key = format!(
        "hl:{CACHE_VERSION}:{blob_sha}:{lang}{}",
        if truncated { ":t" } else { "" }
    );
    if let Some(h) = cache_get::<Highlighted>(state, &key).await {
        return h;
    }
    let (p, s) = (path.to_string(), text.to_string());
    let h = tokio::task::spawn_blocking(move || highlight::highlight(&p, &s))
        .await
        .unwrap_or_else(|_| Highlighted {
            language: None,
            lines: highlight::plain(text),
            highlighted: false,
        });
    cache_put(state, &key, &h).await;
    h
}
