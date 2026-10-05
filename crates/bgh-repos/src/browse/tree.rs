//! Directory listings and last-commit-per-entry.

use std::collections::{BTreeMap, HashMap};

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Response;
use bgh_core::prelude::*;
use bgh_git::{PathLookup, TreeEntryKind};
use serde::Serialize;

use super::readme::{self, Readme};
use super::{
    CACHE_VERSION, CachedCommit, CommitSummary, Target, cache_get, cache_put, json_response,
    precheck, resolve, summarize,
};

#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub name: String,
    pub path: String,
    /// `tree` | `blob` | `symlink` | `commit` (submodule)
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub mode: String,
    pub sha: String,
    /// Blob size in bytes (files and symlinks only).
    pub size: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct TreeView {
    #[serde(rename = "ref")]
    pub refname: String,
    pub commit: String,
    pub path: String,
    /// Tree SHA of the directory.
    pub sha: String,
    pub entries: Vec<Entry>,
    /// Last commit per entry name when already computed (otherwise fetch
    /// `tree-commits/{commit}/{path}`).
    pub last_commits: Option<BTreeMap<String, CommitSummary>>,
    pub readme: Option<Readme>,
}

fn kind_name(k: TreeEntryKind) -> &'static str {
    match k {
        TreeEntryKind::Tree => "tree",
        TreeEntryKind::Commit => "commit",
        TreeEntryKind::Symlink => "symlink",
        TreeEntryKind::Blob | TreeEntryKind::Executable => "blob",
    }
}

fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

/// Redis key of the last-commit map of `path` at `commit`.
fn last_commits_key(repo_id: i64, commit: &str, path: &str) -> String {
    format!("lc:{CACHE_VERSION}:{repo_id}:{commit}:{path}")
}

pub async fn root(
    state: State<AppState>,
    auth: MaybeUser,
    Path((owner, repo)): Path<(String, String)>,
    req: HeaderMap,
) -> ApiResult<Response> {
    view(state, auth, owner, repo, None, req).await
}

pub async fn get(
    state: State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, spec)): Path<(String, String, String)>,
    req: HeaderMap,
) -> ApiResult<Response> {
    view(state, auth, owner, repo, Some(spec), req).await
}

async fn view(
    State(state): State<AppState>,
    auth: MaybeUser,
    owner: String,
    repo: String,
    spec: Option<String>,
    req: HeaderMap,
) -> ApiResult<Response> {
    let t = resolve(&state, auth.as_ref(), &owner, &repo, spec.as_deref()).await?;
    let key = format!("tree:{}:{}", t.commit, t.path);
    if let Some(r) = precheck(&req, &t, &key) {
        return Ok(r);
    }
    let (commit, path) = (t.commit.clone(), t.path.clone());
    let (tree_sha, entries) = t
        .store(&state)
        .read(t.access.repo.id, move |r| {
            let PathLookup::Tree { sha, entries } = r.lookup_path(&commit, &path)? else {
                return Err(bgh_git::GitError::NotFound(format!("tree {path:?}")));
            };
            let mut out = Vec::with_capacity(entries.len());
            for e in entries {
                let size = match e.kind {
                    TreeEntryKind::Tree | TreeEntryKind::Commit => None,
                    _ => r.header(&e.sha)?.map(|(_, s)| s),
                };
                out.push(Entry {
                    path: join(&path, &e.name),
                    name: e.name,
                    kind: kind_name(e.kind),
                    mode: e.mode,
                    sha: e.sha,
                    size,
                });
            }
            Ok((sha, out))
        })
        .await?;
    let mut entries = entries;
    entries.sort_by(|a, b| {
        (a.kind != "tree")
            .cmp(&(b.kind != "tree"))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });

    let cached: Option<HashMap<String, CachedCommit>> = cache_get(
        &state,
        &last_commits_key(t.access.repo.id, &t.commit, &t.path),
    )
    .await;
    let last_commits = match cached {
        Some(map) => {
            let list: Vec<&CachedCommit> = map.values().collect();
            let rendered = summarize(&state, &list).await?;
            Some(
                map.iter()
                    .filter_map(|(name, c)| Some((name.clone(), rendered.get(&c.sha)?.clone())))
                    .collect(),
            )
        }
        None => None,
    };
    let readme = match readme::pick(&entries) {
        Some(e) => Some(readme::render(&state, &t, e).await?),
        None => None,
    };
    let body = TreeView {
        refname: t.refname.clone(),
        commit: t.commit.clone(),
        path: t.path.clone(),
        sha: tree_sha,
        entries,
        last_commits,
        readme,
    };
    json_response(&req, &t, &key, &body)
}

#[derive(Debug, Serialize)]
pub struct LastCommits {
    pub commit: String,
    pub path: String,
    /// Entry name → last commit. Entries whose commit could not be found
    /// within the walk limit are absent.
    pub entries: BTreeMap<String, CommitSummary>,
}

/// Compute (or fetch from cache) the last-commit map of a directory.
pub async fn last_commit_map(
    state: &AppState,
    t: &Target,
) -> ApiResult<HashMap<String, CachedCommit>> {
    let key = last_commits_key(t.access.repo.id, &t.commit, &t.path);
    if let Some(map) = cache_get(state, &key).await {
        return Ok(map);
    }
    let (commit, path) = (t.commit.clone(), t.path.clone());
    let map: HashMap<String, CachedCommit> = t
        .store(state)
        .read(t.access.repo.id, move |r| {
            let found = r.last_commits(&commit, &path, bgh_git::lastcommit::DEFAULT_MAX_COMMITS)?;
            Ok(found
                .iter()
                .map(|(name, c)| (name.clone(), CachedCommit::from(c)))
                .collect())
        })
        .await?;
    cache_put(state, &key, &map).await;
    Ok(map)
}

/// `GET /_bgh/repos/{owner}/{repo}/tree-commits/{ref}[/{path}]`
pub async fn last_commits(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, spec)): Path<(String, String, String)>,
    req: HeaderMap,
) -> ApiResult<Response> {
    let t = resolve(&state, auth.as_ref(), &owner, &repo, Some(&spec)).await?;
    let key = format!("tree-commits:{}:{}", t.commit, t.path);
    if let Some(r) = precheck(&req, &t, &key) {
        return Ok(r);
    }
    let map = last_commit_map(&state, &t).await?;
    let list: Vec<&CachedCommit> = map.values().collect();
    let rendered = summarize(&state, &list).await?;
    let body = LastCommits {
        commit: t.commit.clone(),
        path: t.path.clone(),
        entries: map
            .iter()
            .filter_map(|(name, c)| Some((name.clone(), rendered.get(&c.sha)?.clone())))
            .collect(),
    };
    json_response(&req, &t, &key, &body)
}
