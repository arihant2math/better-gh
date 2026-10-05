//! Web-client JSON for GitHub App registrations (GitHub has no REST API for
//! this; apps are registered in the settings UI):
//!
//! * `GET /_bgh/apps[?owner=login]`, `POST /_bgh/apps`
//! * `GET|PATCH|DELETE /_bgh/apps/{slug}`
//! * `POST /_bgh/apps/{slug}/keys` (PEM shown once),
//!   `DELETE /_bgh/apps/{slug}/keys/{id}`
//! * `POST /_bgh/apps/{slug}/client_secrets` (secret shown once),
//!   `DELETE /_bgh/apps/{slug}/client_secrets/{id}` (P46: OAuth for
//!   user-to-server tokens)
//!
//! Only administrators of the owning account (the user, or org admins)
//! see or change a registration. Writes need a browser session: a token
//! could otherwise mint itself an app with more access than its scopes.

use std::collections::BTreeMap;

use axum::extract::State;
use axum::http::StatusCode;
use bgh_core::apps::{AppRow, GeneratedKey};
use bgh_core::audit;
use bgh_core::prelude::*;
use bgh_core::time::ts;
use chrono::{DateTime, Utc};
use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{EVENTS, administers, app_by_slug, bot_login, slugify, validate_permissions};
use crate::util::{self, Patch};

const RESOURCE: &str = "Integration";
/// GitHub's limit on app names.
const MAX_NAME_LEN: usize = 34;

/// An app's private key (public half).
#[derive(Debug, Serialize)]
pub struct KeyJson {
    pub id: i64,
    pub fingerprint: String,
    pub created_at: Timestamp,
    #[serde(skip)]
    pub created: DateTime<Utc>,
    /// The private key (PKCS#1 PEM), only in the create response.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pem: Option<String>,
}

/// Registration settings of an app (owner's view).
#[derive(Debug, Serialize)]
pub struct AppDetail {
    #[serde(flatten)]
    pub app: api::Integration,
    pub homepage_url: String,
    pub callback_urls: Vec<String>,
    pub setup_url: Option<String>,
    pub setup_on_update: bool,
    pub webhook_active: bool,
    pub webhook_url: Option<String>,
    pub webhook_secret_set: bool,
    pub public: bool,
    pub bot: api::SimpleUser,
    pub keys: Vec<KeyJson>,
    /// `json` or `form` (P46).
    pub webhook_content_type: String,
    pub webhook_insecure_ssl: bool,
    pub client_secrets: Vec<ClientSecretJson>,
}

/// An app's client secret (only the last eight characters are kept).
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct ClientSecretJson {
    pub id: i64,
    pub last_eight: String,
    #[sqlx(skip)]
    pub created_at: Option<Timestamp>,
    #[serde(skip)]
    pub created: DateTime<Utc>,
    #[serde(skip)]
    pub last_used: Option<DateTime<Utc>>,
    #[sqlx(skip)]
    pub last_used_at: Option<Timestamp>,
    /// The secret itself, only in the create response.
    #[sqlx(skip)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
}

