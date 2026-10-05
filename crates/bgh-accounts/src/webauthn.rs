//! WebAuthn security keys (second factor) and passkeys (passwordless,
//! discoverable credentials), with `webauthn-rs`.
//!
//! Management (browser session; adding and deleting needs sudo mode):
//! * `GET /_bgh/user/webauthn` → the user's credentials
//! * `POST /_bgh/user/webauthn/registrations {kind}` → `{id, options}`
//!   (`kind`: `security_key`, which needs TOTP 2FA enabled, or `passkey`)
//! * `POST /_bgh/user/webauthn/registrations/{id} {name, credential}` → 201
//! * `PATCH /_bgh/user/webauthn/{id} {name}` → 200, `DELETE` → 204
//!
//! Sign-in (no session yet):
//! * `POST /_bgh/auth/login/passkey/challenge` → `{id, options}`, then
//!   `POST /_bgh/auth/login/passkey {id, credential}` → boot + cookie
//! * second factor of a password sign-in: `POST
//!   /_bgh/auth/2fa/webauthn/challenge {twoFactorToken}` → `{id, options}`,
//!   then `POST /_bgh/auth/2fa/webauthn {twoFactorToken, id, credential}`
//!
//! Sudo mode accepts an assertion too (`crate::security`).
//!
//! The relying party is the host of `BGH_BASE_URL` (a loopback IP becomes
//! `localhost`, as browsers refuse IP addresses as RP IDs). Ceremony state
//! is kept server side in Redis for 5 minutes and is single use.

use std::sync::{Arc, Mutex, OnceLock};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use bgh_core::audit;
use bgh_core::crypto;
use bgh_core::prelude::*;
use bgh_core::ratelimit;
use bgh_core::time::ts;
use redis::AsyncCommands;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use webauthn_rs::prelude::*;

use crate::util::{self, ClientInfo};
use crate::{boot, session};

/// How long a started ceremony stays valid.
const CEREMONY_TTL_SECS: u64 = 300;
pub const MAX_NAME_LEN: usize = 64;

// ---------------------------------------------------------------------------
// Relying party
// ---------------------------------------------------------------------------

/// `(rp_id, origin)` for a base URL.
pub fn relying_party(base_url: &str) -> ApiResult<(String, Url)> {
    let mut url = Url::parse(base_url).map_err(ApiError::internal)?;
    url.set_path("");
    url.set_query(None);
    url.set_fragment(None);
    let ip_host = matches!(url.host(), Some(url::Host::Ipv4(_) | url::Host::Ipv6(_)));
    if ip_host {
        url.set_host(Some("localhost"))
            .map_err(ApiError::internal)?;
    }
    let rp_id = url
        .domain()
        .ok_or_else(|| ApiError::internal(anyhow::anyhow!("base URL has no host")))?
        .to_string();
    Ok((rp_id, url))
}

/// The `Webauthn` instance for this deployment (cached per base URL).
pub fn webauthn(state: &AppState) -> ApiResult<Arc<Webauthn>> {
    type Cache = Mutex<Vec<(String, Arc<Webauthn>)>>;
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let base = state.config.base_url.clone();
    let mut cache = CACHE
        .get_or_init(Default::default)
        .lock()
        .expect("webauthn cache");
    if let Some((_, w)) = cache.iter().find(|(b, _)| *b == base) {
        return Ok(w.clone());
    }
    let (rp_id, origin) = relying_party(&base)?;
    let w = WebauthnBuilder::new(&rp_id, &origin)
        .and_then(|b| b.rp_name(&state.config.site_name).build())
        .map_err(|e| ApiError::internal(anyhow::anyhow!("webauthn config: {e}")))?;
    let w = Arc::new(w);
    cache.push((base, w.clone()));
    Ok(w)
}

fn verification_failed() -> ApiError {
    ApiError::unprocessable("Security key verification failed.")
}

// ---------------------------------------------------------------------------
// Storage
// ---------------------------------------------------------------------------

