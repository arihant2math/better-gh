//! Web-client authentication: sign-up, login (password + second factor),
//! logout, session management, password change and reset.
//!
//! * `POST /_bgh/signup` → 201 private-user + session cookie
//! * `POST /_bgh/session {login, password, otp?}` → 200 private-user +
//!   cookie, or 202 `{two_factor_required, two_factor_token}` when the
//!   account has 2FA (then `POST /_bgh/session/two_factor {two_factor_token,
//!   code}`)
//! * `DELETE /_bgh/session` → 204
//! * `GET /_bgh/sessions`, `DELETE /_bgh/sessions/{id}`, `DELETE /_bgh/sessions`
//! * `PUT /_bgh/user/password {current_password, password}`
//! * `POST /_bgh/password_reset {email}`, `GET|POST /_bgh/password_reset/{token}`
//!
//! Failed logins are throttled per login (10 / 15 min) and per IP (50 / 15
//! min); second-factor attempts per pending login (5).

use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bgh_core::audit;
use bgh_core::auth::{self, AuthMethod};
use bgh_core::crypto;
use bgh_core::mail;
use bgh_core::prelude::*;
use bgh_core::ratelimit;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::twofa;
use crate::users::{self, NewAccount};
use crate::util::{self, ClientInfo};
use crate::validate;

use bgh_core::auth::{LOGIN_WINDOW_SECS, MAX_FAILS_PER_LOGIN};
const PENDING_2FA_TTL_SECS: u64 = 300;
const MAX_2FA_ATTEMPTS: u64 = 5;
const RESET_TTL_MINUTES: i64 = 60;

fn too_many(msg: &str) -> ApiError {
    ApiError::Status(StatusCode::TOO_MANY_REQUESTS, msg.into())
}

