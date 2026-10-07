//! Per-user notification settings and email unsubscribe links.
//!
//! * `GET/PUT /_bgh/notifications/settings` — which reasons reach the web
//!   inbox and email, master email switch, routing address, own activity.
//! * `GET/POST /_bgh/notifications/unsubscribe?token=…` — signed links in
//!   notification emails (`List-Unsubscribe`, RFC 8058 one-click POST).
//!   A thread token unsubscribes from that thread; an `all` token turns
//!   notification email off.

use std::collections::HashMap;

use axum::extract::State;
use axum::response::Html;
use bgh_core::prelude::*;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::reasons::Reason;
use crate::subscriptions::set_thread_subscription;

/// `notification_settings` row (defaults when absent: everything on).
#[derive(Debug, Clone, Default, sqlx::FromRow)]
pub struct Prefs {
    pub user_id: i64,
    pub web_disabled: Vec<String>,
    pub email_disabled: Vec<String>,
    pub email_enabled: bool,
    pub notification_email: Option<String>,
    pub own_activity_email: bool,
}

impl Prefs {
    pub const COLUMNS: &'static str = "user_id, web_disabled, email_disabled, email_enabled, \
        notification_email, own_activity_email";

    pub fn defaults(user_id: i64) -> Self {
        Self {
            user_id,
            email_enabled: true,
            ..Self::default()
        }
    }

    pub fn web_allows(&self, r: Reason) -> bool {
        !self.web_disabled.iter().any(|d| d == r.as_str())
    }

    pub fn email_allows(&self, r: Reason) -> bool {
        self.email_enabled && !self.email_disabled.iter().any(|d| d == r.as_str())
    }
}

/// Settings of `user_ids` that have a row (absent users use defaults).
pub async fn load_many(
    conn: &mut sqlx::PgConnection,
    user_ids: &[i64],
) -> ApiResult<HashMap<i64, Prefs>> {
    if user_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows: Vec<Prefs> = sqlx::query_as(&format!(
        "SELECT {} FROM notification_settings WHERE user_id = ANY($1)",
        Prefs::COLUMNS
    ))
    .bind(user_ids)
    .fetch_all(conn)
    .await?;
    Ok(rows.into_iter().map(|p| (p.user_id, p)).collect())
}

async fn load(state: &AppState, user_id: i64) -> ApiResult<Prefs> {
    let mut conn = state.db.acquire().await?;
    Ok(load_many(&mut conn, &[user_id])
        .await?
        .remove(&user_id)
        .unwrap_or_else(|| Prefs::defaults(user_id)))
}

/// JSON of the settings endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettingsJson {
    /// reason → delivered to the web inbox
    pub web: HashMap<String, bool>,
    /// reason → delivered by email
    pub email: HashMap<String, bool>,
    pub email_enabled: bool,
    pub notification_email: Option<String>,
    pub own_activity_email: bool,
}

fn to_json(p: &Prefs) -> SettingsJson {
    SettingsJson {
        web: Reason::ALL
            .iter()
            .map(|r| (r.as_str().to_string(), p.web_allows(*r)))
            .collect(),
        email: Reason::ALL
            .iter()
            .map(|r| {
                (
                    r.as_str().to_string(),
                    !p.email_disabled.iter().any(|d| d == r.as_str()),
                )
            })
            .collect(),
        email_enabled: p.email_enabled,
        notification_email: p.notification_email.clone(),
        own_activity_email: p.own_activity_email,
    }
}

/// `GET /_bgh/notifications/settings`
pub async fn get_settings(
    State(state): State<AppState>,
    auth: RequireUser,
) -> ApiResult<Json<SettingsJson>> {
    Ok(Json(to_json(&load(&state, auth.id()).await?)))
}

#[derive(Debug, Default, Deserialize)]
pub struct SettingsPatch {
    pub web: Option<HashMap<String, bool>>,
    pub email: Option<HashMap<String, bool>>,
    pub email_enabled: Option<bool>,
    /// `null`/empty resets to the primary address.
    #[serde(default, deserialize_with = "double_option")]
    pub notification_email: Option<Option<String>>,
    pub own_activity_email: Option<bool>,
}

fn double_option<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<Option<String>>, D::Error> {
    Ok(Some(Option::deserialize(d)?))
}

fn apply(disabled: &mut Vec<String>, patch: &HashMap<String, bool>, field: &str) -> ApiResult<()> {
    for (reason, on) in patch {
        let r = Reason::parse(reason).ok_or_else(|| {
            ApiError::invalid_field(FieldError::custom(
                "NotificationSettings",
                field,
                format!("unknown reason {reason:?}"),
            ))
        })?;
        disabled.retain(|d| d != r.as_str());
        if !on {
            disabled.push(r.as_str().to_string());
        }
    }
    disabled.sort();
    Ok(())
}

