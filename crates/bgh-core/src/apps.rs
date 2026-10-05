//! GitHub Apps: app JWT authentication (RS256) and installation access
//! tokens.
//!
//! * An app authenticates as itself with a JWT signed by one of its private
//!   keys (`iss` = app id or client id, lifetime at most 10 minutes). The
//!   caller becomes [`AuthMethod::App`] acting as the app's bot user, and
//!   may only call the app endpoints ([`jwt_route`]): `/app`,
//!   `/app/installations/…`, `/apps/{slug}` and the `…/installation`
//!   lookups. Registration, keys and installations live in
//!   `bgh_accounts::apps`.
//! * Installation access tokens (`bghs_…`, 1 hour) are `access_tokens` rows
//!   of kind `app` owned by the bot user. Their scopes carry the
//!   installation ([`INSTALLATION_SCOPE_PREFIX`]), the repositories they
//!   cover ([`OWNER_SCOPE_PREFIX`] for every repository of the account,
//!   else one [`REPO_SCOPE_PREFIX`] per repository) and the permission map
//!   (`token_permissions` scopes), so enforcement needs no query:
//!   [`effective_cap`] limits repository roles (`perms::effective`), the
//!   route-category table of `token_permissions` limits categories.

use axum::extract::{Request, State};
use axum::http::{Method, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use std::collections::BTreeMap;

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use rsa::pkcs1::{DecodeRsaPrivateKey, EncodeRsaPrivateKey};
use rsa::pkcs1v15::{Signature, SigningKey, VerifyingKey};
use rsa::pkcs8::{DecodePrivateKey, DecodePublicKey, EncodePublicKey, LineEnding};
use rsa::signature::{SignatureEncoding, Signer, Verifier};
use rsa::{RsaPrivateKey, RsaPublicKey};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::auth::{AuthContext, AuthMethod};
use crate::error::{ApiError, ApiResult};
use crate::models::api::SimpleUser;
use crate::models::db;
use crate::node_id::{self, NodeType};
use crate::perms::Permission;
use crate::state::AppState;
use crate::time::{Timestamp, ts};
use crate::urls::Urls;

/// Scope naming the installation of an installation token:
/// `app:installation:{id}`.
pub const INSTALLATION_SCOPE_PREFIX: &str = "app:installation:";
/// Scope of an installation token covering every repository of an account
/// (`repository_selection: all`): `app:owner:{account_id}`.
pub const OWNER_SCOPE_PREFIX: &str = "app:owner:";
/// Scope of an installation token covering one repository: `app:repo:{id}`.
pub const REPO_SCOPE_PREFIX: &str = "app:repo:";
/// Scope naming the app of a user-to-server token (`bghu_…`):
/// `app:user:{app_id}`. Such tokens also carry the repositories of the
/// app's installations the user can reach ([`OWNER_SCOPE_PREFIX`],
/// [`REPO_SCOPE_PREFIX`]) and the app's permission map.
pub const USER_SCOPE_PREFIX: &str = "app:user:";
/// Lifetime of user-to-server tokens (GitHub: 8 hours).
pub const USER_TOKEN_TTL_SECS: i64 = 8 * 3600;
/// Lifetime of refresh tokens (GitHub: 6 months).
pub const REFRESH_TOKEN_TTL_SECS: i64 = 15_897_600;

/// Scope prefix of a token's permission map entries
/// (`actions:permission:contents:read`).
pub use crate::token_permissions::PERMISSION_SCOPE_PREFIX;

/// Longest accepted JWT lifetime (`exp` - now), like GitHub.
pub const JWT_MAX_LIFETIME_SECS: i64 = 600;
/// Clock drift tolerated on `iat` / `exp`.
const JWT_LEEWAY_SECS: i64 = 60;
/// Lifetime of installation access tokens.
pub const INSTALLATION_TOKEN_TTL_SECS: i64 = 3600;

/// GitHub's message for credentials that aren't a valid app JWT where one
/// is required.
pub const JWT_REQUIRED: &str = "A JSON web token could not be decoded";
/// GitHub's message for calls an integration may not make.
pub const NOT_ACCESSIBLE: &str = "Resource not accessible by integration";

/// App id when `auth` is an app JWT.
pub fn jwt_app_id(auth: &AuthContext) -> Option<i64> {
    match auth.method {
        AuthMethod::App { app_id } => Some(app_id),
        _ => None,
    }
}

/// Installation id when `auth` is an installation access token.
pub fn installation_id(auth: &AuthContext) -> Option<i64> {
    scope_id(auth, INSTALLATION_SCOPE_PREFIX)
}

/// App id when `auth` is a user-to-server token (acts as the user).
pub fn user_to_server_app_id(auth: &AuthContext) -> Option<i64> {
    scope_id(auth, USER_SCOPE_PREFIX)
}

/// Whether `auth` is a GitHub App credential (JWT or installation token).
pub fn is_integration(auth: &AuthContext) -> bool {
    jwt_app_id(auth).is_some() || installation_id(auth).is_some()
}

fn scope_id(auth: &AuthContext, prefix: &str) -> Option<i64> {
    auth.scopes
        .as_ref()?
        .iter()
        .find_map(|s| s.strip_prefix(prefix)?.parse().ok())
}

/// Whether an installation token covers `repo` (`None` for other
/// credentials).
pub fn token_covers(auth: &AuthContext, repo: &db::Repository) -> Option<bool> {
    installation_id(auth).or_else(|| user_to_server_app_id(auth))?;
    let scopes = auth.scopes.as_deref().unwrap_or_default();
    let owner = format!("{OWNER_SCOPE_PREFIX}{}", repo.owner_id);
    let one = format!("{REPO_SCOPE_PREFIX}{}", repo.id);
    Some(scopes.iter().any(|s| *s == owner || *s == one))
}

/// Whether an installation token covers the repository `owner/name`
/// (fails closed: lookup errors count as covered, so category checks
/// apply).
pub async fn covers_repo_named(
    state: &AppState,
    auth: &AuthContext,
    owner: &str,
    name: &str,
) -> bool {
    let row: Result<Option<(i64, i64)>, _> = sqlx::query_as(
        "SELECT r.id, r.owner_id FROM repositories r JOIN users u ON u.id = r.owner_id
          WHERE lower(u.login) = lower($1) AND lower(r.name) = lower($2)",
    )
    .bind(owner)
    .bind(name.strip_suffix(".git").unwrap_or(name))
    .fetch_optional(&state.db)
    .await;
    match row {
        Ok(Some((id, owner_id))) => {
            let scopes = auth.scopes.as_deref().unwrap_or_default();
            let o = format!("{OWNER_SCOPE_PREFIX}{owner_id}");
            let r = format!("{REPO_SCOPE_PREFIX}{id}");
            scopes.iter().any(|s| *s == o || *s == r)
        }
        Ok(None) => false,
        Err(_) => true,
    }
}

/// The scopes recording which repositories an installation token covers.
pub fn repo_scopes(account_id: i64, all: bool, repo_ids: &[i64]) -> Vec<String> {
    if all {
        vec![format!("{OWNER_SCOPE_PREFIX}{account_id}")]
    } else {
        repo_ids
            .iter()
            .map(|id| format!("{REPO_SCOPE_PREFIX}{id}"))
            .collect()
    }
}

/// Repository role of an installation token on `repo` (`None` for other
/// credentials): covered repositories get Write when any permission is
/// `write`, else Read; the per-category map is enforced by
/// `token_permissions` (administration endpoints stay closed). Other
/// repositories are seen like an anonymous caller would.
pub fn effective_cap(auth: &AuthContext, repo: &db::Repository) -> Option<Permission> {
    installation_id(auth)?;
    let covers = token_covers(auth, repo)?;
    if !covers {
        return Some(if repo.is_private() {
            Permission::None
        } else {
            Permission::Read
        });
    }
    Some(
        if permission_scopes(auth).iter().any(|(_, a)| *a == "write") {
            Permission::Write
        } else {
            Permission::Read
        },
    )
}

/// Repository role of a user-to-server token on `repo` (`None` for other
/// credentials): the user's own role `raw`, capped like an installation
/// token on the repositories the app is installed on; elsewhere the user
/// sees what an anonymous caller would.
pub fn user_to_server_cap(
    auth: &AuthContext,
    repo: &db::Repository,
    raw: Permission,
) -> Option<Permission> {
    user_to_server_app_id(auth)?;
    let floor = if repo.is_private() {
        Permission::None
    } else {
        Permission::Read
    };
    if !token_covers(auth, repo)? {
        return Some(raw.min(floor));
    }
    let cap = if permission_scopes(auth).iter().any(|(_, a)| *a == "write") {
        Permission::Write
    } else {
        Permission::Read
    };
    Some(raw.min(cap))
}

/// `(category, access)` pairs of a token's permission scopes
/// (`token_permissions::PERMISSION_SCOPE_PREFIX`).
fn permission_scopes(auth: &AuthContext) -> Vec<(&str, &str)> {
    auth.scopes
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter_map(|s| s.strip_prefix(PERMISSION_SCOPE_PREFIX))
        .filter_map(|s| s.split_once(':'))
        .collect()
}

/// Whether a git request may proceed for an installation token: pushes need
/// `contents: write`, fetches `contents: read` (public repositories excepted,
/// like anonymous clones). `Ok` for other credentials.
pub fn check_git(auth: Option<&AuthContext>, repo: &db::Repository, write: bool) -> ApiResult<()> {
    let Some(auth) = auth else { return Ok(()) };
    if jwt_app_id(auth).is_some() {
        return Err(ApiError::forbidden(NOT_ACCESSIBLE));
    }
    if installation_id(auth).is_none() && user_to_server_app_id(auth).is_none() {
        return Ok(());
    }
    let contents = permission_scopes(auth)
        .into_iter()
        .find(|(c, _)| *c == "contents")
        .map(|(_, a)| a);
    let ok = match contents {
        Some("write") => true,
        Some("read") => !write,
        _ => !write && !repo.is_private(),
    };
    if ok {
        Ok(())
    } else {
        Err(ApiError::forbidden(format!(
            "Permission to {} denied to {}.",
            repo.name, auth.user.login
        )))
    }
}

// ---------------------------------------------------------------------------
// Rows and GitHub shapes
// ---------------------------------------------------------------------------

/// `github_apps` row.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AppRow {
    pub id: i64,
    pub owner_id: i64,
    pub bot_user_id: i64,
    pub slug: String,
    pub name: String,
    pub description: String,
    pub homepage_url: String,
    pub callback_urls: Vec<String>,
    pub setup_url: Option<String>,
    pub setup_on_update: bool,
    pub webhook_active: bool,
    pub webhook_url: Option<String>,
    pub webhook_secret: Option<Vec<u8>>,
    pub permissions: sqlx::types::Json<BTreeMap<String, String>>,
    pub events: Vec<String>,
    pub public: bool,
    pub client_id: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// `json` or `form` (P46).
    pub webhook_content_type: String,
    pub webhook_insecure_ssl: bool,
}

