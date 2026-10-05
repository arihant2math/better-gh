//! TOTP two-factor authentication and recovery codes.
//!
//! Web-client endpoints (browser session required):
//! `GET /_bgh/user/two_factor`, `POST /_bgh/user/two_factor/totp` (start
//! setup), `POST /_bgh/user/two_factor/totp/enable {code}` (→ recovery
//! codes), `DELETE /_bgh/user/two_factor {password}`,
//! `POST /_bgh/user/two_factor/recovery_codes {password}` (regenerate).
//! The login flow itself lives in `session`.

use axum::extract::State;
use axum::http::StatusCode;
use bgh_core::audit;
use bgh_core::auth;
use bgh_core::crypto;
use bgh_core::mail;
use bgh_core::prelude::*;
use bgh_core::time::ts;
use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::totp;
use crate::util;

pub const RECOVERY_CODE_COUNT: usize = 10;

/// `xxxxx-xxxxx` (lowercase hex).
fn new_recovery_code() -> String {
    let mut b = [0u8; 5];
    rand::rng().fill(&mut b);
    let h = hex::encode(b);
    format!("{}-{}", &h[..5], &h[5..])
}

fn normalize_recovery_code(code: &str) -> String {
    let c: String = code
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_lowercase();
    if c.len() == 10 {
        format!("{}-{}", &c[..5], &c[5..])
    } else {
        c
    }
}

/// Replace the user's recovery codes; returns the plaintext codes.
async fn regenerate_codes(tx: &mut Tx, user_id: i64) -> ApiResult<Vec<String>> {
    sqlx::query("DELETE FROM user_recovery_codes WHERE user_id = $1")
        .bind(user_id)
        .execute(&mut **tx)
        .await?;
    let codes: Vec<String> = (0..RECOVERY_CODE_COUNT)
        .map(|_| new_recovery_code())
        .collect();
    let hashes: Vec<String> = codes.iter().map(|c| crypto::sha256_hex(c)).collect();
    sqlx::query(
        "INSERT INTO user_recovery_codes (user_id, code_hash) SELECT $1, unnest($2::text[])",
    )
    .bind(user_id)
    .bind(&hashes)
    .execute(&mut **tx)
    .await?;
    Ok(codes)
}

/// The TOTP secret of a `user_two_factor` row: decrypted from
/// `totp_secret_enc`, or the legacy plaintext column (which is then
/// encrypted in `tx`, see `security::encrypt_legacy_totp`).
pub async fn stored_secret(
    state: &AppState,
    tx: &mut Tx,
    user_id: i64,
    plain: Option<String>,
    sealed: Option<Vec<u8>>,
) -> ApiResult<String> {
    if let Some(sealed) = sealed {
        return bgh_core::secretbox::open(state, &sealed);
    }
    let plain = plain.ok_or_else(|| ApiError::internal(anyhow::anyhow!("TOTP secret missing")))?;
    sqlx::query(
        "UPDATE user_two_factor SET totp_secret_enc = $2, totp_secret = NULL WHERE user_id = $1",
    )
    .bind(user_id)
    .bind(bgh_core::secretbox::seal(state, &plain)?)
    .execute(&mut **tx)
    .await?;
    Ok(plain)
}

/// `(totp_secret, totp_secret_enc, …)` columns of a locked row.
type SecretRow = (Option<String>, Option<Vec<u8>>, i64);

