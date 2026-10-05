//! Repository contents: files, directories, READMEs and raw downloads.
//!
//! * `GET /repos/{o}/{r}/contents[/{path}]?ref=` (JSON, `.raw`, `.html`,
//!   `.object` media types)
//! * `PUT /repos/{o}/{r}/contents/{path}`: create or update a file
//! * `DELETE /repos/{o}/{r}/contents/{path}`: delete a file
//! * `GET /repos/{o}/{r}/readme[/{dir}]?ref=`
//! * `GET /repos/{o}/{r}/license?ref=` (`license-content`)
//! * raw downloads (`/{o}/{r}/raw/...`) live in [`crate::download`]
//!
//! Reads resolve the ref once to a commit SHA and do everything else in one
//! in-process gix pass (entry sizes come from object headers, no per-entry
//! subprocesses). JSON renderings are cached in Redis keyed by the resolved
//! commit SHA. Writes build the new tree with git plumbing and move the
//! branch through [`crate::refs::write_ref`] (branch protection +
//! post-receive).

use std::collections::HashMap;

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use bgh_core::markdown::{self, RenderContext};
use bgh_core::prelude::*;
use bgh_core::urls::encode_path;
use bgh_git::write::Identity;
use bgh_git::{GitError, GitRepo, GitResult, PathLookup, TreeEdit, TreeEntryKind};
use serde::{Deserialize, Serialize};

use crate::cache;
use crate::gitjson::{GitCommit, RepoRef, git_commit};
use crate::identity::{IdentityInput, default_identity};
use crate::media::{self, Media};

/// Largest blob served raw (`.raw` media type and raw downloads).
const RAW_LIMIT: u64 = 100 * 1024 * 1024;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/repos/{owner}/{repo}/contents", get(get_root))
        .route("/repos/{owner}/{repo}/contents/", get(get_root))
        .route(
            "/repos/{owner}/{repo}/contents/{*path}",
            get(get_path).put(put_file).delete(delete_file),
        )
        .route("/repos/{owner}/{repo}/readme", get(readme_root))
        .route("/repos/{owner}/{repo}/readme/", get(readme_root))
        .route("/repos/{owner}/{repo}/readme/{*dir}", get(readme_dir))
        .route("/repos/{owner}/{repo}/license", get(license))
}

// ----- JSON shapes -------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContentLinks {
    #[serde(rename = "self")]
    pub self_url: String,
    pub git: Option<String>,
    pub html: Option<String>,
}

/// `content-file`, `content-directory` entries, `content-symlink`,
/// `content-submodule` and the `.object` directory shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContentEntry {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encoding: Option<String>,
    pub size: u64,
    pub name: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub submodule_git_url: Option<String>,
    pub sha: String,
    pub url: String,
    pub git_url: Option<String>,
    pub html_url: Option<String>,
    pub download_url: Option<String>,
    #[serde(rename = "_links")]
    pub links: ContentLinks,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entries: Option<Vec<ContentEntry>>,
}

/// A single object or a directory listing.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum ContentsJson {
    One(Box<ContentEntry>),
    Many(Vec<ContentEntry>),
}

/// `file-commit` (PUT / DELETE response).
#[derive(Debug, Clone, Serialize)]
struct FileCommit {
    content: Option<ContentEntry>,
    commit: GitCommit,
}

// ----- git-side resolution -----------------------------------------------------

#[derive(Debug)]
enum Found {
    File {
        path: String,
        sha: String,
        size: u64,
        /// `None` when larger than the limit.
        data: Option<Vec<u8>>,
    },
    Symlink {
        path: String,
        sha: String,
        size: u64,
        target: String,
    },
    Submodule {
        path: String,
        sha: String,
        url: Option<String>,
    },
    Dir {
        path: String,
        sha: String,
        entries: Vec<DirEntry>,
    },
}

#[derive(Debug)]
struct DirEntry {
    name: String,
    kind: TreeEntryKind,
    sha: String,
    size: u64,
    submodule_url: Option<String>,
}

