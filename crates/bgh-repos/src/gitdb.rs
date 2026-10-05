//! Git database API: blobs, trees, commits, refs and annotated tags.
//!
//! * `GET|POST /repos/{o}/{r}/git/blobs[/{sha}]`
//! * `GET|POST /repos/{o}/{r}/git/trees[/{tree_sha}]` (`?recursive`)
//! * `GET|POST /repos/{o}/{r}/git/commits[/{sha}]`
//! * `GET /repos/{o}/{r}/git/ref/{ref}`, `GET /git/matching-refs/{ref}`,
//!   `GET /git/refs[/{prefix}]` (legacy), `POST /git/refs`,
//!   `PATCH|DELETE /git/refs/{ref}`
//! * `GET|POST /repos/{o}/{r}/git/tags[/{sha}]`
//!
//! Object lookups and validation run in one in-process gix pass per request
//! (object headers, no per-entry subprocesses). Ref writes go through
//! [`crate::refs::write_ref`] (branch protection + post-receive).

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use bgh_core::node_id::{self, NodeType};
use bgh_core::prelude::*;
use bgh_git::ops::LsTreeEntry;
use bgh_git::write::Identity;
use bgh_git::{GitRepo, GitResult, RefInfo, TreeEdit, is_sha};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::cache;
use crate::contents::base64_lines;
use crate::gitjson::{GitRef, GitTree, RepoRef, git_commit, git_ref, git_tag, tree_entry};
use crate::identity::{IdentityInput, default_identity};
use crate::media::{self, Media};

/// Maximum entries of a recursive tree listing.
const MAX_TREE_ENTRIES: usize = 100_000;
/// Largest blob served with the raw media type.
const RAW_LIMIT: u64 = 100 * 1024 * 1024;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/repos/{owner}/{repo}/git/blobs", post(create_blob))
        .route("/repos/{owner}/{repo}/git/blobs/{sha}", get(get_blob))
        .route("/repos/{owner}/{repo}/git/trees", post(create_tree))
        .route("/repos/{owner}/{repo}/git/trees/{*tree}", get(get_tree))
        .route("/repos/{owner}/{repo}/git/commits", post(create_commit))
        .route("/repos/{owner}/{repo}/git/commits/{sha}", get(get_commit))
        .route("/repos/{owner}/{repo}/git/ref/{*ref}", get(get_ref))
        .route(
            "/repos/{owner}/{repo}/git/matching-refs/{*ref}",
            get(matching_refs),
        )
        .route(
            "/repos/{owner}/{repo}/git/matching-refs/",
            get(matching_refs_all),
        )
        .route(
            "/repos/{owner}/{repo}/git/refs",
            get(list_refs_all).post(create_ref),
        )
        .route(
            "/repos/{owner}/{repo}/git/refs/{*ref}",
            get(list_refs).patch(update_ref).delete(delete_ref),
        )
        .route("/repos/{owner}/{repo}/git/tags", post(create_tag))
        .route("/repos/{owner}/{repo}/git/tags/{sha}", get(get_tag))
}

// ----- helpers ----------------------------------------------------------------

fn not_supplied(field: &str) -> ApiError {
    ApiError::unprocessable(format!("Invalid request.\n\n\"{field}\" wasn't supplied."))
}

fn created_at(location: &str, body: impl Serialize) -> Response {
    let mut resp = (StatusCode::CREATED, Json(body)).into_response();
    if let Ok(v) = HeaderValue::from_str(location) {
        resp.headers_mut().insert(header::LOCATION, v);
    }
    resp
}

/// Load the repository for a write to its object database / refs.
async fn writable(
    state: &AppState,
    auth: &AuthContext,
    owner: &str,
    repo: &str,
) -> ApiResult<RepoAccess> {
    let access = RepoAccess::load(state, Some(auth), owner, repo).await?;
    access.require(Permission::Write)?;
    access.require_not_archived()?;
    access.require_not_mirror()?;
    Ok(access)
}

