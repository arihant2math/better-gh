//! Account security policies (P36):
//!
//! * sudo mode endpoints (`bgh_core::sudo` holds the check):
//!   `GET /_bgh/sudo` → `{active, expires_at, methods}`,
//!   `POST /_bgh/sudo/webauthn/challenge` → `{id, options}`,
//!   `POST /_bgh/sudo {password | otp | webauthn: {id, credential}}` → 200;
//! * the site's `auth_providers.require_2fa` policy: boot flags the user
//!   (`twoFactorSetupRequired`) and [`require_two_factor_middleware`] answers
//!   403 to their cookie sessions outside the setup endpoints (tokens are
//!   unaffected);
//! * PAT expiry reminders (7 days and 1 day before), sent by the
//!   `accounts.security` service, which also encrypts legacy plaintext TOTP
//!   secrets at start-up.

use std::time::Duration;

use axum::extract::{Request, State};
use axum::http::{Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use bgh_core::audit;
use bgh_core::auth::{self, AuthMethod};
use bgh_core::mail;
use bgh_core::prelude::*;
use bgh_core::ratelimit;
use bgh_core::sudo;
use bgh_core::time::ts;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio_util::sync::CancellationToken;
use webauthn_rs::prelude::{PublicKeyCredential, RequestChallengeResponse};

use crate::util;
use crate::webauthn::{self, ChallengeJson};

// ---------------------------------------------------------------------------
// Sudo mode
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct SudoMethods {
    pub password: bool,
    pub totp: bool,
    pub webauthn: bool,
}

#[derive(Debug, Serialize)]
pub struct SudoStatus {
    pub active: bool,
    pub expires_at: Option<Timestamp>,
    /// Ways this account can re-authenticate.
    pub methods: SudoMethods,
}

fn session_id(auth: &AuthContext) -> ApiResult<i64> {
    match auth.method {
        AuthMethod::Session { session_id } => Ok(session_id),
        _ => Err(ApiError::forbidden(
            "This action requires a browser session.",
        )),
    }
}

async fn status_for(
    state: &AppState,
    auth: &AuthContext,
    expires: Option<chrono::DateTime<chrono::Utc>>,
) -> ApiResult<SudoStatus> {
    let (keys, passkeys) = webauthn::counts(&state.db, auth.user.id).await?;
    Ok(SudoStatus {
        active: expires.is_some(),
        expires_at: ts(expires),
        methods: SudoMethods {
            password: auth.user.password_hash.is_some(),
            totp: util::two_factor_enabled(&state.db, auth.user.id).await?,
            webauthn: keys + passkeys > 0,
        },
    })
}

/// `GET /_bgh/sudo`
pub async fn sudo_status(
    State(state): State<AppState>,
    auth: RequireUser,
) -> ApiResult<Json<SudoStatus>> {
    let sid = session_id(&auth)?;
    let expires = sudo::expires_at(&state, sid).await?;
    Ok(Json(status_for(&state, &auth, expires).await?))
}

/// `POST /_bgh/sudo/webauthn/challenge` → `{id, options}`; 422 without
/// registered keys.
pub async fn sudo_challenge(
    State(state): State<AppState>,
    auth: RequireUser,
) -> ApiResult<Json<ChallengeJson<RequestChallengeResponse>>> {
    let sid = session_id(&auth)?;
    webauthn::start_assertion(&state, "sudo", auth.user.id, &sid.to_string())
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::unprocessable("No security keys are registered for this account."))
}

#[derive(Debug, Deserialize)]
pub struct WebauthnAssertion {
    #[serde(default)]
    pub id: String,
    pub credential: PublicKeyCredential,
}

#[derive(Debug, Deserialize)]
pub struct SudoBody {
    pub password: Option<String>,
    /// TOTP or recovery code.
    pub otp: Option<String>,
    pub webauthn: Option<WebauthnAssertion>,
}