fn join_path(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

fn base_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Strip leading/trailing slashes and empty components.
fn normalize(path: &str) -> String {
    path.split('/')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("/")
}

/// Parse `.gitmodules` into `path → url`.
fn parse_gitmodules(text: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let (mut path, mut url): (Option<String>, Option<String>) = (None, None);
    let mut flush = |path: &mut Option<String>, url: &mut Option<String>| {
        if let (Some(p), Some(u)) = (path.take(), url.take()) {
            out.insert(normalize(&p), u);
        }
    };
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            flush(&mut path, &mut url);
            continue;
        }
        if line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let v = v.trim().trim_matches('"').to_string();
            match k.trim().to_ascii_lowercase().as_str() {
                "path" => path = Some(v),
                "url" => url = Some(v),
                _ => {}
            }
        }
    }
    flush(&mut path, &mut url);
    out
}

fn gitmodules(r: &GitRepo, commit: &str) -> GitResult<HashMap<String, String>> {
    match r.lookup_path(commit, ".gitmodules") {
        Ok(PathLookup::Entry(e)) if e.kind != TreeEntryKind::Commit => {
            let blob = r.blob(&e.sha)?;
            Ok(parse_gitmodules(&String::from_utf8_lossy(&blob.data)))
        }
        Ok(_) | Err(GitError::NotFound(_)) => Ok(HashMap::new()),
        Err(e) => Err(e),
    }
}

/// Resolve a symlink target relative to the link's directory; `None` if it
/// leaves the repository.
fn symlink_target(link: &str, target: &str) -> Option<String> {
    if target.starts_with('/') {
        return None;
    }
    let mut parts: Vec<&str> = link.split('/').collect();
    parts.pop();
    for c in target.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            c => parts.push(c),
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

fn file_found(r: &GitRepo, path: String, sha: String, limit: u64) -> GitResult<Found> {
    let size = r.header(&sha)?.map(|h| h.1).unwrap_or(0);
    let data = if size > limit {
        None
    } else {
        Some(r.blob_with_limit(&sha, limit)?.data)
    };
    Ok(Found::File {
        path,
        sha,
        size,
        data,
    })
}

/// Look up `path` in `commit`. Symlinks to regular files are followed when
/// `follow` is set (like GitHub's contents API).
fn find(r: &GitRepo, commit: &str, path: &str, limit: u64, follow: bool) -> GitResult<Found> {
    let entry = match r.lookup_path(commit, path)? {
        PathLookup::Tree { sha, entries } => {
            let mut modules: Option<HashMap<String, String>> = None;
            let mut out = Vec::with_capacity(entries.len());
            for e in entries {
                let size = match e.kind {
                    TreeEntryKind::Tree | TreeEntryKind::Commit => 0,
                    _ => r.header(&e.sha)?.map(|h| h.1).unwrap_or(0),
                };
                let submodule_url = if e.kind == TreeEntryKind::Commit {
                    if modules.is_none() {
                        modules = Some(gitmodules(r, commit)?);
                    }
                    modules
                        .as_ref()
                        .and_then(|m| m.get(&join_path(path, &e.name)).cloned())
                } else {
                    None
                };
                out.push(DirEntry {
                    name: e.name,
                    kind: e.kind,
                    sha: e.sha,
                    size,
                    submodule_url,
                });
            }
            return Ok(Found::Dir {
                path: path.to_string(),
                sha,
                entries: out,
            });
        }
        PathLookup::Entry(e) => e,
    };
    match entry.kind {
        TreeEntryKind::Commit => Ok(Found::Submodule {
            path: path.to_string(),
            url: gitmodules(r, commit)?.remove(path),
            sha: entry.sha,
        }),
        TreeEntryKind::Symlink => {
            let blob = r.blob(&entry.sha)?;
            let target = String::from_utf8_lossy(&blob.data).into_owned();
            if follow
                && let Some(tp) = symlink_target(path, &target)
                && let Ok(PathLookup::Entry(t)) = r.lookup_path(commit, &tp)
                && matches!(t.kind, TreeEntryKind::Blob | TreeEntryKind::Executable)
            {
                return file_found(r, tp, t.sha, limit);
            }
            Ok(Found::Symlink {
                path: path.to_string(),
                sha: entry.sha,
                size: blob.size,
                target,
            })
        }
        _ => file_found(r, path.to_string(), entry.sha, limit),
    }
}

/// Resolve `?ref=` (default branch when absent) to `(ref name, commit SHA)`.
async fn resolve_ref(
    state: &AppState,
    access: &RepoAccess,
    requested: Option<&str>,
) -> ApiResult<(String, String)> {
    let name = requested
        .filter(|s| !s.is_empty())
        .unwrap_or(&access.repo.default_branch)
        .to_string();
    let rev = name.clone();
    let (commit, empty) = crate::store(state)
        .read(access.repo.id, move |r| {
            let commit = match r.resolve_commit(&rev) {
                Ok(c) => Some(c),
                Err(GitError::NotFound(_)) => None,
                Err(e) => return Err(e),
            };
            let empty = commit.is_none() && r.branches()?.is_empty();
            Ok((commit, empty))
        })
        .await?;
    match commit {
        Some(c) => Ok((name, c)),
        None if empty => Err(ApiError::Status(
            StatusCode::NOT_FOUND,
            "This repository is empty.".into(),
        )),
        None => Err(ApiError::Status(
            StatusCode::NOT_FOUND,
            format!("No commit found for the ref {name}"),
        )),
    }
}

// ----- rendering ---------------------------------------------------------------

/// Base64 with a newline after every 60 characters (Ruby's `encode64`, as
/// GitHub sends it).
pub fn base64_lines(data: &[u8]) -> String {
    let b64 = STANDARD.encode(data);
    let mut out = String::with_capacity(b64.len() + b64.len() / 60 + 1);
    for chunk in b64.as_bytes().chunks(60) {
        out.push_str(std::str::from_utf8(chunk).unwrap_or_default());
        out.push('\n');
    }
    out
}

/// URL context for entries rendered at `git_ref`.
struct Ctx<'a> {
    r: RepoRef<'a>,
    git_ref: &'a str,
}