/// Expand a full or abbreviated (7–39 hex digits, unique) object SHA;
/// 404 for anything else. The flag tells whether the SHA was given in
/// full (only then is the response cacheable as immutable).
async fn full_sha(state: &AppState, access: &RepoAccess, sha: &str) -> ApiResult<(String, bool)> {
    let sha = sha.to_ascii_lowercase();
    if is_sha(&sha) {
        return Ok((sha, true));
    }
    if !(7..40).contains(&sha.len()) || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(ApiError::NotFound);
    }
    let prefix = sha.clone();
    let found = crate::store(state)
        .read(access.repo.id, move |r| r.resolve(&prefix))
        .await?;
    // `resolve` also accepts ref names; keep only object-prefix matches.
    match found {
        Some(full) if full.starts_with(&sha) => Ok((full, false)),
        _ => Err(ApiError::NotFound),
    }
}

/// Immutable caching only for responses addressed by a full SHA.
fn cacheable(resp: Response, private: bool, full: bool) -> Response {
    if full {
        media::immutable(resp, private)
    } else {
        resp
    }
}

/// `{type, size}` of an object, if it exists.
fn header_of(r: &GitRepo, sha: &str) -> GitResult<Option<(&'static str, u64)>> {
    if !is_sha(sha) {
        return Ok(None);
    }
    r.header(&sha.to_ascii_lowercase())
}

// ----- blobs --------------------------------------------------------------------

#[derive(Serialize)]
struct BlobJson {
    content: String,
    encoding: &'static str,
    url: String,
    sha: String,
    size: u64,
    node_id: String,
}

async fn get_blob(
    State(state): State<AppState>,
    auth: MaybeUser,
    headers: HeaderMap,
    Path((owner, repo, sha)): Path<(String, String, String)>,
) -> ApiResult<Response> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let (sha, full) = full_sha(&state, &access, &sha).await?;
    let m = media::media(&headers);
    let limit = if m == Media::Raw {
        RAW_LIMIT
    } else {
        state.config.max_blob_size
    };
    let s = sha.clone();
    let blob = crate::store(&state)
        .read(access.repo.id, move |r| r.blob_with_limit(&s, limit))
        .await?;
    let private = access.repo.is_private();
    if m == Media::Raw {
        let ct = if blob.is_binary() {
            "application/octet-stream"
        } else {
            "text/plain; charset=utf-8"
        };
        return Ok(cacheable(media::body(m, ct, blob.data), private, full));
    }
    let r = RepoRef::new(&state.urls, &access);
    let json = BlobJson {
        content: base64_lines(&blob.data),
        encoding: "base64",
        url: r.api(&format!("/git/blobs/{sha}")),
        size: blob.size,
        node_id: node_id::encode_str(NodeType::Blob, &format!("{}:{sha}", access.repo.id)),
        sha,
    };
    Ok(cacheable(Json(json).into_response(), private, full))
}

#[derive(Deserialize)]
struct CreateBlob {
    content: Option<String>,
    encoding: Option<String>,
}

#[derive(Serialize)]
struct ShaUrlJson {
    url: String,
    sha: String,
}

async fn create_blob(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo)): Path<(String, String)>,
    Json(body): Json<CreateBlob>,
) -> ApiResult<Response> {
    let access = writable(&state, &auth, &owner, &repo).await?;
    let content = body.content.ok_or_else(|| not_supplied("content"))?;
    let data = match body.encoding.as_deref().unwrap_or("utf-8") {
        "utf-8" | "utf8" => content.into_bytes(),
        "base64" => {
            let compact: String = content
                .chars()
                .filter(|c| !c.is_ascii_whitespace())
                .collect();
            STANDARD.decode(compact.as_bytes()).map_err(|_| {
                ApiError::unprocessable("Invalid request.\n\ncontent is not valid Base64.")
            })?
        }
        other => {
            return Err(ApiError::unprocessable(format!(
                "encoding must be either 'utf-8' or 'base64', got {other:?}"
            )));
        }
    };
    let sha = crate::store(&state)
        .cli(access.repo.id)?
        .write_blob(&data)
        .await?;
    let url = RepoRef::new(&state.urls, &access).api(&format!("/git/blobs/{sha}"));
    Ok(created_at(&url.clone(), ShaUrlJson { url, sha }))
}

