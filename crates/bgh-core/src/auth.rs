//! Authentication: sessions, personal access tokens, Basic auth, extractors.
//!
//! Credentials accepted (first match wins):
//! 1. `Authorization: token <pat>` / `Authorization: Bearer <pat>`
//! 2. `Authorization: Basic base64(user:<pat>)`; `user:<password>` only when
//!    [`AuthOptions::allow_password`] is set (git transport), checked by
//!    [`check_password`] (directory/LDAP, `password_login` policy, failure
//!    throttling and auditing shared with the web sign-in)
//! 3. `bgh_session` cookie (web client)
//!
//! GitHub App credentials: `Bearer <jwt>` authenticates an app
//! ([`AuthMethod::App`]), `bghs_…` installation tokens are access tokens of
//! the app's bot user (see [`crate::apps`]).
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

use futures::future::BoxFuture;
use serde_json::json;

use crate::config::Config;
use crate::crypto;
use crate::error::{ApiError, ApiResult};
use crate::models::db;
use crate::state::AppState;
use crate::{audit, ratelimit, settings};

/// Name of the session cookie.
pub const SESSION_COOKIE: &str = "bgh_session";
/// Redis TTL for cached session → user id lookups.
const SESSION_CACHE_TTL_SECS: u64 = 300;

/// How the caller authenticated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthMethod {
    Session {
        session_id: i64,
    },
    Token {
        token_id: i64,
    },
    Password,
    /// A GitHub App JWT; the context's user is the app's bot.
    App {
        app_id: i64,
    },
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
        "admin:ssh_signing_key" => {
            matches!(wanted, "write:ssh_signing_key" | "read:ssh_signing_key")
        }
        "write:ssh_signing_key" => wanted == "read:ssh_signing_key",
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
        // Fine-grained tokens: mapped to their permissions.
        if let Some(granted) = crate::pat::has_scope(self, scope) {
            return granted;
        }
        match &self.scopes {
            None => true,
            Some(granted) => granted.iter().any(|g| implied(g, scope)),
        }
    }

    /// 403 unless the credential grants `scope`. Records the scope for the
    /// `X-Accepted-OAuth-Scopes` response header.
    pub fn require_scope(&self, scope: &str) -> ApiResult<()> {
        record_accepted_scope(scope);
        if self.has_scope(scope) {
            Ok(())
        } else {
            Err(ApiError::forbidden(format!(
                "Resource not accessible by personal access token (requires the `{scope}` scope)"
            )))
        }
    }

    /// Value for the `X-OAuth-Scopes` response header (none for GitHub App
    /// credentials, whose scopes are internal).
    pub fn scopes_header(&self) -> Option<String> {
        if crate::apps::is_integration(self) || crate::pat::is_fine_grained(self) {
            return None;
        }
        self.scopes.as_ref().map(|s| {
            s.iter()
                .filter(|s| !crate::pat::is_internal_scope(s))
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
    }
}

/// Classic OAuth scopes, in the order GitHub lists them.
const KNOWN_SCOPES: &[&str] = &[
    "admin:enterprise",
    "admin:gpg_key",
    "admin:org",
    "admin:org_hook",
    "admin:public_key",
    "admin:repo_hook",
    "codespace",
    "delete:packages",
    "delete_repo",
    "gist",
    "manage_runners:org",
    "notifications",
    "project",
    "public_repo",
    "read:gpg_key",
    "read:org",
    "read:packages",
    "read:project",
    "read:public_key",
    "read:repo_hook",
    "read:user",
    "repo",
    "repo:invite",
    "repo:status",
    "repo_deployment",
    "security_events",
    "site_admin",
    "user",
    "user:email",
    "user:follow",
    "workflow",
    "write:gpg_key",
    "write:org",
    "write:packages",
    "write:public_key",
    "write:repo_hook",
];

tokio::task_local! {
    /// Scopes the current request's endpoint checked ([`AuthContext::require_scope`]).
    static ACCEPTED_SCOPES: Arc<std::sync::Mutex<Vec<String>>>;
}

fn record_accepted_scope(scope: &str) {
    let _ = ACCEPTED_SCOPES.try_with(|s| {
        if let Ok(mut s) = s.lock()
            && !s.iter().any(|x| x == scope)
        {
            s.push(scope.to_string());
        }
    });
}

/// Value of `X-Accepted-OAuth-Scopes` for the scopes an endpoint checked:
/// each scope plus every scope that implies it (`read:org` → `admin:org,
/// read:org, write:org`), like GitHub.
pub fn accepted_scopes_header(wanted: &[String]) -> String {
    let mut out: Vec<&str> = KNOWN_SCOPES
        .iter()
        .copied()
        .filter(|g| wanted.iter().any(|w| implied(g, w)))
        .collect();
    for w in wanted {
        if !out.contains(&w.as_str()) {
            out.push(w);
        }
    }
    out.join(", ")
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
            "basic" => Some(basic_auth(state, headers, rest, opts).await?),
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
    if let Some(actor) = ctx.as_ref().and_then(crate::perms::job_token_actor) {
        crate::sync::context::note_actions_actor(actor);
    }
    if let Some(ctx) = &ctx {
        crate::observability::record_caller(ctx);
    }
    Ok(ctx)
}

#[derive(sqlx::FromRow)]
struct TokenRow {
    token_id: i64,
    kind: String,
    scopes: Vec<String>,
    /// Organizations whose token policy blocks this token (P47).
    blocked_orgs: Option<Vec<i64>>,
    expires_at: Option<DateTime<Utc>>,
    last_used_at: Option<DateTime<Utc>>,
    #[sqlx(flatten)]
    user: db::User,
}

async fn token_auth(state: &AppState, token: &str) -> ApiResult<AuthContext> {
    if token.is_empty() {
        return Err(ApiError::bad_credentials());
    }
    if crate::apps::looks_like_jwt(token) {
        return crate::apps::jwt_auth(state, token).await;
    }
    let hash = crypto::sha256_hex(token);
    let row: Option<TokenRow> = sqlx::query_as(&format!(
        "SELECT t.id AS token_id, t.kind, t.scopes, {} AS blocked_orgs,
                t.expires_at, t.last_used_at, {}
           FROM access_tokens t JOIN users u ON u.id = t.user_id
          WHERE t.token_hash = $1",
        crate::pat::BLOCKED_ORGS_SQL,
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
    let _ = TOKEN_EXPIRATION.try_with(|e| e.set(row.expires_at));
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
    // Fine-grained markers only count on fine-grained rows; policy blocks
    // are computed, never stored.
    let mut scopes: Vec<String> = row
        .scopes
        .into_iter()
        .filter(|s| row.kind == "fine_grained" || !crate::pat::is_internal_scope(s))
        .collect();
    scopes.extend(crate::pat::blocked_scopes(
        row.blocked_orgs.as_deref().unwrap_or_default(),
    ));
    Ok(AuthContext {
        user: row.user,
        method: AuthMethod::Token {
            token_id: row.token_id,
        },
        scopes: Some(scopes),
    })
}

async fn basic_auth(
    state: &AppState,
    headers: &HeaderMap,
    encoded: &str,
    opts: AuthOptions,
) -> ApiResult<AuthContext> {
    let decoded = STANDARD
        .decode(encoded)
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .ok_or_else(ApiError::bad_credentials)?;
    let (login, secret) = decoded
        .split_once(':')
        .ok_or_else(ApiError::bad_credentials)?;
    if secret.starts_with(crypto::PAT_PREFIX)
        || secret.starts_with(crypto::OAUTH_TOKEN_PREFIX)
        || secret.starts_with(crypto::INSTALLATION_TOKEN_PREFIX)
        || secret.starts_with(crypto::FINE_GRAINED_PAT_PREFIX)
    {
        // Like GitHub, the username is ignored for token auth.
        return token_auth(state, secret).await;
    }
    if !opts.allow_password {
        return Err(ApiError::bad_credentials());
    }
    let ip = request_ip(&state.config, headers);
    let user = check_password(state, login, secret, PasswordTransport::Git, &ip).await?;
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

// ---------------------------------------------------------------------------
// Password sign-in policy (web login and git/LFS basic auth)
// ---------------------------------------------------------------------------

/// Failed password attempts are counted per login and per client IP in
/// this window (seconds).
pub const LOGIN_WINDOW_SECS: u64 = 900;
/// Failures per login within the window before sign-in is locked.
pub const MAX_FAILS_PER_LOGIN: u64 = 10;
/// Failures per client IP within the window before sign-in is locked.
pub const MAX_FAILS_PER_IP: u64 = 50;

/// Throttle counter of failed sign-ins for a login (as submitted).
pub fn login_fail_key(login: &str) -> String {
    format!("login_fail:{}", login.to_lowercase())
}

/// Throttle counter of failed sign-ins from a client IP.
pub fn ip_fail_key(ip: &str) -> String {
    format!("login_fail_ip:{ip}")
}

/// Where a password was presented (audited as `transport`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasswordTransport {
    /// Web / `_bgh` sign-in.
    Web,
    /// Git smart HTTP, LFS and wiki git (HTTP basic auth).
    Git,
}

impl PasswordTransport {
    pub fn name(self) -> &'static str {
        match self {
            Self::Web => "web",
            Self::Git => "git",
        }
    }
}

/// Answer of a password directory (LDAP) for a sign-in attempt.
#[derive(Debug)]
pub enum DirectoryAuth {
    /// No directory configured: built-in passwords only.
    NotConfigured,
    /// The directory has no such user; built-in accounts may still sign in.
    UnknownUser,
    /// The directory could not be reached; built-in passwords are checked
    /// as usual (directory accounts are refused, site admins excepted).
    Unavailable,
    /// The directory refused the credentials (wrong password, not in the
    /// restricted group, ...).
    Rejected,
    /// Authenticated; the local account (provisioned / updated).
    Authenticated(Box<db::User>),
}

/// A password directory consulted before built-in passwords
/// (`bgh_accounts::ldap` registers the LDAP one at startup).
pub trait PasswordDirectory: Send + Sync + 'static {
    /// Check `login` + `password` against the directory.
    fn authenticate<'a>(
        &'a self,
        state: &'a AppState,
        login: &'a str,
        password: &'a str,
    ) -> BoxFuture<'a, ApiResult<DirectoryAuth>>;

    /// Whether the local account is managed by the directory (then its
    /// built-in password, if any, is not accepted while the directory is
    /// enabled, except for break-glass site administrators).
    fn manages<'a>(&'a self, state: &'a AppState, user_id: i64) -> BoxFuture<'a, ApiResult<bool>>;
}