/// `PUT /_bgh/notifications/settings` (partial update).
pub async fn put_settings(
    State(state): State<AppState>,
    auth: RequireUser,
    Json(patch): Json<SettingsPatch>,
) -> ApiResult<Json<SettingsJson>> {
    let mut p = load(&state, auth.id()).await?;
    if let Some(web) = &patch.web {
        apply(&mut p.web_disabled, web, "web")?;
    }
    if let Some(email) = &patch.email {
        apply(&mut p.email_disabled, email, "email")?;
    }
    if let Some(v) = patch.email_enabled {
        p.email_enabled = v;
    }
    if let Some(v) = patch.own_activity_email {
        p.own_activity_email = v;
    }
    if let Some(addr) = patch.notification_email {
        let addr = addr.map(|a| a.trim().to_string()).filter(|a| !a.is_empty());
        if let Some(a) = &addr {
            let ok: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM user_emails
                                 WHERE user_id = $1 AND lower(email) = lower($2) AND verified)",
            )
            .bind(auth.id())
            .bind(a)
            .fetch_one(&state.db)
            .await?;
            if !ok {
                return Err(ApiError::invalid_field(FieldError::custom(
                    "NotificationSettings",
                    "notification_email",
                    "must be one of your verified email addresses",
                )));
            }
        }
        p.notification_email = addr;
    }
    save(&state, &p).await?;
    Ok(Json(to_json(&p)))
}