async fn detail(state: &AppState, app: &AppRow) -> ApiResult<AppDetail> {
    let integration = super::integration(state, app, true).await?;
    let bot = db::User::find(&state.db, app.bot_user_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(AppDetail {
        app: integration,
        homepage_url: app.homepage_url.clone(),
        callback_urls: app.callback_urls.clone(),
        setup_url: app.setup_url.clone(),
        setup_on_update: app.setup_on_update,
        webhook_active: app.webhook_active,
        webhook_url: app.webhook_url.clone(),
        webhook_secret_set: app.webhook_secret.is_some(),
        public: app.public,
        bot: api::SimpleUser::new(&state.urls, &bot),
        keys: keys(state, app.id).await?,
        webhook_content_type: app.webhook_content_type.clone(),
        webhook_insecure_ssl: app.webhook_insecure_ssl,
        client_secrets: client_secrets(state, app.id).await?,
    })
}

async fn client_secrets(state: &AppState, app_id: i64) -> ApiResult<Vec<ClientSecretJson>> {
    let mut rows: Vec<ClientSecretJson> = sqlx::query_as(
        "SELECT id, last_eight, created_at AS created, last_used_at AS last_used
           FROM github_app_client_secrets WHERE app_id = $1 ORDER BY id",
    )
    .bind(app_id)
    .fetch_all(&state.db)
    .await?;
    for r in &mut rows {
        r.created_at = Some(r.created.into());
        r.last_used_at = ts(r.last_used);
    }
    Ok(rows)
}

/// Add a client secret to an app; returns its id and the secret.
pub(crate) async fn insert_client_secret(
    conn: &mut sqlx::PgConnection,
    app_id: i64,
    creator_id: Option<i64>,
) -> ApiResult<(i64, String, DateTime<Utc>)> {
    let secret = hex::encode(rand::rng().random::<[u8; 20]>());
    let (id, created): (i64, DateTime<Utc>) = sqlx::query_as(
        "INSERT INTO github_app_client_secrets (app_id, secret_hash, last_eight, creator_id)
         VALUES ($1, $2, $3, $4) RETURNING id, created_at",
    )
    .bind(app_id)
    .bind(bgh_core::crypto::sha256_hex(&secret))
    .bind(&secret[secret.len() - 8..])
    .bind(creator_id)
    .fetch_one(conn)
    .await?;
    Ok((id, secret, created))
}

/// Store the public half of a freshly generated key.
pub(crate) async fn insert_key(
    conn: &mut sqlx::PgConnection,
    app_id: i64,
    key: &GeneratedKey,
) -> ApiResult<(i64, DateTime<Utc>)> {
    Ok(sqlx::query_as(
        "INSERT INTO github_app_keys (app_id, public_key, fingerprint) VALUES ($1, $2, $3)
         RETURNING id, created_at",
    )
    .bind(app_id)
    .bind(&key.public_pem)
    .bind(&key.fingerprint)
    .fetch_one(conn)
    .await?)
}

async fn keys(state: &AppState, app_id: i64) -> ApiResult<Vec<KeyJson>> {
    let rows: Vec<(i64, String, DateTime<Utc>)> = sqlx::query_as(
        "SELECT id, fingerprint, created_at FROM github_app_keys WHERE app_id = $1 ORDER BY id",
    )
    .bind(app_id)
    .fetch_all(&state.db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, fingerprint, created)| KeyJson {
            id,
            fingerprint,
            created_at: created.into(),
            created,
            pem: None,
        })
        .collect())
}

/// The app `slug` if `auth` administers its owner, else 404.
pub async fn admin_app(state: &AppState, auth: &AuthContext, slug: &str) -> ApiResult<AppRow> {
    if bgh_core::apps::is_integration(auth) {
        return Err(ApiError::NotFound);
    }
    let app = app_by_slug(&state.db, slug).await?;
    let owner = db::User::find(&state.db, app.owner_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if !administers(state, &auth.user, &owner).await? {
        return Err(ApiError::NotFound);
    }
    Ok(app)
}

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    pub owner: Option<String>,
}