static DIRECTORY: OnceLock<Arc<dyn PasswordDirectory>> = OnceLock::new();

/// Install the password directory (first call wins; it is process-wide and
/// reads its configuration from the site settings of each request).
pub fn set_password_directory(directory: Arc<dyn PasswordDirectory>) {
    let _ = DIRECTORY.set(directory);
}

/// 403 for a built-in password while `auth_providers.password_login` is off.
pub fn password_login_disabled(transport: PasswordTransport) -> ApiError {
    ApiError::forbidden(match transport {
        PasswordTransport::Web => {
            "Password sign-in is disabled on this instance. Sign in with single sign-on."
        }
        PasswordTransport::Git => {
            "Password authentication is disabled on this instance. Use a personal access token instead."
        }
    })
}

/// 429 while failed sign-ins of a login or IP are over the threshold.
pub fn too_many_failed_logins() -> ApiError {
    ApiError::Status(
        axum::http::StatusCode::TOO_MANY_REQUESTS,
        "Too many failed login attempts. Please try again later.".into(),
    )
}

/// Check a sign-in password: the one policy for web sign-in and git/LFS
/// basic auth.
///
/// * Locked (429) after [`MAX_FAILS_PER_LOGIN`] failures of the login or
///   [`MAX_FAILS_PER_IP`] of the client IP within [`LOGIN_WINDOW_SECS`].
/// * The password directory (LDAP), when configured, is asked first.
/// * Built-in passwords are refused (403) when `auth_providers.password_login`
///   is off, except for site admins with `password_login_admin_exempt`.
/// * Failures count against both throttles and are audited as
///   `user.failed_login` with `transport`.
/// * Suspended accounts → 403. Two-factor is up to the caller.
pub async fn check_password(
    state: &AppState,
    login: &str,
    password: &str,
    transport: PasswordTransport,
    ip: &str,
) -> ApiResult<db::User> {
    let login = login.trim();
    if login.is_empty() || password.is_empty() {
        return Err(ApiError::bad_credentials());
    }
    if ratelimit::count(state, &login_fail_key(login)).await >= MAX_FAILS_PER_LOGIN
        || ratelimit::count(state, &ip_fail_key(ip)).await >= MAX_FAILS_PER_IP
    {
        return Err(too_many_failed_logins());
    }
    let s = settings::load(state).await?;
    let ap = &s.auth_providers;
    let directory = DIRECTORY.get();
    if let Some(dir) = directory {
        match dir.authenticate(state, login, password).await? {
            DirectoryAuth::NotConfigured
            | DirectoryAuth::UnknownUser
            | DirectoryAuth::Unavailable => {}
            DirectoryAuth::Rejected => {
                return Err(failed_login(state, login, None, transport, ip).await?);
            }
            DirectoryAuth::Authenticated(user) => {
                return signed_in(state, login, *user, transport, ip).await;
            }
        }
    }
    let admin_exempt = |u: &db::User| u.site_admin && ap.password_login_admin_exempt;
    if !ap.password_login {
        // Decide before checking the password, so a disabled sign-in is
        // not a password oracle.
        let user = find_login(state, login).await?;
        if !user.as_ref().is_some_and(admin_exempt) {
            return Err(password_login_disabled(transport));
        }
    }
    let Some(user) = verify_login(state, login, password).await? else {
        return Err(failed_login(state, login, None, transport, ip).await?);
    };
    if let Some(dir) = directory
        && ap.ldap.enabled
        && !user.site_admin
        && dir.manages(state, user.id).await?
    {
        // Directory accounts use their directory password only (site
        // admins keep a break-glass built-in password).
        return Err(failed_login(state, login, Some(&user), transport, ip).await?);
    }
    signed_in(state, login, user, transport, ip).await
}