/// The user's WebAuthn user handle (created on first use).
pub async fn user_handle(db: &sqlx::PgPool, user_id: i64) -> ApiResult<Uuid> {
    sqlx::query(
        "INSERT INTO user_webauthn_handles (user_id, handle) VALUES ($1, $2)
         ON CONFLICT (user_id) DO NOTHING",
    )
    .bind(user_id)
    .bind(Uuid::new_v4())
    .execute(db)
    .await?;
    Ok(
        sqlx::query_scalar("SELECT handle FROM user_webauthn_handles WHERE user_id = $1")
            .bind(user_id)
            .fetch_one(db)
            .await?,
    )
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct CredentialRow {
    pub id: i64,
    pub user_id: i64,
    pub kind: String,
    pub name: String,
    pub credential: sqlx::types::Json<Credential>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub last_used_at: Option<chrono::DateTime<chrono::Utc>>,
}

const CREDENTIAL_COLUMNS: &str = "id, user_id, kind, name, credential, created_at, last_used_at";

/// Every credential of `user_id`, oldest first.
pub async fn credentials(db: &sqlx::PgPool, user_id: i64) -> ApiResult<Vec<CredentialRow>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {CREDENTIAL_COLUMNS} FROM user_webauthn_credentials WHERE user_id = $1 ORDER BY id"
    ))
    .bind(user_id)
    .fetch_all(db)
    .await?)
}

/// Number of `(security_key, passkey)` credentials of a user.
pub async fn counts(db: &sqlx::PgPool, user_id: i64) -> ApiResult<(i64, i64)> {
    Ok(sqlx::query_as(
        "SELECT count(*) FILTER (WHERE kind = 'security_key'), count(*) FILTER (WHERE kind = 'passkey')
           FROM user_webauthn_credentials WHERE user_id = $1",
    )
    .bind(user_id)
    .fetch_one(db)
    .await?)
}

#[derive(Debug, Serialize)]
pub struct CredentialJson {
    pub id: i64,
    pub name: String,
    /// `security_key` | `passkey`
    pub kind: String,
    pub created_at: Timestamp,
    pub last_used_at: Option<Timestamp>,
}

impl From<&CredentialRow> for CredentialJson {
    fn from(r: &CredentialRow) -> Self {
        Self {
            id: r.id,
            name: r.name.clone(),
            kind: r.kind.clone(),
            created_at: r.created_at.into(),
            last_used_at: ts(r.last_used_at),
        }
    }
}

/// Persist the counter / backup state after a successful assertion.
async fn record_use(
    state: &AppState,
    rows: &[CredentialRow],
    res: &AuthenticationResult,
) -> ApiResult<i64> {
    let row = rows
        .iter()
        .find(|r| r.credential.cred_id == *res.cred_id())
        .ok_or_else(verification_failed)?;
    let mut key = SecurityKey::from(row.credential.0.clone());
    key.update_credential(res);
    sqlx::query(
        "UPDATE user_webauthn_credentials SET credential = $2, last_used_at = now() WHERE id = $1",
    )
    .bind(row.id)
    .bind(sqlx::types::Json(Credential::from(key)))
    .execute(&state.db)
    .await?;
    Ok(row.id)
}

// ---------------------------------------------------------------------------
// Ceremony state (Redis, single use)
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
struct Pending {
    /// Owner of the ceremony; `None` for passwordless sign-in.
    user_id: Option<i64>,
    /// Extra binding (credential kind, pending-login hash).
    tag: String,
    state: Value,
}

fn ceremony_key(state: &AppState, purpose: &str, id: &str) -> String {
    state.redis_key(&format!("webauthn:{purpose}:{}", crypto::sha256_hex(id)))
}

async fn save_ceremony<T: Serialize>(
    state: &AppState,
    purpose: &str,
    user_id: Option<i64>,
    tag: &str,
    ceremony: &T,
) -> ApiResult<String> {
    let id = crypto::random_token(32);
    let value = serde_json::to_string(&Pending {
        user_id,
        tag: tag.to_string(),
        state: serde_json::to_value(ceremony).map_err(ApiError::internal)?,
    })
    .map_err(ApiError::internal)?;
    let mut redis = state.redis.clone();
    let _: () = redis
        .set_ex(ceremony_key(state, purpose, &id), value, CEREMONY_TTL_SECS)
        .await?;
    Ok(id)
}