// ----- trees --------------------------------------------------------------------

/// Peel a revision (tree SHA, commit, tag, branch) to a tree SHA.
fn resolve_tree(r: &GitRepo, rev: &str) -> GitResult<Option<String>> {
    let Some(mut sha) = r.resolve(rev)? else {
        return Ok(None);
    };
    for _ in 0..16 {
        match r.header(&sha)? {
            Some(("tree", _)) => return Ok(Some(sha)),
            Some(("commit", _)) => sha = r.commit(&sha)?.tree,
            Some(("tag", _)) => sha = r.tag(&sha)?.object,
            _ => return Ok(None),
        }
    }
    Ok(None)
}

fn render_tree(r: &RepoRef, sha: &str, entries: &[LsTreeEntry], truncated: bool) -> GitTree {
    GitTree {
        sha: sha.to_string(),
        url: r.api(&format!("/git/trees/{sha}")),
        tree: entries
            .iter()
            .map(|e| tree_entry(r, e.path.clone(), &e.mode, e.kind, &e.sha, e.size))
            .collect(),
        truncated,
    }
}

#[derive(Deserialize)]
struct TreeQuery {
    recursive: Option<String>,
}

async fn get_tree(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, tree)): Path<(String, String, String)>,
    Query(q): Query<TreeQuery>,
) -> ApiResult<Response> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let store = crate::store(&state);
    let rev = tree.clone();
    let sha = store
        .read(access.repo.id, move |r| resolve_tree(r, &rev))
        .await?
        .ok_or(ApiError::NotFound)?;
    let git = store.cli(access.repo.id)?;
    // GitHub: any value of `recursive` enables it.
    let recursive = q.recursive.is_some();
    let entries: Vec<LsTreeEntry> = if recursive {
        cache::cached(&state, &format!("tree-r:{sha}"), || async {
            Ok(git.ls_tree(&sha, true).await?)
        })
        .await?
    } else {
        git.ls_tree(&sha, false).await?
    };
    let truncated = entries.len() > MAX_TREE_ENTRIES;
    let shown = &entries[..entries.len().min(MAX_TREE_ENTRIES)];
    let json = render_tree(&RepoRef::new(&state.urls, &access), &sha, shown, truncated);
    let resp = Json(json).into_response();
    Ok(if is_sha(&tree) && tree.eq_ignore_ascii_case(&sha) {
        media::immutable(resp, access.repo.is_private())
    } else {
        resp
    })
}

#[derive(Deserialize)]
struct CreateTree {
    base_tree: Option<String>,
    tree: Option<Vec<Map<String, Value>>>,
}

/// A validated `tree[]` entry.
enum TreeInput {
    Object {
        path: String,
        mode: String,
        kind: &'static str,
        sha: String,
    },
    Content {
        path: String,
        mode: String,
        content: String,
    },
    Delete {
        path: String,
    },
}

