//! Smart HTTP for `/{owner}/{repo}.wiki.git/...`.
//!
//! axum's router can't match a `{repo}.wiki.git` suffix, so these requests
//! reach bgh-repos' git routes with `repo = "x.wiki.git"` (or `"x.wiki"`);
//! bgh-repos delegates them here (see [`wiki_repo_name`]).
//!
//! Auth mirrors repository git access (Basic password/token, a `401`
//! challenge for anonymous callers that need credentials, `404` without
//! read access). `has_wiki = false` and fetching a wiki that doesn't exist
//! yet → `404`; pushes need edit rights (see [`crate::access`]) and create
//! the wiki on first push.

use axum::body::Body;
use axum::extract::Request;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use bgh_core::auth::{self, AuthOptions};
use bgh_core::prelude::*;
use bgh_git::smart_http::{self, Service};
use bgh_git::write;

use crate::access::{WikiAccess, store};
use crate::pages::DEFAULT_BRANCH;

const REALM: &str = "Basic realm=\"Better GitHub\"";

/// The repository name of a wiki git path: `x.wiki.git` / `x.wiki` → `x`.
pub fn wiki_repo_name(name: &str) -> Option<&str> {
    let base = name.strip_suffix(".git").unwrap_or(name);
    base.strip_suffix(".wiki").filter(|b| !b.is_empty())
}

fn challenge(message: &str) -> ApiError {
    ApiError::Unauthorized {
        message: message.to_string(),
        www_authenticate: Some(REALM.to_string()),
    }
}

async fn git_access(
    state: &AppState,
    headers: &HeaderMap,
    owner: &str,
    name: &str,
    service: Service,
) -> ApiResult<(WikiAccess, Option<AuthContext>)> {
    let repo = wiki_repo_name(name).ok_or(ApiError::NotFound)?;
    let auth = match auth::authenticate(
        state,
        headers,
        AuthOptions {
            allow_password: true,
        },
    )
    .await
    {
        Ok(a) => a,
        Err(ApiError::Unauthorized { .. }) => return Err(challenge("Invalid username or token.")),
        Err(e) => return Err(e),
    };
    let access = match RepoAccess::load(state, auth.as_ref(), owner, repo).await {
        Ok(a) => a,
        Err(ApiError::NotFound) if auth.is_none() => {
            return Err(challenge("Authentication required."));
        }
        Err(e) => return Err(e),
    };
    if access.repo.disabled {
        return Err(ApiError::forbidden("Repository access blocked."));
    }
    if !access.repo.has_wiki {
        return Err(ApiError::NotFound);
    }
    let wiki = WikiAccess::for_access(state, auth.as_ref(), access).await?;
    if service.is_write() {
        if auth.is_none() {
            return Err(challenge("Authentication required."));
        }
        if !wiki.may_edit {
            return Err(ApiError::forbidden(format!(
                "Permission to {}.wiki denied to {}.",
                wiki.access.full_name(),
                auth.as_ref().map(|a| a.login()).unwrap_or("anonymous")
            )));
        }
        wiki.access.require_not_archived()?;
        ensure_repo(state, wiki.repo_id()).await?;
    } else if !store(state).exists(wiki.repo_id()) {
        return Err(ApiError::NotFound);
    }
    Ok((wiki, auth))
}

static INIT_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Create the wiki repository (branch `master`) if it doesn't exist yet.
pub async fn ensure_repo(state: &AppState, repo_id: i64) -> ApiResult<()> {
    let store = store(state);
    if store.exists(repo_id) {
        return Ok(());
    }
    let _guard = INIT_LOCK.lock().await;
    if !store.exists(repo_id) {
        store.init(repo_id, DEFAULT_BRANCH).await?;
    }
    Ok(())
}

/// `GET /{owner}/{repo}.wiki.git/info/refs?service=...`
pub async fn info_refs(
    state: &AppState,
    owner: &str,
    name: &str,
    service: Option<&str>,
    headers: &HeaderMap,
) -> ApiResult<Response> {
    let service = service.and_then(Service::parse).ok_or_else(|| {
        ApiError::forbidden("Dumb HTTP protocol is not supported; use a smart git client.")
    })?;
    let (wiki, _) = git_access(state, headers, owner, name, service).await?;
    Ok(smart_http::info_refs(&store(state), wiki.repo_id(), service, headers).await?)
}

/// `POST /{owner}/{repo}.wiki.git/git-upload-pack`
pub async fn upload_pack(
    state: &AppState,
    owner: &str,
    name: &str,
    req: Request,
) -> ApiResult<Response> {
    let (parts, body) = req.into_parts();
    let (wiki, _) = git_access(state, &parts.headers, owner, name, Service::UploadPack).await?;
    Ok(smart_http::upload_pack(&store(state), wiki.repo_id(), &parts.headers, body).await?)
}

/// `POST /{owner}/{repo}.wiki.git/git-receive-pack`
pub async fn receive_pack(
    state: &AppState,
    owner: &str,
    name: &str,
    req: Request,
) -> ApiResult<Response> {
    let (parts, body): (_, Body) = req.into_parts();
    let (wiki, _) = git_access(state, &parts.headers, owner, name, Service::ReceivePack).await?;
    let store = store(state);
    let outcome = smart_http::receive_pack(
        &store,
        wiki.repo_id(),
        &parts.headers,
        body,
        |_updates| async { Ok(()) },
    )
    .await?;
    if !outcome.applied.is_empty() {
        fix_head(&store, wiki.repo_id(), &outcome.applied).await?;
    }
    Ok(outcome.response.into_response())
}

/// If HEAD's branch doesn't exist after a push (e.g. only `main` was
/// pushed), point HEAD at a pushed branch so the pages API sees it.
async fn fix_head(
    store: &bgh_git::RepoStore,
    repo_id: i64,
    applied: &[bgh_git::RefUpdate],
) -> ApiResult<()> {
    let (head, branches) = store
        .read(repo_id, |r| {
            Ok((
                r.head_branch()?,
                r.branches()?
                    .iter()
                    .map(|b| b.short_name().to_string())
                    .collect::<Vec<_>>(),
            ))
        })
        .await?;
    if head.as_ref().is_some_and(|h| branches.contains(h)) {
        return Ok(());
    }
    let pushed = applied
        .iter()
        .filter(|u| !u.is_delete())
        .filter_map(|u| u.branch())
        .find(|b| branches.iter().any(|x| x == b))
        .map(str::to_string)
        .or_else(|| branches.first().cloned());
    if let Some(branch) = pushed {
        write::set_head(store, repo_id, &branch).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(wiki_repo_name("x.wiki.git"), Some("x"));
        assert_eq!(wiki_repo_name("x.wiki"), Some("x"));
        assert_eq!(wiki_repo_name("x.git"), None);
        assert_eq!(wiki_repo_name(".wiki.git"), None);
    }
}
