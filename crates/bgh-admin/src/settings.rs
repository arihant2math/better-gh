//! Site settings: `GET/PATCH /_bgh/admin/settings`, the public banner info
//! `GET /_bgh/site`, GHES `/enterprise/announcement` and `GET /rate_limit`.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use bgh_core::audit::Target;
use bgh_core::prelude::*;
use bgh_core::settings::{self, Announcement, SECTIONS, SiteSettings};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::common::log;

/// Placeholder returned instead of stored secrets; sending it back keeps
/// the stored value.
pub const REDACTED: &str = "********";

/// Paths (`section`, `field`) of secret values inside settings.
fn redact(mut v: Value) -> Value {
    if let Some(p) = v.pointer_mut("/smtp/password")
        && !p.is_null()
    {
        *p = json!(REDACTED);
    }
    if let Some(list) = v
        .pointer_mut("/auth_providers/oidc")
        .and_then(Value::as_array_mut)
    {
        for p in list {
            if let Some(s) = p.get_mut("client_secret")
                && !s.is_null()
            {
                *s = json!(REDACTED);
            }
        }
    }
    v
}

/// Replace redacted placeholders in `new` with the stored secrets.
fn keep_secrets(section: &str, new: &mut Value, old: &Value) {
    match section {
        "smtp" => {
            if new.get("password").and_then(Value::as_str) == Some(REDACTED) {
                new["password"] = old.get("password").cloned().unwrap_or(Value::Null);
            }
        }
        "auth_providers" => {
            let old_list = old
                .get("oidc")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if let Some(list) = new.get_mut("oidc").and_then(Value::as_array_mut) {
                for p in list {
                    if p.get("client_secret").and_then(Value::as_str) == Some(REDACTED) {
                        let name = p.get("name").cloned();
                        p["client_secret"] = old_list
                            .iter()
                            .find(|o| o.get("name") == name.as_ref())
                            .and_then(|o| o.get("client_secret").cloned())
                            .unwrap_or(Value::Null);
                    }
                }
            }
        }
        _ => {}
    }
}

fn validate(s: &SiteSettings) -> ApiResult<()> {
    let bad = |field: &str| ApiError::invalid_field(FieldError::invalid("SiteSettings", field));
    if !matches!(
        s.repositories.default_visibility.as_str(),
        "public" | "private" | "internal"
    ) {
        return Err(bad("repositories.default_visibility"));
    }
    if s.repositories.max_repo_size_mb.is_some_and(|m| m <= 0) {
        return Err(bad("repositories.max_repo_size_mb"));
    }
    if s.rate_limits.authenticated_per_hour <= 0 || s.rate_limits.unauthenticated_per_hour <= 0 {
        return Err(bad("rate_limits"));
    }
    if !matches!(s.smtp.tls.as_str(), "none" | "starttls" | "tls") {
        return Err(bad("smtp.tls"));
    }
    if s.smtp.enabled && (s.smtp.host.is_empty() || s.smtp.from.is_empty()) {
        return Err(ApiError::invalid_field(FieldError::custom(
            "SiteSettings",
            "smtp",
            "host and from are required when SMTP is enabled",
        )));
    }
    let mut names = std::collections::HashSet::new();
    for p in &s.auth_providers.oidc {
        if p.name.is_empty()
            || !p
                .name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || !names.insert(p.name.to_ascii_lowercase())
        {
            return Err(bad("auth_providers.oidc.name"));
        }
        if !p.issuer.starts_with("https://") && !p.issuer.starts_with("http://") {
            return Err(bad("auth_providers.oidc.issuer"));
        }
        if p.client_id.is_empty() {
            return Err(bad("auth_providers.oidc.client_id"));
        }
    }
    if !s.auth_providers.password_login && s.auth_providers.oidc.is_empty() {
        return Err(ApiError::invalid_field(FieldError::custom(
            "SiteSettings",
            "auth_providers",
            "at least one sign-in method must stay enabled",
        )));
    }
    for d in &s.signup.allowed_email_domains {
        if d.is_empty() || d.contains('@') || d.contains(char::is_whitespace) {
            return Err(bad("signup.allowed_email_domains"));
        }
    }
    Ok(())
}

/// `GET /_bgh/admin/settings` → all sections (secrets redacted).
pub async fn get(State(state): State<AppState>, _auth: RequireSiteAdmin) -> ApiResult<Json<Value>> {
    let s = settings::load_uncached(&state.db).await?;
    Ok(Json(redact(serde_json::to_value(&s)?)))
}