fn parse_tree_entry(m: &Map<String, Value>) -> ApiResult<TreeInput> {
    let invalid = |msg: String| ApiError::unprocessable(format!("Invalid request.\n\n{msg}"));
    let path = m
        .get("path")
        .and_then(Value::as_str)
        .filter(|p| !p.is_empty())
        .ok_or_else(|| not_supplied("path"))?
        .to_string();
    let mode = m
        .get("mode")
        .and_then(Value::as_str)
        .ok_or_else(|| not_supplied("mode"))?
        .to_string();
    let mode_kind = match mode.as_str() {
        "100644" | "100755" | "120000" => "blob",
        "040000" | "40000" => "tree",
        "160000" => "commit",
        other => return Err(invalid(format!("{other:?} is not a valid tree mode."))),
    };
    let kind = match m.get("type").and_then(Value::as_str) {
        Some("blob") => "blob",
        Some("tree") => "tree",
        Some("commit") => "commit",
        None => mode_kind,
        Some(other) => return Err(invalid(format!("{other:?} is not a valid tree type."))),
    };
    if kind != mode_kind {
        return Err(invalid(format!(
            "tree.type {kind} does not match tree.mode {mode}"
        )));
    }
    let both = || {
        invalid(
            "Must supply either tree.sha or tree.content. Request will be rejected if both are present."
                .into(),
        )
    };
    match (m.get("sha"), m.get("content")) {
        (Some(Value::Null), None | Some(Value::Null)) => Ok(TreeInput::Delete { path }),
        (Some(Value::String(sha)), None | Some(Value::Null)) => {
            if !is_sha(sha) {
                return Err(invalid(format!("tree.sha {sha} is not a valid {kind}")));
            }
            Ok(TreeInput::Object {
                path,
                mode,
                kind,
                sha: sha.to_ascii_lowercase(),
            })
        }
        (None, Some(Value::String(content))) if kind == "blob" => Ok(TreeInput::Content {
            path,
            mode,
            content: content.clone(),
        }),
        _ => Err(both()),
    }
}

async fn create_tree(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo)): Path<(String, String)>,
    Json(body): Json<CreateTree>,
) -> ApiResult<Response> {
    let access = writable(&state, &auth, &owner, &repo).await?;
    let inputs = body
        .tree
        .ok_or_else(|| not_supplied("tree"))?
        .iter()
        .map(parse_tree_entry)
        .collect::<ApiResult<Vec<_>>>()?;
    let base = body.base_tree.filter(|b| !b.is_empty());
    let store = crate::store(&state);

    // One gix pass: resolve base_tree, check referenced objects, write
    // inline contents as blobs.
    let checked = store
        .read(access.repo.id, move |r| {
            let base = match base {
                Some(b) => match resolve_tree(r, &b)? {
                    Some(t) => Some(t),
                    None => return Ok(Err("base_tree is not a valid tree oid".to_string())),
                },
                None => None,
            };
            let mut edits = Vec::with_capacity(inputs.len());
            for input in inputs {
                edits.push(match input {
                    TreeInput::Delete { path } => TreeEdit::Delete { path },
                    TreeInput::Object {
                        path,
                        mode,
                        kind,
                        sha,
                    } => {
                        if kind != "commit" && r.header(&sha)?.map(|h| h.0) != Some(kind) {
                            return Ok(Err(format!("tree.sha {sha} is not a valid {kind}")));
                        }
                        TreeEdit::Object { path, mode, sha }
                    }
                    TreeInput::Content {
                        path,
                        mode,
                        content,
                    } => {
                        let id = r
                            .gix()
                            .write_blob(content.as_bytes())
                            .map_err(|e| bgh_git::GitError::Object(e.to_string()))?;
                        TreeEdit::Object {
                            path,
                            mode,
                            sha: id.detach().to_string(),
                        }
                    }
                });
            }
            Ok(Ok((base, edits)))
        })
        .await?;
    let (base, edits) = checked.map_err(ApiError::unprocessable)?;
    let git = store.cli(access.repo.id)?;
    let sha = git.build_tree(base.as_deref(), &edits).await?;
    let entries = git.ls_tree(&sha, false).await?;
    let r = RepoRef::new(&state.urls, &access);
    let json = render_tree(&r, &sha, &entries, false);
    Ok(created_at(&json.url.clone(), json))
}

// ----- commits ------------------------------------------------------------------

async fn get_commit(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, sha)): Path<(String, String, String)>,
) -> ApiResult<Response> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let (s, full) = full_sha(&state, &access, &sha).await?;
    let commit = crate::store(&state)
        .read(access.repo.id, move |r| r.commit(&s))
        .await?;
    let json = git_commit(&RepoRef::new(&state.urls, &access), &commit);
    Ok(cacheable(
        Json(json).into_response(),
        access.repo.is_private(),
        full,
    ))
}

