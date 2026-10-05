//! GitHub App manifest flow (P46).
//!
//! 1. An integration's page posts a form with a `manifest` (JSON) to
//!    `POST /settings/apps/new[?state=…]` (or
//!    `/organizations/{org}/settings/apps/new`). The manifest is stored
//!    (one hour) and the browser is sent to the web client's confirmation
//!    page (`…/settings/apps/new?manifest=<token>`).
//! 2. The user confirms (`GET|POST /_bgh/app-manifests/{token}`): the app is
//!    registered with a private key, a client secret and a generated
//!    webhook secret, and the browser goes to the manifest's `redirect_url`
//!    with `?code=…&state=…`.
//! 3. The integration exchanges the code once, within an hour:
//!    `POST /app-manifests/{code}/conversions` → 201 with the app and its
//!    `client_id`, `client_secret`, `webhook_secret` and `pem`.

use std::collections::{BTreeMap, HashMap};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use bgh_core::apps::{AppRow, GeneratedKey, Integration};
use bgh_core::audit;
use bgh_core::prelude::*;
use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::manage::{self, AppBody, insert_app, insert_client_secret, insert_key};
use super::{administers, app_by_id};
use crate::util::{self, Patch};

/// Pending manifests and conversion codes expire after an hour.
const TTL: &str = "1 hour";

#[derive(Debug, sqlx::FromRow)]
struct ManifestRow {
    id: i64,
    org_id: Option<i64>,
    manifest: sqlx::types::Json<Value>,
    state: Option<String>,
    app_id: Option<i64>,
}

fn text(status: StatusCode, msg: &str) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        msg.to_string(),
    )
        .into_response()
}

/// The form fields (`application/x-www-form-urlencoded`), or a JSON body.
fn form(uri: &axum::http::Uri, body: &[u8]) -> HashMap<String, String> {
    let mut out: HashMap<String, String> = uri
        .query()
        .map(|q| {
            url::form_urlencoded::parse(q.as_bytes())
                .into_owned()
                .collect()
        })
        .unwrap_or_default();
    let trimmed = body.trim_ascii();
    if trimmed.first() == Some(&b'{') {
        out.insert(
            "manifest".into(),
            String::from_utf8_lossy(trimmed).into_owned(),
        );
    } else {
        out.extend(url::form_urlencoded::parse(trimmed).into_owned());
    }
    out
}

async fn store(
    state: &AppState,
    org: Option<&db::User>,
    uri: &axum::http::Uri,
    body: &[u8],
) -> ApiResult<Response> {
    let f = form(uri, body);
    let Some(raw) = f.get("manifest").filter(|m| !m.trim().is_empty()) else {
        return Ok(text(
            StatusCode::UNPROCESSABLE_ENTITY,
            "The manifest parameter is missing.",
        ));
    };
    let manifest: Value = match serde_json::from_str(raw) {
        Ok(v @ Value::Object(_)) => v,
        _ => {
            return Ok(text(
                StatusCode::UNPROCESSABLE_ENTITY,
                "The manifest is not a valid JSON object.",
            ));
        }
    };
    if manifest["url"].as_str().is_none_or(|u| u.is_empty()) {
        return Ok(text(
            StatusCode::UNPROCESSABLE_ENTITY,
            "The manifest must contain a url.",
        ));
    }
    let token = bgh_core::crypto::random_token(32);
    sqlx::query(
        "INSERT INTO github_app_manifests (token, org_id, manifest, state) VALUES ($1, $2, $3, $4)",
    )
    .bind(&token)
    .bind(org.map(|o| o.id))
    .bind(&manifest)
    .bind(f.get("state").filter(|s| !s.is_empty()))
    .execute(&state.db)
    .await?;
    let page = match org {
        Some(o) => format!("/organizations/{}/settings/apps/new", o.login),
        None => "/settings/apps/new".into(),
    };
    Ok(Redirect::to(&format!("{page}?manifest={token}")).into_response())
}

/// `POST /settings/apps/new` (form `manifest`, query `state`) → 303 to the
/// confirmation page.
pub async fn post_user(
    State(state): State<AppState>,
    axum::extract::OriginalUri(uri): axum::extract::OriginalUri,
    body: Bytes,
) -> ApiResult<Response> {
    store(&state, None, &uri, &body).await
}

/// `POST /organizations/{org}/settings/apps/new`
pub async fn post_org(
    State(state): State<AppState>,
    Path(org): Path<String>,
    axum::extract::OriginalUri(uri): axum::extract::OriginalUri,
    body: Bytes,
) -> ApiResult<Response> {
    let Some(org) = db::User::find_by_login(&state.db, &org)
        .await?
        .filter(|u| u.is_org())
    else {
        return Ok(text(StatusCode::NOT_FOUND, "Organization not found."));
    };
    store(&state, Some(&org), &uri, &body).await
}