impl AppRow {
    pub const COLUMNS: &'static str = "id, owner_id, bot_user_id, slug, name, description, \
        homepage_url, callback_urls, setup_url, setup_on_update, webhook_active, webhook_url, \
        webhook_secret, permissions, events, public, client_id, created_at, updated_at, \
        webhook_content_type, webhook_insecure_ssl";
}

/// `app_installations` row.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct InstallationRow {
    pub id: i64,
    pub app_id: i64,
    pub account_id: i64,
    pub repository_selection: String,
    pub permissions: sqlx::types::Json<BTreeMap<String, String>>,
    pub events: Vec<String>,
    pub installed_by_id: Option<i64>,
    pub suspended_at: Option<DateTime<Utc>>,
    pub suspended_by_id: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl InstallationRow {
    pub const COLUMNS: &'static str = "id, app_id, account_id, repository_selection, \
        permissions, events, installed_by_id, suspended_at, suspended_by_id, created_at, \
        updated_at";

    pub fn all_repositories(&self) -> bool {
        self.repository_selection == "all"
    }
}

/// GitHub's `integration` (a GitHub App).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Integration {
    pub id: i64,
    pub slug: String,
    pub node_id: String,
    pub client_id: String,
    pub owner: SimpleUser,
    pub name: String,
    pub description: Option<String>,
    pub external_url: String,
    pub html_url: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub permissions: BTreeMap<String, String>,
    pub events: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installations_count: Option<i64>,
}