#[derive(Deserialize)]
struct CreateCommit {
    message: Option<String>,
    tree: Option<String>,
    #[serde(default)]
    parents: Vec<String>,
    author: Option<IdentityInput>,
    committer: Option<IdentityInput>,
    signature: Option<String>,
}

fn signature_line(id: &Identity) -> String {
    let when = id.when.unwrap_or_else(chrono::Utc::now);
    format!(
        "{} <{}> {} +0000",
        id.name.replace(['<', '>', '\n'], ""),
        id.email.replace(['<', '>', '\n'], ""),
        when.timestamp()
    )
}

/// Raw commit object text with a `gpgsig` header.
fn signed_commit_text(
    tree: &str,
    parents: &[String],
    author: &Identity,
    committer: &Identity,
    signature: &str,
    message: &str,
) -> String {
    let mut s = format!("tree {tree}\n");
    for p in parents {
        s.push_str(&format!("parent {p}\n"));
    }
    s.push_str(&format!("author {}\n", signature_line(author)));
    s.push_str(&format!("committer {}\n", signature_line(committer)));
    let sig = signature.trim_end_matches('\n');
    if !sig.is_empty() {
        s.push_str("gpgsig ");
        s.push_str(&sig.replace('\n', "\n "));
        s.push('\n');
    }
    s.push('\n');
    s.push_str(message);
    s
}

async fn create_commit(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo)): Path<(String, String)>,
    Json(body): Json<CreateCommit>,
) -> ApiResult<Response> {
    let access = writable(&state, &auth, &owner, &repo).await?;
    let message = body.message.ok_or_else(|| not_supplied("message"))?;
    let tree = body
        .tree
        .filter(|t| !t.is_empty())
        .ok_or_else(|| not_supplied("tree"))?
        .to_ascii_lowercase();
    let parents: Vec<String> = body
        .parents
        .iter()
        .map(|p| p.to_ascii_lowercase())
        .collect();
    let author = match &body.author {
        Some(a) => a.to_identity("Commit", "author")?,
        None => default_identity(&state, &auth.user).await?,
    };
    let committer = match &body.committer {
        Some(c) => c.to_identity("Commit", "committer")?,
        None => author.clone(),
    };
    let store = crate::store(&state);
    let (t, ps) = (tree.clone(), parents.clone());
    let problem = store
        .read(access.repo.id, move |r| {
            if header_of(r, &t)?.map(|h| h.0) != Some("tree") {
                return Ok(Some("Tree SHA does not exist"));
            }
            for p in &ps {
                if header_of(r, p)?.map(|h| h.0) != Some("commit") {
                    return Ok(Some("Parent SHA does not exist or is not a commit object"));
                }
            }
            Ok(None)
        })
        .await?;
    if let Some(msg) = problem {
        return Err(ApiError::unprocessable(msg));
    }
    let git = store.cli(access.repo.id)?;
    let sha = match body.signature.as_deref().filter(|s| !s.trim().is_empty()) {
        None => {
            git.commit_tree(&tree, &parents, &message, &author, &committer)
                .await?
        }
        Some(sig) => {
            let text = signed_commit_text(&tree, &parents, &author, &committer, sig, &message);
            let out = git
                .run(
                    &["hash-object", "-t", "commit", "-w", "--stdin"],
                    &[],
                    Some(text.as_bytes()),
                )
                .await?;
            String::from_utf8_lossy(&out).trim().to_string()
        }
    };
    let commit = git.commit(&sha).await?;
    let json = git_commit(&RepoRef::new(&state.urls, &access), &commit);
    Ok(created_at(&json.url.clone(), json))
}

// ----- refs ---------------------------------------------------------------------

