//! GHES global webhooks (`/admin/hooks`). Stored in `webhooks` with neither
//! `repo_id` nor `org_id`; delivery is bgh-notify's job, driven by the
//! `UserAccountChanged` / `OrganizationChanged` / `GlobalHookPing` events.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use bgh_core::audit::Target;
use bgh_core::prelude::*;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::FromRow;

use crate::common::log;

/// Events a global webhook may subscribe to.
pub const GLOBAL_EVENTS: &[&str] = &[
    "*",
    "user",
    "organization",
    "repository",
    "team",
    "membership",
];
const DEFAULT_EVENTS: &[&str] = &["user", "organization"];

#[derive(Debug, FromRow)]
struct HookRow {
    id: i64,
    name: String,
    url: String,
    content_type: String,
    secret: Option<String>,
    insecure_ssl: bool,
    events: Vec<String>,
    active: bool,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

const COLUMNS: &str = "id, name, url, content_type, secret, insecure_ssl, events, active, \
    created_at, updated_at";

/// `global-hook`
#[derive(Debug, Serialize)]
pub struct GlobalHook {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub id: i64,
    pub name: String,
    pub active: bool,
    pub events: Vec<String>,
    pub config: HookConfig,
    pub updated_at: Timestamp,
    pub created_at: Timestamp,
    pub url: String,
    pub ping_url: String,
}

#[derive(Debug, Serialize)]
pub struct HookConfig {
    pub url: String,
    pub content_type: String,
    pub insecure_ssl: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
}

fn render(state: &AppState, h: HookRow) -> GlobalHook {
    let url = state.urls.api(&format!("/admin/hooks/{}", h.id));
    GlobalHook {
        kind: "Global",
        id: h.id,
        name: h.name,
        active: h.active,
        events: h.events,
        config: HookConfig {
            url: h.url,
            content_type: h.content_type,
            insecure_ssl: if h.insecure_ssl { "1" } else { "0" }.into(),
            secret: h.secret.map(|_| "********".into()),
        },
        updated_at: h.updated_at.into(),
        created_at: h.created_at.into(),
        ping_url: format!("{url}/pings"),
        url,
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct ConfigBody {
    pub url: Option<String>,
    pub content_type: Option<String>,
    pub secret: Option<String>,
    /// `"0"` / `"1"` (GitHub) or a number / boolean.
    pub insecure_ssl: Option<Value>,
}

#[derive(Debug, Default, Deserialize)]
pub struct HookBody {
    pub name: Option<String>,
    pub config: Option<ConfigBody>,
    pub events: Option<Vec<String>>,
    pub active: Option<bool>,
}

fn insecure(v: &Value) -> ApiResult<bool> {
    match v {
        Value::Bool(b) => Ok(*b),
        Value::Number(n) => Ok(n.as_i64() == Some(1)),
        Value::String(s) if s == "0" => Ok(false),
        Value::String(s) if s == "1" => Ok(true),
        _ => Err(ApiError::invalid_field(FieldError::invalid(
            "Hook",
            "insecure_ssl",
        ))),
    }
}

fn validate_url(url: &str) -> ApiResult<()> {
    if url.starts_with("http://") || url.starts_with("https://") {
        Ok(())
    } else {
        Err(ApiError::invalid_field(FieldError::custom(
            "Hook",
            "url",
            "url must be an http(s) URL",
        )))
    }
}

fn validate_events(events: &[String]) -> ApiResult<Vec<String>> {
    if events.is_empty() {
        return Err(ApiError::invalid_field(FieldError::invalid(
            "Hook", "events",
        )));
    }
    let mut out = Vec::new();
    for e in events {
        if !GLOBAL_EVENTS.contains(&e.as_str()) {
            return Err(ApiError::invalid_field(FieldError::custom(
                "Hook",
                "events",
                format!("{e:?} is not a valid global webhook event"),
            )));
        }
        if !out.contains(e) {
            out.push(e.clone());
        }
    }
    Ok(out)
}

fn validate_content_type(ct: &str) -> ApiResult<()> {
    if matches!(ct, "json" | "form") {
        Ok(())
    } else {
        Err(ApiError::invalid_field(FieldError::invalid(
            "Hook",
            "content_type",
        )))
    }
}

async fn find(state: &AppState, id: i64) -> ApiResult<HookRow> {
    sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM webhooks WHERE id = $1 AND repo_id IS NULL AND org_id IS NULL"
    ))
    .bind(id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)
}

/// `GET /admin/hooks`
pub async fn list(
    State(state): State<AppState>,
    _auth: RequireSiteAdmin,
    p: Pagination,
) -> ApiResult<Page<GlobalHook>> {
    let rows: Vec<HookRow> = sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM webhooks WHERE repo_id IS NULL AND org_id IS NULL
          ORDER BY id LIMIT $1 OFFSET $2"
    ))
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    Ok(p.page(rows).map(|h| render(&state, h)))
}

/// `GET /admin/hooks/{hook_id}`
pub async fn get(
    State(state): State<AppState>,
    _auth: RequireSiteAdmin,
    Path(id): Path<i64>,
) -> ApiResult<Json<GlobalHook>> {
    let h = find(&state, id).await?;
    Ok(Json(render(&state, h)))
}