/// Take (consume) a ceremony; 422 when unknown, expired or not the caller's.
async fn take_ceremony<T: DeserializeOwned>(
    state: &AppState,
    purpose: &str,
    id: &str,
    user_id: Option<i64>,
) -> ApiResult<(String, T)> {
    let mut redis = state.redis.clone();
    let raw: Option<String> = redis.get_del(ceremony_key(state, purpose, id)).await?;
    let expired = || ApiError::unprocessable("The security key request expired. Please try again.");
    let pending: Pending = raw
        .and_then(|r| serde_json::from_str(&r).ok())
        .ok_or_else(expired)?;
    if pending.user_id != user_id {
        return Err(expired());
    }
    let ceremony = serde_json::from_value(pending.state).map_err(|_| expired())?;
    Ok((pending.tag, ceremony))
}

#[derive(Debug, Serialize)]
pub struct ChallengeJson<T: Serialize> {
    /// Ceremony id, sent back with the authenticator's response.
    pub id: String,
    /// `PublicKeyCredentialCreationOptions` / `…RequestOptions` as JSON
    /// (`{publicKey: {...}}`, binary fields base64url).
    pub options: T,
}

// ---------------------------------------------------------------------------
// Assertions (second factor, sudo)
// ---------------------------------------------------------------------------

/// Start an assertion against all of `user_id`'s credentials (user
/// verification preferred, not required). `None` when there are none.
pub async fn start_assertion(
    state: &AppState,
    purpose: &str,
    user_id: i64,
    tag: &str,
) -> ApiResult<Option<ChallengeJson<RequestChallengeResponse>>> {
    let rows = credentials(&state.db, user_id).await?;
    if rows.is_empty() {
        return Ok(None);
    }
    let keys: Vec<SecurityKey> = rows
        .iter()
        .map(|r| SecurityKey::from(r.credential.0.clone()))
        .collect();
    let (options, ceremony) = webauthn(state)?
        .start_securitykey_authentication(&keys)
        .map_err(|e| ApiError::internal(anyhow::anyhow!("webauthn: {e}")))?;
    let id = save_ceremony(state, purpose, Some(user_id), tag, &ceremony).await?;
    Ok(Some(ChallengeJson { id, options }))
}

/// Finish an assertion started by [`start_assertion`]: `Ok(true)` when the
/// authenticator proved possession of one of the user's credentials.
pub async fn finish_assertion(
    state: &AppState,
    purpose: &str,
    user_id: i64,
    tag: &str,
    id: &str,
    credential: &PublicKeyCredential,
) -> ApiResult<bool> {
    let (stored_tag, ceremony): (String, SecurityKeyAuthentication) =
        take_ceremony(state, purpose, id, Some(user_id)).await?;
    if stored_tag != tag {
        return Ok(false);
    }
    let Ok(res) = webauthn(state)?.finish_securitykey_authentication(credential, &ceremony) else {
        return Ok(false);
    };
    let rows = credentials(&state.db, user_id).await?;
    let cred_id = record_use(state, &rows, &res).await?;
    audit::log(
        &state.db,
        None,
        "two_factor_authentication.webauthn_used",
        audit::Target::User(user_id),
        json!({ "credential_id": cred_id, "purpose": purpose }),
    )
    .await?;
    Ok(true)
}

// ---------------------------------------------------------------------------
// Management
// ---------------------------------------------------------------------------

/// `GET /_bgh/user/webauthn`
pub async fn list(
    State(state): State<AppState>,
    auth: RequireUser,
) -> ApiResult<Json<Vec<CredentialJson>>> {
    util::require_session(&auth)?;
    let rows = credentials(&state.db, auth.user.id).await?;
    Ok(Json(rows.iter().map(CredentialJson::from).collect()))
}