/// Check a second factor for `user_id`: a TOTP code (replays rejected) or
/// an unused recovery code (consumed). `Ok(false)` when wrong.
pub async fn verify_second_factor(state: &AppState, user_id: i64, code: &str) -> ApiResult<bool> {
    let code = code.trim();
    let mut tx = Tx::begin(state).await?;
    let row: Option<SecretRow> = sqlx::query_as(
        "SELECT totp_secret, totp_secret_enc, last_used_step FROM user_two_factor
          WHERE user_id = $1 AND enabled_at IS NOT NULL FOR UPDATE",
    )
    .bind(user_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((plain, sealed, last_step)) = row else {
        return Ok(false);
    };
    let secret = stored_secret(state, &mut tx, user_id, plain, sealed).await?;
    let now = chrono::Utc::now().timestamp();
    if let Some(step) = totp::verify(&secret, code, now, last_step) {
        sqlx::query("UPDATE user_two_factor SET last_used_step = $2 WHERE user_id = $1")
            .bind(user_id)
            .bind(step)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        return Ok(true);
    }
    let used = sqlx::query(
        "UPDATE user_recovery_codes SET used_at = now()
          WHERE user_id = $1 AND code_hash = $2 AND used_at IS NULL",
    )
    .bind(user_id)
    .bind(crypto::sha256_hex(&normalize_recovery_code(code)))
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if used > 0 {
        audit::log(
            &mut *tx,
            None,
            "two_factor_authentication.recovery_code_used",
            audit::Target::User(user_id),
            json!({}),
        )
        .await?;
        tx.commit().await?;
        return Ok(true);
    }
    // Keep a lazy re-encryption of a legacy secret.
    tx.commit().await?;
    Ok(false)
}

#[derive(Debug, Serialize)]
pub struct TwoFactorStatus {
    pub enabled: bool,
    pub enabled_at: Option<Timestamp>,
    pub recovery_codes_remaining: i64,
    /// Registered WebAuthn security keys / passkeys.
    pub security_keys: i64,
    pub passkeys: i64,
    /// The site requires 2FA for every account.
    pub required_by_site: bool,
}

/// `GET /_bgh/user/two_factor`
pub async fn status(
    State(state): State<AppState>,
    auth: RequireUser,
) -> ApiResult<Json<TwoFactorStatus>> {
    util::require_session(&auth)?;
    let enabled_at: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT enabled_at FROM user_two_factor WHERE user_id = $1")
            .bind(auth.user.id)
            .fetch_optional(&state.db)
            .await?
            .flatten();
    let remaining: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM user_recovery_codes WHERE user_id = $1 AND used_at IS NULL",
    )
    .bind(auth.user.id)
    .fetch_one(&state.db)
    .await?;
    let (security_keys, passkeys) = crate::webauthn::counts(&state.db, auth.user.id).await?;
    Ok(Json(TwoFactorStatus {
        enabled: enabled_at.is_some(),
        enabled_at: ts(enabled_at),
        recovery_codes_remaining: if enabled_at.is_some() { remaining } else { 0 },
        security_keys,
        passkeys,
        required_by_site: bgh_core::settings::load(&state)
            .await?
            .auth_providers
            .require_2fa,
    }))
}

#[derive(Debug, Serialize)]
pub struct TotpSetup {
    pub secret: String,
    pub otpauth_uri: String,
}

/// `POST /_bgh/user/two_factor/totp` → a new pending secret (409 when 2FA
/// is already enabled).
pub async fn start_totp(
    State(state): State<AppState>,
    auth: RequireUser,
) -> ApiResult<(StatusCode, Json<TotpSetup>)> {
    util::require_session(&auth)?;
    if util::two_factor_enabled(&state.db, auth.user.id).await? {
        return Err(ApiError::conflict(
            "Two-factor authentication is already enabled.",
        ));
    }
    let secret = totp::new_secret();
    sqlx::query(
        "INSERT INTO user_two_factor (user_id, totp_secret, totp_secret_enc) VALUES ($1, NULL, $2)
         ON CONFLICT (user_id) DO UPDATE SET totp_secret = NULL,
             totp_secret_enc = EXCLUDED.totp_secret_enc, last_used_step = 0, created_at = now()",
    )
    .bind(auth.user.id)
    .bind(bgh_core::secretbox::seal(&state, &secret)?)
    .execute(&state.db)
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(TotpSetup {
            otpauth_uri: totp::otpauth_uri(&state.config.site_name, &auth.user.login, &secret),
            secret,
        }),
    ))
}

#[derive(Debug, Deserialize)]
pub struct CodeBody {
    #[serde(default)]
    pub code: String,
}

#[derive(Debug, Serialize)]
pub struct RecoveryCodes {
    pub recovery_codes: Vec<String>,
}