impl Ctx<'_> {
    fn entry(&self, kind: TreeEntryKind, path: &str, sha: &str, size: u64) -> ContentEntry {
        let p = encode_path(path);
        let rf = encode_path(self.git_ref);
        let url = self.r.api(&format!("/contents/{p}?ref={rf}"));
        let (git_url, html_url, download_url) = match kind {
            TreeEntryKind::Tree => (
                Some(self.r.api(&format!("/git/trees/{sha}"))),
                Some(self.r.html(&format!("/tree/{rf}/{p}"))),
                None,
            ),
            TreeEntryKind::Commit => (None, None, None),
            _ => (
                Some(self.r.api(&format!("/git/blobs/{sha}"))),
                Some(self.r.html(&format!("/blob/{rf}/{p}"))),
                Some(self.r.html(&format!("/raw/{rf}/{p}"))),
            ),
        };
        ContentEntry {
            kind: kind.content_type().to_string(),
            encoding: None,
            size,
            name: base_name(path).to_string(),
            path: path.to_string(),
            content: None,
            target: None,
            submodule_git_url: None,
            sha: sha.to_string(),
            links: ContentLinks {
                self_url: url.clone(),
                git: git_url.clone(),
                html: html_url.clone(),
            },
            url,
            git_url,
            html_url,
            download_url,
            entries: None,
        }
    }

    /// Full JSON rendering. `object` selects the `.object` media type
    /// (directories as an object with `entries`).
    fn render(&self, found: &Found, object: bool) -> ContentsJson {
        match found {
            Found::File {
                path,
                sha,
                size,
                data,
            } => {
                let mut e = self.entry(TreeEntryKind::Blob, path, sha, *size);
                match data {
                    Some(d) => {
                        e.encoding = Some("base64".into());
                        e.content = Some(base64_lines(d));
                    }
                    None => {
                        e.encoding = Some("none".into());
                        e.content = Some(String::new());
                    }
                }
                ContentsJson::One(Box::new(e))
            }
            Found::Symlink {
                path,
                sha,
                size,
                target,
            } => {
                let mut e = self.entry(TreeEntryKind::Symlink, path, sha, *size);
                e.target = Some(target.clone());
                ContentsJson::One(Box::new(e))
            }
            Found::Submodule { path, sha, url } => {
                let mut e = self.entry(TreeEntryKind::Commit, path, sha, 0);
                e.submodule_git_url = url.clone();
                ContentsJson::One(Box::new(e))
            }
            Found::Dir { path, sha, entries } => {
                let items: Vec<ContentEntry> = entries
                    .iter()
                    .map(|d| {
                        let mut e = self.entry(d.kind, &join_path(path, &d.name), &d.sha, d.size);
                        if d.kind == TreeEntryKind::Commit {
                            if object {
                                e.submodule_git_url = d.submodule_url.clone();
                            } else {
                                // GitHub lists submodules as `file` in
                                // directory arrays (backwards compatibility).
                                e.kind = "file".into();
                            }
                        }
                        e
                    })
                    .collect();
                if object {
                    let mut e = self.entry(TreeEntryKind::Tree, path, sha, 0);
                    e.entries = Some(items);
                    ContentsJson::One(Box::new(e))
                } else {
                    ContentsJson::Many(items)
                }
            }
        }
    }
}

