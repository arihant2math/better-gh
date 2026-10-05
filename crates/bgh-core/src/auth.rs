//! Authentication: sessions, personal access tokens, Basic auth, extractors.
//!
//! Credentials accepted (first match wins):
//! 1. `Authorization: token <pat>` / `Authorization: Bearer <pat>`
//! 2. `Authorization: Basic base64(user:<pat>)`; `user:<password>` only when
//!    [`AuthOptions::allow_password`] is set (git transport)
//! 3. `bgh_session` cookie (web client)
//!
//! Handlers use the extractors [`MaybeUser`], [`RequireUser`] and
//! [`RequireSiteAdmin`]. Resolution happens once per request and is cached in
//! the request extensions.

use std::sync::{Arc, OnceLock};

use axum::extract::{FromRequestParts, OptionalFromRequestParts, Request};
use axum::http::request::Parts;
use axum::http::{Extensions, HeaderMap, HeaderValue, header};
use axum::middleware::Next;
use axum::response::Response;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use chrono::{DateTime, Duration, Utc};
use redis::AsyncCommands;

use crate::config::Config;
use crate::crypto;
use crate::error::{ApiError, ApiResult};
use crate::models::db;
use crate::state::AppState;

/// Name of the session cookie.
pub const SESSION_COOKIE: &str = "bgh_session";
/// Redis TTL for cached session → user id lookups.
const SESSION_CACHE_TTL_SECS: u64 = 300;

/// How the caller authenticated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthMethod {
    Session { session_id: i64 },
    Token { token_id: i64 },
    Password,
}

/// The authenticated caller.
#[derive(Debug, Clone)]
pub struct AuthContext {
    pub user: db::User,
    pub method: AuthMethod,
    /// OAuth scopes of the token; `None` = unrestricted (session/password).
    pub scopes: Option<Vec<String>>,
}

/// Scopes implied by a granted scope (GitHub's scope hierarchy).
fn implied(granted: &str, wanted: &str) -> bool {
    if granted == wanted {
        return true;
    }
    match granted {
        "repo" => matches!(
            wanted,
            "public_repo" | "repo:status" | "repo_deployment" | "repo:invite" | "security_events"
        ),
        "admin:org" => matches!(wanted, "write:org" | "read:org" | "manage_runners:org"),
        "write:org" => wanted == "read:org",
        "admin:public_key" => matches!(wanted, "write:public_key" | "read:public_key"),
        "write:public_key" => wanted == "read:public_key",
        "admin:repo_hook" => matches!(wanted, "write:repo_hook" | "read:repo_hook"),
        "write:repo_hook" => wanted == "read:repo_hook",
        "admin:gpg_key" => matches!(wanted, "write:gpg_key" | "read:gpg_key"),
        "write:gpg_key" => wanted == "read:gpg_key",
        "user" => matches!(wanted, "read:user" | "user:email" | "user:follow"),
        "write:packages" => wanted == "read:packages",
        "project" => wanted == "read:project",
        "workflow" => false,
        "site_admin" => false,
        _ => false,
    }
}

impl AuthContext {
    pub fn id(&self) -> i64 {
        self.user.id
    }

    pub fn login(&self) -> &str {
        &self.user.login
    }

    pub fn is_session(&self) -> bool {
        matches!(self.method, AuthMethod::Session { .. })
    }

    /// Whether the credential grants `scope` (always true for sessions and
    /// passwords).
    pub fn has_scope(&self, scope: &str) -> bool {
        match &self.scopes {
            None => true,
            Some(granted) => granted.iter().any(|g| implied(g, scope)),
        }
    }

    /// 403 unless the credential grants `scope`.
    pub fn require_scope(&self, scope: &str) -> ApiResult<()> {
        if self.has_scope(scope) {
            Ok(())
        } else {
            Err(ApiError::forbidden(format!(
                "Resource not accessible by personal access token (requires the `{scope}` scope)"
            )))
        }
    }

    /// Value for the `X-OAuth-Scopes` response header.
    pub fn scopes_header(&self) -> Option<String> {
        self.scopes.as_ref().map(|s| s.join(", "))
    }
}

/// Options for [`authenticate`].
#[derive(Debug, Clone, Copy, Default)]
pub struct AuthOptions {
    /// Accept `Basic login:password` (git over HTTP). The REST API does not.
    pub allow_password: bool,
}

