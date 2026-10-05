//! Git plumbing for pull requests: mirroring the head into the base
//! repository (`refs/pull/{n}/head`), diff stats, cached file diffs and
//! commit identities.

use std::sync::Arc;

use bgh_core::prelude::*;
use bgh_git::merge::Person;
use bgh_git::patch::{self, DiffLimits, DiffResult};
use bgh_git::write::Identity;
use bgh_git::{GitError, RepoStore};
use redis::AsyncCommands;

use crate::model::Pull;

pub fn store(state: &AppState) -> RepoStore {
    RepoStore::from_config(&state.config)
}

/// Commit identity for a user: display name and primary email (or the
/// `{id}+{login}@users.noreply.{host}` address).
pub async fn identity(state: &AppState, user: &db::User) -> ApiResult<Identity> {
    let email: Option<String> = sqlx::query_scalar(
        "SELECT email FROM user_emails WHERE user_id = $1 AND is_primary AND verified",
    )
    .bind(user.id)
    .fetch_optional(&state.db)
    .await?;
    Ok(Identity::new(
        user.name.clone().unwrap_or_else(|| user.login.clone()),
        email.unwrap_or_else(|| noreply_email(state, user)),
    ))
}

pub fn noreply_email(state: &AppState, user: &db::User) -> String {
    format!(
        "{}+{}@users.noreply.{}",
        user.id,
        user.login,
        state.config.hostname()
    )
}

/// Committer used for server-side merges (like GitHub's
/// `GitHub <noreply@github.com>`).
pub fn site_committer(state: &AppState) -> Identity {
    Identity::new(
        state.config.site_name.clone(),
        format!("noreply@{}", state.config.hostname()),
    )
}

pub fn person(id: &Identity) -> Person {
    Person::from(id)
}