async fn signed_in(
    state: &AppState,
    login: &str,
    user: db::User,
    transport: PasswordTransport,
    ip: &str,
) -> ApiResult<db::User> {
    if user.is_suspended() {
        audit::log_with_ip(
            &state.db,
            Some(&user),
            "user.failed_login",
            audit::Target::User(user.id),
            json!({ "reason": "suspended", "transport": transport.name() }),
            Some(ip),
        )
        .await?;
        return Err(ApiError::forbidden("Sorry. Your account was suspended."));
    }
    ratelimit::clear(state, &login_fail_key(login)).await;
    Ok(user)
}

/// Count and audit a failed sign-in; returns the 401 to answer with.
async fn failed_login(
    state: &AppState,
    login: &str,
    user: Option<&db::User>,
    transport: PasswordTransport,
    ip: &str,
) -> ApiResult<ApiError> {
    ratelimit::hit(state, &login_fail_key(login), LOGIN_WINDOW_SECS).await?;
    ratelimit::hit(state, &ip_fail_key(ip), LOGIN_WINDOW_SECS).await?;
    audit::log_with_ip(
        &state.db,
        user,
        "user.failed_login",
        user.map_or(audit::Target::None, |u| audit::Target::User(u.id)),
        json!({ "login": login, "transport": transport.name() }),
        Some(ip),
    )
    .await?;
    Ok(ApiError::bad_credentials())
}