/// Resolve the caller from request headers. `Ok(None)` = anonymous.
/// Invalid credentials yield 401 "Bad credentials"; suspended users 403.
pub async fn authenticate(
    state: &AppState,
    headers: &HeaderMap,
    opts: AuthOptions,
) -> ApiResult<Option<AuthContext>> {
    let ctx = if let Some(value) = headers.get(header::AUTHORIZATION) {
        let value = value.to_str().map_err(|_| ApiError::bad_credentials())?;
        let (scheme, rest) = value.split_once(' ').unwrap_or((value, ""));
        let rest = rest.trim();
        match scheme.to_ascii_lowercase().as_str() {
            "token" | "bearer" => Some(token_auth(state, rest).await?),
            "basic" => Some(basic_auth(state, rest, opts).await?),
            _ => return Err(ApiError::bad_credentials()),
        }
    } else if let Some(token) = cookie(headers, SESSION_COOKIE) {
        session_auth(state, &token).await?
    } else {
        None
    };
    if let Some(ctx) = &ctx
        && ctx.user.is_suspended()
    {
        return Err(ApiError::forbidden("Sorry. Your account was suspended."));
    }
    Ok(ctx)
}

#[derive(sqlx::FromRow)]
struct TokenRow {
    token_id: i64,
    scopes: Vec<String>,
    expires_at: Option<DateTime<Utc>>,
    last_used_at: Option<DateTime<Utc>>,
    #[sqlx(flatten)]
    user: db::User,
}

async fn token_auth(state: &AppState, token: &str) -> ApiResult<AuthContext> {
    if token.is_empty() {
        return Err(ApiError::bad_credentials());
    }
    let hash = crypto::sha256_hex(token);
    let row: Option<TokenRow> = sqlx::query_as(&format!(
        "SELECT t.id AS token_id, t.scopes, t.expires_at, t.last_used_at, {}
           FROM access_tokens t JOIN users u ON u.id = t.user_id
          WHERE t.token_hash = $1",
        db::prefixed("u", db::User::COLUMNS)
    ))
    .bind(&hash)
    .fetch_optional(&state.db)
    .await?;
    let row = row.ok_or_else(ApiError::bad_credentials)?;
    let now = Utc::now();
    if row.expires_at.is_some_and(|e| e <= now) {
        return Err(ApiError::bad_credentials());
    }
    // Touch last_used_at at most once a minute, off the request path.
    if row
        .last_used_at
        .is_none_or(|t| now - t > Duration::minutes(1))
    {
        let db = state.db.clone();
        let id = row.token_id;
        tokio::spawn(async move {
            let _ = sqlx::query("UPDATE access_tokens SET last_used_at = now() WHERE id = $1")
                .bind(id)
                .execute(&db)
                .await;
        });
    }
    Ok(AuthContext {
        user: row.user,
        method: AuthMethod::Token {
            token_id: row.token_id,
        },
        scopes: Some(row.scopes),
    })
}

async fn basic_auth(state: &AppState, encoded: &str, opts: AuthOptions) -> ApiResult<AuthContext> {
    let decoded = STANDARD
        .decode(encoded)
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .ok_or_else(ApiError::bad_credentials)?;
    let (login, secret) = decoded
        .split_once(':')
        .ok_or_else(ApiError::bad_credentials)?;
    if secret.starts_with(crypto::PAT_PREFIX) || secret.starts_with(crypto::OAUTH_TOKEN_PREFIX) {
        // Like GitHub, the username is ignored for token auth.
        return token_auth(state, secret).await;
    }
    if !opts.allow_password {
        return Err(ApiError::bad_credentials());
    }
    let user = verify_login(state, login, secret)
        .await?
        .ok_or_else(ApiError::bad_credentials)?;
    // Accounts with two-factor authentication must use a token.
    let two_factor: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM user_two_factor WHERE user_id = $1 AND enabled_at IS NOT NULL)",
    )
    .bind(user.id)
    .fetch_one(&state.db)
    .await?;
    if two_factor {
        return Err(ApiError::bad_credentials());
    }
    Ok(AuthContext {
        user,
        method: AuthMethod::Password,
        scopes: None,
    })
}