/// `POST /_bgh/sudo` → 200 status; 403 on a wrong password or code, 429
/// after 10 failures in 15 minutes.
pub async fn sudo(
    State(state): State<AppState>,
    auth: RequireUser,
    Json(body): Json<SudoBody>,
) -> ApiResult<Json<SudoStatus>> {
    let sid = session_id(&auth)?;
    let key = format!("sudo_fail:{}", auth.user.id);
    if ratelimit::count(&state, &key).await >= 10 {
        return Err(ApiError::Status(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many failed attempts. Please try again later.".into(),
        ));
    }
    let (ok, method) = if let Some(a) = &body.webauthn {
        let ok = webauthn::finish_assertion(
            &state,
            "sudo",
            auth.user.id,
            &sid.to_string(),
            &a.id,
            &a.credential,
        )
        .await?;
        (ok, "webauthn")
    } else if let Some(code) = body.otp.as_deref().filter(|c| !c.trim().is_empty()) {
        let ok = crate::twofa::verify_second_factor(&state, auth.user.id, code).await?;
        (ok, "otp")
    } else if let Some(password) = body.password.as_deref().filter(|p| !p.is_empty()) {
        let ok = auth.user.password_hash.is_some()
            && auth::verify_login(&state, &auth.user.login, password)
                .await?
                .is_some_and(|u| u.id == auth.user.id);
        (ok, "password")
    } else {
        return Err(ApiError::invalid_field(FieldError::missing_field(
            "Sudo", "password",
        )));
    };
    if !ok {
        ratelimit::hit(&state, &key, 900).await?;
        return Err(ApiError::forbidden(match method {
            "password" => "Incorrect password.",
            "otp" => "Incorrect two-factor code.",
            _ => "Security key verification failed.",
        }));
    }
    let expires = sudo::grant(&state, sid).await?;
    audit::log(
        &state.db,
        Some(&auth.user),
        "user.sudo",
        audit::Target::User(auth.user.id),
        json!({ "method": method }),
    )
    .await?;
    Ok(Json(status_for(&state, &auth, Some(expires)).await?))
}

// ---------------------------------------------------------------------------
// Two-factor policies
// ---------------------------------------------------------------------------

/// The site requires 2FA and `user_id` has none.
pub async fn two_factor_setup_required(state: &AppState, user_id: i64) -> ApiResult<bool> {
    if !bgh_core::settings::load(state)
        .await?
        .auth_providers
        .require_2fa
    {
        return Ok(false);
    }
    Ok(!util::two_factor_enabled(&state.db, user_id).await?)
}

/// Second-factor methods offered to a pending login.
pub async fn second_factor_methods(state: &AppState, token: &str) -> ApiResult<Vec<&'static str>> {
    let mut methods = vec!["totp", "recovery_code"];
    if let Ok(p) = crate::session::pending_login(state, token).await
        && webauthn::counts(&state.db, p.user_id).await?.0 > 0
    {
        methods.push("webauthn");
    }
    Ok(methods)
}

/// 422 when 2FA can't be turned off: the site or an organization the user
/// belongs to (or collaborates with) requires it.
pub async fn check_two_factor_removable(state: &AppState, user_id: i64) -> ApiResult<()> {
    if bgh_core::settings::load(state)
        .await?
        .auth_providers
        .require_2fa
    {
        return Err(ApiError::unprocessable(
            "Two-factor authentication is required on this site and can't be disabled.",
        ));
    }
    let org: Option<String> = sqlx::query_scalar(
        "SELECT o.login FROM users o JOIN org_settings s ON s.org_id = o.id
          WHERE s.two_factor_requirement_enabled
            AND (EXISTS (SELECT 1 FROM org_members m WHERE m.org_id = o.id AND m.user_id = $1)
                 OR EXISTS (SELECT 1 FROM collaborators c JOIN repositories r ON r.id = c.repo_id
                             WHERE r.owner_id = o.id AND c.user_id = $1))
          ORDER BY o.login LIMIT 1",
    )
    .bind(user_id)
    .fetch_optional(&state.db)
    .await?;
    if let Some(org) = org {
        return Err(ApiError::unprocessable(format!(
            "The @{org} organization requires two-factor authentication. Leave it before disabling two-factor authentication."
        )));
    }
    Ok(())
}

