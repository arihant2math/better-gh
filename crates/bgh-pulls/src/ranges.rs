//! Commit-range diffs of a pull request (P38): the Files tab's commit
//! picker shows all changes, one commit, a range of commits or the changes
//! since the viewer's last review.
//!
//! `GET /_bgh/repos/{o}/{r}/pulls/{n}/files?base_sha=&head_sha=` answers in
//! the REST `/pulls/{n}/files` shape (paginated, `Link` header). Without
//! parameters it is the PR diff (merge base → head). With both SHAs given
//! the response is immutable and cached like every SHA-keyed diff.

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use bgh_core::prelude::*;
use serde::Deserialize;

use crate::git;
use crate::pulls::{diff_entry, load_pull};

#[derive(Debug, Default, Deserialize)]
pub struct RangeQuery {
    pub base_sha: Option<String>,
    pub head_sha: Option<String>,
}

/// Resolve a commit SHA given by the client (422 when malformed or missing
/// from the repository).
async fn commit_param(state: &AppState, repo_id: i64, field: &str, sha: &str) -> ApiResult<String> {
    if !bgh_git::is_sha(sha) {
        return Err(ApiError::invalid_field(FieldError::invalid(
            "PullRequest",
            field,
        )));
    }
    git::resolve_commit(&git::store(state), repo_id, sha)
        .await?
        .ok_or_else(|| ApiError::unprocessable(format!("No commit found for SHA: {sha}")))
}

/// The `(base, head)` pair a range query selects for `pull`.
pub async fn resolve_range(
    state: &AppState,
    pull: &crate::model::Pull,
    q: &RangeQuery,
) -> ApiResult<(String, String)> {
    let repo_id = pull.pr.repo_id;
    let head = match q.head_sha.as_deref().filter(|s| !s.is_empty()) {
        Some(s) => commit_param(state, repo_id, "head_sha", s).await?,
        None => pull.pr.head_sha.clone(),
    };
    let base = match q.base_sha.as_deref().filter(|s| !s.is_empty()) {
        Some(s) => commit_param(state, repo_id, "base_sha", s).await?,
        None if head == pull.pr.head_sha => pull
            .pr
            .merge_base_sha
            .clone()
            .unwrap_or_else(|| pull.pr.base_sha.clone()),
        // A head inside the PR: compare against the PR's merge base.
        None => bgh_git::merge::merge_base(&git::store(state), repo_id, &pull.pr.base_sha, &head)
            .await?
            .unwrap_or_else(|| pull.pr.base_sha.clone()),
    };
    Ok((base, head))
}

/// `GET /_bgh/repos/{o}/{r}/pulls/{n}/files?base_sha=&head_sha=`.
pub async fn range_files(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    Query(q): Query<RangeQuery>,
) -> ApiResult<Response> {
    let (access, pull) = load_pull(&state, auth.as_ref(), &owner, &repo, number).await?;
    let (base, head) = resolve_range(&state, &pull, &q).await?;
    let diff = git::diff(&state, access.repo.id, &base, &head).await?;
    let total = diff.files.len() as i64;
    let items: Vec<_> = diff
        .files
        .iter()
        .skip(p.offset() as usize)
        .take(p.limit() as usize)
        .map(|f| diff_entry(&state, &access.owner.login, &access.repo.name, &head, f))
        .collect();
    let mut resp = p.page_with_total(items, total).into_response();
    let pinned = q.base_sha.as_deref().is_some_and(bgh_git::is_sha)
        && q.head_sha.as_deref().is_some_and(bgh_git::is_sha);
    if pinned {
        // `private`: visibility can change, browsers may keep it.
        resp.headers_mut().insert(
            axum::http::header::CACHE_CONTROL,
            axum::http::HeaderValue::from_static("private, max-age=31536000, immutable"),
        );
    }
    Ok(resp)
}