/// `POST /_bgh/user/two_factor/totp/enable {code}` → recovery codes.
pub async fn enable_totp(
    State(state): State<AppState>,
    auth: RequireUser,
    Json(body): Json<CodeBody>,
) -> ApiResult<Json<RecoveryCodes>> {
    util::require_session(&auth)?;
    let mut tx = Tx::begin(&state).await?;
    type Row = (
        Option<String>,
        Option<Vec<u8>>,
        Option<chrono::DateTime<chrono::Utc>>,
    );
    let row: Option<Row> = sqlx::query_as(
        "SELECT totp_secret, totp_secret_enc, enabled_at FROM user_two_factor
          WHERE user_id = $1 FOR UPDATE",
    )
    .bind(auth.user.id)
    .fetch_optional(&mut *tx)
    .await?;
    let secret = match row {
        Some((_, _, Some(_))) => {
            return Err(ApiError::conflict(
                "Two-factor authentication is already enabled.",
            ));
        }
        Some((plain, sealed, None)) => {
            stored_secret(&state, &mut tx, auth.user.id, plain, sealed).await?
        }
        None => {
            return Err(ApiError::unprocessable(
                "Start two-factor setup before enabling it.",
            ));
        }
    };
    let now = chrono::Utc::now().timestamp();
    let step = totp::verify(&secret, &body.code, now, 0).ok_or_else(|| {
        ApiError::invalid_field(FieldError::custom(
            "TwoFactor",
            "code",
            "Two-factor code verification failed",
        ))
    })?;
    sqlx::query(
        "UPDATE user_two_factor SET enabled_at = now(), last_used_step = $2 WHERE user_id = $1",
    )
    .bind(auth.user.id)
    .bind(step)
    .execute(&mut *tx)
    .await?;
    let codes = regenerate_codes(&mut tx, auth.user.id).await?;
    audit::log(
        &mut *tx,
        Some(&auth.user),
        "two_factor_authentication.enabled",
        audit::Target::User(auth.user.id),
        json!({}),
    )
    .await?;
    if let Some(email) = util::primary_email(&mut *tx, auth.user.id).await? {
        util::queue_mail(
            &mut tx,
            mail::templates::two_factor_enabled(&state.config.site_name, &email, &auth.user.login),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(Json(RecoveryCodes {
        recovery_codes: codes,
    }))
}

#[derive(Debug, Deserialize)]
pub struct PasswordBody {
    #[serde(default)]
    pub password: String,
}

/// Re-authentication for sensitive changes (users without a password, e.g.
/// SSO-only accounts, must give a second factor instead).
pub async fn confirm_password(state: &AppState, user: &db::User, password: &str) -> ApiResult<()> {
    let key = format!("sudo_fail:{}", user.id);
    if bgh_core::ratelimit::count(state, &key).await >= 10 {
        return Err(ApiError::Status(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many failed attempts. Please try again later.".into(),
        ));
    }
    let ok = match &user.password_hash {
        Some(_) => auth::verify_login(state, &user.login, password)
            .await?
            .is_some(),
        None => verify_second_factor(state, user.id, password).await?,
    };
    if !ok {
        bgh_core::ratelimit::hit(state, &key, 900).await?;
        return Err(ApiError::forbidden("Incorrect password."));
    }
    Ok(())
}

/// `DELETE /_bgh/user/two_factor {password}` → 204.
pub async fn disable(
    State(state): State<AppState>,
    auth: RequireUser,
    Json(body): Json<PasswordBody>,
) -> ApiResult<StatusCode> {
    util::require_session(&auth)?;
    confirm_password(&state, &auth.user, &body.password).await?;
    crate::security::check_two_factor_removable(&state, auth.user.id).await?;
    let mut tx = Tx::begin(&state).await?;
    sqlx::query("DELETE FROM user_two_factor WHERE user_id = $1")
        .bind(auth.user.id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM user_recovery_codes WHERE user_id = $1")
        .bind(auth.user.id)
        .execute(&mut *tx)
        .await?;
    // Security keys are second factors; passkeys stay (they sign in alone).
    sqlx::query(
        "DELETE FROM user_webauthn_credentials WHERE user_id = $1 AND kind = 'security_key'",
    )
    .bind(auth.user.id)
    .execute(&mut *tx)
    .await?;
    audit::log(
        &mut *tx,
        Some(&auth.user),
        "two_factor_authentication.disabled",
        audit::Target::User(auth.user.id),
        json!({}),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /_bgh/user/two_factor/recovery_codes {password}` → new codes.
pub async fn regenerate(
    State(state): State<AppState>,
    auth: RequireUser,
    Json(body): Json<PasswordBody>,
) -> ApiResult<Json<RecoveryCodes>> {
    util::require_session(&auth)?;
    if !util::two_factor_enabled(&state.db, auth.user.id).await? {
        return Err(ApiError::unprocessable(
            "Two-factor authentication is not enabled.",
        ));
    }
    confirm_password(&state, &auth.user, &body.password).await?;
    let mut tx = Tx::begin(&state).await?;
    let codes = regenerate_codes(&mut tx, auth.user.id).await?;
    audit::log(
        &mut *tx,
        Some(&auth.user),
        "two_factor_authentication.recovery_codes_regenerated",
        audit::Target::User(auth.user.id),
        json!({}),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(RecoveryCodes {
        recovery_codes: codes,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_code_format() {
        let c = new_recovery_code();
        assert_eq!(c.len(), 11);
        assert_eq!(
            normalize_recovery_code(&c.to_uppercase().replace('-', " ")),
            c
        );
    }
}