/// Check a login (or primary email) + password. Returns the user on success.
pub async fn verify_login(
    state: &AppState,
    login_or_email: &str,
    password: &str,
) -> ApiResult<Option<db::User>> {
    let user: Option<db::User> = sqlx::query_as(&format!(
        "SELECT {} FROM users u
          WHERE u.type = 'User' AND (lower(u.login) = lower($1)
             OR u.id = (SELECT user_id FROM user_emails WHERE lower(email) = lower($1) AND verified))",
        db::prefixed("u", db::User::COLUMNS)
    ))
    .bind(login_or_email)
    .fetch_optional(&state.db)
    .await?;
    // Unknown users still pay for a hash check to equalize timing.
    let hash = user
        .as_ref()
        .and_then(|u| u.password_hash.clone())
        .unwrap_or_else(|| DUMMY_HASH.to_string());
    let ok = tokio::task::spawn_blocking({
        let password = password.to_string();
        move || crypto::verify_password(&password, &hash)
    })
    .await?;
    Ok(user.filter(|u| ok && u.password_hash.is_some()))
}

const DUMMY_HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$c29tZXNhbHRzb21lc2FsdA$4Ilce1Hv5Ym3l4Sq7oY+7aMnLnVq7kK7mU7S1cZ3o1Q";

async fn session_auth(state: &AppState, token: &str) -> ApiResult<Option<AuthContext>> {
    let hash = crypto::sha256_hex(token);
    let key = state.redis_key(&format!("session:{hash}"));
    let mut redis = state.redis.clone();
    let cached: Option<String> = redis.get(&key).await.unwrap_or(None);
    let (session_id, user_id) = match cached.as_deref().and_then(|v| v.split_once(':')) {
        Some((sid, uid)) => match (sid.parse::<i64>(), uid.parse::<i64>()) {
            (Ok(s), Ok(u)) => (s, u),
            _ => return Ok(None),
        },
        None => {
            let row: Option<(i64, i64)> = sqlx::query_as(
                "UPDATE sessions SET last_seen_at = now()
                  WHERE token_hash = $1 AND expires_at > now()
                  RETURNING id, user_id",
            )
            .bind(&hash)
            .fetch_optional(&state.db)
            .await?;
            let Some((sid, uid)) = row else {
                return Ok(None);
            };
            let _: Result<(), _> = redis
                .set_ex(&key, format!("{sid}:{uid}"), SESSION_CACHE_TTL_SECS)
                .await;
            (sid, uid)
        }
    };
    let Some(user) = db::User::find(&state.db, user_id).await? else {
        return Ok(None);
    };
    Ok(Some(AuthContext {
        user,
        method: AuthMethod::Session { session_id },
        scopes: None,
    }))
}

/// Read a cookie value from request headers.
pub fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.trim_matches('"').to_string())
        .filter(|v| !v.is_empty())
}

// ---------------------------------------------------------------------------
// Session & token management (used by bgh-accounts and the test harness)
// ---------------------------------------------------------------------------