/// `GET /_bgh/apps[?owner=login]`: apps registered by the viewer (or by
/// an organization it administers).
pub async fn list(
    State(state): State<AppState>,
    auth: RequireUser,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<Vec<AppDetail>>> {
    let owner = match q.owner.as_deref() {
        Some(login) => util::find_account(&state, login).await?,
        None => auth.user.clone(),
    };
    if bgh_core::apps::is_integration(&auth) || !administers(&state, &auth.user, &owner).await? {
        return Err(ApiError::NotFound);
    }
    let rows: Vec<AppRow> = sqlx::query_as(&format!(
        "SELECT {} FROM github_apps WHERE owner_id = $1 ORDER BY id",
        AppRow::COLUMNS
    ))
    .bind(owner.id)
    .fetch_all(&state.db)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for app in &rows {
        out.push(detail(&state, app).await?);
    }
    Ok(Json(out))
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct AppBody {
    /// Owning account login (create only; default: the viewer).
    pub owner: Option<String>,
    pub name: Option<String>,
    pub description: Option<String>,
    pub homepage_url: Option<String>,
    pub callback_urls: Option<Vec<String>>,
    pub setup_url: Patch<String>,
    pub setup_on_update: Option<bool>,
    pub webhook_active: Option<bool>,
    pub webhook_url: Patch<String>,
    /// New webhook secret; `null` or `""` clears it.
    pub webhook_secret: Patch<String>,
    pub permissions: Option<BTreeMap<String, String>>,
    pub events: Option<Vec<String>>,
    pub public: Option<bool>,
}

fn valid_url(u: &str) -> bool {
    url::Url::parse(u).is_ok_and(|u| matches!(u.scheme(), "http" | "https"))
}

pub(crate) fn validate(body: &AppBody, creating: bool) -> ApiResult<()> {
    let mut errors = Vec::new();
    match body.name.as_deref().map(str::trim) {
        None if creating => errors.push(FieldError::missing_field(RESOURCE, "name")),
        Some(n) if n.is_empty() || slugify(n).is_empty() => {
            errors.push(FieldError::invalid(RESOURCE, "name"))
        }
        Some(n) if n.chars().count() > MAX_NAME_LEN => errors.push(FieldError::custom(
            RESOURCE,
            "name",
            format!("name is too long (maximum is {MAX_NAME_LEN} characters)"),
        )),
        _ => {}
    }
    match body.homepage_url.as_deref() {
        None if creating => errors.push(FieldError::missing_field(RESOURCE, "homepage_url")),
        Some(u) if !valid_url(u) => errors.push(FieldError::invalid(RESOURCE, "homepage_url")),
        _ => {}
    }
    if body
        .callback_urls
        .as_ref()
        .is_some_and(|c| c.len() > 10 || c.iter().any(|u| !valid_url(u)))
    {
        errors.push(FieldError::invalid(RESOURCE, "callback_urls"));
    }
    for (field, v) in [
        ("setup_url", &body.setup_url),
        ("webhook_url", &body.webhook_url),
    ] {
        if let Patch::Value(u) = v
            && !u.is_empty()
            && !valid_url(u)
        {
            errors.push(FieldError::invalid(RESOURCE, field));
        }
    }
    if let Some(events) = &body.events
        && let Some(bad) = events.iter().find(|e| !EVENTS.contains(&e.as_str()))
    {
        errors.push(FieldError::custom(
            RESOURCE,
            "events",
            format!("{bad} is not a valid event"),
        ));
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(ApiError::validation(errors))
    }
}

fn new_client_id() -> String {
    let mut b = [0u8; 10];
    rand::rng().fill(&mut b);
    format!("Iv23{}", hex::encode(b))
}

fn name_taken(e: sqlx::Error) -> ApiError {
    match bgh_core::db::unique_violation(&e).as_deref() {
        Some("github_apps_name_key" | "github_apps_slug_key" | "users_login_key") => {
            ApiError::invalid_field(FieldError::custom(
                RESOURCE,
                "name",
                "Name is already in use",
            ))
        }
        _ => e.into(),
    }
}

pub(crate) fn audit_target(owner: &db::User) -> audit::Target {
    if owner.is_org() {
        audit::Target::Org(owner.id)
    } else {
        audit::Target::User(owner.id)
    }
}

fn seal_secret(state: &AppState, secret: &Patch<String>) -> ApiResult<Option<Option<Vec<u8>>>> {
    Ok(match secret {
        Patch::Absent => None,
        Patch::Null => Some(None),
        Patch::Value(s) if s.is_empty() => Some(None),
        Patch::Value(s) => Some(Some(bgh_core::secretbox::seal(state, s)?)),
    })
}

/// `POST /_bgh/apps` → 201 app.
pub async fn create(
    State(state): State<AppState>,
    auth: RequireUser,
    Json(body): Json<AppBody>,
) -> ApiResult<(StatusCode, Json<AppDetail>)> {
    util::require_session(&auth)?;
    validate(&body, true)?;
    let owner = match body.owner.as_deref() {
        Some(login) => util::find_account(&state, login).await?,
        None => auth.user.clone(),
    };
    if owner.kind == "Bot" || !administers(&state, &auth.user, &owner).await? {
        return Err(ApiError::NotFound);
    }
    let mut tx = Tx::begin(&state).await?;
    let app = insert_app(&state, &mut tx, &auth.user, &owner, &body).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(detail(&state, &app).await?)))
}

