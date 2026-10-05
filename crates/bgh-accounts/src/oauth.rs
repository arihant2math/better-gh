//! OAuth apps: registration (web client), the authorization code flow
//! (with PKCE), the device flow (`gh auth login`), and GitHub's
//! `/applications/{client_id}/token|grant` API.
//!
//! Browser endpoints (absolute paths, like github.com):
//! * `GET /login/oauth/authorize` → consent page (or immediate redirect when
//!   already granted); `POST /login/oauth/authorize` (consent form)
//! * `POST /login/oauth/access_token` (code / device_code grants)
//! * `POST /login/device/code`, `GET|POST /login/device` (code entry page)
//!
//! Web-client JSON: `/_bgh/applications[/{id}[/client_secret]]`,
//! `/_bgh/authorizations[/{id}]`, `GET|POST /_bgh/oauth/authorize`,
//! `GET /_bgh/device/{user_code}`, `POST /_bgh/device`.
//!
//! Ephemeral state (authorization codes, device codes, consent nonces) lives
//! in Redis. Access tokens are `bgho_…` rows in `access_tokens` (kind
//! `oauth`); a user's grant per app is remembered in `oauth_authorizations`.
//! Token-endpoint errors are returned with status 200 and an `error` field,
//! like GitHub.

use std::collections::HashMap;

use axum::body::Bytes;
use axum::extract::{OriginalUri, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use bgh_core::audit;
use bgh_core::crypto;
use bgh_core::models::api::SimpleUser;
use bgh_core::prelude::*;
use bgh_core::time::ts;
use chrono::{DateTime, Utc};
use rand::Rng;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::FromRow;

use crate::tokens::KNOWN_SCOPES;
use crate::util;

const CODE_TTL_SECS: u64 = 600;
const DEVICE_TTL_SECS: u64 = 900;
const DEVICE_INTERVAL_SECS: i64 = 5;
const CONSENT_TTL_SECS: u64 = 600;

// ---------------------------------------------------------------------------
// Apps
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, FromRow)]
pub struct OauthApp {
    pub id: i64,
    pub owner_id: Option<i64>,
    pub name: String,
    pub description: Option<String>,
    pub homepage_url: String,
    pub callback_url: String,
    pub client_id: String,
    pub client_secret_hash: Option<String>,
    pub client_secret_last_eight: Option<String>,
    pub device_flow_enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl OauthApp {
    pub const COLUMNS: &'static str = "id, owner_id, name, description, homepage_url, callback_url, \
        client_id, client_secret_hash, client_secret_last_eight, device_flow_enabled, created_at, \
        updated_at";

    pub async fn by_client_id(
        db: impl sqlx::PgExecutor<'_>,
        client_id: &str,
    ) -> Result<Option<Self>, sqlx::Error> {
        sqlx::query_as(&format!(
            "SELECT {} FROM oauth_apps WHERE client_id = $1",
            Self::COLUMNS
        ))
        .bind(client_id)
        .fetch_optional(db)
        .await
    }

    /// Whether `secret` is this app's client secret (public clients accept
    /// any secret, including none).
    pub fn check_secret(&self, secret: Option<&str>) -> bool {
        match &self.client_secret_hash {
            None => true,
            Some(hash) => {
                secret.is_some_and(|s| crypto::constant_time_eq(&crypto::sha256_hex(s), hash))
            }
        }
    }
}

#[derive(Debug, Serialize)]
pub struct AppJson {
    pub id: i64,
    pub name: String,
    pub description: Option<String>,
    pub homepage_url: String,
    pub callback_url: String,
    pub client_id: String,
    pub client_secret_last_eight: Option<String>,
    pub device_flow_enabled: bool,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    /// Only in create / regenerate responses.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
}

impl AppJson {
    fn new(a: &OauthApp, secret: Option<String>) -> Self {
        Self {
            id: a.id,
            name: a.name.clone(),
            description: a.description.clone(),
            homepage_url: a.homepage_url.clone(),
            callback_url: a.callback_url.clone(),
            client_id: a.client_id.clone(),
            client_secret_last_eight: a.client_secret_last_eight.clone(),
            device_flow_enabled: a.device_flow_enabled,
            created_at: a.created_at.into(),
            updated_at: a.updated_at.into(),
            client_secret: secret,
        }
    }
}

fn new_client_id() -> String {
    let mut b = [0u8; 10];
    rand::rng().fill(&mut b);
    format!("Iv1.{}", hex::encode(b))
}

fn new_client_secret() -> String {
    let mut b = [0u8; 20];
    rand::rng().fill(&mut b);
    hex::encode(b)
}

fn valid_url(u: &str) -> bool {
    url::Url::parse(u)
        .is_ok_and(|u| matches!(u.scheme(), "http" | "https") || !u.cannot_be_a_base())
}

#[derive(Debug, Deserialize)]
pub struct AppBody {
    pub name: Option<String>,
    pub description: Option<String>,
    pub homepage_url: Option<String>,
    pub callback_url: Option<String>,
    pub device_flow_enabled: Option<bool>,
}

fn validate_app(body: &AppBody, creating: bool) -> ApiResult<()> {
    let mut errors = Vec::new();
    match body.name.as_deref().map(str::trim) {
        Some("") => errors.push(FieldError::invalid("OauthApplication", "name")),
        None if creating => errors.push(FieldError::missing_field("OauthApplication", "name")),
        _ => {}
    }
    for (field, v) in [
        ("homepage_url", &body.homepage_url),
        ("callback_url", &body.callback_url),
    ] {
        match v.as_deref() {
            Some(u) if !u.is_empty() && !valid_url(u) => {
                errors.push(FieldError::invalid("OauthApplication", field))
            }
            None if creating && field == "callback_url" => {
                errors.push(FieldError::missing_field("OauthApplication", field))
            }
            _ => {}
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(ApiError::validation(errors))
    }
}

async fn my_app(state: &AppState, user_id: i64, id: i64) -> ApiResult<OauthApp> {
    sqlx::query_as(&format!(
        "SELECT {} FROM oauth_apps WHERE id = $1 AND owner_id = $2",
        OauthApp::COLUMNS
    ))
    .bind(id)
    .bind(user_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)
}

/// `GET /_bgh/applications` → the caller's OAuth apps.
pub async fn list_apps(
    State(state): State<AppState>,
    auth: RequireUser,
) -> ApiResult<Json<Vec<AppJson>>> {
    util::require_session(&auth)?;
    let rows: Vec<OauthApp> = sqlx::query_as(&format!(
        "SELECT {} FROM oauth_apps WHERE owner_id = $1 ORDER BY id",
        OauthApp::COLUMNS
    ))
    .bind(auth.user.id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(rows.iter().map(|a| AppJson::new(a, None)).collect()))
}

/// `POST /_bgh/applications` → 201 with the client secret (shown once).
pub async fn create_app(
    State(state): State<AppState>,
    auth: RequireUser,
    Json(body): Json<AppBody>,
) -> ApiResult<(StatusCode, Json<AppJson>)> {
    util::require_session(&auth)?;
    validate_app(&body, true)?;
    let secret = new_client_secret();
    let mut tx = Tx::begin(&state).await?;
    let app: OauthApp = sqlx::query_as(&format!(
        "INSERT INTO oauth_apps (owner_id, name, description, homepage_url, callback_url, client_id,
                                 client_secret_hash, client_secret_last_eight, device_flow_enabled)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING {}",
        OauthApp::COLUMNS
    ))
    .bind(auth.user.id)
    .bind(body.name.as_deref().unwrap_or_default().trim())
    .bind(util::non_empty(body.description))
    .bind(body.homepage_url.unwrap_or_default())
    .bind(body.callback_url.unwrap_or_default())
    .bind(new_client_id())
    .bind(crypto::sha256_hex(&secret))
    .bind(&secret[secret.len() - 8..])
    .bind(body.device_flow_enabled.unwrap_or(false))
    .fetch_one(&mut *tx)
    .await?;
    audit::log(
        &mut *tx,
        Some(&auth.user),
        "oauth_application.create",
        audit::Target::User(auth.user.id),
        json!({ "client_id": app.client_id }),
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(AppJson::new(&app, Some(secret)))))
}