/// Create a browser session; returns the secret cookie value.
pub async fn create_session(
    state: &AppState,
    user_id: i64,
    user_agent: Option<&str>,
    ip: Option<&str>,
) -> ApiResult<String> {
    let token = crypto::random_token(48);
    let expires = Utc::now() + Duration::days(state.config.session_ttl_days);
    sqlx::query(
        "INSERT INTO sessions (token_hash, user_id, user_agent, ip, expires_at)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(crypto::sha256_hex(&token))
    .bind(user_id)
    .bind(user_agent)
    .bind(ip)
    .bind(expires)
    .execute(&state.db)
    .await?;
    Ok(token)
}

/// Delete a session by its secret cookie value (and evict the cache).
pub async fn destroy_session(state: &AppState, token: &str) -> ApiResult<()> {
    let hash = crypto::sha256_hex(token);
    let deleted: Option<(i64, i64)> =
        sqlx::query_as("DELETE FROM sessions WHERE token_hash = $1 RETURNING id, user_id")
            .bind(&hash)
            .fetch_optional(&state.db)
            .await?;
    let mut redis = state.redis.clone();
    let _: Result<(), _> = redis.del(state.redis_key(&format!("session:{hash}"))).await;
    if let Some((session_id, user_id)) = deleted {
        crate::sync::signal_signed_out(state, user_id, Some(session_id)).await;
    }
    Ok(())
}

/// Delete all sessions of a user (e.g. on password change or suspension).
pub async fn destroy_user_sessions(state: &AppState, user_id: i64) -> ApiResult<()> {
    let hashes: Vec<String> =
        sqlx::query_scalar("DELETE FROM sessions WHERE user_id = $1 RETURNING token_hash")
            .bind(user_id)
            .fetch_all(&state.db)
            .await?;
    if !hashes.is_empty() {
        let keys: Vec<String> = hashes
            .iter()
            .map(|h| state.redis_key(&format!("session:{h}")))
            .collect();
        let mut redis = state.redis.clone();
        let _: Result<(), _> = redis.del(keys).await;
        crate::sync::signal_signed_out(state, user_id, None).await;
    }
    Ok(())
}

/// `Set-Cookie` value establishing a session.
pub fn session_cookie(config: &Config, token: &str) -> HeaderValue {
    let max_age = config.session_ttl_days * 86_400;
    let secure = if config.secure_cookies() {
        "; Secure"
    } else {
        ""
    };
    HeaderValue::from_str(&format!(
        "{SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}{secure}"
    ))
    .expect("cookie is ASCII")
}

/// `Set-Cookie` value clearing the session cookie.
pub fn clear_session_cookie(config: &Config) -> HeaderValue {
    let secure = if config.secure_cookies() {
        "; Secure"
    } else {
        ""
    };
    HeaderValue::from_str(&format!(
        "{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0{secure}"
    ))
    .expect("cookie is ASCII")
}

/// Create a personal access token. Returns the row and the plaintext token
/// (shown to the user once).
pub async fn create_access_token(
    db: impl sqlx::PgExecutor<'_>,
    user_id: i64,
    name: &str,
    scopes: &[String],
    expires_at: Option<DateTime<Utc>>,
) -> ApiResult<(db::AccessToken, String)> {
    let token = crypto::new_pat();
    let row: db::AccessToken = sqlx::query_as(&format!(
        "INSERT INTO access_tokens (user_id, name, token_hash, token_last_eight, scopes, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING {}",
        db::AccessToken::COLUMNS
    ))
    .bind(user_id)
    .bind(name)
    .bind(crypto::sha256_hex(&token))
    .bind(&token[token.len() - 8..])
    .bind(scopes)
    .bind(expires_at)
    .fetch_one(db)
    .await?;
    Ok((row, token))
}

// ---------------------------------------------------------------------------
// Extractors
// ---------------------------------------------------------------------------

/// Per-request cache of the resolved caller.
#[derive(Clone)]
struct Resolved(Option<AuthContext>);

/// Shared slot filled when auth is resolved, so outer middleware (see
/// [`auth_headers_middleware`]) can add `X-OAuth-Scopes` to the response.
#[derive(Clone, Default)]
pub struct AuthSlot(Arc<OnceLock<AuthContext>>);

async fn resolve_cached(
    state: &AppState,
    headers: &HeaderMap,
    extensions: &mut Extensions,
) -> ApiResult<Option<AuthContext>> {
    if let Some(Resolved(ctx)) = extensions.get::<Resolved>() {
        return Ok(ctx.clone());
    }
    let ctx = authenticate(state, headers, AuthOptions::default()).await?;
    if let (Some(ctx), Some(slot)) = (&ctx, extensions.get::<AuthSlot>()) {
        let _ = slot.0.set(ctx.clone());
    }
    extensions.insert(Resolved(ctx.clone()));
    Ok(ctx)
}

async fn resolve(parts: &mut Parts, state: &AppState) -> ApiResult<Option<AuthContext>> {
    resolve_cached(state, &parts.headers, &mut parts.extensions).await
}

/// Resolve (and cache for the extractors) the caller of `req`, for
/// middleware that needs the user before the handler runs.
pub async fn resolve_request(
    state: &AppState,
    req: &mut Request,
) -> ApiResult<Option<AuthContext>> {
    let (mut parts, body) = std::mem::take(req).into_parts();
    let ctx = resolve_cached(state, &parts.headers, &mut parts.extensions).await;
    *req = Request::from_parts(parts, body);
    ctx
}

/// Client IP of a request: `X-Forwarded-For` / `X-Real-IP` when
/// `BGH_TRUST_PROXY` is set, else the TCP peer address (when the server was
/// started with connect info), else `"unknown"`.
pub fn client_ip(config: &Config, headers: &HeaderMap, extensions: &Extensions) -> String {
    if config.trust_proxy {
        let forwarded = headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(',').next())
            .or_else(|| headers.get("x-real-ip").and_then(|v| v.to_str().ok()))
            .map(str::trim)
            .filter(|v| !v.is_empty());
        if let Some(ip) = forwarded {
            return ip.to_string();
        }
    }
    extensions
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|ci| ci.0.ip().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

/// First `X-Forwarded-For` hop, else `X-Real-IP`, regardless of
/// `BGH_TRUST_PROXY` (spoofable: informational use only, e.g. audit
/// entries written where only headers are at hand). Prefer [`client_ip`].
pub fn forwarded_ip(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .or_else(|| headers.get("x-real-ip").and_then(|v| v.to_str().ok()))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Optional authentication: `MaybeUser(None)` for anonymous callers.
/// Bad credentials still fail with 401.
#[derive(Debug, Clone)]
pub struct MaybeUser(pub Option<AuthContext>);

impl MaybeUser {
    pub fn as_ref(&self) -> Option<&AuthContext> {
        self.0.as_ref()
    }

    pub fn user_id(&self) -> Option<i64> {
        self.0.as_ref().map(|c| c.user.id)
    }
}

impl FromRequestParts<AppState> for MaybeUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> ApiResult<Self> {
        Ok(Self(resolve(parts, state).await?))
    }
}

/// Required authentication; 401 "Requires authentication" otherwise.
#[derive(Debug, Clone)]
pub struct RequireUser(pub AuthContext);

impl std::ops::Deref for RequireUser {
    type Target = AuthContext;
    fn deref(&self) -> &AuthContext {
        &self.0
    }
}

impl FromRequestParts<AppState> for RequireUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> ApiResult<Self> {
        resolve(parts, state)
            .await?
            .map(Self)
            .ok_or_else(ApiError::requires_auth)
    }
}