#[derive(Debug, Deserialize)]
pub struct SignupBody {
    #[serde(default)]
    pub login: String,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub password: String,
    pub name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct LoginBody {
    /// Login or verified email.
    #[serde(default)]
    pub login: String,
    #[serde(default)]
    pub password: String,
    /// Optional second factor, to log in with a single request.
    pub otp: Option<String>,
}

/// Create a session for `user` and respond with its private profile and the
/// session cookie.
pub async fn start_session(
    state: &AppState,
    client: &ClientInfo,
    user: &db::User,
    status: StatusCode,
) -> ApiResult<Response> {
    let token = auth::create_session(
        state,
        user.id,
        client.user_agent.as_deref(),
        Some(&client.ip),
    )
    .await?;
    if status == StatusCode::OK {
        audit::log_with_ip(
            &state.db,
            Some(user),
            "user.login",
            audit::Target::User(user.id),
            json!({}),
            Some(client.ip.as_str()),
        )
        .await?;
    }
    let body = users::private_user_json(state, user).await?;
    Ok((
        status,
        [(
            header::SET_COOKIE,
            auth::session_cookie(&state.config, &token),
        )],
        Json(body),
    )
        .into_response())
}

/// Outcome of a successful first factor.
pub enum LoginStep {
    /// No second factor needed.
    Done,
    /// The second factor must be presented with this pending-login token.
    TwoFactor(String),
}

/// After the first factor (password, SSO): either finish, or open a
/// pending login that waits for the second factor.
pub async fn after_first_factor(state: &AppState, user: &db::User) -> ApiResult<LoginStep> {
    if !util::two_factor_enabled(&state.db, user.id).await? {
        return Ok(LoginStep::Done);
    }
    let token = crypto::random_token(40);
    let mut redis = state.redis.clone();
    let _: () = redis
        .set_ex(
            state.redis_key(&format!("2fa_pending:{}", crypto::sha256_hex(&token))),
            user.id,
            PENDING_2FA_TTL_SECS,
        )
        .await?;
    Ok(LoginStep::TwoFactor(token))
}

#[derive(Debug, Serialize)]
struct TwoFactorRequired<'a> {
    message: &'a str,
    two_factor_required: bool,
    two_factor_token: String,
    methods: [&'a str; 2],
}

fn two_factor_response(token: String) -> Response {
    let mut resp = (
        StatusCode::ACCEPTED,
        Json(TwoFactorRequired {
            message: "Must specify two-factor authentication OTP code.",
            two_factor_required: true,
            two_factor_token: token,
            methods: ["totp", "recovery_code"],
        }),
    )
        .into_response();
    resp.headers_mut()
        .insert("x-github-otp", HeaderValue::from_static("required; app"));
    resp
}

/// `POST /_bgh/signup` → 201 + session cookie. The first account becomes
/// site admin. 403 when sign-up is disabled.
pub async fn signup(
    State(state): State<AppState>,
    client: ClientInfo,
    Json(body): Json<SignupBody>,
) -> ApiResult<Response> {
    admit_signup(&state, &client, body.email.trim()).await?;
    let user = users::create_user(
        &state,
        NewAccount {
            login: body.login.trim(),
            email: body.email.trim(),
            password: &body.password,
            name: body.name.as_deref(),
            site_admin: None,
        },
        None,
    )
    .await?;
    start_session(&state, &client, &user, StatusCode::CREATED).await
}

/// Gate shared by every self-service password sign-up route
/// (`/_bgh/signup`, `/_bgh/auth/signup`): `BGH_SIGNUP_ENABLED`, the per-IP
/// rate limit (429) and the site sign-up policy
/// ([`bgh_core::settings::check_signup`], 403). Call before creating the
/// account.
pub(crate) async fn admit_signup(
    state: &AppState,
    client: &ClientInfo,
    email: &str,
) -> ApiResult<()> {
    if !state.config.signup_enabled {
        return Err(ApiError::forbidden("Sign up is disabled on this instance."));
    }
    if ratelimit::hit(state, &format!("signup_ip:{}", client.ip), 3600).await? > 50 {
        return Err(too_many("Too many sign ups. Please try again later."));
    }
    bgh_core::settings::check_signup(state, email).await
}

fn login_key(login: &str) -> String {
    auth::login_fail_key(login)
}

/// Result of checking a login + password.
pub enum PasswordLogin {
    /// Fully authenticated.
    Done(Box<db::User>),
    /// Second factor required: pending-login token.
    TwoFactor(String),
}

/// Check credentials ([`auth::check_password`]: directory/LDAP, the
/// `password_login` policy, throttling (429), suspension (403)) and 2FA.
/// Bad credentials → 401 "Bad credentials".
pub async fn password_login(
    state: &AppState,
    client: &ClientInfo,
    login: &str,
    password: &str,
) -> ApiResult<PasswordLogin> {
    let user = auth::check_password(
        state,
        login,
        password,
        auth::PasswordTransport::Web,
        &client.ip,
    )
    .await?;
    Ok(match after_first_factor(state, &user).await? {
        LoginStep::Done => PasswordLogin::Done(Box::new(user)),
        LoginStep::TwoFactor(token) => PasswordLogin::TwoFactor(token),
    })
}

/// `POST /_bgh/session` → 200 + session cookie, 202 when a second factor is
/// required, 401 on bad credentials, 429 when throttled.
pub async fn login(
    State(state): State<AppState>,
    client: ClientInfo,
    Json(body): Json<LoginBody>,
) -> ApiResult<Response> {
    match password_login(&state, &client, &body.login, &body.password).await? {
        PasswordLogin::Done(user) => start_session(&state, &client, &user, StatusCode::OK).await,
        PasswordLogin::TwoFactor(token) => match body.otp.as_deref().filter(|c| !c.is_empty()) {
            Some(code) => {
                let user = verify_pending_two_factor(&state, &token, code).await?;
                start_session(&state, &client, &user, StatusCode::OK).await
            }
            None => Ok(two_factor_response(token)),
        },
    }
}

#[derive(Debug, Deserialize)]
pub struct TwoFactorBody {
    #[serde(default, alias = "twoFactorToken")]
    pub two_factor_token: String,
    #[serde(default)]
    pub code: String,
}

/// A pending login waiting for its second factor (see [`after_first_factor`]).
pub struct PendingLogin {
    pub user_id: i64,
    /// sha256 of the pending-login token.
    pub hash: String,
    key: String,
}

/// Resolve a pending-login token; 401 when it expired.
pub async fn pending_login(state: &AppState, token: &str) -> ApiResult<PendingLogin> {
    let hash = crypto::sha256_hex(token.trim());
    let key = state.redis_key(&format!("2fa_pending:{hash}"));
    let mut redis = state.redis.clone();
    let user_id: Option<i64> = redis.get(&key).await?;
    let user_id = user_id.ok_or_else(|| ApiError::Unauthorized {
        message: "Two-factor login expired. Please sign in again.".into(),
        www_authenticate: None,
    })?;
    Ok(PendingLogin { user_id, hash, key })
}

impl PendingLogin {
    /// Count a second-factor attempt; 429 (and the pending login is
    /// dropped) after [`MAX_2FA_ATTEMPTS`].
    pub async fn count_attempt(&self, state: &AppState) -> ApiResult<()> {
        let attempts = ratelimit::hit(
            state,
            &format!("2fa_attempts:{}", self.hash),
            PENDING_2FA_TTL_SECS,
        )
        .await?;
        if attempts > MAX_2FA_ATTEMPTS {
            let mut redis = state.redis.clone();
            let _: Result<(), _> = redis.del(&self.key).await;
            return Err(too_many(
                "Too many two-factor attempts. Please sign in again.",
            ));
        }
        Ok(())
    }

