//! README detection and rendering (Markdown via GFM; other formats as
//! preformatted text). Relative links resolve to the code browser and
//! relative images to the raw endpoint at the same ref.

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Response;
use bgh_core::markdown::{self, RenderContext, UrlAttr};
use bgh_core::prelude::*;
use bgh_core::urls::encode_path;
use bgh_git::PathLookup;
use serde::{Deserialize, Serialize};

use super::tree::Entry;
use super::{CACHE_VERSION, Target, cache_get, cache_put, json_response, precheck, resolve};

/// READMEs larger than this are not rendered.
pub const MAX_README_SIZE: u64 = 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Readme {
    pub name: String,
    pub path: String,
    pub sha: String,
    /// Sanitized HTML.
    pub html: String,
}

/// Pick the README of a directory listing (GitHub's preference order).
pub fn pick(entries: &[Entry]) -> Option<&Entry> {
    const PREFERRED: &[&str] = &[
        "readme.md",
        "readme.markdown",
        "readme.mdown",
        "readme.mkd",
        "readme.rst",
        "readme.txt",
        "readme",
    ];
    let files = || entries.iter().filter(|e| e.kind == "blob");
    PREFERRED
        .iter()
        .find_map(|p| files().find(|e| e.name.to_lowercase() == *p))
        .or_else(|| files().find(|e| e.name.to_lowercase().starts_with("readme.")))
}

/// Whether a file name is rendered as Markdown.
pub fn is_markdown(name: &str) -> bool {
    let lower = name.to_lowercase();
    [".md", ".markdown", ".mdown", ".mkd", ".mkdn"]
        .iter()
        .any(|ext| lower.ends_with(ext))
}

/// Resolve `url` found in a document at directory `dir` to a repo path.
/// `None` for absolute URLs, anchors, and paths escaping the repository.
pub fn resolve_relative(dir: &str, url: &str) -> Option<(String, String)> {
    if url.is_empty() || url.starts_with('#') || url.starts_with("//") || url.starts_with('?') {
        return None;
    }
    // Any scheme (`https:`, `mailto:`, `data:`) before the first slash.
    if let Some(colon) = url.find(':')
        && !url[..colon].contains('/')
    {
        return None;
    }
    let split = url.find(['?', '#']).unwrap_or(url.len());
    let (path, suffix) = url.split_at(split);
    let mut parts: Vec<&str> = if path.starts_with('/') {
        Vec::new()
    } else {
        dir.split('/').filter(|p| !p.is_empty()).collect()
    };
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            s => parts.push(s),
        }
    }
    Some((parts.join("/"), suffix.to_string()))
}

/// Render a document at `path` (`dir` is its directory) for the browser.
pub fn render_document(
    state: &AppState,
    owner: &str,
    repo: &str,
    refname: &str,
    dir: &str,
    name: &str,
    text: &str,
) -> String {
    if !is_markdown(name) {
        return format!("<pre>{}</pre>", bgh_git::highlight::escape(text));
    }
    let html = markdown::render(
        text,
        &RenderContext::new(&state.config.base_url).with_repo(owner, repo),
    );
    let reference = encode_path(refname);
    markdown::rewrite_urls(&html, |url, attr| {
        let (path, suffix) = resolve_relative(dir, url)?;
        let kind = match attr {
            UrlAttr::Src => "raw",
            UrlAttr::Href => "blob",
        };
        Some(state.urls.html(&format!(
            "/{owner}/{repo}/{kind}/{reference}/{path}{suffix}"
        )))
    })
}

fn dir_of(path: &str) -> &str {
    path.rsplit_once('/').map(|(d, _)| d).unwrap_or("")
}

