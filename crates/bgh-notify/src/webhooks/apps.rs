//! GitHub App webhooks (P46).
//!
//! An app has one hook (P17's `github_apps.webhook_*` columns plus content
//! type / `insecure_ssl`). Every domain event whose webhook name an
//! installation subscribed to (`app_installations.events`, accepted at
//! install time) is delivered there when the installation covers the
//! event's repository (or is installed on the event's organization), with
//! an `installation` object added to the payload. `installation` and
//! `installation_repositories` events go only to the app's hook.
//! Deliveries are `webhook_deliveries` rows with `app_id` set, delivered by
//! the same `notify.deliver_webhook` job (signed with the app's secret).
//!
//! REST (app JWT): `GET|PATCH /app/hook/config`, `GET /app/hook/deliveries`,
//! `GET /app/hook/deliveries/{id}`, `POST /app/hook/deliveries/{id}/attempts`.
//! Web client (app managers): `GET /_bgh/apps/{slug}/hook/deliveries[/{id}]`,
//! `POST /_bgh/apps/{slug}/hook/deliveries/{id}/attempts`.

use std::collections::{HashMap, HashSet};

use axum::extract::State;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bgh_core::apps::AppRow;
use bgh_core::events::{self, Event};
use bgh_core::prelude::*;
use serde::Serialize;
use serde_json::{Value, json};

use super::deliver::DeliverWebhook;
use super::deliveries::{Delivery, DeliveryItem, DeliveryRow, ListParams, detail, item};
use super::{ConfigBody, HookConfig, invalid, parse_insecure, ssrf};
use crate::payloads::{self, HookEvent};

/// An installation whose app hook may receive a delivery.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AppTarget {
    pub installation_id: i64,
    pub app_id: i64,
    pub account_id: i64,
    pub repository_selection: String,
    pub events: Vec<String>,
    pub url: String,
    pub content_type: String,
}

/// App targets for a domain event, with what's needed to scope each
/// delivery.
#[derive(Debug, Default)]
pub struct Candidates {
    targets: Vec<AppTarget>,
    /// Repository → owner.
    owners: HashMap<i64, i64>,
    /// `(installation, repository)` pairs of selected installations.
    selected: HashSet<(i64, i64)>,
}

impl Candidates {
    pub fn is_empty(&self) -> bool {
        self.targets.is_empty()
    }

    /// Targets that receive `d`.
    fn matching<'a>(&'a self, d: &'a HookEvent) -> impl Iterator<Item = &'a AppTarget> + 'a {
        self.targets.iter().filter(move |t| {
            if !t.events.iter().any(|e| e == d.event) {
                return false;
            }
            match d.repo_id {
                Some(r) => {
                    self.owners.get(&r) == Some(&t.account_id)
                        && (t.repository_selection == "all"
                            || self.selected.contains(&(t.installation_id, r)))
                }
                None => d.org_id == Some(t.account_id),
            }
        })
    }
}

const TARGET_SELECT: &str = "SELECT i.id AS installation_id, i.app_id, i.account_id,
        i.repository_selection, i.events, a.webhook_url AS url,
        a.webhook_content_type AS content_type
   FROM app_installations i JOIN github_apps a ON a.id = i.app_id
  WHERE a.webhook_active AND coalesce(a.webhook_url, '') <> ''
    AND i.suspended_at IS NULL";