fn is_markdown(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    [".md", ".markdown", ".mdown", ".mkdn", ".mkd", ".mdwn"]
        .iter()
        .any(|ext| lower.ends_with(ext))
}

fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// `.html` rendering of a file: markdown rendered, anything else escaped in
/// a `<pre>`. Cached by blob SHA.
async fn render_html(
    state: &AppState,
    access: &RepoAccess,
    wrapper_id: &str,
    path: &str,
    sha: &str,
    data: Vec<u8>,
) -> ApiResult<String> {
    let owner = access.owner.login.clone();
    let name = access.repo.name.clone();
    let md = is_markdown(path);
    let key = format!(
        "contents-html:{sha}:{}:{owner}/{name}",
        if md { "md" } else { "pre" }
    );
    let base = state.config.base_url.clone();
    let inner: String = cache::cached(state, &key, || async move {
        let text = String::from_utf8_lossy(&data).into_owned();
        if md {
            Ok(tokio::task::spawn_blocking(move || {
                markdown::render(&text, &RenderContext::new(&base).with_repo(&owner, &name))
            })
            .await?)
        } else {
            Ok(format!("<pre>{}</pre>", escape_html(&text)))
        }
    })
    .await?;
    let class = if md { "md" } else { "file" };
    Ok(format!(
        "<div id=\"{wrapper_id}\" class=\"{class}\" data-path=\"{}\"><article class=\"markdown-body entry-content container-lg\" itemprop=\"text\">{inner}</article></div>",
        escape_html(path)
    ))
}

fn content_type_for(data: &[u8]) -> &'static str {
    if data.iter().take(8000).any(|&b| b == 0) {
        "application/octet-stream"
    } else {
        "text/plain; charset=utf-8"
    }
}

// ----- GET contents / readme ---------------------------------------------------

#[derive(Debug, Deserialize)]
struct RefQuery {
    #[serde(rename = "ref")]
    git_ref: Option<String>,
}

async fn get_root(
    State(state): State<AppState>,
    auth: MaybeUser,
    headers: HeaderMap,
    Path((owner, repo)): Path<(String, String)>,
    Query(q): Query<RefQuery>,
) -> ApiResult<Response> {
    show(&state, &auth, &headers, &owner, &repo, "", q).await
}

async fn get_path(
    State(state): State<AppState>,
    auth: MaybeUser,
    headers: HeaderMap,
    Path((owner, repo, path)): Path<(String, String, String)>,
    Query(q): Query<RefQuery>,
) -> ApiResult<Response> {
    show(&state, &auth, &headers, &owner, &repo, &path, q).await
}