/// `POST /admin/hooks` → 201.
pub async fn create(
    State(state): State<AppState>,
    auth: RequireSiteAdmin,
    headers: HeaderMap,
    Json(body): Json<HookBody>,
) -> ApiResult<(StatusCode, Json<GlobalHook>)> {
    match body.name.as_deref() {
        Some("web") => {}
        None => {
            return Err(ApiError::invalid_field(FieldError::missing_field(
                "Hook", "name",
            )));
        }
        Some(_) => return Err(ApiError::invalid_field(FieldError::invalid("Hook", "name"))),
    }
    let config = body
        .config
        .ok_or_else(|| ApiError::invalid_field(FieldError::missing_field("Hook", "config")))?;
    let url = config
        .url
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .ok_or_else(|| ApiError::invalid_field(FieldError::missing_field("Hook", "url")))?;
    validate_url(url)?;
    let content_type = config.content_type.as_deref().unwrap_or("form");
    validate_content_type(content_type)?;
    let insecure_ssl = match &config.insecure_ssl {
        Some(v) => insecure(v)?,
        None => false,
    };
    let events = match &body.events {
        Some(e) => validate_events(e)?,
        None => DEFAULT_EVENTS.iter().map(|s| s.to_string()).collect(),
    };
    let mut tx = Tx::begin(&state).await?;
    let row: HookRow = sqlx::query_as(&format!(
        "INSERT INTO webhooks (name, url, content_type, secret, insecure_ssl, events, active)
         VALUES ('web', $1, $2, $3, $4, $5, $6) RETURNING {COLUMNS}"
    ))
    .bind(url)
    .bind(content_type)
    .bind(config.secret.as_deref().filter(|s| !s.is_empty()))
    .bind(insecure_ssl)
    .bind(&events)
    .bind(body.active.unwrap_or(true))
    .fetch_one(&mut *tx)
    .await?;
    log(
        &mut tx,
        &auth,
        &headers,
        "hook.create",
        Target::Site,
        json!({ "hook_id": row.id, "url": row.url, "events": row.events, "global": true }),
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(render(&state, row))))
}

/// `PATCH /admin/hooks/{hook_id}`. A given `config` replaces the old one
/// (like GitHub); omitted keys take their defaults, except the secret
/// which is kept unless sent.
pub async fn update(
    State(state): State<AppState>,
    auth: RequireSiteAdmin,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(body): Json<HookBody>,
) -> ApiResult<Json<GlobalHook>> {
    let old = find(&state, id).await?;
    let (url, content_type, secret, insecure_ssl) = match body.config {
        None => (old.url, old.content_type, old.secret, old.insecure_ssl),
        Some(c) => {
            let url = c
                .url
                .map(|u| u.trim().to_string())
                .filter(|u| !u.is_empty())
                .ok_or_else(|| ApiError::invalid_field(FieldError::missing_field("Hook", "url")))?;
            validate_url(&url)?;
            let ct = c.content_type.unwrap_or_else(|| "form".into());
            validate_content_type(&ct)?;
            let insecure_ssl = match &c.insecure_ssl {
                Some(v) => insecure(v)?,
                None => false,
            };
            let secret = match c.secret {
                Some(s) if s.is_empty() => None,
                Some(s) => Some(s),
                None => old.secret,
            };
            (url, ct, secret, insecure_ssl)
        }
    };
    let events = match &body.events {
        Some(e) => validate_events(e)?,
        None => old.events,
    };
    let active = body.active.unwrap_or(old.active);
    let mut tx = Tx::begin(&state).await?;
    let row: HookRow = sqlx::query_as(&format!(
        "UPDATE webhooks SET url = $2, content_type = $3, secret = $4, insecure_ssl = $5,
                events = $6, active = $7, updated_at = now()
          WHERE id = $1 RETURNING {COLUMNS}"
    ))
    .bind(id)
    .bind(&url)
    .bind(&content_type)
    .bind(&secret)
    .bind(insecure_ssl)
    .bind(&events)
    .bind(active)
    .fetch_one(&mut *tx)
    .await?;
    log(
        &mut tx,
        &auth,
        &headers,
        "hook.config_changed",
        Target::Site,
        json!({ "hook_id": id, "url": row.url, "events": row.events, "active": row.active, "global": true }),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(render(&state, row)))
}

/// `DELETE /admin/hooks/{hook_id}` → 204.
pub async fn delete(
    State(state): State<AppState>,
    auth: RequireSiteAdmin,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    let old = find(&state, id).await?;
    let mut tx = Tx::begin(&state).await?;
    sqlx::query("DELETE FROM webhooks WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    log(
        &mut tx,
        &auth,
        &headers,
        "hook.destroy",
        Target::Site,
        json!({ "hook_id": id, "url": old.url, "global": true }),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /admin/hooks/{hook_id}/pings` → 204; emits `GlobalHookPing`.
pub async fn ping(
    State(state): State<AppState>,
    auth: RequireSiteAdmin,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    find(&state, id).await?;
    state.events.emit(Event::GlobalHookPing {
        hook_id: id,
        actor_id: auth.user.id,
    });
    Ok(StatusCode::NO_CONTENT)
}