impl Integration {
    /// `owner` is the row of `app.owner_id`.
    pub fn new(urls: &Urls, app: &AppRow, owner: &db::User, installations: Option<i64>) -> Self {
        Self {
            id: app.id,
            slug: app.slug.clone(),
            node_id: node_id::encode(NodeType::Integration, app.id),
            client_id: app.client_id.clone(),
            owner: SimpleUser::new(urls, owner),
            name: app.name.clone(),
            description: Some(app.description.clone()).filter(|d| !d.is_empty()),
            external_url: app.homepage_url.clone(),
            html_url: urls.html(&format!("/apps/{}", app.slug)),
            created_at: app.created_at.into(),
            updated_at: app.updated_at.into(),
            permissions: with_metadata(&app.permissions),
            events: app.events.clone(),
            installations_count: installations,
        }
    }
}

/// Permission maps always include `metadata: read`, like GitHub's.
pub fn with_metadata(p: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut p = p.clone();
    p.entry("metadata".into()).or_insert_with(|| "read".into());
    p
}

/// GitHub's `installation`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Installation {
    pub id: i64,
    pub account: SimpleUser,
    pub repository_selection: String,
    pub access_tokens_url: String,
    pub repositories_url: String,
    pub html_url: String,
    pub app_id: i64,
    pub app_slug: String,
    pub client_id: String,
    pub target_id: i64,
    pub target_type: String,
    pub permissions: BTreeMap<String, String>,
    pub events: Vec<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub single_file_name: Option<String>,
    pub has_multiple_single_files: bool,
    pub single_file_paths: Vec<String>,
    pub suspended_by: Option<SimpleUser>,
    pub suspended_at: Option<Timestamp>,
    pub contact_email: Option<String>,
}

