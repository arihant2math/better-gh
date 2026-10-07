//! Generic OpenID Connect single sign-on (authorization code + PKCE).
//!
//! Providers come from one accessor, [`providers`]: the `auth_providers.oidc`
//! site setting (`bgh_core::settings::OidcProvider`, edited by site admins),
//! whose default is the single provider configured by `BGH_OIDC_*`
//! (`bgh_core::Config::oidc`; a stored `oidc` list replaces it):
//!
//! | setting field | env | default |
//! |-------|-----|---------|
//! | `name` (URL id) | `BGH_OIDC_ID` | `oidc` |
//! | `display_name` | `BGH_OIDC_NAME` | `Single sign-on` |
//! | `issuer` | `BGH_OIDC_ISSUER` | (required) |
//! | `client_id` | `BGH_OIDC_CLIENT_ID` | (required) |
//! | `client_secret` | `BGH_OIDC_CLIENT_SECRET` | |
//! | `scopes` | `BGH_OIDC_SCOPES` (space separated) | `openid profile email` |
//! | `auto_create_users` | `BGH_OIDC_AUTO_CREATE` | `true` |
//! | `login_claim` | `BGH_OIDC_LOGIN_CLAIM` | `preferred_username` |
//! | `allowed_domains` | `BGH_OIDC_ALLOWED_DOMAINS` (comma separated) | any |
//!
//! Endpoints: `GET /_bgh/sso` (providers for the login page),
//! `GET /_bgh/sso/{id}/login?return_to=/path`, `GET /_bgh/sso/{id}/callback`.
//! Identities are linked in `user_identities` (provider, subject); a new
//! identity attaches to the user owning the same verified email, else a new
//! account is created (when `auto_create`). The ID token comes straight
//! from the token endpoint over the client-authenticated back channel, so
//! its claims (`iss`, `aud`, `exp`, `nonce`) are checked without verifying
//! the signature (OIDC Core 3.1.3.7). Accounts with 2FA still need their
//! second factor (redirect to `/login/two-factor?token=...`).

use std::time::Duration;

use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use bgh_core::audit;
use bgh_core::auth;
use bgh_core::crypto;
use bgh_core::prelude::*;
use bgh_core::settings;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::session::{self, LoginStep};
use crate::util::{self, ClientInfo};
use crate::{group_sync, users, validate};

const STATE_TTL_SECS: u64 = 600;
const DISCOVERY_TTL_SECS: u64 = 3600;

/// A provider as the login flow uses it (resolved from the settings).
#[derive(Debug, Clone)]
pub struct OidcProvider {
    pub id: String,
    pub name: String,
    pub issuer: String,
    pub client_id: String,
    pub client_secret: Option<String>,
    /// Space-separated scopes.
    pub scopes: String,
    pub auto_create: bool,
    pub login_claim: String,
    /// Lower-case domains; empty = any.
    pub allowed_domains: Vec<String>,
    /// Claim listing the user's groups (team sync, [`group_sync`]).
    pub groups_claim: Option<String>,
}

impl From<&settings::OidcProvider> for OidcProvider {
    fn from(p: &settings::OidcProvider) -> Self {
        Self {
            id: p.name.clone(),
            name: p
                .display_name
                .clone()
                .filter(|n| !n.trim().is_empty())
                .unwrap_or_else(|| p.name.clone()),
            issuer: p.issuer.clone(),
            client_id: p.client_id.clone(),
            client_secret: p.client_secret.clone().filter(|s| !s.is_empty()),
            scopes: if p.scopes.is_empty() {
                "openid profile email".into()
            } else {
                p.scopes.join(" ")
            },
            auto_create: p.auto_create_users,
            login_claim: p
                .login_claim
                .clone()
                .filter(|c| !c.is_empty())
                .unwrap_or_else(|| "preferred_username".into()),
            allowed_domains: p
                .allowed_domains
                .iter()
                .map(|d| d.trim().to_lowercase())
                .filter(|d| !d.is_empty())
                .collect(),
            groups_claim: p
                .groups_claim
                .clone()
                .map(|c| c.trim().to_string())
                .filter(|c| !c.is_empty()),
        }
    }
}

/// Configured providers: the effective `auth_providers.oidc` site setting
/// (environment defaults overridden by what site admins stored).
pub async fn providers(state: &AppState) -> ApiResult<Vec<OidcProvider>> {
    let s = settings::load(state).await?;
    Ok(s.auth_providers
        .oidc
        .iter()
        .map(OidcProvider::from)
        .collect())
}