/// Installations (of apps with an active hook) on the owners of `repo_ids`
/// or on `org_ids` that subscribed to one of `names`.
pub async fn candidates(
    state: &AppState,
    repo_ids: &[i64],
    org_ids: &[i64],
    names: &[&str],
) -> ApiResult<Candidates> {
    let owners: Vec<(i64, i64)> = if repo_ids.is_empty() {
        Vec::new()
    } else {
        sqlx::query_as("SELECT id, owner_id FROM repositories WHERE id = ANY($1)")
            .bind(repo_ids)
            .fetch_all(&state.db)
            .await?
    };
    let mut accounts: Vec<i64> = owners.iter().map(|(_, o)| *o).collect();
    accounts.extend_from_slice(org_ids);
    accounts.sort_unstable();
    accounts.dedup();
    if accounts.is_empty() {
        return Ok(Candidates::default());
    }
    let targets: Vec<AppTarget> = sqlx::query_as(&format!(
        "{TARGET_SELECT} AND i.account_id = ANY($1) AND i.events && $2 ORDER BY i.id"
    ))
    .bind(&accounts)
    .bind(names)
    .fetch_all(&state.db)
    .await?;
    if targets.is_empty() {
        return Ok(Candidates::default());
    }
    let selected_ids: Vec<i64> = targets
        .iter()
        .filter(|t| t.repository_selection == "selected")
        .map(|t| t.installation_id)
        .collect();
    let selected: Vec<(i64, i64)> = if selected_ids.is_empty() || repo_ids.is_empty() {
        Vec::new()
    } else {
        sqlx::query_as(
            "SELECT installation_id, repo_id FROM app_installation_repos
              WHERE installation_id = ANY($1) AND repo_id = ANY($2)",
        )
        .bind(&selected_ids)
        .bind(repo_ids)
        .fetch_all(&state.db)
        .await?
    };
    Ok(Candidates {
        targets,
        owners: owners.into_iter().collect(),
        selected: selected.into_iter().collect(),
    })
}

/// The payload delivered to an app: `installation` added.
fn with_installation(payload: &Value, installation_id: i64) -> Value {
    let mut p = payload.clone();
    if let Some(m) = p.as_object_mut() {
        m.insert(
            "installation".into(),
            payloads::apps::installation_ref(installation_id),
        );
    }
    p
}

/// Insert a pending app hook delivery and enqueue its job. `key` is the
/// outbox `(event_id, seq)`: `None` returned when already delivered.
#[allow(clippy::too_many_arguments)]
async fn insert(
    tx: &mut Tx,
    app_id: i64,
    installation_id: Option<i64>,
    url: &str,
    content_type: &str,
    d: (&str, Option<&str>, Option<i64>),
    payload: &Value,
    redelivery: bool,
    key: Option<(i64, i32)>,
) -> ApiResult<Option<i64>> {
    let (event, action, repo_id) = d;
    let raw = serde_json::to_string(payload)?;
    let (event_id, event_seq) = key.unzip();
    let id: Option<i64> = sqlx::query_scalar(
        "INSERT INTO webhook_deliveries
                (app_id, guid, event, action, repo_id, installation_id, redelivery, status, url,
                 payload_raw, content_type, event_id, event_seq)
         VALUES ($1, $2, $3, $4, $5, $6, $7, 'pending', $8, $9, $10, $11, $12)
         ON CONFLICT (app_id, event_id, event_seq)
            WHERE app_id IS NOT NULL AND event_id IS NOT NULL DO NOTHING
         RETURNING id",
    )
    .bind(app_id)
    .bind(uuid::Uuid::new_v4())
    .bind(event)
    .bind(action)
    .bind(repo_id)
    .bind(installation_id)
    .bind(redelivery)
    .bind(url)
    .bind(&raw)
    .bind(content_type)
    .bind(event_id)
    .bind(event_seq)
    .fetch_optional(&mut **tx)
    .await?;
    if let Some(id) = id {
        tx.enqueue(&DeliverWebhook { delivery_id: id }).await?;
    }
    Ok(id)
}

/// Queue `d` for every matching app target. `seq` keys the outbox event;
/// app deliveries use a separate key space from hooks (their own unique
/// index), so the same `seq` is fine.
pub async fn queue_for(
    tx: &mut Tx,
    c: &Candidates,
    d: &HookEvent,
    key: Option<(i64, i32)>,
) -> ApiResult<usize> {
    let mut n = 0;
    for t in c.matching(d) {
        let payload = with_installation(&d.payload, t.installation_id);
        if insert(
            tx,
            t.app_id,
            Some(t.installation_id),
            &t.url,
            &t.content_type,
            (d.event, d.action.as_deref(), d.repo_id),
            &payload,
            false,
            key,
        )
        .await?
        .is_some()
        {
            n += 1;
        }
    }
    Ok(n)
}