/// Requests a 2FA-less session may still make under `require_2fa`.
fn allowed_without_two_factor(method: &Method, path: &str) -> bool {
    const ALLOWED: &[&str] = &[
        "/_bgh/auth/",
        "/_bgh/session",
        "/_bgh/user/two_factor",
        "/_bgh/user/webauthn",
        "/_bgh/sudo",
    ];
    if ALLOWED.iter().any(|p| path.starts_with(p)) {
        return true;
    }
    let read = matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS);
    if path == "/api/v3" || path.starts_with("/api/v3/") || path.starts_with("/api/graphql") {
        return read && (path == "/api/v3/user" || path == "/api/v3" || path == "/api/v3/");
    }
    // Other private endpoints: reads only (boot, sync bootstrap, site info).
    !path.starts_with("/_bgh/") || read
}

/// Middleware for the site's `require_2fa` policy: cookie sessions of users
/// without 2FA get 403 outside the 2FA setup endpoints. Token and anonymous
/// requests pass through.
pub async fn require_two_factor_middleware(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Response {
    if req.headers().contains_key(header::AUTHORIZATION)
        || auth::cookie(req.headers(), auth::SESSION_COOKIE).is_none()
        || allowed_without_two_factor(req.method(), req.uri().path())
    {
        return next.run(req).await;
    }
    let required = match bgh_core::settings::load(&state).await {
        Ok(s) => s.auth_providers.require_2fa,
        Err(_) => false,
    };
    if !required {
        return next.run(req).await;
    }
    let Ok(Some(ctx)) = auth::resolve_request(&state, &mut req).await else {
        return next.run(req).await;
    };
    if !ctx.is_session()
        || util::two_factor_enabled(&state.db, ctx.user.id)
            .await
            .unwrap_or(true)
    {
        return next.run(req).await;
    }
    ApiError::forbidden(
        "Two-factor authentication is required on this site. Set it up in your security settings (/settings/security) to continue.",
    )
    .into_response()
}

// ---------------------------------------------------------------------------
// Background service: PAT expiry reminders, legacy TOTP encryption
// ---------------------------------------------------------------------------

/// Leader lock key ("acsec").
const LEADER_KEY: i64 = 0x0000_0061_6373_6563;

/// Encrypt plaintext TOTP secrets left by older versions. Returns how many
/// rows were converted.
pub async fn encrypt_legacy_totp(state: &AppState) -> ApiResult<u64> {
    let mut done = 0;
    loop {
        let rows: Vec<(i64, String)> = sqlx::query_as(
            "SELECT user_id, totp_secret FROM user_two_factor
              WHERE totp_secret IS NOT NULL ORDER BY user_id LIMIT 500",
        )
        .fetch_all(&state.db)
        .await?;
        if rows.is_empty() {
            return Ok(done);
        }
        for (user_id, secret) in rows {
            let sealed = bgh_core::secretbox::seal(state, &secret)?;
            done += sqlx::query(
                "UPDATE user_two_factor SET totp_secret_enc = $3, totp_secret = NULL
                  WHERE user_id = $1 AND totp_secret = $2",
            )
            .bind(user_id)
            .bind(&secret)
            .bind(sealed)
            .execute(&state.db)
            .await?
            .rows_affected();
        }
    }
}

/// `(id, user_id, name, expires_at, login)` of a token due for a reminder.
type DueToken = (i64, i64, String, chrono::DateTime<chrono::Utc>, String);

/// Queue the expiry reminder mails due now (rows are locked, not skipped:
/// requests touch `last_used_at` concurrently, and the service's leader
/// lock already keeps passes from overlapping): 7 days before a PAT expires,
/// and again 1 day before. Each reminder is sent once. Returns the number
/// of mails queued.
pub async fn send_expiry_reminders(state: &AppState) -> ApiResult<usize> {
    let mut sent = 0;
    // (window, already-sent column, columns to mark)
    let passes = [
        (
            "t.expires_at <= now() + interval '1 day'",
            "expiry_notified_1d_at",
            "expiry_notified_1d_at = now(), expiry_notified_7d_at = coalesce(expiry_notified_7d_at, now())",
        ),
        (
            "t.expires_at <= now() + interval '7 days'",
            "expiry_notified_7d_at",
            "expiry_notified_7d_at = now()",
        ),
    ];
    for (window, column, mark) in passes {
        let mut tx = Tx::begin(state).await?;
        let due: Vec<DueToken> = sqlx::query_as(&format!(
            "WITH due AS (
                 SELECT t.id FROM access_tokens t
                  WHERE t.kind = 'pat' AND t.expires_at > now() AND {window}
                    AND t.{column} IS NULL
                  ORDER BY t.expires_at LIMIT 1000 FOR UPDATE
             )
             UPDATE access_tokens t SET {mark} FROM due, users u
              WHERE t.id = due.id AND u.id = t.user_id
             RETURNING t.id, t.user_id, t.name, t.expires_at, u.login"
        ))
        .fetch_all(&mut *tx)
        .await?;
        for (_, user_id, name, expires, login) in &due {
            let Some(to) = util::primary_email(&mut *tx, *user_id).await? else {
                continue;
            };
            let email = mail::templates::token_expiring(
                &state.config.site_name,
                &to,
                login,
                name,
                &expires.format("%a, %b %-d %Y %H:%M UTC").to_string(),
                &state.urls.html("/settings/tokens"),
            );
            util::queue_mail(&mut tx, email).await?;
            sent += 1;
        }
        tx.commit().await?;
    }
    Ok(sent)
}