async fn provider(state: &AppState, id: &str) -> ApiResult<OidcProvider> {
    providers(state)
        .await?
        .into_iter()
        .find(|p| p.id == id)
        .ok_or(ApiError::NotFound)
}

#[derive(Debug, Serialize)]
pub struct ProviderJson {
    pub id: String,
    pub name: String,
    pub login_url: String,
}

/// `GET /_bgh/sso` → configured providers.
pub async fn list(State(state): State<AppState>) -> ApiResult<Json<Vec<ProviderJson>>> {
    Ok(Json(
        providers(&state)
            .await?
            .into_iter()
            .map(|p| ProviderJson {
                login_url: state.urls.html(&format!("/_bgh/sso/{}/login", p.id)),
                id: p.id,
                name: p.name,
            })
            .collect(),
    ))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Discovery {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    #[serde(default)]
    userinfo_endpoint: Option<String>,
}

fn http() -> ApiResult<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .user_agent("better-github")
        .build()
        .map_err(ApiError::internal)
}

async fn discover(state: &AppState, p: &OidcProvider) -> ApiResult<Discovery> {
    let key = state.redis_key(&format!("oidc_discovery:{}", crypto::sha256_hex(&p.issuer)));
    let mut redis = state.redis.clone();
    if let Some(cached) = redis.get::<_, Option<String>>(&key).await?
        && let Ok(d) = serde_json::from_str(&cached)
    {
        return Ok(d);
    }
    let url = format!(
        "{}/.well-known/openid-configuration",
        p.issuer.trim_end_matches('/')
    );
    let d: Discovery = http()?
        .get(&url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(ApiError::internal)?
        .json()
        .await
        .map_err(ApiError::internal)?;
    let _: Result<(), _> = redis
        .set_ex(&key, serde_json::to_string(&d)?, DISCOVERY_TTL_SECS)
        .await;
    Ok(d)
}

#[derive(Debug, Serialize, Deserialize)]
struct LoginState {
    provider: String,
    nonce: String,
    verifier: String,
    return_to: String,
}

fn callback_url(state: &AppState, provider: &str) -> String {
    state.urls.html(&format!("/_bgh/sso/{provider}/callback"))
}

/// Only same-site relative paths are allowed as `return_to`.
///
/// Browsers treat `\` as `/` and strip tab/newline from URLs, so `/\x` and
/// `/\t/x` would become the protocol-relative `//x`; reject those along with
/// any other control character.
pub(crate) fn safe_return_to(r: Option<&str>) -> String {
    match r {
        Some(p)
            if p.starts_with('/')
                && !p.starts_with("//")
                && !p.chars().any(|c| c == '\\' || c.is_control()) =>
        {
            p.to_string()
        }
        _ => "/".to_string(),
    }
}

#[derive(Debug, Deserialize)]
pub struct LoginQuery {
    pub return_to: Option<String>,
}

/// `GET /_bgh/sso/{id}/login?return_to=` → redirect to the provider.
pub async fn login(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<LoginQuery>,
) -> ApiResult<Response> {
    let p = provider(&state, &id).await?;
    let d = discover(&state, &p).await?;
    let st = crypto::random_token(32);
    let nonce = crypto::random_token(32);
    let verifier = crypto::random_token(64);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let data = LoginState {
        provider: p.id.clone(),
        nonce: nonce.clone(),
        verifier,
        return_to: safe_return_to(q.return_to.as_deref()),
    };
    let mut redis = state.redis.clone();
    let _: () = redis
        .set_ex(
            state.redis_key(&format!("oidc_state:{}", crypto::sha256_hex(&st))),
            serde_json::to_string(&data)?,
            STATE_TTL_SECS,
        )
        .await?;
    let mut url = url::Url::parse(&d.authorization_endpoint).map_err(ApiError::internal)?;
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &p.client_id)
        .append_pair("redirect_uri", &callback_url(&state, &p.id))
        .append_pair("scope", &p.scopes)
        .append_pair("state", &st)
        .append_pair("nonce", &nonce)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256");
    Ok(Redirect::to(url.as_str()).into_response())
}

#[derive(Debug, Deserialize)]
pub struct CallbackQuery {
    pub code: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
    pub error_description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    id_token: Option<String>,
    access_token: Option<String>,
}