/// `installation` / `installation_repositories`: only to the app's hook,
/// whatever its subscriptions (GitHub always sends them).
pub async fn dispatch_lifecycle(state: &AppState, event: &Event) -> ApiResult<usize> {
    let (app_id, installation_id) = match event {
        Event::AppInstallationChanged {
            app_id,
            installation_id,
            ..
        }
        | Event::AppInstallationRepositoriesChanged {
            app_id,
            installation_id,
            ..
        } => (*app_id, *installation_id),
        _ => return Ok(0),
    };
    let app: Option<(String, String)> = sqlx::query_as(
        "SELECT webhook_url, webhook_content_type FROM github_apps
          WHERE id = $1 AND webhook_active AND coalesce(webhook_url, '') <> ''",
    )
    .bind(app_id)
    .fetch_optional(&state.db)
    .await?;
    let Some((url, content_type)) = app else {
        return Ok(0);
    };
    let deliveries = payloads::for_event(state, event)
        .await
        .map_err(ApiError::internal)?;
    let event_id = events::current_event_id();
    let mut tx = Tx::begin(state).await?;
    let mut n = 0;
    for (seq, d) in deliveries.iter().enumerate() {
        let key = event_id.map(|id| (id, seq as i32));
        if insert(
            &mut tx,
            app_id,
            Some(installation_id),
            &url,
            &content_type,
            (d.event, d.action.as_deref(), None),
            &d.payload,
            false,
            key,
        )
        .await?
        .is_some()
        {
            n += 1;
        }
    }
    tx.commit().await?;
    Ok(n)
}

// ---------------------------------------------------------------------------
// REST: /app/hook/*
// ---------------------------------------------------------------------------

/// The app of a JWT caller (401 "A JSON web token could not be decoded"
/// for other credentials).
async fn jwt_app(state: &AppState, auth: &MaybeUser) -> ApiResult<AppRow> {
    let app_id = auth
        .as_ref()
        .and_then(bgh_core::apps::jwt_app_id)
        .ok_or_else(|| ApiError::Unauthorized {
            message: bgh_core::apps::JWT_REQUIRED.into(),
            www_authenticate: None,
        })?;
    load_app(&state.db, app_id).await
}

async fn load_app(db: &sqlx::PgPool, id: i64) -> ApiResult<AppRow> {
    sqlx::query_as(&format!(
        "SELECT {} FROM github_apps WHERE id = $1",
        AppRow::COLUMNS
    ))
    .bind(id)
    .fetch_optional(db)
    .await?
    .ok_or(ApiError::NotFound)
}

/// GitHub's `webhook-config` for an app.
pub fn app_config(app: &AppRow) -> HookConfig {
    HookConfig {
        content_type: app.webhook_content_type.clone(),
        insecure_ssl: if app.webhook_insecure_ssl { "1" } else { "0" }.into(),
        url: app.webhook_url.clone().unwrap_or_default(),
        secret: app.webhook_secret.as_ref().map(|_| "********".into()),
    }
}

/// `GET /app/hook/config`
pub async fn get_config(
    State(state): State<AppState>,
    auth: MaybeUser,
) -> ApiResult<Json<HookConfig>> {
    let app = jwt_app(&state, &auth).await?;
    Ok(Json(app_config(&app)))
}

/// `PATCH /app/hook/config` `{url, content_type, secret, insecure_ssl}`
pub async fn update_config(
    State(state): State<AppState>,
    auth: MaybeUser,
    Json(c): Json<ConfigBody>,
) -> ApiResult<Json<HookConfig>> {
    let app = jwt_app(&state, &auth).await?;
    let mut url = app.webhook_url.clone();
    if let Some(u) = c.url {
        if u.trim().is_empty() {
            url = None;
        } else {
            let policy = ssrf::Policy::load(&state).await;
            url = Some(
                ssrf::validate_url(&policy, &u)
                    .map_err(invalid)?
                    .to_string(),
            );
        }
    }
    let content_type = match c.content_type.as_deref() {
        None => app.webhook_content_type.clone(),
        Some("json" | "application/json") => "json".into(),
        Some("form" | "application/x-www-form-urlencoded") => "form".into(),
        Some(_) => return Err(invalid("Config content_type must be json or form")),
    };
    let insecure = match &c.insecure_ssl {
        Some(v) => parse_insecure(v)?,
        None => app.webhook_insecure_ssl,
    };
    let secret: Option<Option<Vec<u8>>> = match c.secret {
        None => None,
        Some(s) if s.is_empty() => Some(None),
        Some(s) => Some(Some(bgh_core::secretbox::seal(&state, &s)?)),
    };
    let mut tx = Tx::begin(&state).await?;
    sqlx::query(
        "UPDATE github_apps SET webhook_url = $2, webhook_content_type = $3,
                webhook_insecure_ssl = $4,
                webhook_secret = CASE WHEN $5 THEN $6 ELSE webhook_secret END,
                updated_at = now()
          WHERE id = $1",
    )
    .bind(app.id)
    .bind(&url)
    .bind(&content_type)
    .bind(insecure)
    .bind(secret.is_some())
    .bind(secret.flatten())
    .execute(&mut *tx)
    .await?;
    bgh_core::audit::log(
        &mut *tx,
        None,
        "integration.update",
        bgh_core::audit::Target::User(app.owner_id),
        json!({ "integration": app.slug, "webhook_url": url, "via": "app/hook/config" }),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(app_config(&load_app(&state.db, app.id).await?)))
}