async fn pending(state: &AppState, token: &str) -> ApiResult<ManifestRow> {
    sqlx::query_as(&format!(
        "SELECT id, org_id, manifest, state, app_id FROM github_app_manifests
          WHERE token = $1 AND created_at > now() - interval '{TTL}'"
    ))
    .bind(token)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)
}

/// The account the app will belong to, if the viewer may register apps
/// for it.
async fn owner_of(
    state: &AppState,
    auth: &AuthContext,
    m: &ManifestRow,
) -> ApiResult<(db::User, bool)> {
    let owner = match m.org_id {
        Some(id) => db::User::find(&state.db, id)
            .await?
            .ok_or(ApiError::NotFound)?,
        None => auth.user.clone(),
    };
    let ok = administers(state, &auth.user, &owner).await?;
    Ok((owner, ok))
}

fn str_field(m: &Value, k: &str) -> Option<String> {
    m[k].as_str().map(str::to_string).filter(|s| !s.is_empty())
}

/// The registration a manifest describes (`name` overrides its name).
fn body_from(m: &Value, owner: &db::User, name: Option<String>) -> AppBody {
    let hook = &m["hook_attributes"];
    let hook_url = hook["url"].as_str().filter(|u| !u.is_empty());
    let mut callbacks: Vec<String> = m["callback_urls"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if let Some(c) = str_field(m, "callback_url") {
        callbacks.insert(0, c);
    }
    let perms: BTreeMap<String, String> = m["default_permissions"]
        .as_object()
        .map(|o| {
            o.iter()
                .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
                .collect()
        })
        .unwrap_or_default();
    let events: Vec<String> = m["default_events"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    AppBody {
        owner: Some(owner.login.clone()),
        name: name.or_else(|| str_field(m, "name")),
        description: str_field(m, "description"),
        homepage_url: str_field(m, "url"),
        callback_urls: Some(callbacks),
        setup_url: str_field(m, "setup_url").map_or(Patch::Absent, Patch::Value),
        setup_on_update: m["setup_on_update"].as_bool(),
        webhook_active: Some(hook_url.is_some() && hook["active"].as_bool().unwrap_or(true)),
        webhook_url: hook_url.map_or(Patch::Absent, |u| Patch::Value(u.to_string())),
        webhook_secret: Patch::Absent,
        permissions: Some(perms),
        events: Some(events),
        public: m["public"].as_bool(),
        webhook_content_type: None,
        webhook_insecure_ssl: None,
    }
}

#[derive(Debug, Serialize)]
pub struct ManifestInfo {
    pub owner: api::SimpleUser,
    /// Whether the viewer may register apps for `owner`.
    pub can_create: bool,
    pub name: Option<String>,
    pub description: Option<String>,
    pub url: Option<String>,
    pub redirect_url: Option<String>,
    pub webhook_url: Option<String>,
    pub callback_urls: Vec<String>,
    pub setup_url: Option<String>,
    pub public: bool,
    pub permissions: BTreeMap<String, String>,
    pub events: Vec<String>,
    /// Set once the app was created from this manifest.
    pub app_slug: Option<String>,
}

/// `GET /_bgh/app-manifests/{token}`
pub async fn info(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(token): Path<String>,
) -> ApiResult<Json<ManifestInfo>> {
    util::require_session(&auth)?;
    let row = pending(&state, &token).await?;
    let (owner, can_create) = owner_of(&state, &auth, &row).await?;
    let m = &row.manifest.0;
    let body = body_from(m, &owner, None);
    let app_slug = match row.app_id {
        Some(id) => Some(app_by_id(&state.db, id).await?.slug),
        None => None,
    };
    Ok(Json(ManifestInfo {
        owner: api::SimpleUser::new(&state.urls, &owner),
        can_create,
        name: body.name,
        description: body.description,
        url: body.homepage_url,
        redirect_url: str_field(m, "redirect_url"),
        webhook_url: body.webhook_url.into_option().flatten(),
        callback_urls: body.callback_urls.unwrap_or_default(),
        setup_url: body.setup_url.into_option().flatten(),
        public: body.public.unwrap_or(false),
        permissions: body.permissions.unwrap_or_default(),
        events: body.events.unwrap_or_default(),
        app_slug,
    }))
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct CreateBody {
    pub name: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Credentials {
    pem: String,
    webhook_secret: String,
    client_secret: String,
}

/// `POST /_bgh/app-manifests/{token}` `{name?}` → `{redirect_url, app_slug}`.
pub async fn create(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(token): Path<String>,
    Json(req): Json<CreateBody>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    util::require_session(&auth)?;
    let row = pending(&state, &token).await?;
    if row.app_id.is_some() {
        return Err(ApiError::unprocessable(
            "An app was already created from this manifest.",
        ));
    }
    let (owner, can_create) = owner_of(&state, &auth, &row).await?;
    if !can_create || owner.kind == "Bot" {
        return Err(ApiError::NotFound);
    }
    let m = &row.manifest.0;
    let webhook_secret = hex::encode(rand::rng().random::<[u8; 20]>());
    let mut body = body_from(m, &owner, req.name.filter(|n| !n.trim().is_empty()));
    if !body.webhook_url.is_absent() {
        body.webhook_secret = Patch::Value(webhook_secret.clone());
    }
    manage::validate(&body, true)?;
    let key: GeneratedKey = tokio::task::spawn_blocking(bgh_core::apps::generate_key).await??;
    let code = hex::encode(rand::rng().random::<[u8; 20]>());
    let mut tx = Tx::begin(&state).await?;
    let app = insert_app(&state, &mut tx, &auth.user, &owner, &body).await?;
    insert_key(&mut tx, app.id, &key).await?;
    let (_, client_secret, _) = insert_client_secret(&mut tx, app.id, Some(auth.user.id)).await?;
    let creds = Credentials {
        pem: key.private_pem,
        webhook_secret: if body.webhook_url.is_absent() {
            String::new()
        } else {
            webhook_secret
        },
        client_secret,
    };
    let sealed = bgh_core::secretbox::seal(&state, &serde_json::to_string(&creds)?)?;
    let updated = sqlx::query(
        "UPDATE github_app_manifests SET user_id = $2, app_id = $3, code_hash = $4,
                credentials = $5
          WHERE id = $1 AND app_id IS NULL",
    )
    .bind(row.id)
    .bind(auth.user.id)
    .bind(app.id)
    .bind(bgh_core::crypto::sha256_hex(&code))
    .bind(sealed)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if updated == 0 {
        return Err(ApiError::unprocessable(
            "An app was already created from this manifest.",
        ));
    }
    tx.commit().await?;
    let redirect = match str_field(m, "redirect_url").and_then(|u| url::Url::parse(&u).ok()) {
        Some(mut u) => {
            u.query_pairs_mut().append_pair("code", &code);
            if let Some(s) = &row.state {
                u.query_pairs_mut().append_pair("state", s);
            }
            u.to_string()
        }
        None => {
            let base = if owner.is_org() {
                format!("/organizations/{}/settings/apps/{}", owner.login, app.slug)
            } else {
                format!("/settings/apps/{}", app.slug)
            };
            state.urls.html(&base)
        }
    };
    Ok((
        StatusCode::CREATED,
        Json(json!({ "redirect_url": redirect, "app_slug": app.slug })),
    ))
}

/// GitHub's manifest conversion response: the app plus its credentials.
#[derive(Debug, Serialize)]
pub struct Conversion {
    #[serde(flatten)]
    pub app: Integration,
    pub client_secret: String,
    pub webhook_secret: Option<String>,
    pub pem: String,
}

/// `POST /app-manifests/{code}/conversions` → 201 (no authentication; the
/// code is single-use and expires after an hour).
pub async fn convert(
    State(state): State<AppState>,
    _headers: HeaderMap,
    Path(code): Path<String>,
) -> ApiResult<(StatusCode, Json<Conversion>)> {
    let mut tx = Tx::begin(&state).await?;
    let found: Option<(i64, Vec<u8>)> = sqlx::query_as(&format!(
        "WITH old AS (
             SELECT id, app_id, credentials FROM github_app_manifests
              WHERE code_hash = $1 AND converted_at IS NULL AND credentials IS NOT NULL
                AND app_id IS NOT NULL AND created_at > now() - interval '{TTL}'
              FOR UPDATE)
         UPDATE github_app_manifests m SET converted_at = now(), credentials = NULL
           FROM old WHERE m.id = old.id
         RETURNING old.app_id, old.credentials"
    ))
    .bind(bgh_core::crypto::sha256_hex(&code))
    .fetch_optional(&mut *tx)
    .await?;
    let (app_id, sealed) = found.ok_or(ApiError::NotFound)?;
    let creds: Credentials = serde_json::from_str(&bgh_core::secretbox::open(&state, &sealed)?)?;
    let app: AppRow = app_by_id(&mut *tx, app_id).await?;
    let owner = db::User::find(&mut *tx, app.owner_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    audit::log(
        &mut *tx,
        None,
        "integration.manifest_conversion",
        manage::audit_target(&owner),
        json!({ "integration": app.slug, "app_id": app.id }),
    )
    .await?;
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        Json(Conversion {
            app: Integration::new(&state.urls, &app, &owner, None),
            client_secret: creds.client_secret,
            webhook_secret: Some(creds.webhook_secret).filter(|s| !s.is_empty()),
            pem: creds.pem,
        }),
    ))
}
