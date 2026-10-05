//! LDAP administration (logic in `bgh_accounts::ldap`).
//!
//! GHES REST:
//! * `PATCH /admin/ldap/users/{username}/mapping {ldap_dn}` → user + `ldap_dn`
//! * `POST /admin/ldap/users/{username}/sync` → 201 `{"status": "queued"}`
//! * `PATCH /admin/ldap/teams/{team_id}/mapping {ldap_dn}` → team + `ldap_dn`
//! * `POST /admin/ldap/teams/{team_id}/sync` → 201 `{"status": "queued"}`
//!
//! Admin UI: `POST /_bgh/admin/ldap/test` (connection, service bind and an
//! optional user lookup with the stored or submitted settings) and
//! `POST /_bgh/admin/ldap/sync` (full sync now, returns the report).

use axum::extract::State;
use axum::http::StatusCode;
use bgh_accounts::ldap::{self, Directory, sync};
use bgh_core::prelude::*;
use bgh_core::settings::{self, LdapSettings};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::common;
use crate::settings::REDACTED;

#[derive(Debug, Deserialize)]
pub struct MappingBody {
    pub ldap_dn: Option<String>,
}

fn require_ldap_dn(body: &MappingBody) -> ApiResult<Option<&str>> {
    // GHES requires the field; an empty string removes the mapping.
    match body.ldap_dn.as_deref() {
        None => Err(ApiError::invalid_field(FieldError::missing_field(
            "LdapMapping",
            "ldap_dn",
        ))),
        Some(dn) => Ok(Some(dn.trim()).filter(|d| !d.is_empty())),
    }
}

/// `PATCH /admin/ldap/users/{username}/mapping`
pub async fn update_user_mapping(
    State(state): State<AppState>,
    auth: RequireSiteAdmin,
    Path(username): Path<String>,
    Json(body): Json<MappingBody>,
) -> ApiResult<Json<Value>> {
    let dn = require_ldap_dn(&body)?;
    let user = common::user(&state, &username).await?;
    let dn = sync::set_user_mapping(&state, &auth.user, &user, dn).await?;
    let mut out = serde_json::to_value(api::SimpleUser::new(&state.urls, &user))?;
    out["name"] = json!(user.name);
    out["email"] = json!(user.email);
    out["ldap_dn"] = json!(dn);
    Ok(Json(out))
}

/// `POST /admin/ldap/users/{username}/sync` → 201.
pub async fn sync_user(
    State(state): State<AppState>,
    _auth: RequireSiteAdmin,
    Path(username): Path<String>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let user = common::user(&state, &username).await?;
    bgh_core::jobs::enqueue_job(&state.db, &sync::LdapSyncUser { user_id: user.id }).await?;
    Ok((StatusCode::CREATED, Json(json!({ "status": "queued" }))))
}

async fn team(state: &AppState, id: i64) -> ApiResult<db::Team> {
    sqlx::query_as(&format!(
        "SELECT {} FROM teams WHERE id = $1",
        db::Team::COLUMNS
    ))
    .bind(id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)
}

/// `PATCH /admin/ldap/teams/{team_id}/mapping`
pub async fn update_team_mapping(
    State(state): State<AppState>,
    auth: RequireSiteAdmin,
    Path(team_id): Path<i64>,
    Json(body): Json<MappingBody>,
) -> ApiResult<Json<Value>> {
    let dn = require_ldap_dn(&body)?;
    let team = team(&state, team_id).await?;
    let dn = sync::set_team_mapping(&state, &auth.user, &team, dn).await?;
    let org = db::User::find(&state.db, team.org_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let rendered = bgh_accounts::teams::render_teams(&state, &org, vec![team]).await?;
    let mut out = serde_json::to_value(&rendered[0])?;
    out["ldap_dn"] = json!(dn);
    Ok(Json(out))
}

/// `POST /admin/ldap/teams/{team_id}/sync` → 201.
pub async fn sync_team(
    State(state): State<AppState>,
    _auth: RequireSiteAdmin,
    Path(team_id): Path<i64>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let team = team(&state, team_id).await?;
    bgh_core::jobs::enqueue_job(&state.db, &sync::LdapSyncTeam { team_id: team.id }).await?;
    Ok((StatusCode::CREATED, Json(json!({ "status": "queued" }))))
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct TestBody {
    /// Settings to test instead of the stored ones (`bind_password`
    /// `"********"` keeps the stored secret).
    pub settings: Option<LdapSettings>,
    /// Optional login to look up.
    pub login: Option<String>,
}

/// `POST /_bgh/admin/ldap/test` → `{ok, message, user?}` (200 either way).
pub async fn test(
    State(state): State<AppState>,
    _auth: RequireSiteAdmin,
    Json(body): Json<TestBody>,
) -> ApiResult<Json<Value>> {
    let stored = settings::load_uncached(&state.config, &state.db)
        .await?
        .auth_providers
        .ldap;
    let mut cfg = body.settings.unwrap_or_else(|| stored.clone());
    if cfg.bind_password.as_deref() == Some(REDACTED) {
        cfg.bind_password = stored.bind_password;
    }
    let mut dir = match Directory::connect(&cfg).await {
        Ok(d) => d,
        Err(e) => return Ok(Json(json!({ "ok": false, "message": e.to_string() }))),
    };
    let out = match body
        .login
        .as_deref()
        .map(str::trim)
        .filter(|l| !l.is_empty())
    {
        None => json!({ "ok": true, "message": "Connected and bound successfully." }),
        Some(login) => match dir.find_user(login).await {
            Ok(Some(u)) => json!({
                "ok": true,
                "message": format!("Found {}.", u.dn),
                "user": {
                    "dn": u.dn, "uid": u.uid, "name": u.name, "emails": u.emails,
                    "ssh_keys": u.ssh_keys.len(), "gpg_keys": u.gpg_keys.len(),
                    "disabled": u.disabled,
                },
            }),
            Ok(None) => json!({ "ok": false, "message": format!("No user {login:?} found.") }),
            Err(e) => json!({ "ok": false, "message": e.to_string() }),
        },
    };
    dir.close().await;
    Ok(Json(out))
}

/// `POST /_bgh/admin/ldap/sync` → the sync report (422 when LDAP is not
/// enabled or unreachable).
pub async fn sync_now(
    State(state): State<AppState>,
    auth: RequireSiteAdmin,
) -> ApiResult<Json<sync::SyncReport>> {
    if ldap::config(&state).await?.is_none() {
        return Err(ApiError::unprocessable("LDAP is not enabled."));
    }
    let report = sync::sync_all(&state).await?;
    let mut tx = Tx::begin(&state).await?;
    bgh_core::audit::log(
        &mut *tx,
        Some(&auth.user),
        "business.ldap_sync",
        bgh_core::audit::Target::Site,
        serde_json::to_value(&report)?,
    )
    .await?;
    tx.commit().await?;
    Ok(Json(report))
}