impl Installation {
    /// `account` is the row of `inst.account_id`, `suspended_by` of
    /// `inst.suspended_by_id`.
    pub fn new(
        urls: &Urls,
        inst: &InstallationRow,
        app: &AppRow,
        account: &db::User,
        suspended_by: Option<&db::User>,
    ) -> Self {
        let html_url = if account.is_org() {
            urls.html(&format!(
                "/organizations/{}/settings/installations/{}",
                account.login, inst.id
            ))
        } else {
            urls.html(&format!("/settings/installations/{}", inst.id))
        };
        Self {
            id: inst.id,
            account: SimpleUser::new(urls, account),
            repository_selection: inst.repository_selection.clone(),
            access_tokens_url: urls.api(&format!("/app/installations/{}/access_tokens", inst.id)),
            repositories_url: urls.api("/installation/repositories"),
            html_url,
            app_id: app.id,
            app_slug: app.slug.clone(),
            client_id: app.client_id.clone(),
            target_id: account.id,
            target_type: account.kind.clone(),
            permissions: with_metadata(&inst.permissions),
            events: inst.events.clone(),
            created_at: inst.created_at.into(),
            updated_at: inst.updated_at.into(),
            single_file_name: None,
            has_multiple_single_files: false,
            single_file_paths: Vec::new(),
            suspended_by: suspended_by.map(|u| SimpleUser::new(urls, u)),
            suspended_at: ts(inst.suspended_at),
            contact_email: None,
        }
    }
}

// ---------------------------------------------------------------------------
// JWT
// ---------------------------------------------------------------------------

/// Whether a bearer credential looks like a JWT (three base64url parts).
pub fn looks_like_jwt(token: &str) -> bool {
    token.starts_with("eyJ") && token.split('.').count() == 3
}

fn jwt_error(message: &str) -> ApiError {
    ApiError::Unauthorized {
        message: message.to_string(),
        www_authenticate: None,
    }
}

#[derive(sqlx::FromRow)]
struct JwtApp {
    app_id: i64,
    #[sqlx(flatten)]
    bot: db::User,
}