/// Render (cached by blob SHA + ref + location) the README `e` of `t`.
pub async fn render(state: &AppState, t: &Target, e: &Entry) -> ApiResult<Readme> {
    let owner = t.access.owner.login.clone();
    let repo = t.access.repo.name.clone();
    let dir = dir_of(&e.path).to_string();
    let key = format!(
        "md:{CACHE_VERSION}:{}:{owner}/{repo}:{}:{}",
        e.sha, t.refname, e.path
    );
    if let Some(html) = cache_get::<String>(state, &key).await {
        return Ok(Readme {
            name: e.name.clone(),
            path: e.path.clone(),
            sha: e.sha.clone(),
            html,
        });
    }
    let sha = e.sha.clone();
    let blob = t
        .store(state)
        .read(t.access.repo.id, move |r| {
            r.blob_with_limit(&sha, MAX_README_SIZE)
        })
        .await;
    let html = match blob {
        Ok(b) => {
            let text = String::from_utf8_lossy(&b.data);
            render_document(state, &owner, &repo, &t.refname, &dir, &e.name, &text)
        }
        Err(bgh_git::GitError::TooLarge { .. }) => {
            "<p><em>This README is too large to display.</em></p>".to_string()
        }
        Err(err) => return Err(err.into()),
    };
    cache_put(state, &key, &html).await;
    Ok(Readme {
        name: e.name.clone(),
        path: e.path.clone(),
        sha: e.sha.clone(),
        html,
    })
}

pub async fn root(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo)): Path<(String, String)>,
    req: HeaderMap,
) -> ApiResult<Response> {
    let t = resolve(&state, auth.as_ref(), &owner, &repo, None).await?;
    respond(&state, t, req).await
}

pub async fn get(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, spec)): Path<(String, String, String)>,
    req: HeaderMap,
) -> ApiResult<Response> {
    let t = resolve(&state, auth.as_ref(), &owner, &repo, Some(&spec)).await?;
    respond(&state, t, req).await
}

/// `GET /_bgh/repos/{owner}/{repo}/readme[/{ref}[/{dir}]]`
async fn respond(state: &AppState, t: Target, req: HeaderMap) -> ApiResult<Response> {
    let key = format!("readme:{}:{}", t.commit, t.path);
    if let Some(r) = precheck(&req, &t, &key) {
        return Ok(r);
    }
    let (commit, path) = (t.commit.clone(), t.path.clone());
    let entries = t
        .store(state)
        .read(t.access.repo.id, move |r| {
            match r.lookup_path(&commit, &path)? {
                PathLookup::Tree { entries, .. } => Ok(entries),
                PathLookup::Entry(_) => Err(bgh_git::GitError::NotFound(path)),
            }
        })
        .await?;
    let entries: Vec<Entry> = entries
        .into_iter()
        .map(|e| Entry {
            path: if t.path.is_empty() {
                e.name.clone()
            } else {
                format!("{}/{}", t.path, e.name)
            },
            kind: if e.kind.object_type() == "blob" && e.kind != bgh_git::TreeEntryKind::Symlink {
                "blob"
            } else {
                "other"
            },
            name: e.name,
            mode: e.mode,
            sha: e.sha,
            size: None,
        })
        .collect();
    let e = pick(&entries).ok_or(ApiError::NotFound)?;
    let readme = render(state, &t, e).await?;
    json_response(&req, &t, &key, &readme)
}

#[cfg(test)]
mod tests {
    use super::resolve_relative as r;

    #[test]
    fn resolves_relative_urls() {
        assert_eq!(
            r("docs", "img/a.png"),
            Some(("docs/img/a.png".into(), "".into()))
        );
        assert_eq!(
            r("docs", "../README.md#x"),
            Some(("README.md".into(), "#x".into()))
        );
        assert_eq!(r("docs", "/LICENSE"), Some(("LICENSE".into(), "".into())));
        assert_eq!(
            r("", "./a/./b.md?raw=1"),
            Some(("a/b.md".into(), "?raw=1".into()))
        );
        assert_eq!(r("", "../../etc"), None);
        assert_eq!(r("", "https://example.com/x"), None);
        assert_eq!(r("", "mailto:a@b"), None);
        assert_eq!(r("", "#anchor"), None);
        assert_eq!(r("", "//cdn.example.com/x.png"), None);
    }
}