#[derive(Debug, Deserialize)]
pub struct StartRegistrationBody {
    #[serde(default)]
    pub kind: String,
}

/// `POST /_bgh/user/webauthn/registrations {kind}` → 201 `{id, options}`.
pub async fn start_registration(
    State(state): State<AppState>,
    auth: RequireUser,
    Json(body): Json<StartRegistrationBody>,
) -> ApiResult<(StatusCode, Json<ChallengeJson<CreationChallengeResponse>>)> {
    util::require_session(&auth)?;
    bgh_core::sudo::require(&state, &auth).await?;
    let kind = body.kind.as_str();
    if !matches!(kind, "security_key" | "passkey") {
        return Err(ApiError::invalid_field(FieldError::invalid(
            "WebauthnCredential",
            "kind",
        )));
    }
    if kind == "security_key" && !util::two_factor_enabled(&state.db, auth.user.id).await? {
        return Err(ApiError::unprocessable(
            "Enable two-factor authentication with an authenticator app before adding a security key.",
        ));
    }
    let handle = user_handle(&state.db, auth.user.id).await?;
    let exclude: Vec<CredentialID> = credentials(&state.db, auth.user.id)
        .await?
        .into_iter()
        .map(|r| r.credential.0.cred_id.clone())
        .collect();
    let exclude = Some(exclude).filter(|e| !e.is_empty());
    let w = webauthn(&state)?;
    let display = auth
        .user
        .name
        .clone()
        .unwrap_or_else(|| auth.user.login.clone());
    let err = |e: WebauthnError| ApiError::internal(anyhow::anyhow!("webauthn: {e}"));
    let (options, id) = if kind == "passkey" {
        let (mut options, ceremony) = w
            .start_passkey_registration(handle, &auth.user.login, &display, exclude)
            .map_err(err)?;
        // Passkeys sign in without a username: ask for a discoverable
        // (resident) credential.
        if let Some(sel) = options.public_key.authenticator_selection.as_mut() {
            sel.require_resident_key = true;
            sel.resident_key = Some(webauthn_rs_proto::ResidentKeyRequirement::Required);
        }
        let id = save_ceremony(&state, "register", Some(auth.user.id), kind, &ceremony).await?;
        (options, id)
    } else {
        let (options, ceremony) = w
            .start_securitykey_registration(handle, &auth.user.login, &display, exclude, None, None)
            .map_err(err)?;
        let id = save_ceremony(&state, "register", Some(auth.user.id), kind, &ceremony).await?;
        (options, id)
    };
    Ok((StatusCode::CREATED, Json(ChallengeJson { id, options })))
}

#[derive(Debug, Deserialize)]
pub struct FinishRegistrationBody {
    #[serde(default)]
    pub name: String,
    pub credential: RegisterPublicKeyCredential,
}

fn valid_name(name: &str) -> ApiResult<String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > MAX_NAME_LEN {
        return Err(ApiError::invalid_field(FieldError::custom(
            "WebauthnCredential",
            "name",
            format!("name must be 1 to {MAX_NAME_LEN} characters"),
        )));
    }
    Ok(name.to_string())
}

