//! `GET /_bgh/repos/{owner}/{repo}/blob-lines/{commitish}?path=…`: lines of
//! one file at a commit, for the diff viewer (P37): context expansion
//! (`start`/`end`, 1-based inclusive), syntax highlighting of a diff side
//! (`hl=1`, optionally `text=0`) and binary/image metadata (size, MIME, raw
//! URL).
//!
//! `{commitish}` is a full commit SHA, `{base}...{head}` (two full SHAs:
//! their merge base, i.e. the old side of a pull request or a three-dot
//! compare) or any ref (then the response is not immutable).

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Response;
use bgh_core::prelude::*;
use bgh_core::urls::encode_path;
use bgh_git::{PathLookup, TreeEntryKind};
use serde::{Deserialize, Serialize};

use super::render::highlighted_blob;
use super::{Target, json_response, precheck};
use crate::download::mime;

/// Most lines returned by one request.
pub const MAX_RANGE: usize = 20_000;

#[derive(Debug, Deserialize)]
pub struct LinesQuery {
    pub path: Option<String>,
    pub start: Option<usize>,
    pub end: Option<usize>,
    /// `1`: include highlighted HTML lines (`html`).
    pub hl: Option<String>,
    /// `0`: omit plain-text lines (`lines`).
    pub text: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct BlobLines {
    /// Commit the path was resolved in (the merge base for `a...b`).
    pub commit: String,
    pub path: String,
    /// Blob SHA.
    pub sha: String,
    pub size: u64,
    pub binary: bool,
    pub image: bool,
    pub mime: &'static str,
    /// Lines in the whole file (0 for binary content).
    pub total_lines: usize,
    /// Range actually returned (1-based, inclusive; `end < start` = empty).
    pub start: usize,
    pub end: usize,
    /// Plain text of `start..=end` (`null` for binary content or `text=0`).
    pub lines: Option<Vec<String>>,
    /// Highlighted HTML of `start..=end` (`hl=1`; `null` when no
    /// highlighter applies, e.g. unknown language or a huge file).
    pub html: Option<Vec<String>>,
    pub language: Option<String>,
    /// Raw download URL pinned to `commit`.
    pub raw_url: String,
}

fn flag(v: &Option<String>, default: bool) -> bool {
    match v.as_deref() {
        Some("1" | "true") => true,
        Some("0" | "false") => false,
        _ => default,
    }
}

pub async fn get(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, spec)): Path<(String, String, String)>,
    Query(q): Query<LinesQuery>,
    req: HeaderMap,
) -> ApiResult<Response> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let path = q
        .path
        .as_deref()
        .map(|p| p.trim_matches('/'))
        .filter(|p| !p.is_empty())
        .ok_or_else(|| ApiError::invalid_field(FieldError::missing_field("Blob", "path")))?
        .to_string();
    let store = crate::store(&state);
    let repo_id = access.repo.id;

    // Resolve the commit (and whether the answer can never change).
    let (refname, commit) = match spec.split_once("...") {
        Some((a, b)) => {
            if !bgh_git::is_sha(a) || !bgh_git::is_sha(b) {
                return Err(ApiError::NotFound);
            }
            let (a, b) = (a.to_ascii_lowercase(), b.to_ascii_lowercase());
            let base = bgh_git::merge::merge_base(&store, repo_id, &a, &b)
                .await
                .map_err(|_| ApiError::NotFound)?
                .unwrap_or(a);
            // Immutable: address the response by the merge-base SHA.
            (base.clone(), base)
        }
        None => {
            let s = spec.clone();
            let commit = store.read(repo_id, move |r| r.resolve_commit(&s)).await?;
            (spec.clone(), commit)
        }
    };
    let t = Target {
        access,
        refname,
        commit: commit.clone(),
        path: path.clone(),
    };
    let hl = flag(&q.hl, false);
    let text = flag(&q.text, true);
    let key = format!("lines:{spec}:{path}:{:?}:{:?}:{hl}:{text}", q.start, q.end);
    if let Some(r) = precheck(&req, &t, &key) {
        return Ok(r);
    }

    let limit = state.config.max_blob_size;
    let (p, c) = (path.clone(), commit.clone());
    let (sha, size, data) = store
        .read(repo_id, move |r| {
            let PathLookup::Entry(e) = r.lookup_path(&c, &p)? else {
                return Err(bgh_git::GitError::NotFound(format!("blob {p:?}")));
            };
            if e.kind == TreeEntryKind::Commit {
                return Err(bgh_git::GitError::NotFound(format!("blob {p:?}")));
            }
            let size = r.header(&e.sha)?.map(|(_, s)| s).unwrap_or(0);
            let data = (size <= limit)
                .then(|| r.blob_with_limit(&e.sha, limit))
                .transpose()?
                .map(|b| b.data);
            Ok((e.sha, size, data))
        })
        .await?;

    let name = path.rsplit('/').next().unwrap_or(&path);
    let mime_type = mime::for_path(name);
    let text_data = data.filter(|d| {
        !d.iter().take(8000).any(|&b| b == 0) && bgh_git::lfs::parse_pointer(d).is_none()
    });
    let mut out = BlobLines {
        commit: commit.clone(),
        path: path.clone(),
        sha: sha.clone(),
        size,
        binary: text_data.is_none(),
        image: mime::is_image(mime_type),
        mime: mime_type,
        total_lines: 0,
        start: 1,
        end: 0,
        lines: None,
        html: None,
        language: None,
        raw_url: state.urls.html(&format!(
            "/{}/{}/raw/{}/{}",
            t.access.owner.login,
            t.access.repo.name,
            commit,
            encode_path(&path)
        )),
    };
    if let Some(d) = text_data {
        let content = String::from_utf8_lossy(&d);
        let all: Vec<&str> = content.lines().collect();
        let total = all.len();
        let start = q.start.unwrap_or(1).max(1);
        let end = q
            .end
            .unwrap_or(total)
            .min(total)
            .min(start.saturating_add(MAX_RANGE - 1));
        out.total_lines = total;
        out.start = start;
        out.end = end.max(start - 1);
        let range = (start - 1)..out.end;
        if text {
            out.lines = Some(
                all.get(range.clone())
                    .unwrap_or_default()
                    .iter()
                    .map(|l| l.to_string())
                    .collect(),
            );
        }
        if hl && let Some(h) = highlighted_blob(&state, repo_id, &sha, &path).await? {
            out.language = Some(h.language);
            out.html = Some(h.lines.get(range).unwrap_or_default().to_vec());
        }
    }
    json_response(&req, &t, &key, &out)
}