/// Register an app for `owner` (validated `body`) inside `tx`, audited as
/// `actor`.
pub(crate) async fn insert_app(
    state: &AppState,
    tx: &mut Tx,
    actor: &db::User,
    owner: &db::User,
    body: &AppBody,
) -> ApiResult<AppRow> {
    let name = body.name.as_deref().unwrap_or_default().trim().to_string();
    let slug = slugify(&name);
    let permissions =
        validate_permissions(RESOURCE, &body.permissions.clone().unwrap_or_default())?;
    let secret = seal_secret(state, &body.webhook_secret)?.flatten();
    let webhook_url = body.webhook_url.clone().into_option().flatten();
    let bot_id: i64 = sqlx::query_scalar(
        "INSERT INTO users (login, type, name) VALUES ($1, 'Bot', $2) RETURNING id",
    )
    .bind(bot_login(&slug))
    .bind(&name)
    .fetch_one(&mut **tx)
    .await
    .map_err(name_taken)?;
    let app: AppRow = sqlx::query_as(&format!(
        "INSERT INTO github_apps (owner_id, bot_user_id, slug, name, description, homepage_url,
                callback_urls, setup_url, setup_on_update, webhook_active, webhook_url,
                webhook_secret, permissions, events, public, client_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)
         RETURNING {}",
        AppRow::COLUMNS
    ))
    .bind(owner.id)
    .bind(bot_id)
    .bind(&slug)
    .bind(&name)
    .bind(body.description.as_deref().unwrap_or_default().trim())
    .bind(body.homepage_url.as_deref().unwrap_or_default())
    .bind(body.callback_urls.clone().unwrap_or_default())
    .bind(
        body.setup_url
            .clone()
            .into_option()
            .flatten()
            .filter(|u| !u.is_empty()),
    )
    .bind(body.setup_on_update.unwrap_or(false))
    .bind(body.webhook_active.unwrap_or(webhook_url.is_some()))
    .bind(webhook_url.filter(|u| !u.is_empty()))
    .bind(secret)
    .bind(sqlx::types::Json(&permissions))
    .bind(body.events.clone().unwrap_or_default())
    .bind(body.public.unwrap_or(false))
    .bind(new_client_id())
    .fetch_one(&mut **tx)
    .await
    .map_err(name_taken)?;
    audit::log(
        &mut **tx,
        Some(actor),
        "integration.create",
        audit_target(owner),
        json!({ "integration": app.slug, "app_id": app.id }),
    )
    .await?;
    Ok(app)
}

/// `GET /_bgh/apps/{slug}`
pub async fn get(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(slug): Path<String>,
) -> ApiResult<Json<AppDetail>> {
    let app = admin_app(&state, &auth, &slug).await?;
    Ok(Json(detail(&state, &app).await?))
}