/// `heads/main` → `refs/heads/main` (a leading `refs/` is tolerated).
fn full_ref(r: &str) -> String {
    let r = r.trim_matches('/');
    if r.starts_with("refs/") {
        r.to_string()
    } else {
        format!("refs/{r}")
    }
}

/// Refs whose name satisfies `keep`, with their object types (one gix pass).
async fn load_refs(
    state: &AppState,
    access: &RepoAccess,
    keep: impl Fn(&str) -> bool + Send + 'static,
) -> ApiResult<Vec<(RefInfo, &'static str)>> {
    Ok(crate::store(state)
        .read(access.repo.id, move |r| {
            let mut out = Vec::new();
            for info in r.refs("refs/")? {
                if info.name.starts_with("refs/bgh-tmp/") || !keep(&info.name) {
                    continue;
                }
                let kind = r.header(&info.target)?.map(|h| h.0).unwrap_or("commit");
                out.push((info, kind));
            }
            Ok(out)
        })
        .await?)
}

/// The ref named exactly `name`, with its object type.
async fn find_exact(
    state: &AppState,
    access: &RepoAccess,
    name: &str,
) -> ApiResult<Option<(RefInfo, &'static str)>> {
    if !bgh_git::is_valid_ref_name(name) {
        return Ok(None);
    }
    let n = name.to_string();
    Ok(crate::store(state)
        .read(access.repo.id, move |r| {
            let Some(info) = r.find_ref(&n)?.filter(|i| i.name == n) else {
                return Ok(None);
            };
            let kind = r.header(&info.target)?.map(|h| h.0).unwrap_or("commit");
            Ok(Some((info, kind)))
        })
        .await?)
}

fn ref_json(r: &RepoRef, info: &RefInfo, kind: &str) -> GitRef {
    git_ref(r, &info.name, kind, &info.target)
}

async fn get_ref(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, name)): Path<(String, String, String)>,
) -> ApiResult<Json<GitRef>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let (info, kind) = find_exact(&state, &access, &full_ref(&name))
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(ref_json(
        &RepoRef::new(&state.urls, &access),
        &info,
        kind,
    )))
}

async fn matching(
    state: &AppState,
    auth: &MaybeUser,
    owner: &str,
    repo: &str,
    prefix: String,
) -> ApiResult<Json<Vec<GitRef>>> {
    let access = RepoAccess::load(state, auth.as_ref(), owner, repo).await?;
    let refs = load_refs(state, &access, move |n| n.starts_with(&prefix)).await?;
    let r = RepoRef::new(&state.urls, &access);
    Ok(Json(
        refs.iter()
            .map(|(info, kind)| ref_json(&r, info, kind))
            .collect(),
    ))
}

async fn matching_refs(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, prefix)): Path<(String, String, String)>,
) -> ApiResult<Json<Vec<GitRef>>> {
    let p = prefix.trim_start_matches('/');
    let prefix = if p.starts_with("refs/") {
        p.to_string()
    } else {
        format!("refs/{p}")
    };
    matching(&state, &auth, &owner, &repo, prefix).await
}

async fn matching_refs_all(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<Json<Vec<GitRef>>> {
    matching(&state, &auth, &owner, &repo, "refs/".into()).await
}

/// Legacy listing: an exact match renders as an object, otherwise every ref
/// starting with the prefix (404 when none).
async fn legacy_list(
    state: &AppState,
    auth: &MaybeUser,
    p: Pagination,
    owner: &str,
    repo: &str,
    prefix: Option<String>,
) -> ApiResult<Response> {
    let access = RepoAccess::load(state, auth.as_ref(), owner, repo).await?;
    let r = RepoRef::new(&state.urls, &access);
    let wanted = prefix.as_deref().map(full_ref);
    if let Some(name) = &wanted
        && let Some((info, kind)) = find_exact(state, &access, name).await?
    {
        return Ok(Json(ref_json(&r, &info, kind)).into_response());
    }
    let pre = wanted.clone().unwrap_or_else(|| "refs/".into());
    let refs = load_refs(state, &access, move |n| n.starts_with(&pre)).await?;
    if refs.is_empty() {
        return Err(if wanted.is_none() {
            ApiError::conflict("Git Repository is empty.")
        } else {
            ApiError::NotFound
        });
    }
    let total = refs.len() as i64;
    let items: Vec<GitRef> = refs
        .iter()
        .skip(p.offset() as usize)
        .take(p.limit() as usize)
        .map(|(info, kind)| ref_json(&r, info, kind))
        .collect();
    Ok(p.page_with_total(items, total).into_response())
}

async fn list_refs_all(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<Response> {
    legacy_list(&state, &auth, p, &owner, &repo, None).await
}

async fn list_refs(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo, prefix)): Path<(String, String, String)>,
) -> ApiResult<Response> {
    legacy_list(&state, &auth, p, &owner, &repo, Some(prefix)).await
}