impl OptionalFromRequestParts<AppState> for RequireUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> ApiResult<Option<Self>> {
        Ok(resolve(parts, state).await?.map(Self))
    }
}

/// Requires a site administrator (and the `site_admin` scope for tokens).
/// Non-admins get 404 like GHES admin endpoints for regular users.
#[derive(Debug, Clone)]
pub struct RequireSiteAdmin(pub AuthContext);

impl std::ops::Deref for RequireSiteAdmin {
    type Target = AuthContext;
    fn deref(&self) -> &AuthContext {
        &self.0
    }
}

impl FromRequestParts<AppState> for RequireSiteAdmin {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> ApiResult<Self> {
        let ctx = resolve(parts, state)
            .await?
            .ok_or_else(ApiError::requires_auth)?;
        if !ctx.user.site_admin {
            return Err(ApiError::forbidden("Must be a site administrator."));
        }
        ctx.require_scope("site_admin")?;
        Ok(Self(ctx))
    }
}

/// Middleware: installs an [`AuthSlot`] and, after the handler ran, emits
/// `X-OAuth-Scopes` for token-authenticated requests (like GitHub).
pub async fn auth_headers_middleware(mut req: Request, next: Next) -> Response {
    let slot = AuthSlot::default();
    req.extensions_mut().insert(slot.clone());
    let mut resp = next.run(req).await;
    if let Some(ctx) = slot.0.get()
        && let Some(scopes) = ctx.scopes_header()
        && let Ok(v) = HeaderValue::from_str(&scopes)
    {
        resp.headers_mut().insert("x-oauth-scopes", v);
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_hierarchy() {
        assert!(implied("repo", "public_repo"));
        assert!(implied("admin:org", "read:org"));
        assert!(!implied("public_repo", "repo"));
        assert!(!implied("read:org", "admin:org"));
    }

    #[test]
    fn parses_cookies() {
        let mut h = HeaderMap::new();
        h.insert(header::COOKIE, "a=1; bgh_session=abc; b=2".parse().unwrap());
        assert_eq!(cookie(&h, "bgh_session").as_deref(), Some("abc"));
        assert_eq!(cookie(&h, "nope"), None);
    }
}