    /// The second factor was verified: consume the pending login and
    /// return the user (403 when suspended meanwhile).
    pub async fn complete(self, state: &AppState) -> ApiResult<db::User> {
        let mut redis = state.redis.clone();
        let _: Result<(), _> = redis.del(&self.key).await;
        let user = db::User::find(&state.db, self.user_id)
            .await?
            .ok_or_else(ApiError::bad_credentials)?;
        if user.is_suspended() {
            audit::log_with_ip(
                &state.db,
                Some(&user),
                "user.failed_login",
                audit::Target::User(user.id),
                json!({ "reason": "suspended" }),
                None,
            )
            .await?;
            return Err(ApiError::forbidden("Sorry. Your account was suspended."));
        }
        Ok(user)
    }
}

/// Check the second factor of a pending login; returns the user and
/// consumes the pending login on success.
pub async fn verify_pending_two_factor(
    state: &AppState,
    token: &str,
    code: &str,
) -> ApiResult<db::User> {
    let pending = pending_login(state, token).await?;
    pending.count_attempt(state).await?;
    if !twofa::verify_second_factor(state, pending.user_id, code).await? {
        return Err(ApiError::Unauthorized {
            message: "Two-factor authentication failed.".into(),
            www_authenticate: None,
        });
    }
    pending.complete(state).await
}

/// `POST /_bgh/session/two_factor {two_factor_token, code}` → 200 + cookie.
/// `code` is a TOTP code or a recovery code.
pub async fn two_factor(
    State(state): State<AppState>,
    client: ClientInfo,
    Json(body): Json<TwoFactorBody>,
) -> ApiResult<Response> {
    let user = verify_pending_two_factor(&state, &body.two_factor_token, &body.code).await?;
    start_session(&state, &client, &user, StatusCode::OK).await
}

/// `DELETE /_bgh/session` → 204, clears the cookie.
pub async fn logout(
    State(state): State<AppState>,
    client: ClientInfo,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let caller = auth::authenticate(&state, &headers, Default::default())
        .await
        .ok()
        .flatten();
    end_cookie_session(&state, &headers).await?;
    if let Some(ctx) = &caller {
        audit::log_with_ip(
            &state.db,
            Some(&ctx.user),
            "user.logout",
            audit::Target::User(ctx.user.id),
            json!({}),
            Some(client.ip.as_str()),
        )
        .await?;
    }
    Ok((
        StatusCode::NO_CONTENT,
        [(
            header::SET_COOKIE,
            auth::clear_session_cookie(&state.config),
        )],
    )
        .into_response())
}

/// Destroy the session of the request's cookie (if any) and tell sync to
/// close its sockets ([`Event::SessionEnded`]).
pub async fn end_cookie_session(state: &AppState, headers: &HeaderMap) -> ApiResult<()> {
    let Some(token) = auth::cookie(headers, auth::SESSION_COOKIE) else {
        return Ok(());
    };
    let row: Option<(i64, i64)> =
        sqlx::query_as("SELECT id, user_id FROM sessions WHERE token_hash = $1")
            .bind(crypto::sha256_hex(&token))
            .fetch_optional(&state.db)
            .await?;
    auth::destroy_session(state, &token).await?;
    if let Some((session_id, user_id)) = row {
        state.events.emit(Event::SessionEnded {
            user_id,
            session_id: Some(session_id),
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct SessionJson {
    pub id: i64,
    pub user_agent: Option<String>,
    pub ip: Option<String>,
    pub created_at: Timestamp,
    pub last_seen_at: Timestamp,
    pub expires_at: Timestamp,
    pub current: bool,
}

/// (id, user_agent, ip, created, last_seen, expires).
type SessionRow = (
    i64,
    Option<String>,
    Option<String>,
    chrono::DateTime<chrono::Utc>,
    chrono::DateTime<chrono::Utc>,
    chrono::DateTime<chrono::Utc>,
);

fn current_session(auth: &AuthContext) -> Option<i64> {
    match auth.method {
        AuthMethod::Session { session_id } => Some(session_id),
        _ => None,
    }
}

/// `GET /_bgh/sessions` → the user's active sessions, newest activity first.
pub async fn list_sessions(
    State(state): State<AppState>,
    auth: RequireUser,
) -> ApiResult<Json<Vec<SessionJson>>> {
    util::require_session(&auth)?;
    let rows: Vec<SessionRow> = sqlx::query_as(
        "SELECT id, user_agent, ip, created_at, last_seen_at, expires_at FROM sessions
              WHERE user_id = $1 AND expires_at > now() ORDER BY last_seen_at DESC, id DESC",
    )
    .bind(auth.user.id)
    .fetch_all(&state.db)
    .await?;
    let current = current_session(&auth);
    Ok(Json(
        rows.into_iter()
            .map(|(id, ua, ip, created, seen, expires)| SessionJson {
                id,
                user_agent: ua,
                ip,
                created_at: created.into(),
                last_seen_at: seen.into(),
                expires_at: expires.into(),
                current: Some(id) == current,
            })
            .collect(),
    ))
}

/// Delete sessions by id (and evict their Redis cache entries).
async fn revoke(
    state: &AppState,
    user_id: i64,
    ids: Option<&[i64]>,
    keep: Option<i64>,
) -> ApiResult<u64> {
    let rows: Vec<(i64, String)> = sqlx::query_as(
        "DELETE FROM sessions WHERE user_id = $1 AND ($2::bigint[] IS NULL OR id = ANY($2))
           AND ($3::bigint IS NULL OR id <> $3) RETURNING id, token_hash",
    )
    .bind(user_id)
    .bind(ids)
    .bind(keep)
    .fetch_all(&state.db)
    .await?;
    for (id, _) in &rows {
        state.events.emit(Event::SessionEnded {
            user_id,
            session_id: Some(*id),
        });
    }
    let hashes: Vec<String> = rows.into_iter().map(|(_, h)| h).collect();
    if !hashes.is_empty() {
        let keys: Vec<String> = hashes
            .iter()
            .map(|h| state.redis_key(&format!("session:{h}")))
            .collect();
        let mut redis = state.redis.clone();
        let _: Result<(), _> = redis.del(keys).await;
    }
    Ok(hashes.len() as u64)
}

/// `DELETE /_bgh/sessions/{id}` → 204.
pub async fn revoke_session(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    util::require_session(&auth)?;
    if revoke(&state, auth.user.id, Some(&[id]), None).await? == 0 {
        return Err(ApiError::NotFound);
    }
    audit::log(
        &state.db,
        Some(&auth.user),
        "user.session_revoked",
        audit::Target::User(auth.user.id),
        json!({ "session_id": id }),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /_bgh/sessions` → 204, revokes every session except the current.
pub async fn revoke_other_sessions(
    State(state): State<AppState>,
    auth: RequireUser,
) -> ApiResult<StatusCode> {
    util::require_session(&auth)?;
    revoke(&state, auth.user.id, None, current_session(&auth)).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Passwords
// ---------------------------------------------------------------------------

fn password_error() -> ApiError {
    ApiError::invalid_field(FieldError::custom(
        "User",
        "password",
        format!(
            "password must be at least {} characters",
            validate::MIN_PASSWORD_LEN
        ),
    ))
}

async fn set_password(tx: &mut Tx, user_id: i64, password: &str) -> ApiResult<()> {
    let password = password.to_string();
    let hash = tokio::task::spawn_blocking(move || crypto::hash_password(&password)).await??;
    sqlx::query("UPDATE users SET password_hash = $2, updated_at = now() WHERE id = $1")
        .bind(user_id)
        .bind(hash)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct ChangePasswordBody {
    #[serde(default)]
    pub current_password: String,
    #[serde(default)]
    pub password: String,
}

/// `PUT /_bgh/user/password` → 204; other sessions are signed out.
pub async fn change_password(
    State(state): State<AppState>,
    auth: RequireUser,
    Json(body): Json<ChangePasswordBody>,
) -> ApiResult<StatusCode> {
    util::require_session(&auth)?;
    let key = login_key(&auth.user.login);
    if ratelimit::count(&state, &key).await >= MAX_FAILS_PER_LOGIN {
        return Err(too_many(
            "Too many failed attempts. Please try again later.",
        ));
    }
    if auth.user.password_hash.is_some()
        && auth::verify_login(&state, &auth.user.login, &body.current_password)
            .await?
            .is_none()
    {
        ratelimit::hit(&state, &key, LOGIN_WINDOW_SECS).await?;
        return Err(ApiError::invalid_field(FieldError::custom(
            "User",
            "current_password",
            "current password is incorrect",
        )));
    }
    if !validate::is_valid_password(&body.password) {
        return Err(password_error());
    }
    let mut tx = Tx::begin(&state).await?;
    set_password(&mut tx, auth.user.id, &body.password).await?;
    audit::log(
        &mut *tx,
        Some(&auth.user),
        "user.change_password",
        audit::Target::User(auth.user.id),
        json!({}),
    )
    .await?;
    if let Some(email) = util::primary_email(&mut *tx, auth.user.id).await? {
        util::queue_mail(
            &mut tx,
            mail::templates::password_changed(&state.config.site_name, &email, &auth.user.login),
        )
        .await?;
    }
    tx.commit().await?;
    revoke(&state, auth.user.id, None, current_session(&auth)).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
pub struct ResetRequestBody {
    /// Email address (or login).
    #[serde(default, alias = "login")]
    pub email: String,
}

/// `POST /_bgh/password_reset {email}` → 202 (always, to avoid account
/// enumeration). Mails a one-hour reset link to the primary address.
pub async fn request_reset(
    State(state): State<AppState>,
    client: ClientInfo,
    Json(body): Json<ResetRequestBody>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let ident = body.email.trim().to_lowercase();
    if ident.is_empty() {
        return Err(ApiError::invalid_field(FieldError::missing_field(
            "User", "email",
        )));
    }
    let accepted = (
        StatusCode::ACCEPTED,
        Json(json!({ "message": "If the account exists, a password reset email is on its way." })),
    );
    if ratelimit::hit(&state, &format!("reset_ip:{}", client.ip), 3600).await? > 20
        || ratelimit::hit(&state, &format!("reset:{ident}"), 3600).await? > 5
    {
        return Err(too_many(
            "Too many password reset requests. Please try again later.",
        ));
    }
    let user: Option<db::User> = sqlx::query_as(&format!(
        "SELECT {} FROM users u WHERE u.type = 'User' AND u.suspended_at IS NULL
           AND (lower(u.login) = $1
                OR u.id = (SELECT user_id FROM user_emails WHERE lower(email) = $1 AND verified))",
        db::prefixed("u", db::User::COLUMNS)
    ))
    .bind(&ident)
    .fetch_optional(&state.db)
    .await?;
    let Some(user) = user else {
        return Ok(accepted);
    };
    let Some(to) = util::primary_email(&state.db, user.id).await? else {
        return Ok(accepted);
    };
    let mut tx = Tx::begin(&state).await?;
    let token = util::create_account_token(
        &mut tx,
        user.id,
        "password_reset",
        None,
        chrono::Duration::minutes(RESET_TTL_MINUTES),
    )
    .await?;
    let link = state.urls.html(&format!("/password_reset/{token}"));
    util::queue_mail(
        &mut tx,
        mail::templates::password_reset(
            &state.config.site_name,
            &to,
            &user.login,
            &link,
            RESET_TTL_MINUTES,
        ),
    )
    .await?;
    audit::log(
        &mut *tx,
        None,
        "user.request_password_reset",
        audit::Target::User(user.id),
        json!({ "ip": client.ip }),
    )
    .await?;
    tx.commit().await?;
    Ok(accepted)
}

#[derive(Debug, Serialize)]
pub struct ResetInfo {
    pub login: String,
    pub two_factor_required: bool,
}

/// `GET /_bgh/password_reset/{token}` → 200 `{login, two_factor_required}`
/// for a valid token, else 404.
pub async fn check_reset(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> ApiResult<Json<ResetInfo>> {
    let (_, user_id, _) = util::find_account_token(&state.db, "password_reset", &token)
        .await?
        .ok_or(ApiError::NotFound)?;
    let user = db::User::find(&state.db, user_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(ResetInfo {
        login: user.login,
        two_factor_required: util::two_factor_enabled(&state.db, user_id).await?,
    }))
}

#[derive(Debug, Deserialize)]
pub struct ResetBody {
    #[serde(default)]
    pub password: String,
    /// Required when the account has two-factor authentication.
    pub otp: Option<String>,
}

/// `POST /_bgh/password_reset/{token} {password, otp?}` → 204; every
/// session of the user is signed out.
pub async fn reset_password(
    State(state): State<AppState>,
    Path(token): Path<String>,
    Json(body): Json<ResetBody>,
) -> ApiResult<StatusCode> {
    let (_, user_id, _) = util::find_account_token(&state.db, "password_reset", &token)
        .await?
        .ok_or(ApiError::NotFound)?;
    if !validate::is_valid_password(&body.password) {
        return Err(password_error());
    }
    if util::two_factor_enabled(&state.db, user_id).await? {
        let key = format!("reset_2fa:{}", crypto::sha256_hex(&token));
        if ratelimit::hit(&state, &key, 3600).await? > MAX_2FA_ATTEMPTS {
            return Err(too_many("Too many two-factor attempts."));
        }
        let ok = match body.otp.as_deref() {
            Some(code) => twofa::verify_second_factor(&state, user_id, code).await?,
            None => false,
        };
        if !ok {
            return Err(ApiError::invalid_field(FieldError::custom(
                "User",
                "otp",
                "a valid two-factor code is required",
            )));
        }
    }
    let mut tx = Tx::begin(&state).await?;
    // Single use: consumes every outstanding reset token of the user.
    let deleted = sqlx::query(
        "DELETE FROM account_tokens WHERE user_id = $1 AND kind = 'password_reset'
           AND token_hash = $2",
    )
    .bind(user_id)
    .bind(crypto::sha256_hex(&token))
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if deleted == 0 {
        return Err(ApiError::NotFound);
    }
    sqlx::query("DELETE FROM account_tokens WHERE user_id = $1 AND kind = 'password_reset'")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    set_password(&mut tx, user_id, &body.password).await?;
    audit::log(
        &mut *tx,
        None,
        "user.reset_password",
        audit::Target::User(user_id),
        json!({}),
    )
    .await?;
    tx.commit().await?;
    auth::destroy_user_sessions(&state, user_id).await?;
    state.events.emit(Event::SessionEnded {
        user_id,
        session_id: None,
    });
    if let Ok(Some(user)) = db::User::find(&state.db, user_id).await {
        ratelimit::clear(&state, &login_key(&user.login)).await;
    }
    Ok(StatusCode::NO_CONTENT)
}
