//! `GET /_bgh/repos/{owner}/{repo}/history[/{ref}[/{path}]]?page=&per_page=`:
//! commits reachable from the ref that touch the path, newest first.

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Response;
use bgh_core::prelude::*;
use serde::{Deserialize, Serialize};

use super::{
    CACHE_VERSION, CachedCommit, CommitSummary, Target, cache_get, cache_put, json_response,
    precheck, resolve, summarize,
};

#[derive(Debug, Deserialize)]
pub struct HistoryQuery {
    pub page: Option<usize>,
    pub per_page: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct History {
    #[serde(rename = "ref")]
    pub refname: String,
    pub commit: String,
    pub path: String,
    pub page: usize,
    pub per_page: usize,
    pub has_more: bool,
    pub commits: Vec<CommitSummary>,
}

pub async fn root(
    state: State<AppState>,
    auth: MaybeUser,
    Path((owner, repo)): Path<(String, String)>,
    q: Query<HistoryQuery>,
    req: HeaderMap,
) -> ApiResult<Response> {
    let t = resolve(&state, auth.as_ref(), &owner, &repo, None).await?;
    respond(&state, t, q.0, req).await
}

pub async fn get(
    state: State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, spec)): Path<(String, String, String)>,
    q: Query<HistoryQuery>,
    req: HeaderMap,
) -> ApiResult<Response> {
    let t = resolve(&state, auth.as_ref(), &owner, &repo, Some(&spec)).await?;
    respond(&state, t, q.0, req).await
}

async fn respond(
    state: &AppState,
    t: Target,
    q: HistoryQuery,
    req: HeaderMap,
) -> ApiResult<Response> {
    let page = q.page.unwrap_or(1).max(1);
    let per_page = q.per_page.unwrap_or(30).clamp(1, 100);
    let key = format!("history:{}:{}:{page}:{per_page}", t.commit, t.path);
    if let Some(r) = precheck(&req, &t, &key) {
        return Ok(r);
    }
    let cache_key = format!(
        "hist:{CACHE_VERSION}:{}:{}:{}:{page}:{per_page}",
        t.access.repo.id, t.commit, t.path
    );
    let commits: Vec<CachedCommit> = match cache_get(state, &cache_key).await {
        Some(c) => c,
        None => {
            let (commit, path) = (t.commit.clone(), t.path.clone());
            let list = t
                .store(state)
                .read(t.access.repo.id, move |r| {
                    let p = (!path.is_empty()).then_some(path.as_str());
                    Ok(r.log(&commit, p, (page - 1) * per_page, per_page + 1)?
                        .iter()
                        .map(CachedCommit::from)
                        .collect::<Vec<_>>())
                })
                .await?;
            cache_put(state, &cache_key, &list).await;
            list
        }
    };
    let has_more = commits.len() > per_page;
    let shown: Vec<&CachedCommit> = commits.iter().take(per_page).collect();
    let rendered = summarize(state, &shown).await?;
    let body = History {
        refname: t.refname.clone(),
        commit: t.commit.clone(),
        path: t.path.clone(),
        page,
        per_page,
        has_more,
        commits: shown
            .iter()
            .filter_map(|c| rendered.get(&c.sha).cloned())
            .collect(),
    };
    json_response(&req, &t, &key, &body)
}
