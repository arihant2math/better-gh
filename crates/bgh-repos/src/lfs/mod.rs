//! Git LFS server: batch API (basic transfer), object upload/download/
//! verify, locks, size accounting and garbage collection.
//!
//! Routes (absolute; `{repo}` may carry `.git`, which is what git-lfs
//! derives from the remote URL):
//!
//! * `POST /{owner}/{repo}/info/lfs/objects/batch`
//! * `GET|PUT /{owner}/{repo}/info/lfs/objects/{oid}`
//! * `POST /{owner}/{repo}/info/lfs/objects/{oid}/verify`
//! * `GET|POST /{owner}/{repo}/info/lfs/locks`, `POST .../locks/verify`,
//!   `POST .../locks/{id}/unlock`
//!
//! Auth: like git over HTTP (Basic password or token, `token`/`Bearer`
//! headers), or `Authorization: RemoteAuth <token>` issued over SSH by
//! `git-lfs-authenticate` (see [`issue_grant`]). Objects are stored once
//! on disk (`bgh_git::lfs::LfsStore`) and linked to repositories in
//! `lfs_objects`; `repositories.lfs_size` sums linked object sizes.

pub mod batch;
pub mod gc;
pub mod locks;
pub mod objects;

use axum::Router;
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use bgh_core::auth::{self, AuthMethod, AuthOptions};
use bgh_core::prelude::*;
use bgh_git::lfs::LfsStore;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};

/// LFS JSON media type.
pub const MEDIA_TYPE: &str = "application/vnd.git-lfs+json";
/// Lifetime of SSH-issued LFS tokens and of batch action links.
pub const TOKEN_TTL_SECS: u64 = 3600;
const REALM: &str = "Basic realm=\"Better GitHub\"";

pub fn web_router() -> Router<AppState> {
    Router::new()
        .route("/{owner}/{repo}/info/lfs/objects/batch", post(batch::batch))
        .route(
            "/{owner}/{repo}/info/lfs/objects/{oid}",
            get(objects::download).put(objects::upload),
        )
        .route(
            "/{owner}/{repo}/info/lfs/objects/{oid}/verify",
            post(objects::verify),
        )
        .route(
            "/{owner}/{repo}/info/lfs/locks",
            get(locks::list).post(locks::create),
        )
        .route("/{owner}/{repo}/info/lfs/locks/verify", post(locks::verify))
        .route(
            "/{owner}/{repo}/info/lfs/locks/{id}/unlock",
            post(locks::unlock),
        )
}

/// The on-disk object store.
pub fn object_store(state: &AppState) -> LfsStore {
    LfsStore::from_data_dir(&state.config.data_dir)
}

/// Whether `oid` was uploaded to `repo_id`.
pub async fn has_object(state: &AppState, repo_id: i64, oid: &str) -> ApiResult<bool> {
    Ok(
        sqlx::query_scalar::<_, i32>("SELECT 1 FROM lfs_objects WHERE repo_id = $1 AND oid = $2")
            .bind(repo_id)
            .bind(oid)
            .fetch_optional(&state.db)
            .await?
            .is_some(),
    )
}

/// An [`ApiError`] rendered for git-lfs (LFS media type; 401s carry
/// `LFS-Authenticate` so the client asks for credentials).
#[derive(Debug)]
pub struct LfsError(pub ApiError);

impl From<ApiError> for LfsError {
    fn from(e: ApiError) -> Self {
        Self(e)
    }
}

impl From<sqlx::Error> for LfsError {
    fn from(e: sqlx::Error) -> Self {
        Self(e.into())
    }
}

impl IntoResponse for LfsError {
    fn into_response(self) -> Response {
        let unauthorized = matches!(self.0, ApiError::Unauthorized { .. });
        let mut resp = self.0.into_response();
        let h = resp.headers_mut();
        h.insert(header::CONTENT_TYPE, HeaderValue::from_static(MEDIA_TYPE));
        if unauthorized {
            h.insert("lfs-authenticate", HeaderValue::from_static(REALM));
            h.insert(header::WWW_AUTHENTICATE, HeaderValue::from_static(REALM));
        }
        resp
    }
}

pub type LfsResult<T> = Result<T, LfsError>;

/// JSON response with the LFS media type.
pub fn lfs_json<T: Serialize>(status: axum::http::StatusCode, body: &T) -> Response {
    let bytes = serde_json::to_vec(body).unwrap_or_default();
    (
        status,
        [(header::CONTENT_TYPE, HeaderValue::from_static(MEDIA_TYPE))],
        bytes,
    )
        .into_response()
}

/// 507 (LFS JSON error) when storing `incoming` more bytes in `repo` would
/// exceed its storage quota (git objects plus LFS, see
/// `bgh_core::settings::quota_headroom`).
pub async fn check_quota(state: &AppState, repo: &db::Repository, incoming: i64) -> LfsResult<()> {
    let Some(h) = bgh_core::settings::quota_headroom(state, repo).await? else {
        return Ok(());
    };
    if incoming > 0 && (incoming + 1023) / 1024 > h.remaining_kb {
        return Err(
            ApiError::Status(axum::http::StatusCode::INSUFFICIENT_STORAGE, h.message()).into(),
        );
    }
    Ok(())
}