async fn show(
    state: &AppState,
    auth: &MaybeUser,
    headers: &HeaderMap,
    owner: &str,
    repo: &str,
    path: &str,
    q: RefQuery,
) -> ApiResult<Response> {
    let access = RepoAccess::load(state, auth.as_ref(), owner, repo).await?;
    let path = normalize(path);
    let (git_ref, commit) = resolve_ref(state, &access, q.git_ref.as_deref()).await?;
    let m = media::media(headers);
    let store = crate::store(state);

    if matches!(m, Media::Raw | Media::Html) {
        let (c, p) = (commit.clone(), path.clone());
        let found = store
            .read(access.repo.id, move |r| find(r, &c, &p, RAW_LIMIT, true))
            .await?;
        if let Found::File {
            path, sha, data, ..
        } = &found
        {
            let Some(data) = data.clone() else {
                return Err(GitError::TooLarge {
                    size: RAW_LIMIT + 1,
                    limit: RAW_LIMIT,
                }
                .into());
            };
            return Ok(if m == Media::Raw {
                let ct = content_type_for(&data);
                media::body(m, ct, data)
            } else {
                let html = render_html(state, &access, "file", path, sha, data).await?;
                media::body(m, "text/html; charset=utf-8", html)
            });
        }
        // Directories, symlinks and submodules: JSON, like GitHub.
    }

    let object = m == Media::Object;
    let key = format!(
        "contents:{}:{commit}:{}:{}/{}:{git_ref}:{path}",
        access.repo.id,
        if object { "o" } else { "j" },
        access.owner.login,
        access.repo.name,
    );
    let limit = state.config.max_blob_size;
    let json: ContentsJson = cache::cached(state, &key, || async {
        let (c, p) = (commit.clone(), path.clone());
        let found = store
            .read(access.repo.id, move |r| find(r, &c, &p, limit, true))
            .await?;
        let ctx = Ctx {
            r: RepoRef::new(&state.urls, &access),
            git_ref: &git_ref,
        };
        Ok(ctx.render(&found, object))
    })
    .await?;
    Ok(Json(json).into_response())
}

/// README preference: `.md`, `.markdown`, `.rst`, `.txt`, no extension,
/// anything else.
fn readme_rank(name: &str) -> Option<u8> {
    let lower = name.to_ascii_lowercase();
    let rest = lower.strip_prefix("readme")?;
    Some(match rest {
        ".md" => 0,
        ".markdown" => 1,
        ".rst" => 2,
        ".txt" => 3,
        "" => 4,
        r if r.starts_with('.') => 5,
        _ => return None,
    })
}

async fn readme_root(
    State(state): State<AppState>,
    auth: MaybeUser,
    headers: HeaderMap,
    Path((owner, repo)): Path<(String, String)>,
    Query(q): Query<RefQuery>,
) -> ApiResult<Response> {
    readme(&state, &auth, &headers, &owner, &repo, "", q).await
}

async fn readme_dir(
    State(state): State<AppState>,
    auth: MaybeUser,
    headers: HeaderMap,
    Path((owner, repo, dir)): Path<(String, String, String)>,
    Query(q): Query<RefQuery>,
) -> ApiResult<Response> {
    readme(&state, &auth, &headers, &owner, &repo, &dir, q).await
}

async fn readme(
    state: &AppState,
    auth: &MaybeUser,
    headers: &HeaderMap,
    owner: &str,
    repo: &str,
    dir: &str,
    q: RefQuery,
) -> ApiResult<Response> {
    let access = RepoAccess::load(state, auth.as_ref(), owner, repo).await?;
    let dir = normalize(dir);
    let (git_ref, commit) = resolve_ref(state, &access, q.git_ref.as_deref()).await?;
    let m = media::media(headers);
    let limit = if matches!(m, Media::Raw | Media::Html) {
        RAW_LIMIT
    } else {
        state.config.max_blob_size
    };
    let c = commit.clone();
    let found = crate::store(state)
        .read(access.repo.id, move |r| {
            let entries = match r.lookup_path(&c, &dir)? {
                PathLookup::Tree { entries, .. } => entries,
                PathLookup::Entry(_) => return Ok(None),
            };
            let best = entries
                .iter()
                .filter(|e| {
                    matches!(
                        e.kind,
                        TreeEntryKind::Blob | TreeEntryKind::Executable | TreeEntryKind::Symlink
                    )
                })
                .filter_map(|e| readme_rank(&e.name).map(|rank| (rank, e)))
                .min_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.name.cmp(&b.1.name)));
            let Some((_, e)) = best else { return Ok(None) };
            let path = join_path(&dir, &e.name);
            match find(r, &c, &path, limit, true)? {
                f @ Found::File { .. } => Ok(Some(f)),
                _ => Ok(None),
            }
        })
        .await?
        .ok_or(ApiError::NotFound)?;
    let Found::File {
        path, sha, data, ..
    } = &found
    else {
        return Err(ApiError::NotFound);
    };
    match (m, data) {
        (Media::Raw, Some(d)) => Ok(media::body(m, content_type_for(d), d.clone())),
        (Media::Html, Some(d)) => {
            let html = render_html(state, &access, "readme", path, sha, d.clone()).await?;
            Ok(media::body(m, "text/html; charset=utf-8", html))
        }
        _ => {
            let ctx = Ctx {
                r: RepoRef::new(&state.urls, &access),
                git_ref: &git_ref,
            };
            Ok(Json(ctx.render(&found, false)).into_response())
        }
    }
}

