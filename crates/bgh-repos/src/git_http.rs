//! Git smart-HTTP routes with authentication and authorization.
//!
//! Auth: HTTP Basic with `login:password` or `anything:<token>`, or
//! `Authorization: token|Bearer`. Anonymous callers that need credentials
//! (private repo, or any push) get `401` + `WWW-Authenticate: Basic` so git
//! prompts; authenticated callers without access get 404 (read) / 403 (write).

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use bgh_core::auth::{self, AuthOptions};
use bgh_core::perms::RepoAccess;
use bgh_core::prelude::*;
use bgh_git::smart_http::{self, Service};
use serde::Deserialize;

use crate::jobs::PostReceive;
use crate::protection;

const REALM: &str = "Basic realm=\"Better GitHub\"";

fn challenge(message: &str) -> ApiError {
    ApiError::Unauthorized {
        message: message.to_string(),
        www_authenticate: Some(REALM.to_string()),
    }
}

/// Authenticate and authorize a git request for `service`.
async fn git_access(
    state: &AppState,
    headers: &HeaderMap,
    owner: &str,
    repo: &str,
    service: Service,
) -> ApiResult<(RepoAccess, Option<AuthContext>)> {
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
    if service.is_write() {
        if auth.is_none() {
            return Err(challenge("Authentication required."));
        }
        access.require(Permission::Write).map_err(|e| match e {
            ApiError::Forbidden(_) => ApiError::forbidden(format!(
                "Permission to {} denied to {}.",
                access.full_name(),
                auth.as_ref().map(|a| a.login()).unwrap_or("anonymous")
            )),
            other => other,
        })?;
        access.require_not_archived()?;
    }
    Ok((access, auth))
}

#[derive(Debug, Deserialize)]
pub struct InfoRefsQuery {
    pub service: Option<String>,
}

/// `GET /{owner}/{repo}/info/refs?service=git-upload-pack|git-receive-pack`
pub async fn info_refs(
    State(state): State<AppState>,
    Path((owner, repo)): Path<(String, String)>,
    Query(q): Query<InfoRefsQuery>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let service = q
        .service
        .as_deref()
        .and_then(Service::parse)
        .ok_or_else(|| {
            ApiError::forbidden("Dumb HTTP protocol is not supported; use a smart git client.")
        })?;
    let (access, _) = git_access(&state, &headers, &owner, &repo, service).await?;
    Ok(smart_http::info_refs(&crate::store(&state), access.repo.id, service, &headers).await?)
}

/// `POST /{owner}/{repo}/git-upload-pack` (fetch / clone)
pub async fn upload_pack(
    State(state): State<AppState>,
    Path((owner, repo)): Path<(String, String)>,
    req: Request,
) -> ApiResult<Response> {
    let (parts, body) = req.into_parts();
    let (access, _) =
        git_access(&state, &parts.headers, &owner, &repo, Service::UploadPack).await?;
    Ok(
        smart_http::upload_pack(&crate::store(&state), access.repo.id, &parts.headers, body)
            .await?,
    )
}

/// `POST /{owner}/{repo}/git-receive-pack` (push)
pub async fn receive_pack(
    State(state): State<AppState>,
    Path((owner, repo)): Path<(String, String)>,
    req: Request,
) -> ApiResult<Response> {
    let (parts, body): (_, Body) = req.into_parts();
    let (access, auth) =
        git_access(&state, &parts.headers, &owner, &repo, Service::ReceivePack).await?;
    let pusher = auth.ok_or_else(|| challenge("Authentication required."))?;
    bgh_core::settings::check_push_quota(&state, &access.repo).await?;
    let rules = protection::load_rules(&state, access.repo.id).await?;

    let outcome = smart_http::receive_pack(
        &crate::store(&state),
        access.repo.id,
        &parts.headers,
        body,
        |updates| {
            let result = protection::check_push(&rules, &access, &pusher, &updates);
            async move { result }
        },
    )
    .await?;

    if !outcome.applied.is_empty() {
        // Enqueue before answering so the client observes a consistent state
        // once `git push` returns.
        bgh_core::jobs::enqueue_job(
            &state.db,
            &PostReceive {
                repo_id: access.repo.id,
                pusher_id: Some(pusher.user.id),
                updates: outcome.applied.clone(),
            },
        )
        .await?;
    }
    Ok(outcome.response.into_response())
}