/// `POST /_bgh/user/webauthn/registrations/{id} {name, credential}` → 201.
pub async fn finish_registration(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(id): Path<String>,
    Json(body): Json<FinishRegistrationBody>,
) -> ApiResult<(StatusCode, Json<CredentialJson>)> {
    util::require_session(&auth)?;
    bgh_core::sudo::require(&state, &auth).await?;
    let name = valid_name(&body.name)?;
    let w = webauthn(&state)?;
    let (kind, stored): (String, Value) =
        take_ceremony(&state, "register", &id, Some(auth.user.id)).await?;
    let credential: Credential = match kind.as_str() {
        "passkey" => {
            let ceremony: PasskeyRegistration =
                serde_json::from_value(stored).map_err(ApiError::internal)?;
            w.finish_passkey_registration(&body.credential, &ceremony)
                .map_err(|_| verification_failed())?
                .into()
        }
        _ => {
            let ceremony: SecurityKeyRegistration =
                serde_json::from_value(stored).map_err(ApiError::internal)?;
            w.finish_securitykey_registration(&body.credential, &ceremony)
                .map_err(|_| verification_failed())?
                .into()
        }
    };
    let mut tx = Tx::begin(&state).await?;
    let row: CredentialRow = sqlx::query_as(&format!(
        "INSERT INTO user_webauthn_credentials (user_id, kind, name, credential_id, credential)
         VALUES ($1, $2, $3, $4, $5) RETURNING {CREDENTIAL_COLUMNS}"
    ))
    .bind(auth.user.id)
    .bind(&kind)
    .bind(&name)
    .bind(credential.cred_id.as_ref())
    .bind(sqlx::types::Json(&credential))
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| match bgh_core::db::unique_violation(&e).as_deref() {
        Some("user_webauthn_credentials_credential_id_key") => {
            ApiError::invalid_field(FieldError::custom(
                "WebauthnCredential",
                "credential",
                "This security key is already registered",
            ))
        }
        _ => e.into(),
    })?;
    audit::log(
        &mut *tx,
        Some(&auth.user),
        if kind == "passkey" {
            "passkey.register"
        } else {
            "two_factor_authentication.add_security_key"
        },
        audit::Target::User(auth.user.id),
        json!({ "id": row.id, "name": name }),
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(CredentialJson::from(&row))))
}

#[derive(Debug, Deserialize)]
pub struct RenameBody {
    #[serde(default)]
    pub name: String,
}

/// `PATCH /_bgh/user/webauthn/{id} {name}` → the renamed credential.
pub async fn rename(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(id): Path<i64>,
    Json(body): Json<RenameBody>,
) -> ApiResult<Json<CredentialJson>> {
    util::require_session(&auth)?;
    let name = valid_name(&body.name)?;
    let row: CredentialRow = sqlx::query_as(&format!(
        "UPDATE user_webauthn_credentials SET name = $3 WHERE id = $1 AND user_id = $2
         RETURNING {CREDENTIAL_COLUMNS}"
    ))
    .bind(id)
    .bind(auth.user.id)
    .bind(&name)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)?;
    Ok(Json(CredentialJson::from(&row)))
}