/// Authenticate an app JWT (`Authorization: Bearer <jwt>`).
pub(crate) async fn jwt_auth(state: &AppState, token: &str) -> ApiResult<AuthContext> {
    let mut parts = token.split('.');
    let (Some(h), Some(p), Some(s)) = (parts.next(), parts.next(), parts.next()) else {
        return Err(jwt_error(JWT_REQUIRED));
    };
    let decode_json = |part: &str| -> Option<Value> {
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(part.trim_end_matches('=')).ok()?).ok()
    };
    let (Some(head), Some(claims), Ok(sig)) = (
        decode_json(h),
        decode_json(p),
        URL_SAFE_NO_PAD.decode(s.trim_end_matches('=')),
    ) else {
        return Err(jwt_error(JWT_REQUIRED));
    };
    if head.get("alg").and_then(Value::as_str) != Some("RS256") {
        return Err(jwt_error(JWT_REQUIRED));
    }
    let now = Utc::now().timestamp();
    let Some(exp) = claims.get("exp").and_then(Value::as_i64) else {
        return Err(jwt_error(
            "'Expiration time' claim ('exp') must be a numeric value representing the future time at which the assertion expires",
        ));
    };
    if exp <= now - JWT_LEEWAY_SECS {
        return Err(jwt_error(
            "'Expiration time' claim ('exp') must be a numeric value representing the future time at which the assertion expires",
        ));
    }
    if exp > now + JWT_MAX_LIFETIME_SECS + JWT_LEEWAY_SECS {
        return Err(jwt_error(
            "'Expiration time' claim ('exp') is too far in the future",
        ));
    }
    match claims.get("iat").and_then(Value::as_i64) {
        Some(iat) if iat <= now + JWT_LEEWAY_SECS && iat < exp => {}
        _ => {
            return Err(jwt_error(
                "'Issued at' claim ('iat') must be an Integer representing the time that the assertion was issued",
            ));
        }
    }
    // `iss`: the app id (number or numeric string) or its client id.
    let (by_id, by_client): (Option<i64>, Option<&str>) = match claims.get("iss") {
        Some(Value::Number(n)) => (n.as_i64(), None),
        Some(Value::String(s)) => match s.parse::<i64>() {
            Ok(id) => (Some(id), None),
            Err(_) => (None, Some(s.as_str())),
        },
        _ => (None, None),
    };
    if by_id.is_none() && by_client.is_none() {
        return Err(jwt_error(
            "'Issuer' claim ('iss') must be an Integer or a client ID",
        ));
    }
    let app: Option<JwtApp> = sqlx::query_as(&format!(
        "SELECT a.id AS app_id, {} FROM github_apps a JOIN users u ON u.id = a.bot_user_id
          WHERE a.id = $1 OR a.client_id = $2",
        db::prefixed("u", db::User::COLUMNS)
    ))
    .bind(by_id)
    .bind(by_client)
    .fetch_optional(&state.db)
    .await?;
    let app = app.ok_or_else(|| jwt_error("Integration not found"))?;
    let keys: Vec<String> =
        sqlx::query_scalar("SELECT public_key FROM github_app_keys WHERE app_id = $1")
            .bind(app.app_id)
            .fetch_all(&state.db)
            .await?;
    let signed = format!("{h}.{p}");
    let Ok(signature) = Signature::try_from(sig.as_slice()) else {
        return Err(jwt_error(JWT_REQUIRED));
    };
    let valid = keys.iter().any(|pem| {
        RsaPublicKey::from_public_key_pem(pem).is_ok_and(|k| {
            VerifyingKey::<Sha256>::new(k)
                .verify(signed.as_bytes(), &signature)
                .is_ok()
        })
    });
    if !valid {
        return Err(jwt_error(JWT_REQUIRED));
    }
    Ok(AuthContext {
        user: app.bot,
        method: AuthMethod::App { app_id: app.app_id },
        scopes: Some(Vec::new()),
    })
}

/// Sign an app JWT with a PEM private key (PKCS#1 or PKCS#8). For tests,
/// tools and docs; apps sign their own JWTs.
pub fn sign_jwt(private_pem: &str, iss: &Value, iat: i64, exp: i64) -> anyhow::Result<String> {
    let key = RsaPrivateKey::from_pkcs1_pem(private_pem)
        .or_else(|_| RsaPrivateKey::from_pkcs8_pem(private_pem))?;
    let head = URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","typ":"JWT"}"#);
    let claims = URL_SAFE_NO_PAD.encode(serde_json::to_vec(
        &serde_json::json!({ "iat": iat, "exp": exp, "iss": iss }),
    )?);
    let signed = format!("{head}.{claims}");
    let sig = SigningKey::<Sha256>::new(key).sign(signed.as_bytes());
    Ok(format!(
        "{signed}.{}",
        URL_SAFE_NO_PAD.encode(sig.to_bytes())
    ))
}