/// `license-content`: the root license file plus the detected license.
#[derive(Debug, Serialize)]
struct LicenseContent {
    #[serde(flatten)]
    entry: ContentEntry,
    license: Option<api::LicenseSimple>,
}

/// `GET /repos/{o}/{r}/license?ref=`: the license file of the root
/// directory (404 without one), detected live for the requested ref.
async fn license(
    State(state): State<AppState>,
    auth: MaybeUser,
    headers: HeaderMap,
    Path((owner, repo)): Path<(String, String)>,
    Query(q): Query<RefQuery>,
) -> ApiResult<Response> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let (git_ref, commit) = resolve_ref(&state, &access, q.git_ref.as_deref()).await?;
    let m = media::media(&headers);
    let limit = state.config.max_blob_size;
    let c = commit.clone();
    let found = crate::store(&state)
        .read(access.repo.id, move |r| {
            let Some((name, _)) = crate::licenses::find_license_file(r, &c)? else {
                return Ok(None);
            };
            match find(r, &c, &name, limit, true)? {
                f @ Found::File { .. } => Ok(Some(f)),
                _ => Ok(None),
            }
        })
        .await?
        .ok_or(ApiError::NotFound)?;
    let Found::File { data, .. } = &found else {
        return Err(ApiError::NotFound);
    };
    if let (Media::Raw, Some(d)) = (m, data) {
        return Ok(media::body(m, content_type_for(d), d.clone()));
    }
    let text = data
        .as_deref()
        .map(|d| String::from_utf8_lossy(d).into_owned())
        .unwrap_or_default();
    let spdx = tokio::task::spawn_blocking(move || crate::licenses::detect_text(&text))
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?;
    let ctx = Ctx {
        r: RepoRef::new(&state.urls, &access),
        git_ref: &git_ref,
    };
    let ContentsJson::One(entry) = ctx.render(&found, false) else {
        return Err(ApiError::NotFound);
    };
    Ok(Json(LicenseContent {
        entry: *entry,
        license: Some(bgh_core::licenses::simple(&state.urls, &spdx)),
    })
    .into_response())
}

// ----- raw downloads ----------------------------------------------------------
// `GET /{o}/{r}/raw/{ref}/{path}` is served by `crate::download::raw`.

// ----- PUT / DELETE -------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
struct WriteBody {
    message: Option<String>,
    content: Option<String>,
    sha: Option<String>,
    branch: Option<String>,
    committer: Option<IdentityInput>,
    author: Option<IdentityInput>,
}

fn not_supplied(field: &str) -> ApiError {
    ApiError::unprocessable(format!("Invalid request.\n\n\"{field}\" wasn't supplied."))
}

/// Author and committer: each defaults to the other, both to the caller.
async fn identities(
    state: &AppState,
    user: &db::User,
    author: Option<&IdentityInput>,
    committer: Option<&IdentityInput>,
) -> ApiResult<(Identity, Identity)> {
    let a = author
        .map(|a| a.to_identity("Commit", "author"))
        .transpose()?;
    let c = committer
        .map(|c| c.to_identity("Commit", "committer"))
        .transpose()?;
    Ok(match (a, c) {
        (Some(a), Some(c)) => (a, c),
        (Some(a), None) => (a.clone(), a),
        (None, Some(c)) => (c.clone(), c),
        (None, None) => {
            let d = default_identity(state, user).await?;
            (d.clone(), d)
        }
    })
}

/// What a path currently is on the target branch.
enum Existing {
    Missing,
    File { sha: String, mode: String },
    Other,
}

