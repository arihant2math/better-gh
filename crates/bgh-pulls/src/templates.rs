//! Pull request templates, exposed to the web client as
//! `GET /_bgh/repos/{owner}/{repo}/pull-templates[?ref=]`.
//!
//! Like GitHub: a single default template (`pull_request_template.md` in
//! `.github/`, the root or `docs/`, any case) plus any number of named ones
//! in a `PULL_REQUEST_TEMPLATE/` directory there (picked with `?template=`).
//! When the repository has none, the owner's public `.github` repository is
//! the fallback. Both are read concurrently; results are cached in Redis by
//! commit SHA (immutable).

use axum::extract::State;
use bgh_core::perms::RepoAccess;
use bgh_core::prelude::*;
use bgh_git::{TreeEntry, TreeEntryKind};
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};

/// Directories searched, in GitHub's precedence order.
const DIRS: [&str; 3] = [".github", "", "docs"];
const FILE: &str = "pull_request_template.md";
const DIR: &str = "pull_request_template";
const MAX_FILES: usize = 50;
const MAX_FILE_SIZE: u64 = 256 * 1024;
const CACHE_TTL_SECS: u64 = 24 * 3600;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Template {
    /// Path in the repository (`.github/PULL_REQUEST_TEMPLATE/feature.md`).
    pub filename: String,
    /// Basename, used by `?template=`.
    pub name: String,
    pub body: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Templates {
    pub commit_sha: Option<String>,
    /// `repo`, `org` (the owner's `.github` repository) or `null` when none.
    pub source: Option<String>,
    /// The single-file template, prefilled when `?template=` is absent.
    pub default: Option<Template>,
    /// Named templates from `PULL_REQUEST_TEMPLATE/`, sorted by name.
    pub templates: Vec<Template>,
}

impl Templates {
    fn is_empty(&self) -> bool {
        self.default.is_none() && self.templates.is_empty()
    }
}

fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

fn find<'a>(entries: &'a [TreeEntry], name: &str, kind: TreeEntryKind) -> Option<&'a TreeEntry> {
    entries
        .iter()
        .find(|e| e.kind == kind && e.name.eq_ignore_ascii_case(name))
}

/// Read the templates of `repo` at `rev` (default branch when `None`).
pub async fn load(
    state: &AppState,
    repo: &db::Repository,
    rev: Option<String>,
) -> ApiResult<Templates> {
    let store = bgh_git::RepoStore::from_config(&state.config);
    let rev = rev.unwrap_or_else(|| repo.default_branch.clone());
    let sha = match store.read(repo.id, move |r| r.resolve(&rev)).await {
        Ok(Some(sha)) => sha,
        Ok(None) | Err(bgh_git::GitError::NotFound(_)) => return Ok(Templates::default()),
        Err(e) => return Err(e.into()),
    };
    let key = state.redis_key(&format!("pulls:templates:{}:{sha}", repo.id));
    let mut redis = state.redis.clone();
    if let Ok(Some(cached)) = redis.get::<_, Option<String>>(&key).await
        && let Ok(t) = serde_json::from_str::<Templates>(&cached)
    {
        return Ok(t);
    }
    let commit = sha.clone();
    let mut out = store
        .read(repo.id, move |r| {
            let mut out = Templates::default();
            let read = |path: String, e: &TreeEntry| -> Option<Template> {
                let b = r.blob_with_limit(&e.sha, MAX_FILE_SIZE).ok()?;
                Some(Template {
                    name: e.name.clone(),
                    filename: path,
                    body: String::from_utf8_lossy(&b.data).into_owned(),
                })
            };
            for dir in DIRS {
                let Ok(bgh_git::PathLookup::Tree { entries, .. }) = r.lookup_path(&commit, dir)
                else {
                    continue;
                };
                if out.default.is_none()
                    && let Some(e) = find(&entries, FILE, TreeEntryKind::Blob)
                {
                    out.default = read(join(dir, &e.name), e);
                }
                if out.templates.is_empty()
                    && let Some(d) = find(&entries, DIR, TreeEntryKind::Tree)
                    && let Ok(files) = r.tree(&d.sha)
                {
                    let base = join(dir, &d.name);
                    out.templates = files
                        .iter()
                        .filter(|e| {
                            e.kind == TreeEntryKind::Blob && e.name.to_lowercase().ends_with(".md")
                        })
                        .take(MAX_FILES)
                        .filter_map(|e| read(format!("{base}/{}", e.name), e))
                        .collect();
                }
            }
            Ok(out)
        })
        .await?;
    out.templates.sort_by(|a, b| a.name.cmp(&b.name));
    out.commit_sha = Some(sha);
    if !out.is_empty() {
        out.source = Some("repo".into());
    }
    if let Ok(s) = serde_json::to_string(&out) {
        let _: Result<(), _> = redis.set_ex(&key, s, CACHE_TTL_SECS).await;
    }
    Ok(out)
}

/// The owner's public `.github` repository's templates (empty when absent).
async fn load_org(state: &AppState, auth: Option<&AuthContext>, owner: &str) -> Templates {
    match RepoAccess::load(state, auth, owner, ".github").await {
        Ok(a) if a.repo.visibility == "public" => {
            let mut t = load(state, &a.repo, None).await.unwrap_or_default();
            if !t.is_empty() {
                t.source = Some("org".into());
            }
            t
        }
        _ => Templates::default(),
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct TemplatesQuery {
    #[serde(rename = "ref")]
    pub git_ref: Option<String>,
}

/// `GET /_bgh/repos/{owner}/{repo}/pull-templates[?ref=]`
pub async fn get(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo)): Path<(String, String)>,
    Query(q): Query<TemplatesQuery>,
) -> ApiResult<Json<Templates>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let (own, org) = tokio::join!(
        load(&state, &access.repo, q.git_ref),
        load_org(&state, auth.as_ref(), &access.owner.login),
    );
    let own = own?;
    if own.is_empty() && access.repo.name != ".github" && !org.is_empty() {
        return Ok(Json(org));
    }
    Ok(Json(own))
}