/// `accounts.security` service: encrypts legacy TOTP secrets once, then
/// sends PAT expiry reminders hourly (one leader via a pg advisory lock).
pub async fn service(state: AppState, shutdown: CancellationToken) -> anyhow::Result<()> {
    match encrypt_legacy_totp(&state).await {
        Ok(0) => {}
        Ok(n) => tracing::info!(n, "encrypted legacy TOTP secrets"),
        Err(err) => tracing::warn!(?err, "encrypting legacy TOTP secrets"),
    }
    let mut tick = tokio::time::interval(Duration::from_secs(3600));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return Ok(()),
            _ = tick.tick() => {}
        }
        let mut conn = state.db.acquire().await?;
        let leader: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
            .bind(LEADER_KEY)
            .fetch_one(&mut *conn)
            .await?;
        if !leader {
            continue;
        }
        match send_expiry_reminders(&state).await {
            Ok(0) => {}
            Ok(n) => tracing::info!(n, "queued token expiry reminders"),
            Err(err) => tracing::warn!(?err, "token expiry reminders failed"),
        }
        let _ = sqlx::query("SELECT pg_advisory_unlock($1)")
            .bind(LEADER_KEY)
            .execute(&mut *conn)
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_factor_setup_allowlist() {
        assert!(allowed_without_two_factor(
            &Method::POST,
            "/_bgh/user/two_factor/totp"
        ));
        assert!(allowed_without_two_factor(
            &Method::POST,
            "/_bgh/auth/logout"
        ));
        assert!(allowed_without_two_factor(&Method::GET, "/_bgh/boot"));
        assert!(allowed_without_two_factor(&Method::GET, "/api/v3/user"));
        assert!(!allowed_without_two_factor(
            &Method::GET,
            "/api/v3/user/repos"
        ));
        assert!(!allowed_without_two_factor(&Method::POST, "/_bgh/tokens"));
        assert!(!allowed_without_two_factor(
            &Method::POST,
            "/api/v3/user/repos"
        ));
        assert!(allowed_without_two_factor(
            &Method::GET,
            "/settings/security"
        ));
    }
}
