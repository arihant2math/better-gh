//! Git smart-HTTP routes with authentication and authorization.
//!
//! Auth: HTTP Basic with `login:password` or `anything:<token>`, or
//! `Authorization: token|Bearer`. Anonymous callers that need credentials
//! (private repo, or any push) get `401` + `WWW-Authenticate: Basic` so git
//! prompts; authenticated callers without access get 404 (read) / 403 (write).
//!
//! Wiki repositories (`{repo}.wiki.git` / `{repo}.wiki`) can't be routed
//! separately by axum, so each handler delegates them to `bgh_wiki::git`.

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use bgh_core::auth::{self, AuthOptions};
use bgh_core::perms::RepoAccess;
use bgh_core::prelude::*;
use bgh_core::token_permissions::{Access, Category, TokenPermissions};
use bgh_git::smart_http::{self, PushLimits, PushPolicy, Service};
use serde::Deserialize;

use crate::jobs::PostReceive;
use crate::protection::{self, Actor, RepoRules};

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
    // Actions job tokens: git needs `contents` (write to push; read to fetch
    // the token's private repository).
    if let Some(perms) = auth.as_ref().and_then(TokenPermissions::of) {
        let has = perms.get(Category::Contents);
        let denied = if service.is_write() {
            has < Access::Write
        } else {
            has < Access::Read && access.repo.is_private()
        };
        if denied {
            return Err(ApiError::forbidden(format!(
                "Permission to {} denied to {}.",
                access.full_name(),
                auth.as_ref().map(|a| a.login()).unwrap_or("anonymous")
            )));
        }
    }
    // GitHub App installation tokens need `contents` (write to push).
    bgh_core::apps::check_git(auth.as_ref(), &access.repo, service.is_write())?;
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
        access.require_not_mirror()?;
        crate::import::require_not_importing(state, access.repo.id).await?;
    }
    Ok((access, auth))
}

/// git shows the `text/plain` error body of a failed ref advertisement to
/// the user (`remote: …`) but not JSON ones, so `info/refs` refusals
/// (archived, mirror, importing, permission) are sent as plain text.
fn git_response(result: ApiResult<Response>) -> Response {
    match result {
        Ok(r) => r,
        Err(ApiError::Forbidden(message)) => (
            axum::http::StatusCode::FORBIDDEN,
            [(
                axum::http::header::CONTENT_TYPE,
                "text/plain; charset=utf-8",
            )],
            format!("{message}\n"),
        )
            .into_response(),
        Err(e) => e.into_response(),
    }
}

/// Site-wide push limits for `repo` (settings `git.*` and the storage
/// quota), enforced by the pre-receive hook over HTTP and SSH alike.
/// Callers first refuse repositories already over quota with
/// `settings::check_push_quota`.
pub(crate) async fn push_limits(state: &AppState, repo: &db::Repository) -> ApiResult<PushLimits> {
    let s = bgh_core::settings::load(state).await?;
    let mb = |v: Option<i64>| v.filter(|n| *n > 0).map(|n| n as u64 * 1024 * 1024);
    let quota = bgh_core::settings::quota_headroom(state, repo).await?;
    Ok(PushLimits {
        fsck: Some(s.git.fsck_on_push),
        max_blob_bytes: mb(s.git.max_object_size_mb),
        warn_blob_bytes: mb(s.git.warn_object_size_mb),
        max_input_bytes: mb(s.git.max_push_size_mb),
        quota_message: quota.as_ref().map(|q| {
            let what = if q.per_repo {
                "the repository over its size limit"
            } else {
                "the repository owner over its storage quota"
            };
            format!("This push would put {what} ({} MB).", q.limit_kb / 1024)
        }),
        quota_remaining_kb: quota.map(|q| q.remaining_kb),
    })
}

/// Refuse a push that only touches server-only refs (`refs/pull/*`,
/// `refs/bgh/*`) before git sees the pack. Mixed pushes (`git push
/// --mirror` of a GitHub clone) reach git, whose `receive.hideRefs`
/// rejects just those refs with the same message.
pub(crate) fn deny_hidden_refs(updates: &[bgh_git::RefUpdate]) -> Result<(), String> {
    if !updates.is_empty()
        && updates
            .iter()
            .all(|u| bgh_git::storage::is_hidden_ref(&u.refname))
    {
        return Err(smart_http::HIDDEN_REF_REASON.to_string());
    }
    Ok(())
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
) -> Response {
    git_response(info_refs_inner(state, owner, repo, q, headers).await)
}

async fn info_refs_inner(
    state: AppState,
    owner: String,
    repo: String,
    q: InfoRefsQuery,
    headers: HeaderMap,
) -> ApiResult<Response> {
    if bgh_wiki::git::wiki_repo_name(&repo).is_some() {
        return bgh_wiki::git::info_refs(&state, &owner, &repo, q.service.as_deref(), &headers)
            .await;
    }
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
    if bgh_wiki::git::wiki_repo_name(&repo).is_some() {
        return bgh_wiki::git::upload_pack(&state, &owner, &repo, req).await;
    }
    let (mut parts, body) = req.into_parts();
    let (access, auth) =
        git_access(&state, &parts.headers, &owner, &repo, Service::UploadPack).await?;
    // Traffic: count clones (P31).
    let ip = auth::client_ip(&state.config, &parts.headers, &parts.extensions);
    let visitor = crate::traffic::visitor(&state, auth.as_ref().map(|a| a.user.id), &ip);
    let body = crate::traffic::observe_http_clone(
        &state,
        access.repo.id,
        visitor,
        &mut parts.headers,
        body,
    );
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
    if bgh_wiki::git::wiki_repo_name(&repo).is_some() {
        return bgh_wiki::git::receive_pack(&state, &owner, &repo, req).await;
    }
    let (parts, body): (_, Body) = req.into_parts();
    let (access, auth) =
        git_access(&state, &parts.headers, &owner, &repo, Service::ReceivePack).await?;
    let pusher = auth.ok_or_else(|| challenge("Authentication required."))?;
    // Already over quota: 403 before reading the pack.
    bgh_core::settings::check_push_quota(&state, &access.repo).await?;
    let limits = push_limits(&state, &access.repo).await?;
    let rules = RepoRules::load(&state.db, &access.repo).await?;
    let actor = if rules.is_empty() {
        None
    } else {
        Some(Actor::load(&state, &access, &pusher.user).await?)
    };
    let workflow_denied = crate::workflow_scope::denial(&state, &pusher).await?;
    let secrets = bgh_security::push::prepare(
        &state,
        &access.repo,
        &access.owner.login,
        Some(pusher.user.id),
    )
    .await?;

    let outcome = smart_http::receive_pack_with_policy(
        &crate::store(&state),
        access.repo.id,
        &parts.headers,
        body,
        |updates| {
            let (state, rules) = (&state, &rules);
            async move {
                deny_hidden_refs(&updates)?;
                let mut policy = match &actor {
                    None => PushPolicy::default(),
                    Some(actor) => {
                        protection::authorize_push(state, rules, actor, &updates).await?
                    }
                };
                policy.workflow_denied = workflow_denied;
                policy.object_check = bgh_security::push::combine(
                    policy.object_check.take(),
                    secrets.as_ref().map(|s| s.object_check(&updates)),
                );
                Ok(policy.with_limits(limits))
            }
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