/// Decode a JWT payload (no signature check; see module docs).
fn jwt_claims(jwt: &str) -> Option<Value> {
    let payload = jwt.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub(crate) fn sso_error(msg: &str) -> Response {
    let to = format!(
        "/login?error={}",
        url::form_urlencoded::byte_serialize(msg.as_bytes()).collect::<String>()
    );
    Redirect::to(&to).into_response()
}

/// Verified claims of a sign-in.
#[derive(Debug)]
struct Identity {
    subject: String,
    email: Option<String>,
    email_verified: bool,
    login: Option<String>,
    name: Option<String>,
    /// Values of the groups claim (`None` when not configured or absent).
    groups: Option<Vec<String>>,
}

/// Values of a groups claim: an array of strings or one string.
fn claim_groups(claims: &Value, claim: Option<&str>) -> Option<Vec<String>> {
    match &claims[claim?] {
        Value::Array(a) => Some(
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect(),
        ),
        Value::String(s) => Some(vec![s.clone()]),
        _ => None,
    }
}

fn claims_identity(
    p: &OidcProvider,
    d: &Discovery,
    claims: &Value,
    nonce: &str,
) -> Result<Identity, String> {
    let iss = claims["iss"].as_str().unwrap_or_default();
    if iss.trim_end_matches('/') != d.issuer.trim_end_matches('/') {
        return Err("ID token issuer mismatch".into());
    }
    let aud_ok = match &claims["aud"] {
        Value::String(a) => a == &p.client_id,
        Value::Array(a) => a.iter().any(|x| x.as_str() == Some(&p.client_id)),
        _ => false,
    };
    if !aud_ok {
        return Err("ID token audience mismatch".into());
    }
    if claims["exp"]
        .as_i64()
        .is_none_or(|e| e < chrono::Utc::now().timestamp() - 60)
    {
        return Err("ID token expired".into());
    }
    if claims["nonce"].as_str() != Some(nonce) {
        return Err("ID token nonce mismatch".into());
    }
    let subject = claims["sub"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or("ID token without subject")?;
    Ok(Identity {
        subject: subject.to_string(),
        email: claims["email"].as_str().map(str::to_string),
        email_verified: claims["email_verified"].as_bool().unwrap_or(false),
        login: claims[p.login_claim.as_str()].as_str().map(str::to_string),
        name: claims["name"].as_str().map(str::to_string),
        groups: claim_groups(claims, p.groups_claim.as_deref()),
    })
}

/// Make a valid, unused login from a claim / email local part.
pub(crate) async fn available_login(state: &AppState, wanted: &str) -> ApiResult<String> {
    let mut base: String = wanted
        .split('@')
        .next()
        .unwrap_or(wanted)
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    while base.contains("--") {
        base = base.replace("--", "-");
    }
    let mut base = base.trim_matches('-').to_string();
    base.truncate(32);
    let base = base.trim_matches('-').to_string();
    let base = if base.is_empty() || validate::is_reserved_login(&base) {
        format!("user-{base}").trim_end_matches('-').to_string()
    } else {
        base
    };
    for n in 0..100 {
        let candidate = if n == 0 {
            base.clone()
        } else {
            format!("{base}-{n}")
        };
        if !validate::is_valid_login(&candidate) {
            continue;
        }
        if db::User::find_by_login(&state.db, &candidate)
            .await?
            .is_none()
        {
            return Ok(candidate);
        }
    }
    Ok(format!("user-{}", crypto::random_token(8).to_lowercase()))
}

/// Find or create the local account for `ident`.
async fn resolve_user(
    state: &AppState,
    p: &OidcProvider,
    ident: &Identity,
) -> ApiResult<Result<db::User, String>> {
    let linked: Option<i64> = sqlx::query_scalar(
        "UPDATE user_identities SET last_login_at = now(), email = coalesce($3, email)
          WHERE provider = $1 AND subject = $2 RETURNING user_id",
    )
    .bind(&p.id)
    .bind(&ident.subject)
    .bind(&ident.email)
    .fetch_optional(&state.db)
    .await?;
    if let Some(uid) = linked {
        return Ok(db::User::find(&state.db, uid)
            .await?
            .ok_or_else(|| "account not found".to_string()));
    }
    let verified_email = ident.email.as_deref().filter(|_| ident.email_verified);
    if !p.allowed_domains.is_empty() {
        let domain = verified_email
            .and_then(|e| e.rsplit_once('@'))
            .map(|(_, d)| d.to_lowercase());
        if !domain.is_some_and(|d| p.allowed_domains.contains(&d)) {
            return Ok(Err(
                "Your email domain is not allowed to sign in here.".into()
            ));
        }
    }
    let mut tx = Tx::begin(state).await?;
    let existing: Option<db::User> = match verified_email {
        Some(email) => {
            sqlx::query_as(&format!(
                "SELECT {} FROM users u JOIN user_emails e ON e.user_id = u.id
                  WHERE lower(e.email) = lower($1) AND e.verified AND u.type = 'User'",
                db::prefixed("u", db::User::COLUMNS)
            ))
            .bind(email)
            .fetch_optional(&mut *tx)
            .await?
        }
        None => None,
    };
    let user = match existing {
        Some(u) => u,
        None => {
            if !p.auto_create {
                return Ok(Err("No account is linked to this identity.".into()));
            }
            let Some(email) = verified_email else {
                return Ok(Err(
                    "The identity provider did not return a verified email address.".into(),
                ));
            };
            let wanted = ident.login.clone().unwrap_or_else(|| email.to_string());
            let login = available_login(state, &wanted).await?;
            let user =
                users::insert_user(&mut tx, &login, email, ident.name.as_deref(), None, None)
                    .await?;
            audit::log(
                &mut *tx,
                Some(&user),
                "user.create",
                audit::Target::User(user.id),
                json!({ "login": login, "sso": p.id }),
            )
            .await?;
            user
        }
    };
    sqlx::query(
        "INSERT INTO user_identities (user_id, provider, subject, email) VALUES ($1, $2, $3, $4)",
    )
    .bind(user.id)
    .bind(&p.id)
    .bind(&ident.subject)
    .bind(&ident.email)
    .execute(&mut *tx)
    .await?;
    audit::log(
        &mut *tx,
        Some(&user),
        "user.sso_link",
        audit::Target::User(user.id),
        json!({ "provider": p.id }),
    )
    .await?;
    tx.commit().await?;
    Ok(Ok(user))
}

/// `GET /_bgh/sso/{id}/callback?code&state` → session cookie + redirect.
pub async fn callback(
    State(state): State<AppState>,
    client: ClientInfo,
    Path(id): Path<String>,
    Query(q): Query<CallbackQuery>,
) -> ApiResult<Response> {
    if let Some(err) = q.error {
        return Ok(sso_error(q.error_description.as_deref().unwrap_or(&err)));
    }
    let (Some(code), Some(st)) = (q.code, q.state) else {
        return Ok(sso_error("Invalid sign-in response."));
    };
    let mut redis = state.redis.clone();
    let saved: Option<String> = redis::cmd("GETDEL")
        .arg(state.redis_key(&format!("oidc_state:{}", crypto::sha256_hex(&st))))
        .query_async(&mut redis)
        .await?;
    let Some(saved) = saved
        .and_then(|s| serde_json::from_str::<LoginState>(&s).ok())
        .filter(|s| s.provider == id)
    else {
        return Ok(sso_error("Sign-in session expired. Please try again."));
    };
    let p = provider(&state, &id).await?;
    let d = discover(&state, &p).await?;
    let mut form = vec![
        ("grant_type", "authorization_code".to_string()),
        ("code", code),
        ("redirect_uri", callback_url(&state, &p.id)),
        ("client_id", p.client_id.clone()),
        ("code_verifier", saved.verifier.clone()),
    ];
    if let Some(secret) = &p.client_secret {
        form.push(("client_secret", secret.clone()));
    }
    let body = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(form.iter().map(|(k, v)| (*k, v.as_str())))
        .finish();
    let resp = http()?
        .post(&d.token_endpoint)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(header::ACCEPT, "application/json")
        .body(body)
        .send()
        .await
        .map_err(ApiError::internal)?;
    if !resp.status().is_success() {
        tracing::warn!(status = %resp.status(), provider = %p.id, "OIDC token exchange failed");
        return Ok(sso_error("Sign-in failed at the identity provider."));
    }
    let tokens: TokenResponse = resp.json().await.map_err(ApiError::internal)?;
    let Some(claims) = tokens.id_token.as_deref().and_then(jwt_claims) else {
        return Ok(sso_error(
            "The identity provider did not return an ID token.",
        ));
    };
    let mut ident = match claims_identity(&p, &d, &claims, &saved.nonce) {
        Ok(i) => i,
        Err(msg) => {
            tracing::warn!(provider = %p.id, %msg, "rejected OIDC ID token");
            return Ok(sso_error("Sign-in failed: invalid ID token."));
        }
    };
    // Fill missing profile claims from the userinfo endpoint.
    if ident.email.is_none()
        && let (Some(ui), Some(at)) = (&d.userinfo_endpoint, &tokens.access_token)
        && let Ok(r) = http()?.get(ui).bearer_auth(at).send().await
        && let Ok(info) = r.json::<Value>().await
        && info["sub"].as_str() == Some(&ident.subject)
    {
        ident.email = info["email"].as_str().map(str::to_string);
        ident.email_verified = info["email_verified"].as_bool().unwrap_or(false);
        ident.login = ident
            .login
            .or_else(|| info[p.login_claim.as_str()].as_str().map(str::to_string));
        ident.name = ident
            .name
            .or_else(|| info["name"].as_str().map(str::to_string));
        ident.groups = ident
            .groups
            .or_else(|| claim_groups(&info, p.groups_claim.as_deref()));
    }
    let user = match resolve_user(&state, &p, &ident).await? {
        Ok(u) => u,
        Err(msg) => return Ok(sso_error(&msg)),
    };
    if user.is_suspended() {
        return Ok(sso_error("Sorry. Your account was suspended."));
    }
    if let Some(groups) = &ident.groups {
        let groups = groups.iter().cloned().collect();
        group_sync::apply_user_groups(&state, &user, group_sync::OIDC, &groups).await?;
    }
    match session::after_first_factor(&state, &user).await? {
        LoginStep::TwoFactor(token) => {
            let to = format!(
                "/login/two-factor?token={token}&return_to={}",
                url::form_urlencoded::byte_serialize(saved.return_to.as_bytes())
                    .collect::<String>()
            );
            Ok(Redirect::to(&to).into_response())
        }
        LoginStep::Done => {
            let token = auth::create_session(
                &state,
                user.id,
                client.user_agent.as_deref(),
                Some(&client.ip),
            )
            .await?;
            let mut resp = Redirect::to(&saved.return_to).into_response();
            resp.headers_mut().insert(
                header::SET_COOKIE,
                auth::session_cookie(&state.config, &token),
            );
            Ok(resp)
        }
    }
}

/// (id, provider, subject, email, created, last_login).
type IdentityRow = (
    i64,
    String,
    String,
    Option<String>,
    chrono::DateTime<chrono::Utc>,
    chrono::DateTime<chrono::Utc>,
);

/// `GET /_bgh/user/identities` → linked SSO identities of the caller.
pub async fn my_identities(
    State(state): State<AppState>,
    auth: RequireUser,
) -> ApiResult<Json<Vec<Value>>> {
    util::require_session(&auth)?;
    let rows: Vec<IdentityRow> = sqlx::query_as(
        "SELECT id, provider, subject, email, created_at, last_login_at FROM user_identities
          WHERE user_id = $1 ORDER BY id",
    )
    .bind(auth.user.id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|(id, provider, subject, email, c, l)| {
                json!({ "id": id, "provider": provider, "subject": subject, "email": email,
                        "created_at": Timestamp::from(c), "last_login_at": Timestamp::from(l) })
            })
            .collect(),
    ))
}