/// Branch state for a contents write: `(tip, tip tree, existing entry)`.
/// `tip` is `None` only when the repository is empty.
async fn branch_state(
    state: &AppState,
    access: &RepoAccess,
    branch: &str,
    path: &str,
) -> ApiResult<(Option<String>, Option<String>, Existing)> {
    let not_found =
        || ApiError::Status(StatusCode::NOT_FOUND, format!("Branch {branch} not found"));
    if !bgh_git::is_valid_ref_name(branch) {
        return Err(not_found());
    }
    let refname = format!("refs/heads/{branch}");
    let p = path.to_string();
    let res = crate::store(state)
        .read(access.repo.id, move |r| {
            let Some(tip) = r.find_ref(&refname)? else {
                return Ok(if r.branches()?.is_empty() {
                    Some((None, None, Existing::Missing))
                } else {
                    None
                });
            };
            let commit = r.commit(&r.resolve_commit(&tip.peeled)?)?;
            let existing = match r.lookup_path(&commit.sha, &p) {
                Ok(PathLookup::Entry(e))
                    if matches!(
                        e.kind,
                        TreeEntryKind::Blob | TreeEntryKind::Executable | TreeEntryKind::Symlink
                    ) =>
                {
                    Existing::File {
                        sha: e.sha,
                        mode: e.mode,
                    }
                }
                Ok(_) => Existing::Other,
                Err(GitError::NotFound(_)) => Existing::Missing,
                Err(e) => return Err(e),
            };
            Ok(Some((Some(commit.sha), Some(commit.tree), existing)))
        })
        .await?;
    res.ok_or_else(not_found)
}

fn valid_path(path: &str) -> ApiResult<()> {
    let bad = path.is_empty()
        || path.contains(['\0', '\n'])
        || path
            .split('/')
            .any(|c| c == "." || c == ".." || c.eq_ignore_ascii_case(".git"));
    if bad {
        Err(ApiError::invalid_field(FieldError::invalid(
            "Content", "path",
        )))
    } else {
        Ok(())
    }
}

/// Commit `edit` on `branch` and move the branch (protection + post-receive).
#[allow(clippy::too_many_arguments)]
async fn commit_edit(
    state: &AppState,
    access: &RepoAccess,
    user: &db::User,
    branch: &str,
    tip: Option<&str>,
    tree: Option<&str>,
    edit: TreeEdit,
    message: &str,
    author: &Identity,
    committer: &Identity,
) -> ApiResult<bgh_git::Commit> {
    let git = crate::store(state).cli(access.repo.id)?;
    let new_tree = git.build_tree(tree, &[edit]).await?;
    let parents: Vec<String> = tip.iter().map(|s| s.to_string()).collect();
    let sha = git
        .commit_tree(&new_tree, &parents, message, author, committer)
        .await?;
    crate::refs::write_ref(
        state,
        access,
        user,
        &format!("refs/heads/{branch}"),
        tip,
        Some(&sha),
        false,
    )
    .await?;
    Ok(git.commit(&sha).await?)
}

/// Decode base64 content, tolerating line breaks.
fn decode_content(s: &str) -> Option<Vec<u8>> {
    let compact: String = s.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    STANDARD.decode(compact.as_bytes()).ok()
}

async fn put_file(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, path)): Path<(String, String, String)>,
    Json(body): Json<WriteBody>,
) -> ApiResult<Response> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    access.require(Permission::Write)?;
    access.require_not_archived()?;
    access.require_not_mirror()?;
    let path = normalize(&path);
    valid_path(&path)?;
    crate::workflow_scope::check_path(&state, &auth, &path).await?;
    let message = body
        .message
        .clone()
        .filter(|m| !m.is_empty())
        .ok_or_else(|| not_supplied("message"))?;
    let content = body
        .content
        .as_deref()
        .ok_or_else(|| not_supplied("content"))?;
    let data = decode_content(content).ok_or_else(|| {
        ApiError::unprocessable("Invalid request.\n\ncontent is not valid Base64.")
    })?;
    let (author, committer) = identities(
        &state,
        &auth.user,
        body.author.as_ref(),
        body.committer.as_ref(),
    )
    .await?;
    let branch = body
        .branch
        .clone()
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| access.repo.default_branch.clone());
    let branch = branch
        .strip_prefix("refs/heads/")
        .unwrap_or(&branch)
        .to_string();
    let (tip, tree, existing) = branch_state(&state, &access, &branch, &path).await?;
    let mode = match &existing {
        Existing::Missing => "100644".to_string(),
        Existing::File { sha, mode } => match body.sha.as_deref() {
            None | Some("") => return Err(not_supplied("sha")),
            Some(given) if !given.eq_ignore_ascii_case(sha) => {
                return Err(ApiError::conflict(format!("{path} does not match {given}")));
            }
            Some(_) => mode.clone(),
        },
        Existing::Other => {
            return Err(ApiError::unprocessable(format!(
                "Invalid request.\n\n{path} is not a file."
            )));
        }
    };
    let git = crate::store(&state).cli(access.repo.id)?;
    let blob = git.write_blob(&data).await?;
    let commit = commit_edit(
        &state,
        &access,
        &auth.user,
        &branch,
        tip.as_deref(),
        tree.as_deref(),
        TreeEdit::Object {
            path: path.clone(),
            mode,
            sha: blob.clone(),
        },
        &message,
        &author,
        &committer,
    )
    .await?;
    let r = RepoRef::new(&state.urls, &access);
    let ctx = Ctx {
        r,
        git_ref: &branch,
    };
    let body = FileCommit {
        content: Some(ctx.entry(TreeEntryKind::Blob, &path, &blob, data.len() as u64)),
        commit: git_commit(&r, &commit),
    };
    let status = if matches!(existing, Existing::Missing) {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(body)).into_response())
}