async fn save(state: &AppState, p: &Prefs) -> ApiResult<()> {
    let mut tx = Tx::begin(state).await?;
    sqlx::query(
        "INSERT INTO notification_settings
                (user_id, web_disabled, email_disabled, email_enabled, notification_email,
                 own_activity_email, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, now())
         ON CONFLICT (user_id) DO UPDATE SET
            web_disabled = EXCLUDED.web_disabled, email_disabled = EXCLUDED.email_disabled,
            email_enabled = EXCLUDED.email_enabled,
            notification_email = EXCLUDED.notification_email,
            own_activity_email = EXCLUDED.own_activity_email, updated_at = now()",
    )
    .bind(p.user_id)
    .bind(&p.web_disabled)
    .bind(&p.email_disabled)
    .bind(p.email_enabled)
    .bind(&p.notification_email)
    .bind(p.own_activity_email)
    .execute(&mut *tx)
    .await?;
    tx.sync(
        &bgh_core::sync::user_scope(p.user_id),
        "notificationSettings",
        p.user_id,
        SyncAction::Update,
        &to_json(p),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Unsubscribe tokens
// ---------------------------------------------------------------------------

const SECRET_KEY: &str = "notify.secret";

/// Server secret for signing unsubscribe links (created on first use and
/// stored in `site_settings`).
pub async fn secret(state: &AppState) -> ApiResult<String> {
    let fresh = bgh_core::crypto::random_token(48);
    sqlx::query(
        "INSERT INTO site_settings (key, value) VALUES ($1, to_jsonb($2::text))
         ON CONFLICT (key) DO NOTHING",
    )
    .bind(SECRET_KEY)
    .bind(&fresh)
    .execute(&state.db)
    .await?;
    let v: serde_json::Value = sqlx::query_scalar("SELECT value FROM site_settings WHERE key = $1")
        .bind(SECRET_KEY)
        .fetch_one(&state.db)
        .await?;
    Ok(v.as_str().unwrap_or_default().to_string())
}

fn mac(secret: &str, msg: &str) -> String {
    let mut m = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("hmac key");
    m.update(msg.as_bytes());
    hex::encode(&m.finalize().into_bytes()[..16])
}

/// What an unsubscribe token refers to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unsub {
    Thread {
        user_id: i64,
        subject_type: String,
        subject_id: i64,
    },
    AllEmail {
        user_id: i64,
    },
}

/// Token for unsubscribing `user_id` from a thread (`subject` =
/// `(type, id)`) or, with `None`, from all notification email.
pub fn token(secret: &str, user_id: i64, subject: Option<(&str, i64)>) -> String {
    let body = match subject {
        Some((kind, id)) => format!("{user_id}.{kind}.{id}"),
        None => format!("{user_id}.all.0"),
    };
    let sig = mac(secret, &format!("unsubscribe:{body}"));
    format!("{body}.{sig}")
}

pub fn verify(secret: &str, token: &str) -> Option<Unsub> {
    let mut parts = token.split('.');
    let (user, kind, id, sig) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    let body = format!("{user}.{kind}.{id}");
    let expected = mac(secret, &format!("unsubscribe:{body}"));
    // Constant-time comparison.
    if expected.len() != sig.len()
        || expected
            .bytes()
            .zip(sig.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            != 0
    {
        return None;
    }
    let user_id: i64 = user.parse().ok()?;
    match kind {
        "all" => Some(Unsub::AllEmail { user_id }),
        "Issue" | "PullRequest" | "Release" | "CheckSuite" | "Commit" => Some(Unsub::Thread {
            user_id,
            subject_type: kind.to_string(),
            subject_id: id.parse().ok()?,
        }),
        _ => None,
    }
}

/// Unsubscribe URL for a token.
pub fn unsubscribe_url(state: &AppState, token: &str) -> String {
    state
        .urls
        .html(&format!("/_bgh/notifications/unsubscribe?token={token}"))
}

#[derive(Debug, Deserialize)]
pub struct TokenQuery {
    pub token: Option<String>,
}

fn page(state: &AppState, title: &str, body_html: &str) -> Html<String> {
    let site = bgh_core::mail::escape_html(&state.config.site_name);
    Html(format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" \
         content=\"width=device-width\"><title>{t} · {site}</title></head>\
         <body style=\"font-family:system-ui,sans-serif;max-width:560px;margin:48px auto;padding:0 16px\">\
         <h1 style=\"font-size:20px\">{t}</h1>{body_html}</body></html>",
        t = bgh_core::mail::escape_html(title),
    ))
}

/// `GET /_bgh/notifications/unsubscribe?token=` — confirmation page (a
/// POST performs the change, so link scanners can't unsubscribe people).
pub async fn unsubscribe_page(
    State(state): State<AppState>,
    Query(q): Query<TokenQuery>,
) -> ApiResult<Html<String>> {
    let secret = secret(&state).await?;
    let token = q.token.unwrap_or_default();
    let what = match verify(&secret, &token) {
        None => return Err(ApiError::NotFound),
        Some(Unsub::AllEmail { .. }) => "all notification emails",
        Some(Unsub::Thread { .. }) => "this conversation",
    };
    let token_html = bgh_core::mail::escape_html(&token);
    Ok(page(
        &state,
        "Unsubscribe",
        &format!(
            "<p>Stop receiving notifications for {what}?</p>\
             <form method=\"post\" action=\"/_bgh/notifications/unsubscribe?token={token_html}\">\
             <button type=\"submit\" style=\"padding:6px 14px\">Unsubscribe</button></form>"
        ),
    ))
}

/// `POST /_bgh/notifications/unsubscribe?token=` (RFC 8058 one-click).
pub async fn unsubscribe(
    State(state): State<AppState>,
    Query(q): Query<TokenQuery>,
) -> ApiResult<Html<String>> {
    let secret = secret(&state).await?;
    let unsub = verify(&secret, &q.token.unwrap_or_default()).ok_or(ApiError::NotFound)?;
    match unsub {
        Unsub::AllEmail { user_id } => {
            let mut p = load(&state, user_id).await?;
            p.email_enabled = false;
            save(&state, &p).await?;
            Ok(page(
                &state,
                "Unsubscribed",
                "<p>You will no longer receive notification emails. You can turn them back on in your notification settings.</p>",
            ))
        }
        Unsub::Thread {
            user_id,
            subject_type,
            subject_id,
        } => {
            let repo_id: Option<i64> = sqlx::query_scalar(
                "SELECT repo_id FROM notifications
                  WHERE user_id = $1 AND subject_type = $2 AND subject_id = $3
                 UNION ALL
                 SELECT repo_id FROM thread_subscriptions
                  WHERE user_id = $1 AND subject_type = $2 AND subject_id = $3
                 LIMIT 1",
            )
            .bind(user_id)
            .bind(&subject_type)
            .bind(subject_id)
            .fetch_optional(&state.db)
            .await?;
            let repo_id = repo_id.ok_or(ApiError::NotFound)?;
            let mut tx = Tx::begin(&state).await?;
            set_thread_subscription(
                &mut tx,
                user_id,
                &subject_type,
                subject_id,
                repo_id,
                false,
                false,
                None,
            )
            .await?;
            tx.commit().await?;
            Ok(page(
                &state,
                "Unsubscribed",
                "<p>You won't receive notifications for this conversation unless you participate or are @mentioned.</p>",
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_roundtrip_and_reject_tampering() {
        let t = token("s3cret", 42, Some(("Issue", 7)));
        assert_eq!(
            verify("s3cret", &t),
            Some(Unsub::Thread {
                user_id: 42,
                subject_type: "Issue".into(),
                subject_id: 7
            })
        );
        assert_eq!(verify("other", &t), None);
        assert_eq!(verify("s3cret", &t.replace("42.", "43.")), None);
        let all = token("s3cret", 42, None);
        assert_eq!(
            verify("s3cret", &all),
            Some(Unsub::AllEmail { user_id: 42 })
        );
        assert_eq!(verify("s3cret", "garbage"), None);
    }

    #[test]
    fn prefs_filter_reasons() {
        let mut p = Prefs::defaults(1);
        assert!(p.web_allows(Reason::Mention) && p.email_allows(Reason::Mention));
        apply(
            &mut p.email_disabled,
            &HashMap::from([("subscribed".to_string(), false)]),
            "email",
        )
        .unwrap();
        assert!(!p.email_allows(Reason::Subscribed));
        assert!(p.email_allows(Reason::Mention));
        p.email_enabled = false;
        assert!(!p.email_allows(Reason::Mention));
        assert!(
            apply(
                &mut p.web_disabled,
                &HashMap::from([("x".to_string(), true)]),
                "web"
            )
            .is_err()
        );
    }
}