/// `DELETE /_bgh/user/identities/{id}` → 204 (refused for the only way to
/// sign in: users without a password must keep one identity).
pub async fn unlink_identity(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    util::require_session(&auth)?;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM user_identities WHERE user_id = $1")
        .bind(auth.user.id)
        .fetch_one(&state.db)
        .await?;
    if auth.user.password_hash.is_none() && count <= 1 {
        return Err(ApiError::unprocessable(
            "Set a password before removing your last sign-in identity.",
        ));
    }
    let deleted = sqlx::query("DELETE FROM user_identities WHERE id = $1 AND user_id = $2")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn return_to_is_relative_only() {
        assert_eq!(safe_return_to(Some("/settings")), "/settings");
        assert_eq!(safe_return_to(Some("//evil.com")), "/");
        assert_eq!(safe_return_to(Some("https://evil.com")), "/");
        assert_eq!(safe_return_to(None), "/");
        for bad in [
            "/\\x",
            "/\t/x",
            "/\n/x",
            "/\r/x",
            "/\0x",
            "\\\\x",
            "//x",
            "https://x",
            "javascript:alert(1)",
            "",
        ] {
            assert_eq!(safe_return_to(Some(bad)), "/", "{bad:?}");
        }
        assert_eq!(safe_return_to(Some("/acme/api?x=1#y")), "/acme/api?x=1#y");
    }
}
