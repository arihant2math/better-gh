//! Private web endpoints for the code browser, under
//! `/_bgh/repos/{owner}/{repo}/...` (compact client shapes, not REST):
//!
//! | path | |
//! |------|-|
//! | `refs` | branches + tags (ref picker) |
//! | `tree[/{ref}[/{path}]]` | directory listing (+ README, cached last commits) |
//! | `tree-commits/{ref}[/{path}]` | last commit per entry |
//! | `blob/{ref}/{path}` | file view: highlighted lines, image/binary/LFS detection |
//! | `blame/{ref}/{path}` | blame (JSON, or NDJSON stream when `Accept: application/x-ndjson`) |
//! | `history/{ref}[/{path}]` | commits touching a path |
//! | `readme/{ref}[/{dir}]` | rendered README of a directory |
//! | `branch-list` | branches with last commit, ahead/behind, protection, PR |
//! | `files[/{ref}]` | every file path of a commit (file finder) |
//! | `commit-status?sha=…` | CI rollup per commit (statuses + check runs) |
//!
//! Plus `GET /_bgh/render/blob/{owner}/{repo}/{blob_sha}?path=` (highlighted
//! lines of one blob, docs/SYNC_PROTOCOL.md §10).
//!
//! `{ref}` may contain slashes (see `GitRepo::split_ref_path`). Responses
//! for a full commit SHA are immutable (`max-age=31536000, immutable`,
//! `private` for private repositories) and carry an ETag derived from the
//! request, so revalidation never recomputes anything; ref-based responses
//! use a short TTL plus a content ETag. Expensive git-derived results are
//! cached in Redis keyed by object/commit SHA.

pub mod blame;
pub mod blob;
pub mod history;
pub mod overview;
pub mod readme;
pub mod refs;
pub mod render;
pub mod tree;

use std::collections::HashMap;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use bgh_core::prelude::*;
use redis::AsyncCommands;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Bumped when cached shapes change.
pub(crate) const CACHE_VERSION: &str = "v2";
/// Redis TTL for git-derived caches (content is immutable; TTL bounds memory).
pub(crate) const CACHE_TTL_SECS: u64 = 7 * 24 * 3600;
/// `max-age` of ref-based (mutable) responses.
pub(crate) const SHORT_TTL_SECS: u32 = 30;

pub fn web_router() -> Router<AppState> {
    Router::new()
        .route("/_bgh/render/blob/{owner}/{repo}/{sha}", get(render::blob))
        .route("/_bgh/render/code", axum::routing::post(render::code))
        .route("/_bgh/repos/{owner}/{repo}/refs", get(refs::list))
        .route("/_bgh/repos/{owner}/{repo}/tree", get(tree::root))
        .route("/_bgh/repos/{owner}/{repo}/tree/{*spec}", get(tree::get))
        .route(
            "/_bgh/repos/{owner}/{repo}/tree-commits/{*spec}",
            get(tree::last_commits),
        )
        .route("/_bgh/repos/{owner}/{repo}/blob/{*spec}", get(blob::get))
        .route("/_bgh/repos/{owner}/{repo}/blame/{*spec}", get(blame::get))
        .route("/_bgh/repos/{owner}/{repo}/history", get(history::root))
        .route(
            "/_bgh/repos/{owner}/{repo}/history/{*spec}",
            get(history::get),
        )
        .route(
            "/_bgh/repos/{owner}/{repo}/branch-list",
            get(overview::branch_list),
        )
        .route(
            "/_bgh/repos/{owner}/{repo}/files",
            get(overview::files_root),
        )
        .route(
            "/_bgh/repos/{owner}/{repo}/files/{*spec}",
            get(overview::files),
        )
        .route(
            "/_bgh/repos/{owner}/{repo}/commit-status",
            get(overview::commit_status),
        )
        .route("/_bgh/repos/{owner}/{repo}/readme", get(readme::root))
        .route(
            "/_bgh/repos/{owner}/{repo}/readme/{*spec}",
            get(readme::get),
        )
}

/// A repository plus a resolved `{ref}/{path}`.
pub struct Target {
    pub access: RepoAccess,
    /// Ref as written in the URL (`main`, `feature/x`, a SHA).
    pub refname: String,
    pub commit: String,
    /// Path inside the tree, without leading/trailing slashes.
    pub path: String,
}