/// `GET /_bgh/applications/{id}`
pub async fn get_app(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(id): Path<i64>,
) -> ApiResult<Json<AppJson>> {
    util::require_session(&auth)?;
    Ok(Json(AppJson::new(
        &my_app(&state, auth.user.id, id).await?,
        None,
    )))
}

/// `PATCH /_bgh/applications/{id}`
pub async fn update_app(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(id): Path<i64>,
    Json(body): Json<AppBody>,
) -> ApiResult<Json<AppJson>> {
    util::require_session(&auth)?;
    my_app(&state, auth.user.id, id).await?;
    validate_app(&body, false)?;
    let app: OauthApp = sqlx::query_as(&format!(
        "UPDATE oauth_apps SET name = coalesce($3, name), description = coalesce($4, description),
                homepage_url = coalesce($5, homepage_url), callback_url = coalesce($6, callback_url),
                device_flow_enabled = coalesce($7, device_flow_enabled), updated_at = now()
          WHERE id = $1 AND owner_id = $2 RETURNING {}",
        OauthApp::COLUMNS
    ))
    .bind(id)
    .bind(auth.user.id)
    .bind(body.name.map(|n| n.trim().to_string()))
    .bind(body.description)
    .bind(body.homepage_url)
    .bind(body.callback_url)
    .bind(body.device_flow_enabled)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(AppJson::new(&app, None)))
}