/// Resolve the current tip of `branch` in `repo_id`.
pub async fn branch_tip(
    store: &RepoStore,
    repo_id: i64,
    branch: &str,
) -> Result<Option<String>, GitError> {
    if !bgh_git::is_valid_ref_name(branch) {
        return Ok(None);
    }
    let refname = format!("refs/heads/{branch}");
    match store
        .read(repo_id, move |r| {
            Ok(r.find_ref(&refname)?.map(|r| r.peeled))
        })
        .await
    {
        Ok(v) => Ok(v),
        Err(GitError::NotFound(_)) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Mirror the head branch into the base repository as `refs/pull/{n}/head`
/// and return its SHA. Same-repo PRs just point the ref at `head_sha`.
pub async fn mirror_head(
    store: &RepoStore,
    base_repo_id: i64,
    head_repo_id: i64,
    head_ref: &str,
    number: i64,
    head_sha: &str,
) -> Result<String, GitError> {
    let pull_ref = format!("refs/pull/{number}/head");
    if base_repo_id == head_repo_id {
        bgh_git::merge::force_ref(store, base_repo_id, &pull_ref, head_sha).await?;
        Ok(head_sha.to_string())
    } else {
        let src = format!("refs/heads/{head_ref}");
        bgh_git::merge::fetch_ref(store, base_repo_id, head_repo_id, &src, &pull_ref).await
    }
}

/// Merge base, commit count and diff stats for `base_sha...head_sha`.
#[derive(Debug, Clone)]
pub struct RangeStats {
    pub merge_base: Option<String>,
    pub commits: i64,
    pub additions: i64,
    pub deletions: i64,
    pub changed_files: i64,
}

pub async fn range_stats(
    state: &AppState,
    repo_id: i64,
    base_sha: &str,
    head_sha: &str,
) -> ApiResult<RangeStats> {
    let store = store(state);
    let merge_base = bgh_git::merge::merge_base(&store, repo_id, base_sha, head_sha).await?;
    let commits = bgh_git::merge::count_commits(&store, repo_id, base_sha, head_sha).await?;
    let from = merge_base.as_deref().unwrap_or(base_sha);
    let diff = diff(state, repo_id, from, head_sha).await?;
    Ok(RangeStats {
        merge_base,
        commits,
        additions: diff.additions as i64,
        deletions: diff.deletions as i64,
        changed_files: diff.total_files as i64,
    })
}

/// Diffs are immutable per (base, head): cache the parsed result in Redis.
const DIFF_CACHE_TTL: u64 = 7 * 24 * 3600;
const DIFF_CACHE_MAX: usize = 16 * 1024 * 1024;

fn diff_key(state: &AppState, repo_id: i64, base: &str, head: &str) -> String {
    state.redis_key(&format!("pulls:diff:v1:{repo_id}:{base}:{head}"))
}

/// File diff between two commits (merge base → head for PRs), cached.
pub async fn diff(
    state: &AppState,
    repo_id: i64,
    base: &str,
    head: &str,
) -> ApiResult<Arc<DiffResult>> {
    let key = diff_key(state, repo_id, base, head);
    let mut redis = state.redis.clone();
    match redis.get::<_, Option<Vec<u8>>>(&key).await {
        Ok(Some(bytes)) => {
            if let Ok(d) = serde_json::from_slice::<DiffResult>(&bytes) {
                return Ok(Arc::new(d));
            }
        }
        Ok(None) => {}
        Err(err) => tracing::warn!(?err, "diff cache read"),
    }
    let d = patch::diff_files(&store(state), repo_id, base, head, DiffLimits::default()).await?;
    if let Ok(bytes) = serde_json::to_vec(&d)
        && bytes.len() <= DIFF_CACHE_MAX
        && let Err(err) = redis.set_ex::<_, _, ()>(&key, bytes, DIFF_CACHE_TTL).await
    {
        tracing::warn!(?err, "diff cache write");
    }
    Ok(Arc::new(d))
}

/// The PR's file diff (merge base → head).
pub async fn pull_diff(state: &AppState, pull: &Pull) -> ApiResult<Arc<DiffResult>> {
    let base = pull
        .pr
        .merge_base_sha
        .clone()
        .unwrap_or_else(|| pull.pr.base_sha.clone());
    diff(state, pull.pr.repo_id, &base, &pull.pr.head_sha).await
}

/// Blob SHA of `path` at `commit` (None if missing).
pub async fn blob_at(
    store: &RepoStore,
    repo_id: i64,
    commit: &str,
    path: &str,
) -> Result<Option<String>, GitError> {
    let (commit, path) = (commit.to_string(), path.to_string());
    store
        .read(repo_id, move |r| match r.lookup_path(&commit, &path) {
            Ok(bgh_git::PathLookup::Entry(e)) => Ok(Some(e.sha)),
            Ok(_) => Ok(None),
            Err(GitError::NotFound(_)) => Ok(None),
            Err(e) => Err(e),
        })
        .await
}

/// Read a text file at `rev` (None if missing or binary/too large).
pub async fn read_text(
    store: &RepoStore,
    repo_id: i64,
    rev: &str,
    path: &str,
) -> Result<Option<String>, GitError> {
    let (rev, path) = (rev.to_string(), path.to_string());
    store
        .read(repo_id, move |r| match r.lookup_path(&rev, &path) {
            Ok(bgh_git::PathLookup::Entry(e)) => match r.blob_with_limit(&e.sha, 1024 * 1024) {
                Ok(b) if !b.is_binary() => Ok(Some(String::from_utf8_lossy(&b.data).into_owned())),
                Ok(_) | Err(GitError::TooLarge { .. }) => Ok(None),
                Err(e) => Err(e),
            },
            Ok(_) | Err(GitError::NotFound(_)) => Ok(None),
            Err(e) => Err(e),
        })
        .await
}

/// Resolve a commit-ish (`sha`, branch, tag) in `repo_id` to a commit SHA.
pub async fn resolve_commit(
    store: &RepoStore,
    repo_id: i64,
    rev: &str,
) -> ApiResult<Option<String>> {
    let rev = rev.to_string();
    match store
        .read(repo_id, move |r| match r.resolve_commit(&rev) {
            Ok(sha) => Ok(Some(sha)),
            Err(GitError::NotFound(_)) | Err(GitError::InvalidInput(_)) => Ok(None),
            Err(e) => Err(e),
        })
        .await
    {
        Ok(v) => Ok(v),
        Err(GitError::NotFound(_)) => Ok(None),
        Err(e) => Err(e.into()),
    }
}