/// `DELETE /_bgh/user/webauthn/{id}` → 204 (sudo).
pub async fn delete(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    util::require_session(&auth)?;
    bgh_core::sudo::require(&state, &auth).await?;
    let mut tx = Tx::begin(&state).await?;
    let removed: Option<(String, String)> = sqlx::query_as(
        "DELETE FROM user_webauthn_credentials WHERE id = $1 AND user_id = $2 RETURNING kind, name",
    )
    .bind(id)
    .bind(auth.user.id)
    .fetch_optional(&mut *tx)
    .await?;
    let (kind, name) = removed.ok_or(ApiError::NotFound)?;
    audit::log(
        &mut *tx,
        Some(&auth.user),
        if kind == "passkey" {
            "passkey.remove"
        } else {
            "two_factor_authentication.remove_security_key"
        },
        audit::Target::User(auth.user.id),
        json!({ "id": id, "name": name }),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Passwordless sign-in with a passkey
// ---------------------------------------------------------------------------

/// `POST /_bgh/auth/login/passkey/challenge` → `{id, options}`.
pub async fn passkey_challenge(
    State(state): State<AppState>,
    client: ClientInfo,
) -> ApiResult<Json<ChallengeJson<RequestChallengeResponse>>> {
    if ratelimit::hit(&state, &format!("passkey_ip:{}", client.ip), 900).await? > 100 {
        return Err(ApiError::Status(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many sign-in attempts. Please try again later.".into(),
        ));
    }
    let (mut options, ceremony) = webauthn(&state)?
        .start_discoverable_authentication()
        .map_err(|e| ApiError::internal(anyhow::anyhow!("webauthn: {e}")))?;
    // A modal prompt from the "Sign in with a passkey" button; the client
    // may still use conditional mediation (autofill) with these options.
    options.mediation = None;
    let id = save_ceremony(&state, "passkey", None, "", &ceremony).await?;
    Ok(Json(ChallengeJson { id, options }))
}

#[derive(Debug, Deserialize)]
pub struct AssertionBody {
    #[serde(default)]
    pub id: String,
    pub credential: PublicKeyCredential,
    #[serde(default, alias = "twoFactorToken")]
    pub two_factor_token: String,
}

/// `POST /_bgh/auth/login/passkey {id, credential}` → 200 boot + cookie;
/// 422 when the passkey isn't registered or doesn't verify.
pub async fn passkey_login(
    State(state): State<AppState>,
    client: ClientInfo,
    Json(body): Json<AssertionBody>,
) -> ApiResult<Response> {
    let (_, ceremony): (String, DiscoverableAuthentication) =
        take_ceremony(&state, "passkey", &body.id, None).await?;
    let w = webauthn(&state)?;
    let (handle, _) = w
        .identify_discoverable_authentication(&body.credential)
        .map_err(|_| verification_failed())?;
    let user_id: Option<i64> =
        sqlx::query_scalar("SELECT user_id FROM user_webauthn_handles WHERE handle = $1")
            .bind(handle)
            .fetch_optional(&state.db)
            .await?;
    let user_id = user_id.ok_or_else(verification_failed)?;
    let rows: Vec<CredentialRow> = credentials(&state.db, user_id)
        .await?
        .into_iter()
        .filter(|r| r.kind == "passkey")
        .collect();
    let keys: Vec<DiscoverableKey> = rows
        .iter()
        .map(|r| DiscoverableKey::from(&Passkey::from(r.credential.0.clone())))
        .collect();
    let res = w
        .finish_discoverable_authentication(&body.credential, ceremony, &keys)
        .map_err(|_| verification_failed())?;
    let cred_id = record_use(&state, &rows, &res).await?;
    let user = db::User::find(&state.db, user_id)
        .await?
        .ok_or_else(verification_failed)?;
    if user.is_suspended() {
        return Err(ApiError::forbidden("Sorry. Your account was suspended."));
    }
    audit::log_with_ip(
        &state.db,
        Some(&user),
        "user.login",
        audit::Target::User(user.id),
        json!({ "method": "passkey", "credential_id": cred_id }),
        Some(client.ip.as_str()),
    )
    .await?;
    boot::signed_in(&state, &client, &user, StatusCode::OK).await
}

// ---------------------------------------------------------------------------
// Second factor of a password sign-in
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct PendingBody {
    #[serde(default, alias = "twoFactorToken")]
    pub two_factor_token: String,
}

/// `POST /_bgh/auth/2fa/webauthn/challenge {twoFactorToken}` → `{id,
/// options}`; 422 when the account has no security keys.
pub async fn two_factor_challenge(
    State(state): State<AppState>,
    Json(body): Json<PendingBody>,
) -> ApiResult<Json<ChallengeJson<RequestChallengeResponse>>> {
    let pending = session::pending_login(&state, &body.two_factor_token).await?;
    start_assertion(&state, "2fa", pending.user_id, &pending.hash)
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::unprocessable("No security keys are registered for this account."))
}

/// `POST /_bgh/auth/2fa/webauthn {twoFactorToken, id, credential}` → 200
/// boot + cookie; 422 when verification fails.
pub async fn two_factor_login(
    State(state): State<AppState>,
    client: ClientInfo,
    Json(body): Json<AssertionBody>,
) -> ApiResult<Response> {
    let pending = session::pending_login(&state, &body.two_factor_token).await?;
    pending.count_attempt(&state).await?;
    if !finish_assertion(
        &state,
        "2fa",
        pending.user_id,
        &pending.hash,
        &body.id,
        &body.credential,
    )
    .await?
    {
        return Err(verification_failed());
    }
    let user = pending.complete(&state).await?;
    boot::signed_in(&state, &client, &user, StatusCode::OK).await
}
