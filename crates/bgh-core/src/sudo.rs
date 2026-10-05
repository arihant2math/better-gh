//! Sudo mode: browser sessions must have re-authenticated (password, TOTP,
//! recovery code or WebAuthn) within the last [`SUDO_TTL_MINUTES`] before
//! sensitive changes (creating tokens, SSH/GPG keys, OAuth and GitHub Apps,
//! changing emails, deleting or transferring repositories, managing
//! security keys).
//!
//! ```ignore
//! bgh_core::sudo::require(&state, &auth).await?;   // 401 SUDO_REQUIRED unless fresh
//! ```
//!
//! Only cookie sessions are checked: tokens are the credential the API is
//! meant to be used with (like GitHub, whose sudo mode is a web-UI feature).
//! Signing in counts as a re-authentication (`sessions.sudo_at` defaults to
//! the session's creation); `POST /_bgh/sudo` (bgh-accounts) renews it.

use chrono::{DateTime, Duration, Utc};

use crate::auth::{AuthContext, AuthMethod};
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

/// How long a re-authentication lasts.
pub const SUDO_TTL_MINUTES: i64 = 120;

/// Message of the 401 answered to sessions without a fresh sudo mode. The
/// web client recognizes it, prompts for re-authentication and retries.
pub const SUDO_REQUIRED: &str = "Sudo mode required: confirm your password, two-factor code or security key (POST /_bgh/sudo) and retry.";

/// The 401 for a sudo-less session.
pub fn required_error() -> ApiError {
    ApiError::Unauthorized {
        message: SUDO_REQUIRED.into(),
        www_authenticate: None,
    }
}

/// When the session's sudo mode ends (`None` = not in sudo mode).
pub async fn expires_at(state: &AppState, session_id: i64) -> ApiResult<Option<DateTime<Utc>>> {
    let at: Option<Option<DateTime<Utc>>> =
        sqlx::query_scalar("SELECT sudo_at FROM sessions WHERE id = $1")
            .bind(session_id)
            .fetch_optional(&state.db)
            .await?;
    let end = at
        .flatten()
        .map(|t| t + Duration::minutes(SUDO_TTL_MINUTES))
        .filter(|end| *end > Utc::now());
    Ok(end)
}

/// Ok for token callers and sessions in sudo mode, else 401
/// [`SUDO_REQUIRED`].
pub async fn require(state: &AppState, auth: &AuthContext) -> ApiResult<()> {
    let AuthMethod::Session { session_id } = auth.method else {
        return Ok(());
    };
    if expires_at(state, session_id).await?.is_some() {
        Ok(())
    } else {
        Err(required_error())
    }
}

/// Start (renew) sudo mode for a session; returns when it ends.
pub async fn grant(state: &AppState, session_id: i64) -> ApiResult<DateTime<Utc>> {
    let at: DateTime<Utc> =
        sqlx::query_scalar("UPDATE sessions SET sudo_at = now() WHERE id = $1 RETURNING sudo_at")
            .bind(session_id)
            .fetch_one(&state.db)
            .await?;
    Ok(at + Duration::minutes(SUDO_TTL_MINUTES))
}