/// Deliveries of an app's hook, newest first (cursor pagination like
/// repository hooks).
async fn list(state: &AppState, app: &AppRow, base: &str, q: ListParams) -> ApiResult<Response> {
    let per_page = i64::from(q.per_page.unwrap_or(30).clamp(1, 100));
    let before = super::deliveries::parse_cursor(q.cursor.as_deref())?;
    let status = match q.status.as_deref() {
        None | Some("") => None,
        Some("success") => Some(true),
        Some("failure") => Some(false),
        Some(_) => {
            return Err(ApiError::invalid_field(FieldError::invalid(
                "HookDelivery",
                "status",
            )));
        }
    };
    let mut rows: Vec<DeliveryRow> = sqlx::query_as(&format!(
        "SELECT {} FROM webhook_deliveries
          WHERE app_id = $1 AND ($2::bigint IS NULL OR id < $2)
            AND ($3::boolean IS NULL OR (status = 'OK') = $3)
            AND ($4::boolean IS NULL OR redelivery = $4)
          ORDER BY id DESC LIMIT $5",
        DeliveryRow::COLUMNS
    ))
    .bind(app.id)
    .bind(before)
    .bind(status)
    .bind(q.redelivery)
    .bind(per_page + 1)
    .fetch_all(&state.db)
    .await?;
    let has_next = rows.len() as i64 > per_page;
    rows.truncate(per_page as usize);
    let items: Vec<DeliveryItem> = rows.iter().map(item).collect();
    let mut resp = Json(items).into_response();
    if has_next && let Some(last) = rows.last() {
        let mut extra = String::new();
        if let Some(s) = &q.status {
            extra.push_str(&format!("&status={s}"));
        }
        let link = format!(
            "<{base}?per_page={per_page}&cursor=v1_{}{extra}>; rel=\"next\"",
            last.id
        );
        if let Ok(v) = HeaderValue::from_str(&link) {
            resp.headers_mut().insert(header::LINK, v);
        }
    }
    Ok(resp)
}

async fn load(state: &AppState, app: &AppRow, id: i64) -> ApiResult<DeliveryRow> {
    sqlx::query_as(&format!(
        "SELECT {} FROM webhook_deliveries WHERE id = $1 AND app_id = $2",
        DeliveryRow::COLUMNS
    ))
    .bind(id)
    .bind(app.id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)
}

async fn redeliver(
    state: &AppState,
    app: &AppRow,
    id: i64,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let d = load(state, app, id).await?;
    if d.payload_raw.is_empty() {
        return Err(ApiError::unprocessable(
            "The payload of this delivery is no longer available for redelivery.",
        ));
    }
    let Some(url) = app.webhook_url.as_deref().filter(|u| !u.is_empty()) else {
        return Err(ApiError::unprocessable(
            "This app has no webhook URL configured.",
        ));
    };
    let mut tx = Tx::begin(state).await?;
    let new_id: i64 = sqlx::query_scalar(
        "INSERT INTO webhook_deliveries
                (app_id, guid, event, action, repo_id, installation_id, redelivery, status,
                 url, payload_raw, content_type)
         VALUES ($1, $2, $3, $4, $5, $6, true, 'pending', $7, $8, $9) RETURNING id",
    )
    .bind(app.id)
    .bind(d.guid)
    .bind(&d.event)
    .bind(&d.action)
    .bind(d.repo_id)
    .bind(d.installation_id)
    .bind(url)
    .bind(&d.payload_raw)
    .bind(&app.webhook_content_type)
    .fetch_one(&mut *tx)
    .await?;
    tx.enqueue(&DeliverWebhook {
        delivery_id: new_id,
    })
    .await?;
    tx.commit().await?;
    Ok((StatusCode::ACCEPTED, Json(json!({}))))
}