#[derive(Deserialize)]
pub struct CreateRef {
    #[serde(rename = "ref")]
    pub refname: Option<String>,
    pub sha: Option<String>,
}

async fn object_exists(state: &AppState, access: &RepoAccess, sha: &str) -> ApiResult<bool> {
    let s = sha.to_string();
    Ok(crate::store(state)
        .read(access.repo.id, move |r| Ok(header_of(r, &s)?.is_some()))
        .await?)
}

pub async fn create_ref(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo)): Path<(String, String)>,
    Json(body): Json<CreateRef>,
) -> ApiResult<Response> {
    let access = writable(&state, &auth, &owner, &repo).await?;
    let name = body
        .refname
        .filter(|r| !r.is_empty())
        .ok_or_else(|| not_supplied("ref"))?;
    let sha = body
        .sha
        .filter(|s| !s.is_empty())
        .ok_or_else(|| not_supplied("sha"))?
        .to_ascii_lowercase();
    if !name.starts_with("refs/")
        || name.matches('/').count() < 2
        || !bgh_git::is_valid_ref_name(&name)
    {
        return Err(ApiError::unprocessable(format!(
            "{name} is not a valid ref name."
        )));
    }
    if !object_exists(&state, &access, &sha).await? {
        return Err(ApiError::unprocessable("Object does not exist"));
    }
    if find_exact(&state, &access, &name).await?.is_some() {
        return Err(ApiError::unprocessable("Reference already exists"));
    }
    crate::refs::write_ref(&state, &access, &auth.user, &name, None, Some(&sha), false).await?;
    let (info, kind) = find_exact(&state, &access, &name)
        .await?
        .ok_or(ApiError::NotFound)?;
    let json = ref_json(&RepoRef::new(&state.urls, &access), &info, kind);
    Ok(created_at(&json.url.clone(), json))
}

#[derive(Deserialize)]
pub struct UpdateRef {
    pub sha: Option<String>,
    #[serde(default)]
    pub force: bool,
}

pub async fn update_ref(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, name)): Path<(String, String, String)>,
    Json(body): Json<UpdateRef>,
) -> ApiResult<Json<GitRef>> {
    let access = writable(&state, &auth, &owner, &repo).await?;
    let name = full_ref(&name);
    let sha = body
        .sha
        .filter(|s| !s.is_empty())
        .ok_or_else(|| not_supplied("sha"))?
        .to_ascii_lowercase();
    let (current, _) = find_exact(&state, &access, &name)
        .await?
        .ok_or_else(|| ApiError::unprocessable("Reference does not exist"))?;
    if !object_exists(&state, &access, &sha).await? {
        return Err(ApiError::unprocessable("Object does not exist"));
    }
    crate::refs::write_ref(
        &state,
        &access,
        &auth.user,
        &name,
        Some(&current.target),
        Some(&sha),
        body.force,
    )
    .await?;
    let (info, kind) = find_exact(&state, &access, &name)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(ref_json(
        &RepoRef::new(&state.urls, &access),
        &info,
        kind,
    )))
}