/// Authorization granted by `git-lfs-authenticate` over SSH.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LfsGrant {
    pub repo_id: i64,
    pub user_id: Option<i64>,
    pub deploy_key_id: Option<i64>,
    pub write: bool,
}

fn grant_key(state: &AppState, token: &str) -> String {
    state.redis_key(&format!(
        "lfs:grant:{}",
        bgh_core::crypto::sha256_hex(token)
    ))
}

/// Store a grant and return its bearer token (`RemoteAuth <token>`).
pub async fn issue_grant(state: &AppState, grant: &LfsGrant) -> ApiResult<String> {
    let token = bgh_core::crypto::random_token(40);
    let mut redis = state.redis.clone();
    let _: () = redis
        .set_ex(
            grant_key(state, &token),
            serde_json::to_string(grant).map_err(ApiError::internal)?,
            TOKEN_TTL_SECS,
        )
        .await
        .map_err(ApiError::internal)?;
    Ok(token)
}

async fn load_grant(state: &AppState, token: &str) -> ApiResult<Option<LfsGrant>> {
    let mut redis = state.redis.clone();
    let raw: Option<String> = redis
        .get(grant_key(state, token))
        .await
        .map_err(ApiError::internal)?;
    Ok(raw.and_then(|r| serde_json::from_str(&r).ok()))
}

/// The authorized caller of an LFS request.
pub struct LfsAccess {
    pub access: RepoAccess,
    /// The acting user (absent for anonymous and deploy-key access).
    pub user: Option<AuthContext>,
    /// `Authorization` header to repeat in batch action links.
    pub authorization: Option<String>,
}

impl LfsAccess {
    pub fn can_write(&self) -> bool {
        self.access.permission >= Permission::Write && !self.access.repo.archived
    }
}

fn challenge(message: &str) -> ApiError {
    ApiError::Unauthorized {
        message: message.to_string(),
        www_authenticate: Some(REALM.to_string()),
    }
}

/// Authenticate and authorize an LFS request (`write` for uploads/locks).
pub async fn authorize(
    state: &AppState,
    headers: &HeaderMap,
    owner: &str,
    repo: &str,
    write: bool,
) -> ApiResult<LfsAccess> {
    let authorization = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let lfs = if let Some(token) = authorization
        .as_deref()
        .and_then(|a| a.strip_prefix("RemoteAuth "))
    {
        let grant = load_grant(state, token.trim())
            .await?
            .ok_or_else(|| challenge("Invalid or expired LFS token."))?;
        let name = repo.strip_suffix(".git").unwrap_or(repo);
        let owner_row = db::User::find_by_login(&state.db, owner)
            .await?
            .ok_or(ApiError::NotFound)?;
        let repo_row = db::Repository::find_by_name(&state.db, owner_row.id, name)
            .await?
            .ok_or(ApiError::NotFound)?;
        if repo_row.id != grant.repo_id {
            return Err(ApiError::NotFound);
        }
        match grant.user_id {
            Some(uid) => {
                let user = db::User::find(&state.db, uid)
                    .await?
                    .ok_or_else(|| challenge("Invalid or expired LFS token."))?;
                let ctx = AuthContext {
                    user,
                    method: AuthMethod::Password,
                    scopes: None,
                };
                let access = RepoAccess::for_repo(state, Some(&ctx), repo_row, owner_row).await?;
                LfsAccess {
                    access,
                    user: Some(ctx),
                    authorization,
                }
            }
            None => LfsAccess {
                access: RepoAccess {
                    repo: repo_row,
                    owner: owner_row,
                    permission: if grant.write {
                        Permission::Write
                    } else {
                        Permission::Read
                    },
                    authenticated: true,
                },
                user: None,
                authorization,
            },
        }
    } else {
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
            Err(ApiError::Unauthorized { .. }) => {
                return Err(challenge("Invalid username or token."));
            }
            Err(e) => return Err(e),
        };
        let access = match RepoAccess::load(state, auth.as_ref(), owner, repo).await {
            Ok(a) => a,
            Err(ApiError::NotFound) if auth.is_none() => {
                return Err(challenge("Authentication required."));
            }
            Err(e) => return Err(e),
        };
        if write && auth.is_none() {
            return Err(challenge("Authentication required."));
        }
        bgh_core::apps::check_git(auth.as_ref(), &access.repo, write)?;
        bgh_core::pat::check_git(auth.as_ref(), &access.repo, write)?;
        LfsAccess {
            access,
            user: auth,
            authorization,
        }
    };
    if lfs.access.repo.disabled {
        return Err(ApiError::forbidden("Repository access blocked."));
    }
    if write {
        lfs.access.require(Permission::Write)?;
        lfs.access.require_not_archived()?;
        lfs.access.require_not_mirror()?;
    }
    Ok(lfs)
}