/// A newly generated app private key.
pub struct GeneratedKey {
    /// PKCS#1 PEM (`BEGIN RSA PRIVATE KEY`), shown once like GitHub's
    /// download.
    pub private_pem: String,
    /// SubjectPublicKeyInfo PEM (stored).
    pub public_pem: String,
    /// `SHA256:<base64>` of the DER public key.
    pub fingerprint: String,
}

/// Generate a 2048-bit RSA key pair (blocking: run on the blocking pool).
pub fn generate_key() -> anyhow::Result<GeneratedKey> {
    let mut rng = rsa::rand_core::OsRng;
    let key = RsaPrivateKey::new(&mut rng, 2048)?;
    let public = RsaPublicKey::from(&key);
    let der = public.to_public_key_der()?;
    Ok(GeneratedKey {
        private_pem: key.to_pkcs1_pem(LineEnding::LF)?.to_string(),
        public_pem: public.to_public_key_pem(LineEnding::LF)?,
        fingerprint: format!("SHA256:{}", STANDARD.encode(Sha256::digest(der.as_bytes()))),
    })
}

// ---------------------------------------------------------------------------
// Route restrictions
// ---------------------------------------------------------------------------

/// REST routes (relative to `/api/v3`) an app JWT may call.
pub fn jwt_route(method: &Method, path: &str) -> bool {
    let path = path.strip_prefix("/api/v3").unwrap_or(path);
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let get = matches!(*method, Method::GET | Method::HEAD);
    match segs.as_slice() {
        ["app"] | ["apps", _] => get,
        ["app", "installations", ..] | ["app", "hook", ..] | ["app", "installation-requests"] => {
            true
        }
        ["orgs", _, "installation"]
        | ["users", _, "installation"]
        | ["repos", _, _, "installation"] => get,
        // Unauthenticated-style metadata.
        [] | ["meta"] | ["rate_limit"] => get,
        _ => false,
    }
}

/// Middleware (mounted for every route by bgh-server): app JWTs may only
/// call the app endpoints ([`jwt_route`]); everything else answers 403
/// "Resource not accessible by integration". Installation tokens are
/// limited by `token_permissions::middleware` and [`effective_cap`].
pub async fn middleware(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    let bearer_jwt = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split_once(' '))
        .is_some_and(|(_, t)| looks_like_jwt(t.trim()));
    if !bearer_jwt {
        return next.run(req).await;
    }
    let path = req.uri().path().to_string();
    let Ok(Some(auth)) = crate::auth::resolve_request(&state, &mut req).await else {
        return next.run(req).await;
    };
    if jwt_app_id(&auth).is_none() {
        return next.run(req).await;
    }
    let rest_api = path == "/api/v3" || path.starts_with("/api/v3/");
    if rest_api && jwt_route(req.method(), &path) {
        return next.run(req).await;
    }
    ApiError::forbidden(NOT_ACCESSIBLE).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jwt_routes() {
        assert!(jwt_route(&Method::GET, "/api/v3/app"));
        assert!(jwt_route(&Method::GET, "/app/installations"));
        assert!(jwt_route(
            &Method::POST,
            "/api/v3/app/installations/1/access_tokens"
        ));
        assert!(jwt_route(&Method::GET, "/api/v3/repos/o/r/installation"));
        assert!(jwt_route(&Method::GET, "/api/v3/apps/my-app"));
        assert!(!jwt_route(&Method::GET, "/api/v3/repos/o/r"));
        assert!(!jwt_route(&Method::GET, "/api/v3/user"));
        assert!(!jwt_route(&Method::PATCH, "/api/v3/app"));
    }

    #[test]
    fn sign_and_shape() {
        let key = generate_key().unwrap();
        assert!(
            key.private_pem
                .starts_with("-----BEGIN RSA PRIVATE KEY-----")
        );
        assert!(key.fingerprint.starts_with("SHA256:"));
        let jwt = sign_jwt(&key.private_pem, &serde_json::json!(1), 0, 600).unwrap();
        assert!(looks_like_jwt(&jwt));
        let (signed, sig) = jwt.rsplit_once('.').unwrap();
        let public = RsaPublicKey::from_public_key_pem(&key.public_pem).unwrap();
        let sig = Signature::try_from(URL_SAFE_NO_PAD.decode(sig).unwrap().as_slice()).unwrap();
        assert!(
            VerifyingKey::<Sha256>::new(public)
                .verify(signed.as_bytes(), &sig)
                .is_ok()
        );
    }
}
