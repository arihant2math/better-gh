//! Helpers shared by the accounts modules: lookups, PATCH semantics, sync
//! shapes, request metadata, mail.

use axum::http::{HeaderMap, header};
use bgh_core::mail;
use bgh_core::prelude::*;
use serde::{Deserialize, Deserializer};

/// A PATCH body field: absent (leave unchanged), `null`, or a value. Use
/// with `#[serde(default)]`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Patch<T> {
    #[default]
    Absent,
    Null,
    Value(T),
}

impl<T> Patch<T> {
    pub fn is_absent(&self) -> bool {
        matches!(self, Self::Absent)
    }

    /// `None` = absent; `Some(None)` = null; `Some(Some(v))` = value.
    pub fn into_option(self) -> Option<Option<T>> {
        match self {
            Self::Absent => None,
            Self::Null => Some(None),
            Self::Value(v) => Some(Some(v)),
        }
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Patch<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(match Option::<T>::deserialize(d)? {
            None => Self::Null,
            Some(v) => Self::Value(v),
        })
    }
}

pub fn user_agent(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
}

/// Extractor: client IP (see `bgh_core::auth::client_ip`) and user agent.
#[derive(Debug, Clone)]
pub struct ClientInfo {
    pub ip: String,
    pub user_agent: Option<String>,
}

impl axum::extract::FromRequestParts<AppState> for ClientInfo {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &AppState,
    ) -> ApiResult<Self> {
        Ok(Self {
            ip: bgh_core::auth::client_ip(&state.config, &parts.headers, &parts.extensions),
            user_agent: user_agent(&parts.headers).map(str::to_string),
        })
    }
}

/// A user or organization by login, else 404.
/// Renamed accounts keep resolving on their old login
/// (`bgh_core::lifecycle::resolve_owner`).
pub async fn find_account(state: &AppState, login: &str) -> ApiResult<db::User> {
    bgh_core::lifecycle::resolve_owner(&state.db, login)
        .await?
        .ok_or(ApiError::NotFound)
}

/// A user (not an organization) by login, else 404.
pub async fn find_user(state: &AppState, login: &str) -> ApiResult<db::User> {
    find_account(state, login).await.and_then(|u| {
        if u.is_org() {
            Err(ApiError::NotFound)
        } else {
            Ok(u)
        }
    })
}

/// An organization by login, else 404.
pub async fn find_org(state: &AppState, login: &str) -> ApiResult<db::User> {
    find_account(state, login).await.and_then(|u| {
        if u.is_org() {
            Ok(u)
        } else {
            Err(ApiError::NotFound)
        }
    })
}

/// Parse an optional JSON body (empty body → `T::default()`), for PUT
/// endpoints where GitHub clients often send no body.
pub fn optional_json<T: serde::de::DeserializeOwned + Default>(body: &[u8]) -> ApiResult<T> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(T::default());
    }
    serde_json::from_slice(body).map_err(|_| ApiError::bad_request("Problems parsing JSON"))
}

/// Trim and turn empty strings into `None`.
pub fn non_empty(s: Option<String>) -> Option<String> {
    s.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

// ---------------------------------------------------------------------------
// Sync
// ---------------------------------------------------------------------------

/// Record a profile change of `user`: a user's `user` row in its own scope
/// and every org scope it belongs to, an organization's `org` row.
pub async fn sync_profile(tx: &mut Tx, user: &db::User) -> ApiResult<()> {
    if user.is_org() {
        tx.sync_model(SyncModel::Org, user.id, SyncAction::Update)
            .await?;
        return Ok(());
    }
    tx.sync_user(user.id).await
}

// ---------------------------------------------------------------------------
// Mail
// ---------------------------------------------------------------------------

/// Queue an email in `tx` (delivered after commit by the shared `mail.send`
/// job, see `bgh_core::mail`).
pub async fn queue_mail(tx: &mut Tx, email: mail::Email) -> ApiResult<()> {
    tx.enqueue(&mail::SendEmail::new(email)).await?;
    Ok(())
}

/// The primary email of a user, if any.
pub async fn primary_email(
    db: impl sqlx::PgExecutor<'_>,
    user_id: i64,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT email FROM user_emails WHERE user_id = $1 AND is_primary")
        .bind(user_id)
        .fetch_optional(db)
        .await
}

/// The address a password reset may be mailed to: the primary email when
/// verified, else the oldest verified one. Never an unverified address,
/// which anyone could have typed in at sign-up.
pub async fn verified_email(
    db: impl sqlx::PgExecutor<'_>,
    user_id: i64,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT email FROM user_emails WHERE user_id = $1 AND verified
          ORDER BY is_primary DESC, id LIMIT 1",
    )
    .bind(user_id)
    .fetch_optional(db)
    .await
}

/// Whether `user_id` has two-factor authentication enabled.
pub async fn two_factor_enabled(
    db: impl sqlx::PgExecutor<'_>,
    user_id: i64,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM user_two_factor WHERE user_id = $1 AND enabled_at IS NOT NULL)",
    )
    .bind(user_id)
    .fetch_one(db)
    .await
}

/// 403 unless the caller is a browser session (account security settings
/// can't be changed with tokens).
pub fn require_session(auth: &AuthContext) -> ApiResult<()> {
    if auth.is_session() {
        Ok(())
    } else {
        Err(ApiError::forbidden(
            "This action requires a browser session.",
        ))
    }
}

// ---------------------------------------------------------------------------
// One-time mailed secrets (`account_tokens`)
// ---------------------------------------------------------------------------

/// Create a one-time token of `kind` (`password_reset` |
/// `email_verification`), valid for `ttl`. Returns the plaintext.
pub async fn create_account_token(
    tx: &mut Tx,
    user_id: i64,
    kind: &str,
    email_id: Option<i64>,
    ttl: chrono::Duration,
) -> ApiResult<String> {
    let token = bgh_core::crypto::random_token(40);
    sqlx::query(
        "INSERT INTO account_tokens (user_id, kind, token_hash, email_id, expires_at)
         VALUES ($1, $2, $3, $4, now() + $5)",
    )
    .bind(user_id)
    .bind(kind)
    .bind(bgh_core::crypto::sha256_hex(&token))
    .bind(email_id)
    .bind(ttl)
    .execute(&mut **tx)
    .await?;
    Ok(token)
}

/// A valid (unexpired) token of `kind`: `(id, user_id, email_id)`.
pub async fn find_account_token(
    db: impl sqlx::PgExecutor<'_>,
    kind: &str,
    token: &str,
) -> Result<Option<(i64, i64, Option<i64>)>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, user_id, email_id FROM account_tokens
          WHERE token_hash = $1 AND kind = $2 AND expires_at > now()",
    )
    .bind(bgh_core::crypto::sha256_hex(token))
    .bind(kind)
    .fetch_optional(db)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize)]
    struct Body {
        #[serde(default)]
        a: Patch<String>,
        #[serde(default)]
        b: Patch<String>,
        #[serde(default)]
        c: Patch<String>,
    }

    #[test]
    fn patch_semantics() {
        let b: Body = serde_json::from_str(r#"{"b": null, "c": "x"}"#).unwrap();
        assert_eq!(b.a, Patch::Absent);
        assert_eq!(b.b, Patch::Null);
        assert_eq!(b.c, Patch::Value("x".into()));
    }
}