/// `PATCH /_bgh/admin/settings` with `{section: {field: value}}`: fields
/// are merged into the stored section; the result is validated as a whole.
pub async fn update(
    State(state): State<AppState>,
    auth: RequireSiteAdmin,
    headers: HeaderMap,
    Json(body): Json<Map<String, Value>>,
) -> ApiResult<Json<Value>> {
    for key in body.keys() {
        if !SECTIONS.contains(&key.as_str()) {
            return Err(ApiError::invalid_field(FieldError::custom(
                "SiteSettings",
                key,
                format!("unknown settings section {key:?}"),
            )));
        }
    }
    let mut tx = Tx::begin(&state).await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('bgh_site_settings'))")
        .execute(&mut *tx)
        .await?;
    let current = serde_json::to_value(settings::load_uncached(&mut *tx).await?)?;
    let mut merged = current.clone();
    for (key, patch) in &body {
        let Value::Object(patch) = patch else {
            return Err(ApiError::invalid_field(FieldError::invalid(
                "SiteSettings",
                key,
            )));
        };
        let old = current.get(key).cloned().unwrap_or(json!({}));
        let mut section = old.clone();
        for (k, v) in patch {
            section[k] = v.clone();
        }
        keep_secrets(key, &mut section, &old);
        merged[key] = section;
    }
    let typed: SiteSettings = serde_json::from_value(merged.clone())
        .map_err(|e| ApiError::unprocessable(format!("Invalid settings: {e}")))?;
    validate(&typed)?;
    let normalized = serde_json::to_value(&typed)?;
    for key in body.keys() {
        settings::store_section(&mut *tx, key, &normalized[key]).await?;
    }
    let changed: Map<String, Value> = body
        .keys()
        .map(|k| (k.clone(), redact(normalized.clone())[k].clone()))
        .collect();
    log(
        &mut tx,
        &auth,
        &headers,
        "business.update_settings",
        Target::Site,
        Value::Object(changed),
    )
    .await?;
    tx.commit().await?;
    settings::invalidate(&state);
    Ok(Json(redact(normalized)))
}

/// `GET /_bgh/site` (public): banner, maintenance and sign-in options for
/// the web client.
pub async fn site(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let s = settings::load(&state).await?;
    Ok(Json(settings::public_info(&state, &s)))
}

/// GHES `announcement`
#[derive(Debug, Serialize)]
pub struct AnnouncementJson {
    pub announcement: Option<String>,
    pub expires_at: Option<Timestamp>,
    pub user_dismissible: bool,
}

impl From<&Announcement> for AnnouncementJson {
    fn from(a: &Announcement) -> Self {
        Self {
            announcement: a.message.clone(),
            expires_at: a.expires_at.map(Timestamp::from),
            user_dismissible: a.user_dismissible,
        }
    }
}

/// `GET /enterprise/announcement`
pub async fn get_announcement(
    State(state): State<AppState>,
    _auth: RequireSiteAdmin,
) -> ApiResult<Json<AnnouncementJson>> {
    let s = settings::load_uncached(&state.db).await?;
    Ok(Json(AnnouncementJson::from(&s.announcement)))
}

#[derive(Debug, Deserialize)]
pub struct AnnouncementBody {
    pub announcement: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub user_dismissible: bool,
}

async fn store_announcement(
    state: &AppState,
    auth: &AuthContext,
    headers: &HeaderMap,
    a: &Announcement,
    action: &str,
) -> ApiResult<()> {
    let mut tx = Tx::begin(state).await?;
    settings::store_section(&mut *tx, "announcement", &serde_json::to_value(a)?).await?;
    log(
        &mut tx,
        auth,
        headers,
        action,
        Target::Site,
        json!({ "announcement": a.message, "expires_at": a.expires_at }),
    )
    .await?;
    tx.commit().await?;
    settings::invalidate(state);
    Ok(())
}

/// `PATCH /enterprise/announcement`
pub async fn set_announcement(
    State(state): State<AppState>,
    auth: RequireSiteAdmin,
    headers: HeaderMap,
    Json(body): Json<AnnouncementBody>,
) -> ApiResult<Json<AnnouncementJson>> {
    let message = body
        .announcement
        .filter(|m| !m.trim().is_empty())
        .ok_or_else(|| {
            ApiError::invalid_field(FieldError::missing_field("Announcement", "announcement"))
        })?;
    let a = Announcement {
        message: Some(message),
        expires_at: body.expires_at,
        user_dismissible: body.user_dismissible,
    };
    store_announcement(&state, &auth, &headers, &a, "business.set_announcement").await?;
    Ok(Json(AnnouncementJson::from(&a)))
}

/// `DELETE /enterprise/announcement` → 204.
pub async fn delete_announcement(
    State(state): State<AppState>,
    auth: RequireSiteAdmin,
    headers: HeaderMap,
) -> ApiResult<StatusCode> {
    store_announcement(
        &state,
        &auth,
        &headers,
        &Announcement::default(),
        "business.remove_announcement",
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /rate_limit`: 404 "Rate limiting is not enabled." unless enabled.
pub async fn rate_limit(
    State(state): State<AppState>,
    auth: MaybeUser,
    req: axum::extract::Request,
) -> ApiResult<Json<Value>> {
    let ip = bgh_core::auth::client_ip(&state.config, req.headers(), req.extensions());
    let q = bgh_core::ratelimit::quota(&state, auth.as_ref(), &ip, false)
        .await?
        .ok_or_else(|| {
            ApiError::Status(
                StatusCode::NOT_FOUND,
                "Rate limiting is not enabled.".into(),
            )
        })?;
    Ok(Json(json!({
        "resources": { "core": q },
        "rate": q,
    })))
}