/// `GET /app/hook/deliveries`
pub async fn list_deliveries(
    State(state): State<AppState>,
    auth: MaybeUser,
    Query(q): Query<ListParams>,
) -> ApiResult<Response> {
    let app = jwt_app(&state, &auth).await?;
    let base = state.urls.api("/app/hook/deliveries");
    list(&state, &app, &base, q).await
}

/// `GET /app/hook/deliveries/{delivery_id}`
pub async fn get_delivery(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path(id): Path<i64>,
) -> ApiResult<Json<Delivery>> {
    let app = jwt_app(&state, &auth).await?;
    Ok(Json(detail(&load(&state, &app, id).await?)))
}

/// `POST /app/hook/deliveries/{delivery_id}/attempts` → 202
pub async fn redeliver_delivery(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path(id): Path<i64>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let app = jwt_app(&state, &auth).await?;
    redeliver(&state, &app, id).await
}

// ---------------------------------------------------------------------------
// Web client: app managers ("Advanced" tab)
// ---------------------------------------------------------------------------

/// App `slug` if the viewer manages it (owner, or an admin of the owning
/// organization), else 404. Browser sessions only.
async fn managed_app(state: &AppState, auth: &AuthContext, slug: &str) -> ApiResult<AppRow> {
    if !auth.is_session() {
        return Err(ApiError::NotFound);
    }
    let app: AppRow = sqlx::query_as(&format!(
        "SELECT {} FROM github_apps WHERE lower(slug) = lower($1)",
        AppRow::COLUMNS
    ))
    .bind(slug)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)?;
    let ok = auth.user.site_admin
        || app.owner_id == auth.user.id
        || bgh_core::perms::org_role(&state.db, app.owner_id, auth.user.id)
            .await?
            .as_deref()
            == Some("admin");
    if ok { Ok(app) } else { Err(ApiError::NotFound) }
}

#[derive(Debug, Serialize)]
pub struct WebHookStatus {
    pub config: HookConfig,
    pub active: bool,
    pub last_response: Value,
}

/// `GET /_bgh/apps/{slug}/hook`: config and last response.
pub async fn web_hook(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(slug): Path<String>,
) -> ApiResult<Json<WebHookStatus>> {
    let app = managed_app(&state, &auth, &slug).await?;
    let last: Value =
        sqlx::query_scalar("SELECT webhook_last_response FROM github_apps WHERE id = $1")
            .bind(app.id)
            .fetch_one(&state.db)
            .await?;
    Ok(Json(WebHookStatus {
        config: app_config(&app),
        active: app.webhook_active,
        last_response: last,
    }))
}

/// `GET /_bgh/apps/{slug}/hook/deliveries`
pub async fn web_list(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(slug): Path<String>,
    Query(q): Query<ListParams>,
) -> ApiResult<Response> {
    let app = managed_app(&state, &auth, &slug).await?;
    let base = state
        .urls
        .html(&format!("/_bgh/apps/{}/hook/deliveries", app.slug));
    list(&state, &app, &base, q).await
}

/// `GET /_bgh/apps/{slug}/hook/deliveries/{id}`
pub async fn web_get(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((slug, id)): Path<(String, i64)>,
) -> ApiResult<Json<Delivery>> {
    let app = managed_app(&state, &auth, &slug).await?;
    Ok(Json(detail(&load(&state, &app, id).await?)))
}

/// `POST /_bgh/apps/{slug}/hook/deliveries/{id}/attempts` → 202
pub async fn web_redeliver(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((slug, id)): Path<(String, i64)>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let app = managed_app(&state, &auth, &slug).await?;
    redeliver(&state, &app, id).await
}