/// `PATCH /_bgh/apps/{slug}`. Renaming changes the slug and the bot login.
/// Permission changes apply to new installations; existing ones keep theirs
/// until accepted (see `install::accept_permissions`).
pub async fn update(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(slug): Path<String>,
    Json(body): Json<AppBody>,
) -> ApiResult<Json<AppDetail>> {
    util::require_session(&auth)?;
    let app = admin_app(&state, &auth, &slug).await?;
    validate(&body, false)?;
    let permissions = body
        .permissions
        .as_ref()
        .map(|p| validate_permissions(RESOURCE, p))
        .transpose()?;
    let name = body.name.as_deref().map(str::trim);
    let new_slug = name.map(slugify);
    let secret = seal_secret(&state, &body.webhook_secret)?;
    let mut tx = Tx::begin(&state).await?;
    if let Some(s) = &new_slug
        && *s != app.slug
    {
        sqlx::query("UPDATE users SET login = $2, name = $3, updated_at = now() WHERE id = $1")
            .bind(app.bot_user_id)
            .bind(bot_login(s))
            .bind(name)
            .execute(&mut *tx)
            .await
            .map_err(name_taken)?;
        tx.sync_user(app.bot_user_id).await?;
    }
    let setup_url = body.setup_url.clone().into_option();
    let webhook_url = body.webhook_url.clone().into_option();
    let row: AppRow = sqlx::query_as(&format!(
        "UPDATE github_apps SET
            name = coalesce($2, name), slug = coalesce($3, slug),
            description = coalesce($4, description), homepage_url = coalesce($5, homepage_url),
            callback_urls = coalesce($6, callback_urls),
            setup_url = CASE WHEN $7 THEN $8 ELSE setup_url END,
            setup_on_update = coalesce($9, setup_on_update),
            webhook_active = coalesce($10, webhook_active),
            webhook_url = CASE WHEN $11 THEN $12 ELSE webhook_url END,
            webhook_secret = CASE WHEN $13 THEN $14 ELSE webhook_secret END,
            permissions = coalesce($15, permissions), events = coalesce($16, events),
            public = coalesce($17, public), updated_at = now()
         WHERE id = $1 RETURNING {}",
        AppRow::COLUMNS
    ))
    .bind(app.id)
    .bind(name)
    .bind(&new_slug)
    .bind(body.description.as_deref().map(str::trim))
    .bind(&body.homepage_url)
    .bind(&body.callback_urls)
    .bind(setup_url.is_some())
    .bind(setup_url.flatten().filter(|u| !u.is_empty()))
    .bind(body.setup_on_update)
    .bind(body.webhook_active)
    .bind(webhook_url.is_some())
    .bind(webhook_url.flatten().filter(|u| !u.is_empty()))
    .bind(secret.is_some())
    .bind(secret.flatten())
    .bind(permissions.as_ref().map(sqlx::types::Json))
    .bind(&body.events)
    .bind(body.public)
    .fetch_one(&mut *tx)
    .await
    .map_err(name_taken)?;
    let owner = db::User::find(&mut *tx, row.owner_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    audit::log(
        &mut *tx,
        Some(&auth.user),
        "integration.update",
        audit_target(&owner),
        json!({ "integration": row.slug, "app_id": row.id }),
    )
    .await?;
    if row.permissions.0 != app.permissions.0 || row.events != app.events {
        notify_permission_upgrade(&state, &mut tx, &row).await?;
    }
    tx.commit().await?;
    Ok(Json(detail(&state, &row).await?))
}

/// Permission-upgrade flow: installations keep their accepted permissions;
/// administrators of every account whose installation now differs from the
/// app's request are mailed a link to review and accept them
/// (`POST /_bgh/installations/{id}/accept_permissions`).
async fn notify_permission_upgrade(state: &AppState, tx: &mut Tx, app: &AppRow) -> ApiResult<()> {
    #[derive(sqlx::FromRow)]
    struct Recipient {
        installation_id: i64,
        account_login: String,
        account_is_org: bool,
        login: String,
        email: String,
    }
    let rows: Vec<Recipient> = sqlx::query_as(
        "SELECT i.id AS installation_id, a.login AS account_login,
                a.type = 'Organization' AS account_is_org, u.login, e.email
           FROM app_installations i
           JOIN users a ON a.id = i.account_id
           JOIN users u ON u.id = a.id
                        OR u.id IN (SELECT user_id FROM org_members
                                     WHERE org_id = a.id AND role = 'admin')
           JOIN user_emails e ON e.user_id = u.id AND e.is_primary AND e.verified
          WHERE i.app_id = $1
            AND (i.permissions <> $2 OR NOT (i.events @> $3 AND i.events <@ $3))
          ORDER BY i.id, u.id",
    )
    .bind(app.id)
    .bind(&app.permissions)
    .bind(&app.events)
    .fetch_all(&mut **tx)
    .await?;
    for r in rows {
        let path = if r.account_is_org {
            format!(
                "/organizations/{}/settings/installations/{}",
                r.account_login, r.installation_id
            )
        } else {
            format!("/settings/installations/{}", r.installation_id)
        };
        let email = bgh_core::mail::templates::app_permissions_requested(
            &state.config.site_name,
            &r.email,
            &r.login,
            &app.name,
            &r.account_login,
            &state.urls.html(&path),
        );
        tx.enqueue(&bgh_core::mail::SendEmail::new(email)).await?;
    }
    Ok(())
}

/// `DELETE /_bgh/apps/{slug}` → 204. Removes every installation and token;
/// content authored by the bot is shown as the ghost user.
pub async fn delete(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(slug): Path<String>,
) -> ApiResult<StatusCode> {
    util::require_session(&auth)?;
    let app = admin_app(&state, &auth, &slug).await?;
    let owner = db::User::find(&state.db, app.owner_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let mut tx = Tx::begin(&state).await?;
    // Deleting the bot cascades to the app, its installations and tokens.
    sqlx::query("DELETE FROM users WHERE id = $1 AND type = 'Bot'")
        .bind(app.bot_user_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM github_apps WHERE id = $1")
        .bind(app.id)
        .execute(&mut *tx)
        .await?;
    audit::log(
        &mut *tx,
        Some(&auth.user),
        "integration.destroy",
        audit_target(&owner),
        json!({ "integration": app.slug, "app_id": app.id }),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /_bgh/apps/{slug}/keys` → 201 with the private key PEM (shown
/// once; only the public key is stored).
pub async fn create_key(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(slug): Path<String>,
) -> ApiResult<(StatusCode, Json<KeyJson>)> {
    util::require_session(&auth)?;
    let app = admin_app(&state, &auth, &slug).await?;
    let key: GeneratedKey = tokio::task::spawn_blocking(bgh_core::apps::generate_key).await??;
    let mut tx = Tx::begin(&state).await?;
    let (id, created) = insert_key(&mut tx, app.id, &key).await?;
    let owner = db::User::find(&mut *tx, app.owner_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    audit::log(
        &mut *tx,
        Some(&auth.user),
        "integration.generate_private_key",
        audit_target(&owner),
        json!({ "integration": app.slug, "fingerprint": key.fingerprint }),
    )
    .await?;
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        Json(KeyJson {
            id,
            fingerprint: key.fingerprint,
            created_at: created.into(),
            created,
            pem: Some(key.private_pem),
        }),
    ))
}

/// `DELETE /_bgh/apps/{slug}/keys/{id}` → 204.
pub async fn delete_key(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((slug, id)): Path<(String, i64)>,
) -> ApiResult<StatusCode> {
    util::require_session(&auth)?;
    let app = admin_app(&state, &auth, &slug).await?;
    let mut tx = Tx::begin(&state).await?;
    let fingerprint: Option<String> = sqlx::query_scalar(
        "DELETE FROM github_app_keys WHERE id = $1 AND app_id = $2 RETURNING fingerprint",
    )
    .bind(id)
    .bind(app.id)
    .fetch_optional(&mut *tx)
    .await?;
    let fingerprint = fingerprint.ok_or(ApiError::NotFound)?;
    let owner = db::User::find(&mut *tx, app.owner_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    audit::log(
        &mut *tx,
        Some(&auth.user),
        "integration.remove_private_key",
        audit_target(&owner),
        json!({ "integration": app.slug, "fingerprint": fingerprint }),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /_bgh/apps/{slug}/client_secrets` → 201 with the secret (shown
/// once).
pub async fn create_client_secret(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(slug): Path<String>,
) -> ApiResult<(StatusCode, Json<ClientSecretJson>)> {
    util::require_session(&auth)?;
    let app = admin_app(&state, &auth, &slug).await?;
    let mut tx = Tx::begin(&state).await?;
    let (id, secret, created) = insert_client_secret(&mut tx, app.id, Some(auth.user.id)).await?;
    let owner = db::User::find(&mut *tx, app.owner_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    audit::log(
        &mut *tx,
        Some(&auth.user),
        "integration.generate_client_secret",
        audit_target(&owner),
        json!({ "integration": app.slug, "client_secret_id": id }),
    )
    .await?;
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        Json(ClientSecretJson {
            id,
            last_eight: secret[secret.len() - 8..].to_string(),
            created_at: Some(created.into()),
            created,
            last_used: None,
            last_used_at: None,
            client_secret: Some(secret),
        }),
    ))
}

/// `DELETE /_bgh/apps/{slug}/client_secrets/{id}` → 204.
pub async fn delete_client_secret(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((slug, id)): Path<(String, i64)>,
) -> ApiResult<StatusCode> {
    util::require_session(&auth)?;
    let app = admin_app(&state, &auth, &slug).await?;
    let mut tx = Tx::begin(&state).await?;
    let found: Option<i64> = sqlx::query_scalar(
        "DELETE FROM github_app_client_secrets WHERE id = $1 AND app_id = $2 RETURNING id",
    )
    .bind(id)
    .bind(app.id)
    .fetch_optional(&mut *tx)
    .await?;
    found.ok_or(ApiError::NotFound)?;
    let owner = db::User::find(&mut *tx, app.owner_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    audit::log(
        &mut *tx,
        Some(&auth.user),
        "integration.remove_client_secret",
        audit_target(&owner),
        json!({ "integration": app.slug, "client_secret_id": id }),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
