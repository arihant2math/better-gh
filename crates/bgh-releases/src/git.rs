//! Git helpers: tag lookup and creation for releases.

use bgh_core::events::{RefUpdate, ZERO_SHA};
use bgh_core::prelude::*;
use bgh_git::RepoStore;

pub fn store(state: &AppState) -> RepoStore {
    RepoStore::from_config(&state.config)
}

/// Commit a tag points to (peeled), if the tag exists.
pub async fn tag_commit(state: &AppState, repo_id: i64, tag: &str) -> ApiResult<Option<String>> {
    let refname = format!("refs/tags/{tag}");
    match store(state)
        .read(repo_id, move |r| r.find_ref(&refname))
        .await
    {
        Ok(r) => Ok(r.map(|r| r.peeled)),
        Err(bgh_git::GitError::NotFound(_)) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Resolve a commit-ish (branch, tag or SHA) to a commit SHA.
pub async fn resolve_commit(
    state: &AppState,
    repo_id: i64,
    rev: &str,
) -> ApiResult<Option<String>> {
    let rev = rev.to_string();
    match store(state)
        .read(repo_id, move |r| r.resolve_commit(&rev))
        .await
    {
        Ok(sha) => Ok(Some(sha)),
        Err(bgh_git::GitError::NotFound(_) | bgh_git::GitError::InvalidInput(_)) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Validate a release tag name (a valid ref component path).
pub fn validate_tag_name(tag: &str) -> ApiResult<()> {
    if bgh_git::is_valid_ref_name(tag) {
        Ok(())
    } else {
        Err(ApiError::invalid_field(FieldError::invalid(
            "Release", "tag_name",
        )))
    }
}

/// Create `refs/tags/{tag}` at `target` (default branch when empty) unless
/// the tag exists. Returns the ref update when a tag was created.
pub async fn ensure_tag(
    state: &AppState,
    repo: &db::Repository,
    tag: &str,
    target: &str,
) -> ApiResult<Option<RefUpdate>> {
    if tag_commit(state, repo.id, tag).await?.is_some() {
        return Ok(None);
    }
    let target = if target.is_empty() {
        repo.default_branch.as_str()
    } else {
        target
    };
    let Some(sha) = resolve_commit(state, repo.id, target).await? else {
        return Err(ApiError::invalid_field(FieldError::invalid(
            "Release",
            "target_commitish",
        )));
    };
    let refname = format!("refs/tags/{tag}");
    match bgh_git::write::update_ref(&store(state), repo.id, &refname, &sha, None).await {
        Ok(()) => Ok(Some(RefUpdate {
            old: ZERO_SHA.to_string(),
            new: sha,
            refname,
        })),
        // Created concurrently: fine.
        Err(_) if tag_commit(state, repo.id, tag).await?.is_some() => Ok(None),
        Err(e) => Err(e.into()),
    }
}