async fn delete_file(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, path)): Path<(String, String, String)>,
    Json(body): Json<WriteBody>,
) -> ApiResult<Response> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    access.require(Permission::Write)?;
    access.require_not_archived()?;
    access.require_not_mirror()?;
    let path = normalize(&path);
    valid_path(&path)?;
    crate::workflow_scope::check_path(&state, &auth, &path).await?;
    let message = body
        .message
        .clone()
        .filter(|m| !m.is_empty())
        .ok_or_else(|| not_supplied("message"))?;
    let given = body
        .sha
        .clone()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| not_supplied("sha"))?;
    let (author, committer) = identities(
        &state,
        &auth.user,
        body.author.as_ref(),
        body.committer.as_ref(),
    )
    .await?;
    let branch = body
        .branch
        .clone()
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| access.repo.default_branch.clone());
    let branch = branch
        .strip_prefix("refs/heads/")
        .unwrap_or(&branch)
        .to_string();
    let (tip, tree, existing) = branch_state(&state, &access, &branch, &path).await?;
    match (&existing, &tip) {
        (Existing::File { sha, .. }, Some(_)) => {
            if !given.eq_ignore_ascii_case(sha) {
                return Err(ApiError::conflict(format!("{path} does not match {given}")));
            }
        }
        _ => return Err(ApiError::NotFound),
    }
    let commit = commit_edit(
        &state,
        &access,
        &auth.user,
        &branch,
        tip.as_deref(),
        tree.as_deref(),
        TreeEdit::Delete { path },
        &message,
        &author,
        &committer,
    )
    .await?;
    let r = RepoRef::new(&state.urls, &access);
    Ok(Json(FileCommit {
        content: None,
        commit: git_commit(&r, &commit),
    })
    .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_wraps_at_60() {
        let s = base64_lines(&[b'a'; 100]);
        let lines: Vec<&str> = s.split_terminator('\n').collect();
        assert_eq!(lines[0].len(), 60);
        assert!(s.ends_with('\n'));
        assert_eq!(decode_content(&s).unwrap(), vec![b'a'; 100]);
        assert_eq!(base64_lines(b""), "");
    }

    #[test]
    fn readme_preference() {
        let mut names = [
            "README",
            "readme.txt",
            "README.md",
            "Readme.rst",
            "README.x",
        ];
        names.sort_by_key(|n| readme_rank(n));
        assert_eq!(names[0], "README.md");
        assert_eq!(readme_rank("READMEish"), None);
        assert_eq!(readme_rank("README.markdown"), Some(1));
    }

    #[test]
    fn symlink_targets() {
        assert_eq!(symlink_target("a/b", "c").as_deref(), Some("a/c"));
        assert_eq!(symlink_target("a/b", "../c").as_deref(), Some("c"));
        assert_eq!(symlink_target("b", "../c"), None);
        assert_eq!(symlink_target("b", "/etc/passwd"), None);
    }

    #[test]
    fn gitmodules_parse() {
        let m = parse_gitmodules(
            "[submodule \"lib\"]\n\tpath = vendor/lib\n\turl = https://example.com/lib.git\n",
        );
        assert_eq!(
            m.get("vendor/lib").map(String::as_str),
            Some("https://example.com/lib.git")
        );
    }
}
