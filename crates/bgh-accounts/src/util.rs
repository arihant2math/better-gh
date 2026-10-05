//! Helpers shared by the accounts modules: lookups, PATCH semantics, sync
//! shapes, request metadata, mail.

use axum::http::{HeaderMap, header};
use bgh_core::jobs::JobPayload;
use bgh_core::mail;
use bgh_core::prelude::*;
use bgh_core::sync;
use bgh_core::urls::Urls;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};

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
pub async fn find_account(state: &AppState, login: &str) -> ApiResult<db::User> {
    db::User::find_by_login(&state.db, login)
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
// Sync (compact client shapes, docs/SYNC_PROTOCOL.md section 3)
// ---------------------------------------------------------------------------

/// `user` sync model.
pub fn user_sync_json(urls: &Urls, u: &db::User) -> Value {
    json!({
        "id": u.id,
        "login": u.login,
        "name": u.name,
        "avatarUrl": urls.avatar(u.id, u.avatar_url.as_deref()),
        "type": if u.kind == "Bot" { "Bot" } else { "User" },
    })
}

/// `org` sync model.
pub fn org_sync_json(urls: &Urls, org: &db::User, description: Option<&str>) -> Value {
    json!({
        "id": org.id,
        "login": org.login,
        "name": org.name,
        "avatarUrl": urls.avatar(org.id, org.avatar_url.as_deref()),
        "description": description,
    })
}

/// `membership` sync model.
pub fn membership_sync_json(id: i64, org_id: i64, user_id: i64, role: &str) -> Value {
    json!({ "id": id, "orgId": org_id, "userId": user_id, "role": role })
}

/// Record a profile change of `user` in its own scope and every org scope
/// it belongs to (orgs record their `org` row instead).
pub async fn sync_profile(tx: &mut Tx, urls: &Urls, user: &db::User) -> ApiResult<()> {
    if user.is_org() {
        let description: Option<String> =
            sqlx::query_scalar("SELECT description FROM org_settings WHERE org_id = $1")
                .bind(user.id)
                .fetch_optional(&mut **tx)
                .await?
                .flatten();
        return tx
            .sync(
                &sync::org_scope(user.id),
                "org",
                user.id,
                SyncAction::Update,
                &org_sync_json(urls, user, description.as_deref()),
            )
            .await;
    }
    let data = user_sync_json(urls, user);
    tx.sync(
        &sync::user_scope(user.id),
        "user",
        user.id,
        SyncAction::Update,
        &data,
    )
    .await?;
    let orgs: Vec<i64> = sqlx::query_scalar("SELECT org_id FROM org_members WHERE user_id = $1")
        .bind(user.id)
        .fetch_all(&mut **tx)
        .await?;
    for org_id in orgs {
        tx.sync(
            &sync::org_scope(org_id),
            "user",
            user.id,
            SyncAction::Update,
            &data,
        )
        .await?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Mail
// ---------------------------------------------------------------------------

/// Background job delivering one email (`bgh_core::mail`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendMail(pub mail::Message);

impl JobPayload for SendMail {
    const KIND: &'static str = "accounts.send_mail";
}

pub async fn send_mail_job(state: AppState, job: SendMail) -> anyhow::Result<()> {
    mail::send(&state, &job.0).await
}

/// Queue an email in `tx` (sent after commit by the job worker).
pub async fn queue_mail(tx: &mut Tx, to: &str, subject: &str, text: String) -> ApiResult<()> {
    tx.enqueue(&SendMail(mail::Message::new(to, subject, text)))
        .await?;
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
