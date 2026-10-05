//! Organizations: creation service, `POST /admin/organizations` (GHES),
//! `GET /orgs/{org}`.

use axum::extract::State;
use axum::http::StatusCode;
use bgh_core::audit;
use bgh_core::error::unique_violation;
use bgh_core::events::Event;
use bgh_core::models::api::{OrganizationFull, OrganizationSimple};
use bgh_core::perms;
use bgh_core::prelude::*;
use bgh_core::sync;
use serde::Deserialize;
use serde_json::json;

use crate::{users, validate};

/// Create an organization with `admin` as its first admin member.
pub async fn create_org(
    state: &AppState,
    login: &str,
    name: Option<&str>,
    admin: &db::User,
    actor: &db::User,
) -> ApiResult<db::User> {
    if login.is_empty() {
        return Err(ApiError::invalid_field(FieldError::missing_field(
            "Organization",
            "login",
        )));
    }
    if !validate::is_valid_login(login) || validate::is_reserved_login(login) {
        return Err(ApiError::invalid_field(FieldError::invalid(
            "Organization",
            "login",
        )));
    }
    if admin.is_org() {
        return Err(ApiError::invalid_field(FieldError::invalid(
            "Organization",
            "admin",
        )));
    }
    let mut tx = Tx::begin(state).await?;
    let org = db::insert_org(&mut tx, login, name, admin.id)
        .await
        .map_err(|e| match unique_violation(&e).as_deref() {
            Some("users_login_key") => {
                ApiError::invalid_field(FieldError::already_exists("Organization", "login"))
            }
            _ => e.into(),
        })?;
    audit::log(
        &mut *tx,
        Some(actor),
        "org.create",
        audit::Target::Org(org.id),
        json!({ "login": org.login, "admin": admin.login }),
    )
    .await?;
    let scope = sync::org_scope(org.id);
    tx.sync(
        &scope,
        "organization",
        org.id,
        SyncAction::Insert,
        &json!({ "id": org.id, "login": org.login, "name": org.name }),
    )
    .await?;
    tx.sync(
        &scope,
        "org_member",
        admin.id,
        SyncAction::Insert,
        &json!({ "org_id": org.id, "user_id": admin.id, "role": "admin" }),
    )
    .await?;
    tx.emit(Event::OrganizationChanged {
        org_id: org.id,
        login: org.login.clone(),
        action: "created".into(),
        actor_id: actor.id,
        data: json!({}),
    });
    tx.emit(Event::OrgMemberAdded {
        org_id: org.id,
        user_id: admin.id,
        actor_id: actor.id,
    });
    tx.commit().await?;
    Ok(org)
}

#[derive(Debug, Deserialize)]
pub struct AdminCreateOrgBody {
    #[serde(default)]
    pub login: String,
    /// Login of the user who will administer the organization.
    #[serde(default)]
    pub admin: String,
    pub profile_name: Option<String>,
}

/// `POST /admin/organizations` (site admins) → 201 organization-simple.
pub async fn admin_create_org(
    State(state): State<AppState>,
    auth: RequireSiteAdmin,
    Json(body): Json<AdminCreateOrgBody>,
) -> ApiResult<(StatusCode, Json<OrganizationSimple>)> {
    if body.admin.is_empty() {
        return Err(ApiError::invalid_field(FieldError::missing_field(
            "Organization",
            "admin",
        )));
    }
    let admin = db::User::find_by_login(&state.db, &body.admin)
        .await?
        .filter(|u| !u.is_org())
        .ok_or_else(|| ApiError::invalid_field(FieldError::invalid("Organization", "admin")))?;
    let org = create_org(
        &state,
        body.login.trim(),
        body.profile_name.as_deref(),
        &admin,
        &auth.user,
    )
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(OrganizationSimple::new(&state.urls, &org, None)),
    ))
}

/// `GET /orgs/{org}` → organization-full (member-only fields for members).
pub async fn get_org(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path(org): Path<String>,
) -> ApiResult<Json<OrganizationFull>> {
    let org = db::User::find_by_login(&state.db, &org)
        .await?
        .filter(db::User::is_org)
        .ok_or(ApiError::NotFound)?;
    let settings = db::OrgSettings::find(&state.db, org.id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let member = match auth.user_id() {
        Some(uid) => perms::org_role(&state.db, org.id, uid).await?.is_some(),
        None => false,
    };
    let stats = users::user_stats(&state, org.id).await?;
    let private_repos: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM repositories WHERE owner_id = $1 AND visibility <> 'public'",
    )
    .bind(org.id)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(OrganizationFull::new(
        &state.urls,
        &org,
        &settings,
        stats,
        Some(private_repos),
        member,
    )))
}