/// The user signing in with `login_or_email` (login or verified email).
pub async fn find_login(state: &AppState, login_or_email: &str) -> ApiResult<Option<db::User>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {} FROM users u
          WHERE u.type = 'User' AND (lower(u.login) = lower($1)
             OR u.id = (SELECT user_id FROM user_emails WHERE lower(email) = lower($1) AND verified))",
        db::prefixed("u", db::User::COLUMNS)
    ))
    .bind(login_or_email)
    .fetch_optional(&state.db)
    .await?)
}

/// Check a login (or primary email) + password. Returns the user on success.
pub async fn verify_login(
    state: &AppState,
    login_or_email: &str,
    password: &str,
) -> ApiResult<Option<db::User>> {
    let user = find_login(state, login_or_email).await?;
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
    let cached: Option<String> =
        crate::observability::redis_result("session", redis.get(&key).await).unwrap_or(None);
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

tokio::task_local! {
    /// TCP peer of the request being handled (set by
    /// [`auth_headers_middleware`]), for code that only sees headers.
    static PEER: Option<std::net::SocketAddr>;
}

/// [`client_ip`] for code that has the request headers but not its
/// extensions (e.g. git basic auth): the peer address comes from the
/// request scope installed by [`auth_headers_middleware`].
pub fn request_ip(config: &Config, headers: &HeaderMap) -> String {
    let mut ext = Extensions::new();
    if let Ok(Some(peer)) = PEER.try_with(|p| *p) {
        ext.insert(axum::extract::ConnectInfo(peer));
    }
    client_ip(config, headers, &ext)
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

// ---------------------------------------------------------------------------
// CSRF
// ---------------------------------------------------------------------------

/// Header carrying the CSRF token (`boot.csrf`, docs/SYNC_PROTOCOL.md §10).
pub const CSRF_HEADER: &str = "x-csrf-token";

/// CSRF token bound to a session: derived from the secret session cookie
/// (which scripts can't read), so it needs no storage.
pub fn csrf_token(session_token: &str) -> String {
    crypto::sha256_hex(&format!("bgh-csrf:{session_token}"))[..40].to_string()
}

/// Paths that accept cookie-carrying mutations without `X-CSRF-Token`:
/// sign-in endpoints (no session yet) and server-rendered forms that carry
/// their own one-time nonce.
const CSRF_EXEMPT: &[&str] = &[
    "/_bgh/auth/login",
    "/_bgh/auth/signup",
    "/_bgh/auth/2fa",
    "/_bgh/signup",
    "/_bgh/session/two_factor",
    "/_bgh/password_reset",
    "/_bgh/emails/verify",
    "/login/oauth/",
    "/login/device",
];

fn csrf_exempt(path: &str, method: &axum::http::Method) -> bool {
    // `POST /_bgh/session` (login) is exempt; `DELETE` (logout) is not.
    (path == "/_bgh/session" && method == axum::http::Method::POST)
        || CSRF_EXEMPT.iter().any(|p| path.starts_with(p))
}

/// Middleware: cookie-authenticated mutating requests (no `Authorization`
/// header, a `bgh_session` cookie, method other than GET/HEAD/OPTIONS) must
/// carry `X-CSRF-Token` matching [`csrf_token`], else 403. Token-authenticated
/// clients are unaffected.
pub async fn csrf_middleware(req: Request, next: Next) -> Response {
    use axum::http::Method;
    use axum::response::IntoResponse;
    let mutating = !matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS);
    if mutating
        && !req.headers().contains_key(header::AUTHORIZATION)
        && !csrf_exempt(req.uri().path(), req.method())
        && let Some(session) = cookie(req.headers(), SESSION_COOKIE)
    {
        let sent = req
            .headers()
            .get(CSRF_HEADER)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if !crypto::constant_time_eq(sent, &csrf_token(&session)) {
            return ApiError::forbidden("Missing or invalid CSRF token (X-CSRF-Token).")
                .into_response();
        }
    }
    next.run(req).await
}

tokio::task_local! {
    /// Expiry of the access token that authenticated the current request
    /// (set by `token_auth` inside [`auth_headers_middleware`]).
    static TOKEN_EXPIRATION: std::cell::Cell<Option<DateTime<Utc>>>;
}

/// `GitHub-Authentication-Token-Expiration` value (`2024-01-01 00:00:00 UTC`).
pub fn token_expiration_header(at: DateTime<Utc>) -> String {
    at.format("%Y-%m-%d %H:%M:%S UTC").to_string()
}

/// Middleware: installs an [`AuthSlot`] and, after the handler ran, emits
/// `X-OAuth-Scopes` for token-authenticated requests and
/// `X-Accepted-OAuth-Scopes` for scope-gated endpoints (like GitHub), and
/// `GitHub-Authentication-Token-Expiration` for tokens that expire.
pub async fn auth_headers_middleware(mut req: Request, next: Next) -> Response {
    let slot = AuthSlot::default();
    req.extensions_mut().insert(slot.clone());
    let peer = req
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|c| c.0);
    let accepted = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (mut resp, expiry) = TOKEN_EXPIRATION
        .scope(
            std::cell::Cell::new(None),
            PEER.scope(
                peer,
                ACCEPTED_SCOPES.scope(accepted.clone(), async {
                    let resp = next.run(req).await;
                    (resp, TOKEN_EXPIRATION.with(|e| e.get()))
                }),
            ),
        )
        .await;
    if let Some(at) = expiry
        && let Ok(v) = HeaderValue::from_str(&token_expiration_header(at))
    {
        resp.headers_mut()
            .insert("github-authentication-token-expiration", v);
    }
    if let Ok(wanted) = accepted.lock()
        && !wanted.is_empty()
        && let Ok(v) = HeaderValue::from_str(&accepted_scopes_header(&wanted))
    {
        resp.headers_mut().insert("x-accepted-oauth-scopes", v);
    }
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
    fn accepted_scopes_include_implying_scopes() {
        assert_eq!(
            accepted_scopes_header(&["read:org".into()]),
            "admin:org, read:org, write:org"
        );
        assert_eq!(accepted_scopes_header(&["repo".into()]), "repo");
        assert_eq!(accepted_scopes_header(&["custom".into()]), "custom");
    }

    #[test]
    fn csrf_exemptions() {
        use axum::http::Method;
        assert!(csrf_exempt("/_bgh/auth/login", &Method::POST));
        assert!(csrf_exempt("/_bgh/session", &Method::POST));
        assert!(!csrf_exempt("/_bgh/session", &Method::DELETE));
        assert!(!csrf_exempt("/api/v3/user", &Method::PATCH));
        assert_eq!(csrf_token("a"), csrf_token("a"));
        assert_ne!(csrf_token("a"), csrf_token("b"));
    }

    #[test]
    fn parses_cookies() {
        let mut h = HeaderMap::new();
        h.insert(header::COOKIE, "a=1; bgh_session=abc; b=2".parse().unwrap());
        assert_eq!(cookie(&h, "bgh_session").as_deref(), Some("abc"));
        assert_eq!(cookie(&h, "nope"), None);
    }
}