pub async fn delete_ref(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, name)): Path<(String, String, String)>,
) -> ApiResult<StatusCode> {
    let access = writable(&state, &auth, &owner, &repo).await?;
    let name = full_ref(&name);
    let (current, _) = find_exact(&state, &access, &name)
        .await?
        .ok_or_else(|| ApiError::unprocessable("Reference does not exist"))?;
    crate::refs::write_ref(
        &state,
        &access,
        &auth.user,
        &name,
        Some(&current.target),
        None,
        false,
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

// ----- tags ---------------------------------------------------------------------

async fn get_tag(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, sha)): Path<(String, String, String)>,
) -> ApiResult<Response> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let (s, full) = full_sha(&state, &access, &sha).await?;
    let tag = crate::store(&state)
        .read(access.repo.id, move |r| r.tag(&s))
        .await?;
    let json = git_tag(&RepoRef::new(&state.urls, &access), &tag);
    Ok(cacheable(
        Json(json).into_response(),
        access.repo.is_private(),
        full,
    ))
}

#[derive(Deserialize)]
struct CreateTag {
    tag: Option<String>,
    message: Option<String>,
    object: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    tagger: Option<IdentityInput>,
}

async fn create_tag(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo)): Path<(String, String)>,
    Json(body): Json<CreateTag>,
) -> ApiResult<Response> {
    let access = writable(&state, &auth, &owner, &repo).await?;
    let name = body
        .tag
        .filter(|t| !t.is_empty())
        .ok_or_else(|| not_supplied("tag"))?;
    let message = body.message.ok_or_else(|| not_supplied("message"))?;
    let object = body
        .object
        .filter(|o| !o.is_empty())
        .ok_or_else(|| not_supplied("object"))?
        .to_ascii_lowercase();
    let kind = body.kind.ok_or_else(|| not_supplied("type"))?;
    if !matches!(kind.as_str(), "commit" | "tree" | "blob" | "tag") {
        return Err(ApiError::unprocessable(format!(
            "Invalid request.\n\n{kind:?} is not a valid object type."
        )));
    }
    if !bgh_git::is_valid_ref_name(&name) {
        return Err(ApiError::unprocessable(format!(
            "{name} is not a valid tag name."
        )));
    }
    let tagger = match &body.tagger {
        Some(t) => t.to_identity("Tag", "tagger")?,
        None => default_identity(&state, &auth.user).await?,
    };
    let store = crate::store(&state);
    let o = object.clone();
    let actual = store
        .read(access.repo.id, move |r| Ok(header_of(r, &o)?.map(|h| h.0)))
        .await?;
    match actual {
        None => return Err(ApiError::unprocessable("Object does not exist")),
        Some(t) if t != kind => {
            return Err(ApiError::unprocessable(format!(
                "Object {object} is a {t}, not a {kind}"
            )));
        }
        Some(_) => {}
    }
    let git = store.cli(access.repo.id)?;
    let sha = git
        .write_tag(&name, &message, &object, &kind, &tagger)
        .await?;
    let tag = git.tag(&sha).await?;
    let json = git_tag(&RepoRef::new(&state.urls, &access), &tag);
    Ok(created_at(&json.url.clone(), json))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_commit_layout() {
        let id = Identity {
            name: "A".into(),
            email: "a@x".into(),
            when: Some(chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap()),
        };
        let text = signed_commit_text(
            "t",
            &["p".into()],
            &id,
            &id,
            "-----BEGIN-----\nabc\n-----END-----\n",
            "msg\n",
        );
        assert_eq!(
            text,
            "tree t\nparent p\nauthor A <a@x> 1700000000 +0000\ncommitter A <a@x> 1700000000 +0000\n\
gpgsig -----BEGIN-----\n abc\n -----END-----\n\nmsg\n"
        );
    }

    #[test]
    fn full_refs() {
        assert_eq!(full_ref("heads/main"), "refs/heads/main");
        assert_eq!(full_ref("refs/tags/v1"), "refs/tags/v1");
    }
}