impl Target {
    /// Addressed by a full commit SHA, so the response never changes.
    pub fn immutable(&self) -> bool {
        bgh_git::is_sha(&self.refname)
    }

    pub fn store(&self, state: &AppState) -> bgh_git::RepoStore {
        crate::store(state)
    }
}

/// Resolve `spec` (`{ref}[/{path}]`, `None` = default branch root) for the
/// caller. 404 if the repository, ref or path doesn't exist.
pub async fn resolve(
    state: &AppState,
    auth: Option<&AuthContext>,
    owner: &str,
    repo: &str,
    spec: Option<&str>,
) -> ApiResult<Target> {
    let access = RepoAccess::load(state, auth, owner, repo).await?;
    resolve_with(state, access, spec).await
}

/// [`resolve`] for an already authorized repository.
pub async fn resolve_with(
    state: &AppState,
    access: RepoAccess,
    spec: Option<&str>,
) -> ApiResult<Target> {
    let store = crate::store(state);
    let spec = match spec.map(|s| s.trim_matches('/')).filter(|s| !s.is_empty()) {
        Some(s) => s.to_string(),
        None => access.repo.default_branch.clone(),
    };
    let (refname, commit, path) = store
        .read(access.repo.id, move |r| r.split_ref_path(&spec))
        .await?;
    Ok(Target {
        access,
        refname,
        commit,
        path,
    })
}

/// `Cache-Control` for a response about `t`.
pub fn cache_control(private: bool, immutable: bool) -> HeaderValue {
    let vis = if private { "private" } else { "public" };
    let v = if immutable {
        format!("{vis}, max-age=31536000, immutable")
    } else {
        format!("{vis}, max-age={SHORT_TTL_SECS}")
    };
    HeaderValue::from_str(&v).expect("valid header")
}

/// Strong ETag from arbitrary parts.
pub fn etag_of(parts: &[&str]) -> String {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p.as_bytes());
        h.update([0]);
    }
    format!("\"{}\"", hex::encode(&h.finalize()[..16]))
}

fn matches_etag(req: &HeaderMap, etag: &str) -> bool {
    req.get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|t| t.trim() == etag || t.trim() == "*"))
}

/// 304 response if the request already has `etag`.
pub fn not_modified(req: &HeaderMap, etag: &str, cache: HeaderValue) -> Option<Response> {
    matches_etag(req, etag).then(|| {
        let mut r = StatusCode::NOT_MODIFIED.into_response();
        r.headers_mut()
            .insert(header::ETAG, HeaderValue::from_str(etag).expect("etag"));
        r.headers_mut().insert(header::CACHE_CONTROL, cache);
        r
    })
}

/// Serialize `body` with caching headers. For immutable targets the ETag
/// is derived from the request (`key`), otherwise from the content.
pub fn json_response<T: Serialize>(
    req: &HeaderMap,
    t: &Target,
    key: &str,
    body: &T,
) -> ApiResult<Response> {
    let cache = cache_control(t.access.repo.is_private(), t.immutable());
    let bytes = serde_json::to_vec(body).map_err(ApiError::internal)?;
    let etag = if t.immutable() {
        etag_of(&[CACHE_VERSION, key])
    } else {
        etag_of(&[CACHE_VERSION, &hex::encode(Sha256::digest(&bytes))])
    };
    if let Some(r) = not_modified(req, &etag, cache.clone()) {
        return Ok(r);
    }
    let mut resp = Response::new(Body::from(bytes));
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    h.insert(header::CACHE_CONTROL, cache);
    h.insert(header::ETAG, HeaderValue::from_str(&etag).expect("etag"));
    h.insert(
        header::VARY,
        HeaderValue::from_static("Accept, Cookie, Authorization"),
    );
    Ok(resp)
}

/// Early 304 for immutable targets, before any work is done.
pub fn precheck(req: &HeaderMap, t: &Target, key: &str) -> Option<Response> {
    t.immutable().then_some(())?;
    let cache = cache_control(t.access.repo.is_private(), true);
    not_modified(req, &etag_of(&[CACHE_VERSION, key]), cache)
}

// ----- Redis cache ------------------------------------------------------