/// `DELETE /_bgh/applications/{id}` → 204 (its tokens are revoked).
pub async fn delete_app(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    util::require_session(&auth)?;
    let deleted = sqlx::query("DELETE FROM oauth_apps WHERE id = $1 AND owner_id = $2")
        .bind(id)
        .bind(auth.user.id)
        .execute(&state.db)
        .await?
        .rows_affected();
    if deleted == 0 {
        return Err(ApiError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /_bgh/applications/{id}/client_secret` → app with a new secret.
pub async fn regenerate_secret(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(id): Path<i64>,
) -> ApiResult<Json<AppJson>> {
    util::require_session(&auth)?;
    my_app(&state, auth.user.id, id).await?;
    let secret = new_client_secret();
    let app: OauthApp = sqlx::query_as(&format!(
        "UPDATE oauth_apps SET client_secret_hash = $2, client_secret_last_eight = $3, updated_at = now()
          WHERE id = $1 RETURNING {}",
        OauthApp::COLUMNS
    ))
    .bind(id)
    .bind(crypto::sha256_hex(&secret))
    .bind(&secret[secret.len() - 8..])
    .fetch_one(&state.db)
    .await?;
    Ok(Json(AppJson::new(&app, Some(secret))))
}

// ---------------------------------------------------------------------------
// Grants
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct GrantJson {
    pub id: i64,
    pub app: GrantApp,
    pub scopes: Vec<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Serialize)]
pub struct GrantApp {
    pub client_id: String,
    pub name: String,
    pub url: String,
}

/// (id, scopes, created, updated, client_id, name, homepage).
type GrantRow = (
    i64,
    Vec<String>,
    DateTime<Utc>,
    DateTime<Utc>,
    String,
    String,
    String,
);

/// `GET /_bgh/authorizations` → apps the caller has authorized.
pub async fn list_grants(
    State(state): State<AppState>,
    auth: RequireUser,
) -> ApiResult<Json<Vec<GrantJson>>> {
    util::require_session(&auth)?;
    let rows: Vec<GrantRow> = sqlx::query_as(
        "SELECT g.id, g.scopes, g.created_at, g.updated_at, a.client_id, a.name, a.homepage_url
           FROM oauth_authorizations g JOIN oauth_apps a ON a.id = g.app_id
          WHERE g.user_id = $1 ORDER BY g.updated_at DESC, g.id",
    )
    .bind(auth.user.id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|(id, scopes, c, u, client_id, name, url)| GrantJson {
                id,
                app: GrantApp {
                    client_id,
                    name,
                    url,
                },
                scopes,
                created_at: c.into(),
                updated_at: u.into(),
            })
            .collect(),
    ))
}

/// Delete a grant and every token of that app for the user.
async fn revoke_grant(state: &AppState, user_id: i64, app_id: i64) -> ApiResult<()> {
    let mut tx = Tx::begin(state).await?;
    sqlx::query("DELETE FROM oauth_authorizations WHERE user_id = $1 AND app_id = $2")
        .bind(user_id)
        .bind(app_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM access_tokens WHERE user_id = $1 AND oauth_app_id = $2")
        .bind(user_id)
        .bind(app_id)
        .execute(&mut *tx)
        .await?;
    audit::log(
        &mut *tx,
        None,
        "oauth_authorization.destroy",
        audit::Target::User(user_id),
        json!({ "app_id": app_id }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// `DELETE /_bgh/authorizations/{id}` → 204, revokes the app's tokens.
pub async fn delete_grant(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    util::require_session(&auth)?;
    let app_id: i64 = sqlx::query_scalar(
        "SELECT app_id FROM oauth_authorizations WHERE id = $1 AND user_id = $2",
    )
    .bind(id)
    .bind(auth.user.id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)?;
    revoke_grant(&state, auth.user.id, app_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Scopes, tokens
// ---------------------------------------------------------------------------

/// Parse a space/comma separated scope list, dropping unknown scopes.
pub fn parse_scopes(s: &str, site_admin: bool) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for sc in s
        .split([' ', ',', '+'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if KNOWN_SCOPES.contains(&sc)
            && (sc != "site_admin" || site_admin)
            && !out.iter().any(|o| o == sc)
        {
            out.push(sc.to_string());
        }
    }
    out
}

/// Issue an OAuth access token for `user` and remember the grant.
pub async fn issue_token(
    state: &AppState,
    user_id: i64,
    app: &OauthApp,
    scopes: &[String],
) -> ApiResult<(i64, String)> {
    let token = crypto::new_oauth_token();
    let mut tx = Tx::begin(state).await?;
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO access_tokens (user_id, kind, name, token_hash, token_last_eight, scopes, oauth_app_id)
         VALUES ($1, 'oauth', $2, $3, $4, $5, $6) RETURNING id",
    )
    .bind(user_id)
    .bind(&app.name)
    .bind(crypto::sha256_hex(&token))
    .bind(&token[token.len() - 8..])
    .bind(scopes)
    .bind(app.id)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO oauth_authorizations (user_id, app_id, scopes) VALUES ($1, $2, $3)
         ON CONFLICT (user_id, app_id) DO UPDATE
           SET scopes = ARRAY(SELECT DISTINCT unnest(oauth_authorizations.scopes || EXCLUDED.scopes)),
               updated_at = now()",
    )
    .bind(user_id)
    .bind(app.id)
    .bind(scopes)
    .execute(&mut *tx)
    .await?;
    audit::log(
        &mut *tx,
        None,
        "oauth_access.create",
        audit::Target::Token(id),
        json!({ "app": app.client_id, "scopes": scopes, "user_id": user_id }),
    )
    .await?;
    tx.commit().await?;
    Ok((id, token))
}

async fn granted_scopes(
    state: &AppState,
    user_id: i64,
    app_id: i64,
) -> ApiResult<Option<Vec<String>>> {
    Ok(sqlx::query_scalar(
        "SELECT scopes FROM oauth_authorizations WHERE user_id = $1 AND app_id = $2",
    )
    .bind(user_id)
    .bind(app_id)
    .fetch_optional(&state.db)
    .await?)
}

// ---------------------------------------------------------------------------
// Request / response helpers
// ---------------------------------------------------------------------------

/// Merge query-string and body parameters (form-urlencoded or JSON).
fn params(uri: &axum::http::Uri, headers: &HeaderMap, body: &[u8]) -> HashMap<String, String> {
    let mut out: HashMap<String, String> = uri
        .query()
        .map(|q| {
            url::form_urlencoded::parse(q.as_bytes())
                .into_owned()
                .collect()
        })
        .unwrap_or_default();
    let is_json = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|c| c.contains("json"));
    let trimmed = body.trim_ascii();
    if is_json || trimmed.first() == Some(&b'{') {
        if let Ok(Value::Object(map)) = serde_json::from_slice::<Value>(trimmed) {
            for (k, v) in map {
                let s = match v {
                    Value::String(s) => s,
                    Value::Null => continue,
                    other => other.to_string(),
                };
                out.insert(k, s);
            }
        }
    } else {
        out.extend(url::form_urlencoded::parse(trimmed).into_owned());
    }
    out
}

/// GitHub answers token endpoints as form-urlencoded unless JSON is asked
/// for.
fn token_response(
    headers: &HeaderMap,
    status: StatusCode,
    fields: Vec<(&str, String)>,
) -> Response {
    let wants_json = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|a| a.contains("json"));
    if wants_json {
        let mut map = serde_json::Map::new();
        for (k, v) in fields {
            let value = if k == "expires_in" || k == "interval" {
                v.parse::<i64>()
                    .map(Value::from)
                    .unwrap_or(Value::String(v))
            } else {
                Value::String(v)
            };
            map.insert(k.to_string(), value);
        }
        return (status, axum::Json(Value::Object(map))).into_response();
    }
    let body = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(fields.iter().map(|(k, v)| (*k, v.as_str())))
        .finish();
    (
        status,
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/x-www-form-urlencoded; charset=utf-8"),
        )],
        body,
    )
        .into_response()
}

fn oauth_error(headers: &HeaderMap, error: &str, description: &str) -> Response {
    token_response(
        headers,
        StatusCode::OK,
        vec![
            ("error", error.to_string()),
            ("error_description", description.to_string()),
            ("error_uri", "https://docs.github.com/apps/managing-oauth-apps/troubleshooting-oauth-app-access-token-request-errors".into()),
        ],
    )
}

fn redis_key(state: &AppState, kind: &str, secret: &str) -> String {
    state.redis_key(&format!("{kind}:{}", crypto::sha256_hex(secret)))
}

async fn redis_set_json(
    state: &AppState,
    key: &str,
    v: &impl Serialize,
    ttl: u64,
) -> ApiResult<()> {
    let mut redis = state.redis.clone();
    let _: () = redis.set_ex(key, serde_json::to_string(v)?, ttl).await?;
    Ok(())
}

async fn redis_get_json<T: serde::de::DeserializeOwned>(
    state: &AppState,
    key: &str,
) -> ApiResult<Option<T>> {
    let mut redis = state.redis.clone();
    let v: Option<String> = redis.get(key).await?;
    Ok(v.and_then(|s| serde_json::from_str(&s).ok()))
}

async fn redis_take_json<T: serde::de::DeserializeOwned>(
    state: &AppState,
    key: &str,
) -> ApiResult<Option<T>> {
    let mut redis = state.redis.clone();
    let v: Option<String> = redis::cmd("GETDEL")
        .arg(key)
        .query_async(&mut redis)
        .await?;
    Ok(v.and_then(|s| serde_json::from_str(&s).ok()))
}

// ---------------------------------------------------------------------------
// Authorization code flow
// ---------------------------------------------------------------------------

/// A validated authorization request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthzRequest {
    pub app_id: i64,
    pub client_id: String,
    pub redirect_uri: String,
    pub scopes: Vec<String>,
    pub state: Option<String>,
    pub code_challenge: Option<String>,
    pub code_challenge_method: Option<String>,
}

/// Stored authorization code.
#[derive(Debug, Serialize, Deserialize)]
struct CodeGrant {
    app_id: i64,
    user_id: i64,
    scopes: Vec<String>,
    redirect_uri: String,
    code_challenge: Option<String>,
    code_challenge_method: Option<String>,
}

fn is_loopback(host: Option<&str>) -> bool {
    matches!(host, Some("127.0.0.1" | "localhost" | "[::1]" | "::1"))
}

/// GitHub's redirect URI rules: same scheme, host and port as the callback
/// URL (any port for loopback callbacks) and a path equal to or below the
/// callback path.
pub fn redirect_allowed(callback: &str, redirect: &str) -> bool {
    let (Ok(cb), Ok(r)) = (url::Url::parse(callback), url::Url::parse(redirect)) else {
        return false;
    };
    if cb.scheme() != r.scheme() || cb.host_str() != r.host_str() {
        return false;
    }
    if !is_loopback(cb.host_str()) && cb.port_or_known_default() != r.port_or_known_default() {
        return false;
    }
    let base = cb.path().trim_end_matches('/');
    let path = r.path();
    path == cb.path() || path.trim_end_matches('/') == base || path.starts_with(&format!("{base}/"))
}

enum AuthzError {
    /// Shown to the user (no redirect possible).
    Page(String),
    /// Redirected back to the client.
    Redirect(String),
}

async fn validate_authorize(
    state: &AppState,
    p: &HashMap<String, String>,
    site_admin: bool,
) -> ApiResult<Result<(AuthzRequest, OauthApp), AuthzError>> {
    let Some(client_id) = p.get("client_id").filter(|c| !c.is_empty()) else {
        return Ok(Err(AuthzError::Page("Missing client_id.".into())));
    };
    let Some(app) = OauthApp::by_client_id(&state.db, client_id).await? else {
        return Ok(Err(AuthzError::Page("Unknown application.".into())));
    };
    let redirect_uri = match p.get("redirect_uri").filter(|r| !r.is_empty()) {
        Some(r) if redirect_allowed(&app.callback_url, r) => r.clone(),
        Some(_) => {
            return Ok(Err(AuthzError::Page(
                "The redirect_uri is not associated with this application.".into(),
            )));
        }
        None if !app.callback_url.is_empty() => app.callback_url.clone(),
        None => return Ok(Err(AuthzError::Page("Missing redirect_uri.".into()))),
    };
    let st = p.get("state").cloned();
    let method = p.get("code_challenge_method").cloned();
    let challenge = p.get("code_challenge").cloned().filter(|c| !c.is_empty());
    if challenge.is_some() && !matches!(method.as_deref(), None | Some("S256") | Some("plain")) {
        return Ok(Err(AuthzError::Redirect(error_redirect(
            &redirect_uri,
            "invalid_request",
            "Unsupported code_challenge_method",
            st.as_deref(),
        ))));
    }
    let scopes = parse_scopes(p.get("scope").map(String::as_str).unwrap_or(""), site_admin);
    Ok(Ok((
        AuthzRequest {
            app_id: app.id,
            client_id: app.client_id.clone(),
            redirect_uri,
            scopes,
            state: st,
            code_challenge: challenge,
            code_challenge_method: method,
        },
        app,
    )))
}

fn with_query(base: &str, pairs: &[(&str, &str)]) -> String {
    let mut u = match url::Url::parse(base) {
        Ok(u) => u,
        Err(_) => return base.to_string(),
    };
    {
        let mut q = u.query_pairs_mut();
        for (k, v) in pairs {
            q.append_pair(k, v);
        }
    }
    u.to_string()
}

fn error_redirect(redirect_uri: &str, error: &str, description: &str, st: Option<&str>) -> String {
    let mut pairs = vec![("error", error), ("error_description", description)];
    if let Some(s) = st {
        pairs.push(("state", s));
    }
    with_query(redirect_uri, &pairs)
}

/// Create an authorization code and the client redirect URL.
async fn approve(state: &AppState, user_id: i64, req: &AuthzRequest) -> ApiResult<String> {
    let code = hex::encode(rand::rng().random::<[u8; 10]>());
    redis_set_json(
        state,
        &redis_key(state, "oauth_code", &code),
        &CodeGrant {
            app_id: req.app_id,
            user_id,
            scopes: req.scopes.clone(),
            redirect_uri: req.redirect_uri.clone(),
            code_challenge: req.code_challenge.clone(),
            code_challenge_method: req.code_challenge_method.clone(),
        },
        CODE_TTL_SECS,
    )
    .await?;
    let mut pairs = vec![("code", code.as_str())];
    if let Some(s) = &req.state {
        pairs.push(("state", s));
    }
    Ok(with_query(&req.redirect_uri, &pairs))
}

fn login_redirect(original: &axum::http::Uri) -> Response {
    let return_to = original.to_string();
    let target = with_query("http://x/login", &[("return_to", &return_to)]);
    Redirect::to(target.trim_start_matches("http://x")).into_response()
}

async fn new_consent(state: &AppState, user_id: i64, req: &AuthzRequest) -> ApiResult<String> {
    let nonce = crypto::random_token(32);
    redis_set_json(
        state,
        &redis_key(state, "oauth_consent", &nonce),
        &json!({ "user_id": user_id, "req": req }),
        CONSENT_TTL_SECS,
    )
    .await?;
    Ok(nonce)
}

/// `GET /login/oauth/authorize?client_id&redirect_uri&scope&state[&code_challenge]`
pub async fn authorize_page(
    State(state): State<AppState>,
    auth: MaybeUser,
    OriginalUri(original): OriginalUri,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let p = params(&original, &headers, &[]);
    let site_admin = auth.as_ref().is_some_and(|a| a.user.site_admin);
    let (req, app) = match validate_authorize(&state, &p, site_admin).await? {
        Ok(v) => v,
        Err(AuthzError::Page(msg)) => return Ok(error_page(&state, StatusCode::BAD_REQUEST, &msg)),
        Err(AuthzError::Redirect(to)) => return Ok(Redirect::to(&to).into_response()),
    };
    let Some(auth) = auth.0.filter(|a| a.is_session()) else {
        return Ok(login_redirect(&original));
    };
    // Already granted these scopes: skip the consent screen.
    if let Some(granted) = granted_scopes(&state, auth.user.id, app.id).await?
        && req.scopes.iter().all(|s| granted.contains(s))
    {
        let to = approve(&state, auth.user.id, &req).await?;
        return Ok(Redirect::to(&to).into_response());
    }
    let nonce = new_consent(&state, auth.user.id, &req).await?;
    Ok(consent_page(&state, &auth.user, &app, &req.scopes, &nonce))
}

/// `POST /login/oauth/authorize` (consent form: `consent`, `authorize=1|0`).
pub async fn authorize_submit(
    State(state): State<AppState>,
    auth: RequireUser,
    OriginalUri(original): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    util::require_session(&auth)?;
    let p = params(&original, &headers, &body);
    let nonce = p.get("consent").cloned().unwrap_or_default();
    let stored: Option<Value> =
        redis_take_json(&state, &redis_key(&state, "oauth_consent", &nonce)).await?;
    let Some(stored) = stored.filter(|v| v["user_id"].as_i64() == Some(auth.user.id)) else {
        return Ok(error_page(
            &state,
            StatusCode::BAD_REQUEST,
            "This authorization request expired. Please try again.",
        ));
    };
    let req: AuthzRequest = serde_json::from_value(stored["req"].clone())?;
    let to = if p.get("authorize").map(String::as_str) == Some("1") {
        approve(&state, auth.user.id, &req).await?
    } else {
        error_redirect(
            &req.redirect_uri,
            "access_denied",
            "The user has denied your application access.",
            req.state.as_deref(),
        )
    };
    Ok(Redirect::to(&to).into_response())
}

#[derive(Debug, Serialize)]
pub struct AuthorizeInfo {
    pub app: AppPublic,
    pub scopes: Vec<String>,
    pub redirect_uri: String,
    /// Consent token for `POST /_bgh/oauth/authorize`.
    pub consent: String,
    pub already_authorized: bool,
}

#[derive(Debug, Serialize)]
pub struct AppPublic {
    pub name: String,
    pub description: Option<String>,
    pub homepage_url: String,
    pub client_id: String,
    pub owner: Option<SimpleUser>,
}

async fn app_public(state: &AppState, app: &OauthApp) -> ApiResult<AppPublic> {
    let owner = match app.owner_id {
        Some(id) => db::User::find(&state.db, id)
            .await?
            .map(|u| SimpleUser::new(&state.urls, &u)),
        None => None,
    };
    Ok(AppPublic {
        name: app.name.clone(),
        description: app.description.clone(),
        homepage_url: app.homepage_url.clone(),
        client_id: app.client_id.clone(),
        owner,
    })
}

/// `GET /_bgh/oauth/authorize?<authorize params>` (web client consent
/// screen data).
pub async fn authorize_info(
    State(state): State<AppState>,
    auth: RequireUser,
    OriginalUri(original): OriginalUri,
    headers: HeaderMap,
) -> ApiResult<Json<AuthorizeInfo>> {
    util::require_session(&auth)?;
    let p = params(&original, &headers, &[]);
    let (req, app) = match validate_authorize(&state, &p, auth.user.site_admin).await? {
        Ok(v) => v,
        Err(AuthzError::Page(msg)) | Err(AuthzError::Redirect(msg)) => {
            return Err(ApiError::unprocessable(msg));
        }
    };
    let already = granted_scopes(&state, auth.user.id, app.id)
        .await?
        .is_some_and(|g| req.scopes.iter().all(|s| g.contains(s)));
    let consent = new_consent(&state, auth.user.id, &req).await?;
    Ok(Json(AuthorizeInfo {
        app: app_public(&state, &app).await?,
        scopes: req.scopes.clone(),
        redirect_uri: req.redirect_uri.clone(),
        consent,
        already_authorized: already,
    }))
}

#[derive(Debug, Deserialize)]
pub struct ConsentBody {
    #[serde(default)]
    pub consent: String,
    #[serde(default)]
    pub authorize: bool,
}

/// `POST /_bgh/oauth/authorize {consent, authorize}` → `{redirect_url}`.
pub async fn authorize_json(
    State(state): State<AppState>,
    auth: RequireUser,
    Json(body): Json<ConsentBody>,
) -> ApiResult<Json<Value>> {
    util::require_session(&auth)?;
    let stored: Option<Value> =
        redis_take_json(&state, &redis_key(&state, "oauth_consent", &body.consent)).await?;
    let stored = stored
        .filter(|v| v["user_id"].as_i64() == Some(auth.user.id))
        .ok_or(ApiError::NotFound)?;
    let req: AuthzRequest = serde_json::from_value(stored["req"].clone())?;
    let to = if body.authorize {
        approve(&state, auth.user.id, &req).await?
    } else {
        error_redirect(
            &req.redirect_uri,
            "access_denied",
            "The user has denied your application access.",
            req.state.as_deref(),
        )
    };
    Ok(Json(json!({ "redirect_url": to })))
}

fn pkce_ok(challenge: &str, method: Option<&str>, verifier: Option<&str>) -> bool {
    let Some(v) = verifier else {
        return false;
    };
    match method.unwrap_or("plain") {
        "S256" => URL_SAFE_NO_PAD.encode(Sha256::digest(v.as_bytes())) == challenge,
        _ => crypto::constant_time_eq(v, challenge),
    }
}

/// `POST /login/oauth/access_token` (grant types: authorization code,
/// `urn:ietf:params:oauth:grant-type:device_code`).
pub async fn access_token(
    State(state): State<AppState>,
    OriginalUri(original): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    let mut p = params(&original, &headers, &body);
    // Client credentials may also come as HTTP Basic.
    if let Some((id, secret)) = basic_credentials(&headers) {
        p.entry("client_id".into()).or_insert(id);
        p.entry("client_secret".into()).or_insert(secret);
    }
    let client_id = p.get("client_id").cloned().unwrap_or_default();
    let Some(app) = OauthApp::by_client_id(&state.db, &client_id).await? else {
        return Ok(oauth_error(
            &headers,
            "incorrect_client_credentials",
            "The client_id and/or client_secret passed are incorrect.",
        ));
    };
    let grant_type = p
        .get("grant_type")
        .map(String::as_str)
        .unwrap_or("authorization_code");
    match grant_type {
        "urn:ietf:params:oauth:grant-type:device_code" => {
            device_token(&state, &headers, &app, &p).await
        }
        "authorization_code" => {
            if !app.check_secret(p.get("client_secret").map(String::as_str)) {
                return Ok(oauth_error(
                    &headers,
                    "incorrect_client_credentials",
                    "The client_id and/or client_secret passed are incorrect.",
                ));
            }
            let code = p.get("code").cloned().unwrap_or_default();
            let grant: Option<CodeGrant> =
                redis_take_json(&state, &redis_key(&state, "oauth_code", &code)).await?;
            let Some(grant) = grant.filter(|g| g.app_id == app.id) else {
                return Ok(oauth_error(
                    &headers,
                    "bad_verification_code",
                    "The code passed is incorrect or expired.",
                ));
            };
            if let Some(r) = p.get("redirect_uri").filter(|r| !r.is_empty())
                && *r != grant.redirect_uri
            {
                return Ok(oauth_error(
                    &headers,
                    "redirect_uri_mismatch",
                    "The redirect_uri MUST match the registered callback URL for this application.",
                ));
            }
            if let Some(ch) = &grant.code_challenge
                && !pkce_ok(
                    ch,
                    grant.code_challenge_method.as_deref(),
                    p.get("code_verifier").map(String::as_str),
                )
            {
                return Ok(oauth_error(
                    &headers,
                    "bad_verification_code",
                    "The code_verifier does not match the code_challenge.",
                ));
            }
            let (_, token) = issue_token(&state, grant.user_id, &app, &grant.scopes).await?;
            Ok(token_response(
                &headers,
                StatusCode::OK,
                vec![
                    ("access_token", token),
                    ("token_type", "bearer".into()),
                    ("scope", grant.scopes.join(",")),
                ],
            ))
        }
        _ => Ok(oauth_error(
            &headers,
            "unsupported_grant_type",
            "The grant type is not supported.",
        )),
    }
}

fn basic_credentials(headers: &HeaderMap) -> Option<(String, String)> {
    let v = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, rest) = v.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("basic") {
        return None;
    }
    let decoded = String::from_utf8(STANDARD.decode(rest.trim()).ok()?).ok()?;
    let (id, secret) = decoded.split_once(':')?;
    Some((id.to_string(), secret.to_string()))
}

// ---------------------------------------------------------------------------
// Device flow
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DeviceGrant {
    app_id: i64,
    scopes: Vec<String>,
    user_code: String,
    /// `pending` | `approved` | `denied`
    status: String,
    user_id: Option<i64>,
    interval: i64,
    last_poll: i64,
    expires_at: i64,
}

fn new_user_code() -> String {
    // No vowels/ambiguous characters (GitHub style `WDJB-MJHT`).
    const ALPHABET: &[u8] = b"BCDFGHJKLMNPQRSTVWXZ";
    let mut rng = rand::rng();
    let mut s: String = (0..8)
        .map(|_| ALPHABET[rng.random_range(0..ALPHABET.len())] as char)
        .collect();
    s.insert(4, '-');
    s
}

fn normalize_user_code(c: &str) -> String {
    let c: String = c
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_uppercase();
    if c.len() == 8 {
        format!("{}-{}", &c[..4], &c[4..])
    } else {
        c
    }
}

/// `POST /login/device/code {client_id, scope}` → device_code, user_code,
/// verification_uri, expires_in, interval.
pub async fn device_code(
    State(state): State<AppState>,
    OriginalUri(original): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    let p = params(&original, &headers, &body);
    let client_id = p.get("client_id").cloned().unwrap_or_default();
    let Some(app) = OauthApp::by_client_id(&state.db, &client_id).await? else {
        return Ok(token_response(
            &headers,
            StatusCode::NOT_FOUND,
            vec![("error", "Not Found".into())],
        ));
    };
    if !app.device_flow_enabled {
        return Ok(token_response(
            &headers,
            StatusCode::BAD_REQUEST,
            vec![
                ("error", "device_flow_disabled".into()),
                (
                    "error_description",
                    "Device flow must be explicitly enabled for this App".into(),
                ),
            ],
        ));
    }
    let device_code = hex::encode(rand::rng().random::<[u8; 20]>());
    let user_code = new_user_code();
    let now = Utc::now().timestamp();
    let grant = DeviceGrant {
        app_id: app.id,
        // Site-admin scope can't be requested through the device flow.
        scopes: parse_scopes(p.get("scope").map(String::as_str).unwrap_or(""), false),
        user_code: user_code.clone(),
        status: "pending".into(),
        user_id: None,
        interval: DEVICE_INTERVAL_SECS,
        last_poll: 0,
        expires_at: now + DEVICE_TTL_SECS as i64,
    };
    let key = redis_key(&state, "device", &device_code);
    redis_set_json(&state, &key, &grant, DEVICE_TTL_SECS).await?;
    let mut redis = state.redis.clone();
    let _: () = redis
        .set_ex(
            state.redis_key(&format!("device_user:{user_code}")),
            &key,
            DEVICE_TTL_SECS,
        )
        .await?;
    Ok(token_response(
        &headers,
        StatusCode::OK,
        vec![
            ("device_code", device_code),
            ("user_code", user_code),
            ("verification_uri", state.urls.html("/login/device")),
            ("expires_in", DEVICE_TTL_SECS.to_string()),
            ("interval", DEVICE_INTERVAL_SECS.to_string()),
        ],
    ))
}

async fn device_token(
    state: &AppState,
    headers: &HeaderMap,
    app: &OauthApp,
    p: &HashMap<String, String>,
) -> ApiResult<Response> {
    if !app.device_flow_enabled {
        return Ok(oauth_error(
            headers,
            "device_flow_disabled",
            "Device flow must be explicitly enabled for this App",
        ));
    }
    let device_code = p.get("device_code").cloned().unwrap_or_default();
    let key = redis_key(state, "device", &device_code);
    let Some(mut grant) = redis_get_json::<DeviceGrant>(state, &key)
        .await?
        .filter(|g| g.app_id == app.id)
    else {
        return Ok(oauth_error(
            headers,
            "expired_token",
            "This 'device_code' has expired.",
        ));
    };
    let now = Utc::now().timestamp();
    if now >= grant.expires_at {
        return Ok(oauth_error(
            headers,
            "expired_token",
            "This 'device_code' has expired.",
        ));
    }
    match grant.status.as_str() {
        "approved" => {
            // Single use.
            let taken: Option<DeviceGrant> = redis_take_json(state, &key).await?;
            let Some(grant) = taken.filter(|g| g.status == "approved") else {
                return Ok(oauth_error(
                    headers,
                    "expired_token",
                    "This 'device_code' has expired.",
                ));
            };
            let user_id = grant.user_id.ok_or_else(|| {
                ApiError::internal(anyhow::anyhow!("approved device grant without user"))
            })?;
            let (_, token) = issue_token(state, user_id, app, &grant.scopes).await?;
            Ok(token_response(
                headers,
                StatusCode::OK,
                vec![
                    ("access_token", token),
                    ("token_type", "bearer".into()),
                    ("scope", grant.scopes.join(",")),
                ],
            ))
        }
        "denied" => {
            let mut redis = state.redis.clone();
            let _: Result<(), _> = redis.del(&key).await;
            Ok(oauth_error(
                headers,
                "access_denied",
                "The authorization request was denied.",
            ))
        }
        _ => {
            let too_fast = grant.last_poll > 0 && now - grant.last_poll < grant.interval;
            if too_fast {
                grant.interval += 5;
            }
            grant.last_poll = now;
            let ttl = (grant.expires_at - now).max(1) as u64;
            redis_set_json(state, &key, &grant, ttl).await?;
            if too_fast {
                return Ok(slow_down(headers, grant.interval));
            }
            Ok(oauth_error(
                headers,
                "authorization_pending",
                "The authorization request is still pending.",
            ))
        }
    }
}

fn slow_down(headers: &HeaderMap, interval: i64) -> Response {
    token_response(headers, StatusCode::OK, vec![
        ("error", "slow_down".into()),
        ("error_description", "Too many requests have been made in the same timeframe.".into()),
        ("error_uri", "https://docs.github.com/developers/apps/authorizing-oauth-apps#error-codes-for-the-device-flow".into()),
        ("interval", interval.to_string()),
    ])
}

async fn device_by_user_code(
    state: &AppState,
    user_code: &str,
) -> ApiResult<Option<(String, DeviceGrant)>> {
    let mut redis = state.redis.clone();
    let key: Option<String> = redis
        .get(state.redis_key(&format!("device_user:{}", normalize_user_code(user_code))))
        .await?;
    let Some(key) = key else {
        return Ok(None);
    };
    Ok(redis_get_json::<DeviceGrant>(state, &key)
        .await?
        .filter(|g| g.status == "pending" && g.expires_at > Utc::now().timestamp())
        .map(|g| (key, g)))
}

/// Approve or deny a pending device grant on behalf of `user_id`.
async fn decide_device(
    state: &AppState,
    user: &db::User,
    user_code: &str,
    authorize: bool,
) -> ApiResult<Option<OauthApp>> {
    let key = format!("device_attempts:{}", user.id);
    if bgh_core::ratelimit::hit(state, &key, 3600).await? > 50 {
        return Err(ApiError::Status(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many attempts.".into(),
        ));
    }
    let Some((key, mut grant)) = device_by_user_code(state, user_code).await? else {
        return Ok(None);
    };
    grant.status = if authorize { "approved" } else { "denied" }.into();
    grant.user_id = Some(user.id);
    let ttl = (grant.expires_at - Utc::now().timestamp()).max(1) as u64;
    redis_set_json(state, &key, &grant, ttl).await?;
    let mut redis = state.redis.clone();
    let _: Result<(), _> = redis
        .del(state.redis_key(&format!("device_user:{}", grant.user_code)))
        .await;
    let app: Option<OauthApp> = sqlx::query_as(&format!(
        "SELECT {} FROM oauth_apps WHERE id = $1",
        OauthApp::COLUMNS
    ))
    .bind(grant.app_id)
    .fetch_optional(&state.db)
    .await?;
    Ok(app)
}

#[derive(Debug, Serialize)]
pub struct DeviceInfo {
    pub user_code: String,
    pub app: AppPublic,
    pub scopes: Vec<String>,
}

/// `GET /_bgh/device/{user_code}` → the pending request (404 if unknown).
pub async fn device_info(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(code): Path<String>,
) -> ApiResult<Json<DeviceInfo>> {
    util::require_session(&auth)?;
    let (_, grant) = device_by_user_code(&state, &code)
        .await?
        .ok_or(ApiError::NotFound)?;
    let app: OauthApp = sqlx::query_as(&format!(
        "SELECT {} FROM oauth_apps WHERE id = $1",
        OauthApp::COLUMNS
    ))
    .bind(grant.app_id)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(DeviceInfo {
        user_code: grant.user_code,
        app: app_public(&state, &app).await?,
        scopes: grant.scopes,
    }))
}

#[derive(Debug, Deserialize)]
pub struct DeviceDecision {
    #[serde(default)]
    pub user_code: String,
    #[serde(default = "yes")]
    pub authorize: bool,
}

fn yes() -> bool {
    true
}

/// `POST /_bgh/device {user_code, authorize}` → 204 (404 for unknown or
/// expired codes).
pub async fn device_decide(
    State(state): State<AppState>,
    auth: RequireUser,
    Json(body): Json<DeviceDecision>,
) -> ApiResult<StatusCode> {
    util::require_session(&auth)?;
    decide_device(&state, &auth.user, &body.user_code, body.authorize)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /login/device` → code entry page (login required).
pub async fn device_page(
    State(state): State<AppState>,
    auth: MaybeUser,
    OriginalUri(original): OriginalUri,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let Some(auth) = auth.0.filter(|a| a.is_session()) else {
        return Ok(login_redirect(&original));
    };
    let p = params(&original, &headers, &[]);
    let nonce = crypto::random_token(32);
    let mut redis = state.redis.clone();
    let _: () = redis
        .set_ex(
            redis_key(&state, "device_csrf", &nonce),
            auth.user.id,
            CONSENT_TTL_SECS,
        )
        .await?;
    let code = p
        .get("user_code")
        .map(|c| normalize_user_code(c))
        .unwrap_or_default();
    let body = format!(
        "<h1>Device activation</h1>\
         <p>Signed in as <b>@{login}</b>. Enter the code displayed on your device.</p>\
         <form method=\"post\" action=\"/login/device\">\
           <input type=\"hidden\" name=\"csrf\" value=\"{nonce}\">\
           <input name=\"user_code\" value=\"{code}\" placeholder=\"XXXX-XXXX\" autocomplete=\"off\" autofocus required>\
           <button type=\"submit\" name=\"authorize\" value=\"1\">Authorize</button>\
           <button type=\"submit\" name=\"authorize\" value=\"0\">Cancel</button>\
         </form>",
        login = esc(&auth.user.login),
        code = esc(&code),
    );
    Ok(page(&state, StatusCode::OK, "Device activation", &body))
}

/// `POST /login/device` (code entry form).
pub async fn device_submit(
    State(state): State<AppState>,
    auth: RequireUser,
    OriginalUri(original): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    util::require_session(&auth)?;
    let p = params(&original, &headers, &body);
    let csrf = p.get("csrf").cloned().unwrap_or_default();
    let mut redis = state.redis.clone();
    let owner: Option<i64> = redis::cmd("GETDEL")
        .arg(redis_key(&state, "device_csrf", &csrf))
        .query_async(&mut redis)
        .await?;
    if owner != Some(auth.user.id) {
        return Ok(error_page(
            &state,
            StatusCode::BAD_REQUEST,
            "This form expired. Please try again.",
        ));
    }
    let authorize = p.get("authorize").map(String::as_str) == Some("1");
    let code = p.get("user_code").cloned().unwrap_or_default();
    match decide_device(&state, &auth.user, &code, authorize).await? {
        Some(app) if authorize => Ok(page(
            &state,
            StatusCode::OK,
            "Device activated",
            &format!(
                "<h1>Congratulations, you're all set!</h1><p>Your device is now connected to <b>{}</b>. You can close this window.</p>",
                esc(&app.name)
            ),
        )),
        Some(_) => Ok(page(
            &state,
            StatusCode::OK,
            "Request denied",
            "<h1>Authorization request denied</h1>",
        )),
        None => Ok(error_page(
            &state,
            StatusCode::NOT_FOUND,
            "Unknown or expired code. Check the code on your device and try again.",
        )),
    }
}

// ---------------------------------------------------------------------------
// HTML
// ---------------------------------------------------------------------------

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn page(state: &AppState, status: StatusCode, title: &str, body: &str) -> Response {
    let html = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <title>{title} · {site}</title>\
         <style>body{{font:15px/1.5 system-ui,sans-serif;max-width:28rem;margin:4rem auto;padding:0 1rem;color:#1f2328}}\
         input{{font:inherit;padding:.4rem;width:100%;box-sizing:border-box;margin:.5rem 0;text-transform:uppercase}}\
         button{{font:inherit;padding:.4rem .9rem;margin-right:.5rem}}ul{{padding-left:1.2rem}}</style></head>\
         <body>{body}</body></html>",
        title = esc(title),
        site = esc(&state.config.site_name),
    );
    let mut resp = (status, Html(html)).into_response();
    let h = resp.headers_mut();
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert("x-frame-options", HeaderValue::from_static("DENY"));
    h.insert(
        "content-security-policy",
        HeaderValue::from_static("default-src 'none'; style-src 'unsafe-inline'; form-action 'self' *; frame-ancestors 'none'"),
    );
    resp
}

fn error_page(state: &AppState, status: StatusCode, msg: &str) -> Response {
    page(
        state,
        status,
        "Error",
        &format!("<h1>Something went wrong</h1><p>{}</p>", esc(msg)),
    )
}

fn consent_page(
    state: &AppState,
    user: &db::User,
    app: &OauthApp,
    scopes: &[String],
    nonce: &str,
) -> Response {
    let scope_list = if scopes.is_empty() {
        "<li>Read-only access to public information</li>".to_string()
    } else {
        scopes
            .iter()
            .map(|s| format!("<li><code>{}</code></li>", esc(s)))
            .collect()
    };
    let body = format!(
        "<h1>Authorize {name}</h1>\
         <p><b>{name}</b> wants to access your <b>@{login}</b> account.</p><ul>{scope_list}</ul>\
         <form method=\"post\" action=\"/login/oauth/authorize\">\
           <input type=\"hidden\" name=\"consent\" value=\"{nonce}\">\
           <button type=\"submit\" name=\"authorize\" value=\"1\">Authorize</button>\
           <button type=\"submit\" name=\"authorize\" value=\"0\">Cancel</button>\
         </form>",
        name = esc(&app.name),
        login = esc(&user.login),
    );
    page(
        state,
        StatusCode::OK,
        &format!("Authorize {}", app.name),
        &body,
    )
}

// ---------------------------------------------------------------------------
// GitHub `/applications/{client_id}/...` API (Basic client_id:client_secret)
// ---------------------------------------------------------------------------

async fn app_from_basic(
    state: &AppState,
    headers: &HeaderMap,
    client_id: &str,
) -> ApiResult<OauthApp> {
    let (id, secret) = basic_credentials(headers).ok_or_else(ApiError::requires_auth)?;
    if id != client_id {
        return Err(ApiError::NotFound);
    }
    let app = OauthApp::by_client_id(&state.db, client_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if app.client_secret_hash.is_none() || !app.check_secret(Some(&secret)) {
        return Err(ApiError::bad_credentials());
    }
    Ok(app)
}

#[derive(Debug, Deserialize)]
pub struct AccessTokenBody {
    #[serde(default)]
    pub access_token: String,
}

#[derive(FromRow)]
struct OauthTokenRow {
    id: i64,
    user_id: i64,
    scopes: Vec<String>,
    token_last_eight: String,
    token_hash: String,
    expires_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
}

async fn find_app_token(state: &AppState, app: &OauthApp, token: &str) -> ApiResult<OauthTokenRow> {
    sqlx::query_as(
        "SELECT id, user_id, scopes, token_last_eight, token_hash, expires_at, created_at
           FROM access_tokens WHERE token_hash = $1 AND oauth_app_id = $2",
    )
    .bind(crypto::sha256_hex(token))
    .bind(app.id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)
}

/// GitHub `authorization` shape.
async fn authorization_json(
    state: &AppState,
    app: &OauthApp,
    t: &OauthTokenRow,
    token: &str,
) -> ApiResult<Value> {
    let user = db::User::find(&state.db, t.user_id).await?;
    Ok(json!({
        "id": t.id,
        "url": state.urls.api(&format!("/authorizations/{}", t.id)),
        "scopes": t.scopes,
        "token": token,
        "token_last_eight": t.token_last_eight,
        "hashed_token": STANDARD.encode(hex::decode(&t.token_hash).unwrap_or_default()),
        "app": { "client_id": app.client_id, "name": app.name, "url": app.homepage_url },
        "note": null,
        "note_url": null,
        "updated_at": Timestamp::from(t.created_at),
        "created_at": Timestamp::from(t.created_at),
        "fingerprint": null,
        "user": user.as_ref().map(|u| SimpleUser::new(&state.urls, u)),
        "installation": null,
        "expires_at": ts(t.expires_at),
    }))
}

/// `POST /applications/{client_id}/token {access_token}` → authorization
/// (check a token).
pub async fn check_app_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(client_id): Path<String>,
    Json(body): Json<AccessTokenBody>,
) -> ApiResult<Json<Value>> {
    let app = app_from_basic(&state, &headers, &client_id).await?;
    let t = find_app_token(&state, &app, &body.access_token).await?;
    Ok(Json(
        authorization_json(&state, &app, &t, &body.access_token).await?,
    ))
}

/// `PATCH /applications/{client_id}/token {access_token}` → authorization
/// with a new token (the old one stops working).
pub async fn reset_app_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(client_id): Path<String>,
    Json(body): Json<AccessTokenBody>,
) -> ApiResult<Json<Value>> {
    let app = app_from_basic(&state, &headers, &client_id).await?;
    let t = find_app_token(&state, &app, &body.access_token).await?;
    let token = crypto::new_oauth_token();
    let t: OauthTokenRow = sqlx::query_as(
        "UPDATE access_tokens SET token_hash = $2, token_last_eight = $3 WHERE id = $1
         RETURNING id, user_id, scopes, token_last_eight, token_hash, expires_at, created_at",
    )
    .bind(t.id)
    .bind(crypto::sha256_hex(&token))
    .bind(&token[token.len() - 8..])
    .fetch_one(&state.db)
    .await?;
    Ok(Json(authorization_json(&state, &app, &t, &token).await?))
}

/// `DELETE /applications/{client_id}/token {access_token}` → 204.
pub async fn delete_app_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(client_id): Path<String>,
    Json(body): Json<AccessTokenBody>,
) -> ApiResult<StatusCode> {
    let app = app_from_basic(&state, &headers, &client_id).await?;
    let t = find_app_token(&state, &app, &body.access_token).await?;
    sqlx::query("DELETE FROM access_tokens WHERE id = $1")
        .bind(t.id)
        .execute(&state.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /applications/{client_id}/grant {access_token}` → 204: revokes
/// the grant and every token of the app for that user.
pub async fn delete_app_grant(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(client_id): Path<String>,
    Json(body): Json<AccessTokenBody>,
) -> ApiResult<StatusCode> {
    let app = app_from_basic(&state, &headers, &client_id).await?;
    let t = find_app_token(&state, &app, &body.access_token).await?;
    revoke_grant(&state, t.user_id, app.id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirect_rules() {
        let cb = "https://example.com/callback";
        assert!(redirect_allowed(cb, "https://example.com/callback"));
        assert!(redirect_allowed(cb, "https://example.com/callback/sub?x=1"));
        assert!(!redirect_allowed(cb, "https://example.com/other"));
        assert!(!redirect_allowed(cb, "https://evil.com/callback"));
        assert!(!redirect_allowed(cb, "http://example.com/callback"));
        assert!(!redirect_allowed(cb, "https://example.com:8443/callback"));
        assert!(redirect_allowed(
            "http://127.0.0.1/callback",
            "http://127.0.0.1:43123/callback"
        ));
    }

    #[test]
    fn scopes_and_codes() {
        assert_eq!(
            parse_scopes("repo read:org,gist bogus repo", false),
            vec!["repo", "read:org", "gist"]
        );
        assert!(parse_scopes("site_admin", false).is_empty());
        let c = new_user_code();
        assert_eq!(c.len(), 9);
        assert_eq!(normalize_user_code(&c.to_lowercase().replace('-', "")), c);
        assert!(pkce_ok(
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
            Some("S256"),
            Some("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk")
        ));
    }
}