pub async fn cache_get<T: DeserializeOwned>(state: &AppState, key: &str) -> Option<T> {
    let mut redis = state.redis.clone();
    let raw: Option<Vec<u8>> = match redis.get(state.redis_key(key)).await {
        Ok(v) => v,
        Err(err) => {
            tracing::warn!(?err, "redis get failed");
            return None;
        }
    };
    serde_json::from_slice(&raw?).ok()
}

pub async fn cache_put<T: Serialize>(state: &AppState, key: &str, value: &T) {
    let Ok(bytes) = serde_json::to_vec(value) else {
        return;
    };
    let mut redis = state.redis.clone();
    let r: redis::RedisResult<()> = redis
        .set_ex(state.redis_key(key), bytes, CACHE_TTL_SECS)
        .await;
    if let Err(err) = r {
        tracing::warn!(?err, "redis set failed");
    }
}

// ----- compact commit shape -----------------------------------------------

/// Commit as cached (git data only; logins are resolved per response).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedCommit {
    pub sha: String,
    pub message: String,
    pub author_name: String,
    pub author_email: String,
    pub author_date: i64,
    pub committer_name: String,
    pub committer_email: String,
    pub committer_date: i64,
    pub parents: Vec<String>,
}

impl From<&bgh_git::Commit> for CachedCommit {
    fn from(c: &bgh_git::Commit) -> Self {
        Self {
            sha: c.sha.clone(),
            message: c.message.clone(),
            author_name: c.author.name.clone(),
            author_email: c.author.email.clone(),
            author_date: c.author.when.timestamp(),
            committer_name: c.committer.name.clone(),
            committer_email: c.committer.email.clone(),
            committer_date: c.committer.when.timestamp(),
            parents: c.parents.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Person {
    pub name: String,
    pub email: String,
    pub date: Timestamp,
    /// Account matched by verified email, if any.
    pub login: Option<String>,
    pub avatar_url: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CommitSummary {
    pub sha: String,
    /// First line of the message.
    pub summary: String,
    pub message: String,
    pub author: Person,
    pub committer: Person,
    pub parents: Vec<String>,
}

fn ts(secs: i64) -> Timestamp {
    Timestamp::from(chrono::DateTime::from_timestamp(secs, 0).unwrap_or_default())
}

/// Accounts by lowercased verified email (one query).
pub async fn accounts_by_email(
    state: &AppState,
    emails: impl IntoIterator<Item = String>,
) -> ApiResult<HashMap<String, (String, String)>> {
    let mut list: Vec<String> = emails.into_iter().map(|e| e.to_lowercase()).collect();
    list.sort();
    list.dedup();
    if list.is_empty() {
        return Ok(HashMap::new());
    }
    let rows: Vec<(String, i64, String, Option<String>)> = sqlx::query_as(
        "SELECT lower(e.email), u.id, u.login, u.avatar_url
           FROM user_emails e JOIN users u ON u.id = e.user_id
          WHERE e.verified AND lower(e.email) = ANY($1)",
    )
    .bind(&list)
    .fetch_all(&state.db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(email, id, login, avatar)| {
            (email, (login, state.urls.avatar(id, avatar.as_deref())))
        })
        .collect())
}

/// Render cached commits with account links (one query for all).
pub async fn summarize(
    state: &AppState,
    commits: &[&CachedCommit],
) -> ApiResult<HashMap<String, CommitSummary>> {
    let accounts = accounts_by_email(
        state,
        commits
            .iter()
            .flat_map(|c| [c.author_email.clone(), c.committer_email.clone()]),
    )
    .await?;
    let person = |name: &str, email: &str, date: i64| {
        let acct = accounts.get(&email.to_lowercase());
        Person {
            name: name.to_string(),
            email: email.to_string(),
            date: ts(date),
            login: acct.map(|a| a.0.clone()),
            avatar_url: acct.map(|a| a.1.clone()),
        }
    };
    Ok(commits
        .iter()
        .map(|c| {
            (
                c.sha.clone(),
                CommitSummary {
                    sha: c.sha.clone(),
                    summary: c.message.lines().next().unwrap_or("").to_string(),
                    message: c.message.clone(),
                    author: person(&c.author_name, &c.author_email, c.author_date),
                    committer: person(&c.committer_name, &c.committer_email, c.committer_date),
                    parents: c.parents.clone(),
                },
            )
        })
        .collect())
}
