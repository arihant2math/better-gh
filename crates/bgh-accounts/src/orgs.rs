//! Organizations: creation, profile/settings, members, public members,
//! memberships, invitations, outside collaborators, blocks.
//!
//! Permissions: organization admins (owners) and site admins manage the
//! organization (`admin:org` scope for tokens); member lists are visible to
//! members (non-members see public members only).

use std::collections::HashMap;

use axum::extract::State;
use axum::http::StatusCode;
use bgh_core::audit;
use bgh_core::error::unique_violation;
use bgh_core::events::Event;
use bgh_core::mail;
use bgh_core::models::api::{OrganizationFull, OrganizationSimple, SimpleUser};
use bgh_core::perms;
use bgh_core::prelude::*;
use bgh_core::sync;
use bgh_core::views;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::json::{InvitationRow, OrgInvitation, OrgMembership};
use crate::util::{self, Patch};
use crate::{social, teams, users, validate};

// ---------------------------------------------------------------------------
// Access
// ---------------------------------------------------------------------------

/// An organization resolved for the current caller.
#[derive(Debug, Clone)]
pub struct OrgAccess {
    pub org: db::User,
    pub settings: db::OrgSettings,
    /// The caller's role: `admin` | `member`.
    pub role: Option<String>,
    pub auth: Option<AuthContext>,
}

impl OrgAccess {
    pub async fn load(
        state: &AppState,
        auth: Option<&AuthContext>,
        login: &str,
    ) -> ApiResult<Self> {
        let org = util::find_org(state, login).await?;
        Self::for_org(state, auth, org).await
    }

    pub async fn for_org(
        state: &AppState,
        auth: Option<&AuthContext>,
        org: db::User,
    ) -> ApiResult<Self> {
        let settings = db::OrgSettings::find(&state.db, org.id)
            .await?
            .ok_or(ApiError::NotFound)?;
        let role = match auth {
            Some(a) => perms::org_role(&state.db, org.id, a.user.id).await?,
            None => None,
        };
        Ok(Self {
            org,
            settings,
            role,
            auth: auth.cloned(),
        })
    }

    pub fn site_admin(&self) -> bool {
        self.auth.as_ref().is_some_and(|a| a.user.site_admin)
    }

    pub fn is_member(&self) -> bool {
        self.role.is_some()
    }

    /// Organization owner (or site admin).
    pub fn is_admin(&self) -> bool {
        self.role.as_deref() == Some("admin") || self.site_admin()
    }

    /// Member or site admin: may see members-only data.
    pub fn can_view_private(&self) -> bool {
        self.is_member() || self.site_admin()
    }

    pub fn user(&self) -> ApiResult<&AuthContext> {
        self.auth.as_ref().ok_or_else(ApiError::requires_auth)
    }

    /// Owners only (`admin:org` scope for tokens).
    pub fn require_admin(&self) -> ApiResult<&AuthContext> {
        let auth = self.user()?;
        auth.require_scope("admin:org")?;
        if !self.is_admin() {
            return Err(ApiError::forbidden(
                "You must be an admin of the organization to do that.",
            ));
        }
        Ok(auth)
    }

    /// Members only (`read:org` scope for tokens).
    pub fn require_member(&self) -> ApiResult<&AuthContext> {
        let auth = self.user()?;
        auth.require_scope("read:org")?;
        if !self.can_view_private() {
            return Err(ApiError::forbidden(
                "You must be a member of the organization to do that.",
            ));
        }
        Ok(auth)
    }

    pub fn scope(&self) -> String {
        sync::org_scope(self.org.id)
    }
}

/// Render organization-full (member view for members and site admins).
pub async fn org_full(
    state: &AppState,
    org: &db::User,
    settings: &db::OrgSettings,
    member_view: bool,
) -> ApiResult<OrganizationFull> {
    let stats = users::user_stats(state, org.id).await?;
    let private_repos: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM repositories WHERE owner_id = $1 AND visibility <> 'public'",
    )
    .bind(org.id)
    .fetch_one(&state.db)
    .await?;
    Ok(OrganizationFull::new(
        &state.urls,
        org,
        settings,
        stats,
        Some(private_repos),
        member_view,
    ))
}

// ---------------------------------------------------------------------------
// Creation
// ---------------------------------------------------------------------------

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
    tx.sync_model(SyncModel::Org, org.id, SyncAction::Insert)
        .await?;
    let membership_id: i64 =
        sqlx::query_scalar("SELECT id FROM org_members WHERE org_id = $1 AND user_id = $2")
            .bind(org.id)
            .bind(admin.id)
            .fetch_one(&mut *tx)
            .await?;
    tx.sync_model(SyncModel::Membership, membership_id, SyncAction::Insert)
        .await?;
    // The member's `user` row, now also in the org scope.
    tx.sync_user(admin.id).await?;
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

#[derive(Debug, Deserialize)]
pub struct CreateOrgBody {
    #[serde(default)]
    pub login: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub billing_email: Option<String>,
}

/// `POST /_bgh/orgs` (web client): any user may create an organization and
/// becomes its owner → 201 organization-full.
pub async fn web_create_org(
    State(state): State<AppState>,
    auth: RequireUser,
    Json(body): Json<CreateOrgBody>,
) -> ApiResult<(StatusCode, Json<OrganizationFull>)> {
    util::require_session(&auth)?;
    let name = util::non_empty(body.name);
    let org = create_org(
        &state,
        body.login.trim(),
        name.as_deref(),
        &auth.user,
        &auth.user,
    )
    .await?;
    let description = util::non_empty(body.description);
    let billing = util::non_empty(body.billing_email);
    if description.is_some() || billing.is_some() {
        sqlx::query(
            "UPDATE org_settings SET description = $2, billing_email = $3 WHERE org_id = $1",
        )
        .bind(org.id)
        .bind(&description)
        .bind(&billing)
        .execute(&state.db)
        .await?;
    }
    let settings = db::OrgSettings::find(&state.db, org.id)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok((
        StatusCode::CREATED,
        Json(org_full(&state, &org, &settings, true).await?),
    ))
}

// ---------------------------------------------------------------------------
// Profile & settings
// ---------------------------------------------------------------------------

/// `GET /orgs/{org}` → organization-full (member-only fields for members).
pub async fn get_org(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path(org): Path<String>,
) -> ApiResult<Json<OrganizationFull>> {
    let access = OrgAccess::load(&state, auth.as_ref(), &org).await?;
    Ok(Json(
        org_full(
            &state,
            &access.org,
            &access.settings,
            access.can_view_private(),
        )
        .await?,
    ))
}

#[derive(Debug, Deserialize)]
pub struct UpdateOrgBody {
    /// New login (rename by an owner, `crate::lifecycle::rename`).
    #[serde(default)]
    pub login: Option<String>,
    #[serde(default)]
    pub name: Patch<String>,
    #[serde(default)]
    pub billing_email: Patch<String>,
    #[serde(default)]
    pub company: Patch<String>,
    #[serde(default)]
    pub email: Patch<String>,
    #[serde(default)]
    pub twitter_username: Patch<String>,
    #[serde(default)]
    pub location: Patch<String>,
    #[serde(default)]
    pub blog: Patch<String>,
    #[serde(default)]
    pub description: Patch<String>,
    pub has_organization_projects: Option<bool>,
    pub has_repository_projects: Option<bool>,
    pub default_repository_permission: Option<String>,
    pub members_can_create_repositories: Option<bool>,
    pub members_can_create_public_repositories: Option<bool>,
    pub members_can_create_private_repositories: Option<bool>,
    pub members_can_create_internal_repositories: Option<bool>,
    pub members_allowed_repository_creation_type: Option<String>,
    pub members_can_fork_private_repositories: Option<bool>,
    pub members_can_create_teams: Option<bool>,
    pub web_commit_signoff_required: Option<bool>,
}

/// `PATCH /orgs/{org}` (owners, `admin:org`) → organization-full.
pub async fn update_org(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path(org): Path<String>,
    Json(body): Json<UpdateOrgBody>,
) -> ApiResult<Json<OrganizationFull>> {
    let access = OrgAccess::load(&state, auth.as_ref(), &org).await?;
    let actor = access.require_admin()?.user.clone();
    if let Some(login) = body.login.as_deref() {
        crate::lifecycle::rename(&state, &actor, &access.org, login).await?;
    }
    let mut errors = Vec::new();
    if let Some(p) = &body.default_repository_permission
        && !matches!(p.as_str(), "none" | "read" | "write" | "admin")
    {
        errors.push(FieldError::invalid(
            "Organization",
            "default_repository_permission",
        ));
    }
    let mut create_flags = (
        body.members_can_create_repositories,
        body.members_can_create_public_repositories,
        body.members_can_create_private_repositories,
        body.members_can_create_internal_repositories,
    );
    let internal = body.members_can_create_internal_repositories;
    match body.members_allowed_repository_creation_type.as_deref() {
        None => {}
        Some("all") => create_flags = (Some(true), Some(true), Some(true), internal.or(Some(true))),
        Some("private") => {
            create_flags = (Some(true), Some(false), Some(true), internal.or(Some(true)))
        }
        Some("none") => create_flags = (Some(false), Some(false), Some(false), Some(false)),
        Some(_) => errors.push(FieldError::invalid(
            "Organization",
            "members_allowed_repository_creation_type",
        )),
    }
    let email = body.email.into_option().map(util::non_empty);
    let billing = body.billing_email.into_option().map(util::non_empty);
    for (field, v) in [("email", &email), ("billing_email", &billing)] {
        if let Some(Some(e)) = v
            && !validate::is_valid_email(e)
        {
            errors.push(FieldError::invalid("Organization", field));
        }
    }
    if !errors.is_empty() {
        return Err(ApiError::validation(errors));
    }
    let text = |p: Patch<String>| p.into_option().map(util::non_empty);
    let name = text(body.name);
    let company = text(body.company);
    let twitter = text(body.twitter_username);
    let location = text(body.location);
    let blog = text(body.blog);
    let description = text(body.description);

    let mut tx = Tx::begin(&state).await?;
    let org: db::User = sqlx::query_as(&format!(
        "UPDATE users SET
            name = CASE WHEN $2 THEN $3 ELSE name END,
            company = CASE WHEN $4 THEN $5 ELSE company END,
            email = CASE WHEN $6 THEN $7 ELSE email END,
            twitter_username = CASE WHEN $8 THEN $9 ELSE twitter_username END,
            location = CASE WHEN $10 THEN $11 ELSE location END,
            blog = CASE WHEN $12 THEN $13 ELSE blog END,
            updated_at = now()
          WHERE id = $1 RETURNING {}",
        db::User::COLUMNS
    ))
    .bind(access.org.id)
    .bind(name.is_some())
    .bind(name.flatten())
    .bind(company.is_some())
    .bind(company.flatten())
    .bind(email.is_some())
    .bind(email.flatten())
    .bind(twitter.is_some())
    .bind(twitter.flatten())
    .bind(location.is_some())
    .bind(location.flatten())
    .bind(blog.is_some())
    .bind(blog.flatten())
    .fetch_one(&mut *tx)
    .await?;
    let settings: db::OrgSettings = sqlx::query_as(&format!(
        "UPDATE org_settings SET
            description = CASE WHEN $2 THEN $3 ELSE description END,
            billing_email = CASE WHEN $4 THEN $5 ELSE billing_email END,
            has_organization_projects = coalesce($6, has_organization_projects),
            has_repository_projects = coalesce($7, has_repository_projects),
            default_repository_permission = coalesce($8, default_repository_permission),
            members_can_create_repositories = coalesce($9, members_can_create_repositories),
            members_can_create_public_repositories = coalesce($10, members_can_create_public_repositories),
            members_can_create_private_repositories = coalesce($11, members_can_create_private_repositories),
            members_can_fork_private_repositories = coalesce($12, members_can_fork_private_repositories),
            members_can_create_internal_repositories = coalesce($15, members_can_create_internal_repositories),
            members_can_create_teams = coalesce($13, members_can_create_teams),
            web_commit_signoff_required = coalesce($14, web_commit_signoff_required)
          WHERE org_id = $1 RETURNING {}",
        db::OrgSettings::COLUMNS
    ))
    .bind(access.org.id)
    .bind(description.is_some())
    .bind(description.flatten())
    .bind(billing.is_some())
    .bind(billing.flatten())
    .bind(body.has_organization_projects)
    .bind(body.has_repository_projects)
    .bind(&body.default_repository_permission)
    .bind(create_flags.0)
    .bind(create_flags.1)
    .bind(create_flags.2)
    .bind(body.members_can_fork_private_repositories)
    .bind(body.members_can_create_teams)
    .bind(body.web_commit_signoff_required)
    .bind(create_flags.3)
    .fetch_one(&mut *tx)
    .await?;
    tx.sync_model(SyncModel::Org, org.id, SyncAction::Update)
        .await?;
    audit::log(
        &mut *tx,
        Some(&actor),
        "org.update",
        audit::Target::Org(org.id),
        json!({ "default_repository_permission": body.default_repository_permission }),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(org_full(&state, &org, &settings, true).await?))
}

#[derive(Debug, Deserialize)]
pub struct SinceQuery {
    pub since: Option<i64>,
    pub per_page: Option<u32>,
}

/// `GET /organizations?since=` → organization-simple list by id.
pub async fn list_all(
    State(state): State<AppState>,
    auth: MaybeUser,
    Query(q): Query<SinceQuery>,
) -> ApiResult<axum::response::Response> {
    bgh_core::privacy::require_directory_access(&state, auth.as_ref()).await?;
    use axum::response::IntoResponse;
    let per_page = q.per_page.unwrap_or(30).clamp(1, 100);
    let rows: Vec<(i64,)> = sqlx::query_as(
        "SELECT id FROM users WHERE type = 'Organization' AND id > $1 ORDER BY id LIMIT $2",
    )
    .bind(q.since.unwrap_or(0))
    .bind(i64::from(per_page) + 1)
    .fetch_all(&state.db)
    .await?;
    let has_next = rows.len() > per_page as usize;
    let ids: Vec<i64> = rows.iter().take(per_page as usize).map(|r| r.0).collect();
    let items = simple_orgs(&state, &ids).await?;
    let base = state.urls.api("/organizations");
    let mut links = Vec::new();
    if has_next && let Some(last) = ids.last() {
        links.push(format!(
            "<{base}?per_page={per_page}&since={last}>; rel=\"next\""
        ));
    }
    links.push(format!("<{base}{{?since}}>; rel=\"first\""));
    let mut resp = Json(items).into_response();
    if let Ok(v) = axum::http::HeaderValue::from_str(&links.join(", ")) {
        resp.headers_mut().insert(axum::http::header::LINK, v);
    }
    Ok(resp)
}

/// organization-simple for `ids`, in order (one query for descriptions).
pub async fn simple_orgs(state: &AppState, ids: &[i64]) -> ApiResult<Vec<OrganizationSimple>> {
    if ids.is_empty() {
        return Ok(vec![]);
    }
    let orgs = views::users_by_id(state, ids.iter().map(|i| Some(*i))).await?;
    let descriptions: HashMap<i64, Option<String>> = sqlx::query_as::<_, (i64, Option<String>)>(
        "SELECT org_id, description FROM org_settings WHERE org_id = ANY($1)",
    )
    .bind(ids)
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .collect();
    Ok(ids
        .iter()
        .filter_map(|id| orgs.get(id))
        .map(|o| {
            OrganizationSimple::new(
                &state.urls,
                o,
                descriptions.get(&o.id).cloned().flatten().as_deref(),
            )
        })
        .collect())
}

/// `GET /user/orgs` (scope `read:org` or `user`) → the caller's orgs.
pub async fn my_orgs(
    State(state): State<AppState>,
    auth: RequireUser,
    p: Pagination,
) -> ApiResult<Page<OrganizationSimple>> {
    if !auth.has_scope("read:org") && !auth.has_scope("user") {
        auth.require_scope("read:org")?;
    }
    let ids: Vec<i64> = sqlx::query_scalar(
        "SELECT m.org_id FROM org_members m JOIN users o ON o.id = m.org_id
          WHERE m.user_id = $1 ORDER BY lower(o.login), m.org_id LIMIT $2 OFFSET $3",
    )
    .bind(auth.user.id)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let page = p.page(ids);
    let items = simple_orgs(&state, &page.items).await?;
    Ok(Page {
        items,
        link: page.link,
    })
}

/// `GET /users/{username}/orgs` → public memberships (all memberships when
/// the caller is that user).
pub async fn user_orgs(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path(username): Path<String>,
    p: Pagination,
) -> ApiResult<Page<OrganizationSimple>> {
    let user = util::find_account(&state, &username).await?;
    let all = auth.user_id() == Some(user.id) || auth.as_ref().is_some_and(|a| a.user.site_admin);
    let ids: Vec<i64> = sqlx::query_scalar(
        "SELECT m.org_id FROM org_members m JOIN users o ON o.id = m.org_id
          WHERE m.user_id = $1 AND (m.is_public OR $2)
          ORDER BY lower(o.login), m.org_id LIMIT $3 OFFSET $4",
    )
    .bind(user.id)
    .bind(all)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let page = p.page(ids);
    let items = simple_orgs(&state, &page.items).await?;
    Ok(Page {
        items,
        link: page.link,
    })
}

// ---------------------------------------------------------------------------
// Members
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct MembersQuery {
    pub filter: Option<String>,
    pub role: Option<String>,
}

/// `GET /orgs/{org}/members?filter=all|2fa_disabled&role=all|admin|member`.
/// Non-members get public members only.
pub async fn list_members(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path(org): Path<String>,
    Query(q): Query<MembersQuery>,
    p: Pagination,
) -> ApiResult<Page<SimpleUser>> {
    let access = OrgAccess::load(&state, auth.as_ref(), &org).await?;
    let private = access.can_view_private();
    let role = match q.role.as_deref() {
        None | Some("all") => None,
        Some(r @ ("admin" | "member")) => Some(r.to_string()),
        Some(_) => {
            return Err(ApiError::invalid_field(FieldError::invalid(
                "Member", "role",
            )));
        }
    };
    let two_fa_disabled = match q.filter.as_deref() {
        None | Some("all") => false,
        Some("2fa_disabled") => {
            if !access.is_admin() {
                return Err(ApiError::forbidden("Only owners can use this filter."));
            }
            true
        }
        Some(_) => {
            return Err(ApiError::invalid_field(FieldError::invalid(
                "Member", "filter",
            )));
        }
    };
    let rows: Vec<db::User> = sqlx::query_as(&format!(
        "SELECT {} FROM org_members m JOIN users u ON u.id = m.user_id
          WHERE m.org_id = $1 AND (m.is_public OR $2)
            AND ($3::text IS NULL OR m.role = $3)
            AND (NOT $4 OR NOT EXISTS (SELECT 1 FROM user_two_factor t
                                        WHERE t.user_id = u.id AND t.enabled_at IS NOT NULL))
          ORDER BY u.id LIMIT $5 OFFSET $6",
        db::prefixed("u", db::User::COLUMNS)
    ))
    .bind(access.org.id)
    .bind(private)
    .bind(&role)
    .bind(two_fa_disabled)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    Ok(p.page(rows).map(|u| SimpleUser::new(&state.urls, &u)))
}

/// `GET /orgs/{org}/public_members`
pub async fn list_public_members(
    State(state): State<AppState>,
    _auth: MaybeUser,
    Path(org): Path<String>,
    p: Pagination,
) -> ApiResult<Page<SimpleUser>> {
    let org = util::find_org(&state, &org).await?;
    let rows: Vec<db::User> = sqlx::query_as(&format!(
        "SELECT {} FROM org_members m JOIN users u ON u.id = m.user_id
          WHERE m.org_id = $1 AND m.is_public ORDER BY u.id LIMIT $2 OFFSET $3",
        db::prefixed("u", db::User::COLUMNS)
    ))
    .bind(org.id)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    Ok(p.page(rows).map(|u| SimpleUser::new(&state.urls, &u)))
}

async fn member_row(
    db: impl sqlx::PgExecutor<'_>,
    org_id: i64,
    user_id: i64,
) -> Result<Option<(i64, String, bool)>, sqlx::Error> {
    sqlx::query_as("SELECT id, role, is_public FROM org_members WHERE org_id = $1 AND user_id = $2")
        .bind(org_id)
        .bind(user_id)
        .fetch_optional(db)
        .await
}

/// `GET /orgs/{org}/members/{username}` → 204 if a member (public members
/// only for non-members), else 404.
pub async fn check_member(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((org, username)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    let access = OrgAccess::load(&state, auth.as_ref(), &org).await?;
    let user = util::find_user(&state, &username).await?;
    match member_row(&state.db, access.org.id, user.id).await? {
        Some((_, _, public)) if public || access.can_view_private() => Ok(StatusCode::NO_CONTENT),
        _ => Err(ApiError::NotFound),
    }
}

/// `GET /orgs/{org}/public_members/{username}` → 204 / 404.
pub async fn check_public_member(
    State(state): State<AppState>,
    _auth: MaybeUser,
    Path((org, username)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    let org = util::find_org(&state, &org).await?;
    let user = util::find_user(&state, &username).await?;
    match member_row(&state.db, org.id, user.id).await? {
        Some((_, _, true)) => Ok(StatusCode::NO_CONTENT),
        _ => Err(ApiError::NotFound),
    }
}

async fn set_publicity(
    state: &AppState,
    auth: &AuthContext,
    org: &str,
    username: &str,
    public: bool,
) -> ApiResult<StatusCode> {
    let org = util::find_org(state, org).await?;
    let user = util::find_user(state, username).await?;
    if user.id != auth.user.id {
        return Err(ApiError::forbidden(
            "You can only publicize or conceal your own membership.",
        ));
    }
    let updated =
        sqlx::query("UPDATE org_members SET is_public = $3 WHERE org_id = $1 AND user_id = $2")
            .bind(org.id)
            .bind(user.id)
            .bind(public)
            .execute(&state.db)
            .await?
            .rows_affected();
    if updated == 0 {
        return Err(ApiError::forbidden(
            "You must be a member of the organization.",
        ));
    }
    Ok(StatusCode::NO_CONTENT)
}

/// `PUT /orgs/{org}/public_members/{username}` (self only) → 204.
pub async fn publicize(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((org, username)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    auth.require_scope("write:org")?;
    set_publicity(&state, &auth, &org, &username, true).await
}

/// `DELETE /orgs/{org}/public_members/{username}` (self only) → 204.
pub async fn conceal(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((org, username)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    auth.require_scope("write:org")?;
    set_publicity(&state, &auth, &org, &username, false).await
}

async fn admin_count(tx: &mut Tx, org_id: i64) -> ApiResult<i64> {
    Ok(
        sqlx::query_scalar("SELECT count(*) FROM org_members WHERE org_id = $1 AND role = 'admin'")
            .bind(org_id)
            .fetch_one(&mut **tx)
            .await?,
    )
}

/// Remove `user` from `org` (and its teams). Refuses to remove the last
/// owner. Returns false when the user wasn't a member.
pub async fn remove_member(
    state: &AppState,
    actor: &db::User,
    org: &db::User,
    user: &db::User,
) -> ApiResult<bool> {
    let mut tx = Tx::begin(state).await?;
    let Some((membership_id, role, _)) = sqlx::query_as::<_, (i64, String, bool)>(
        "SELECT id, role, is_public FROM org_members WHERE org_id = $1 AND user_id = $2 FOR UPDATE",
    )
    .bind(org.id)
    .bind(user.id)
    .fetch_optional(&mut *tx)
    .await?
    else {
        return Ok(false);
    };
    if role == "admin" && admin_count(&mut tx, org.id).await? <= 1 {
        return Err(ApiError::forbidden(
            "You cannot remove the last owner of an organization.",
        ));
    }
    let team_ids: Vec<i64> = sqlx::query_scalar(
        "DELETE FROM team_members tm USING teams t
          WHERE tm.team_id = t.id AND t.org_id = $1 AND tm.user_id = $2 RETURNING tm.team_id",
    )
    .bind(org.id)
    .bind(user.id)
    .fetch_all(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM org_members WHERE id = $1")
        .bind(membership_id)
        .execute(&mut *tx)
        .await?;
    let scope = sync::org_scope(org.id);
    tx.sync_delete(&scope, SyncModel::Membership, membership_id)
        .await?;
    for team_id in &team_ids {
        teams::sync_team(&mut tx, *team_id, SyncAction::Update).await?;
        tx.emit(Event::TeamMemberRemoved {
            org_id: org.id,
            team_id: *team_id,
            user_id: user.id,
            actor_id: actor.id,
        });
    }
    audit::log(
        &mut *tx,
        Some(actor),
        "org.remove_member",
        audit::Target::Org(org.id),
        json!({ "user": user.login }),
    )
    .await?;
    tx.emit(Event::OrgMemberRemoved {
        org_id: org.id,
        user_id: user.id,
        actor_id: actor.id,
    });
    tx.commit().await?;
    Ok(true)
}

/// `DELETE /orgs/{org}/members/{username}` (owners, or the member leaving)
/// → 204.
pub async fn delete_member(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((org, username)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    let access = OrgAccess::load(&state, Some(&auth), &org).await?;
    let user = util::find_user(&state, &username).await?;
    if user.id != auth.user.id {
        access.require_admin()?;
    }
    if !remove_member(&state, &auth.user, &access.org, &user).await? {
        return Err(ApiError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Memberships
// ---------------------------------------------------------------------------

fn can_create_repository(settings: &db::OrgSettings, role: &str) -> bool {
    role == "admin" || settings.members_can_create_repositories
}

fn membership_json(
    state: &AppState,
    access: &OrgAccess,
    user: &db::User,
    state_: &str,
    role: &str,
) -> OrgMembership {
    OrgMembership::new(
        &state.urls,
        &access.org,
        access.settings.description.as_deref(),
        user,
        state_,
        role,
        can_create_repository(&access.settings, role),
    )
}

/// Pending invitation of `user` to `org_id` (by user id or a verified
/// email of the user).
async fn pending_invitation(
    db: impl sqlx::PgExecutor<'_>,
    org_id: i64,
    user_id: i64,
) -> Result<Option<InvitationRow>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT {} FROM org_invitations i
          WHERE i.org_id = $1 AND i.failed_at IS NULL AND {GRANTING_ROLE}
            AND (i.invitee_id = $2 OR lower(i.email) IN
                 (SELECT lower(email) FROM user_emails WHERE user_id = $2 AND verified))
          ORDER BY i.id LIMIT 1",
        db::prefixed("i", InvitationRow::COLUMNS)
    ))
    .bind(org_id)
    .bind(user_id)
    .fetch_optional(db)
    .await
}

/// Membership role granted by an invitation role. Only `admin` and
/// `direct_member` invitations grant a membership (`billing_manager` is
/// not supported and rejected when inviting; legacy rows never match
/// [`pending_invitation`]).
fn invitation_member_role(role: &str) -> &'static str {
    match role {
        "admin" => "admin",
        _ => "member",
    }
}

/// SQL predicate (on alias `i`) for invitations that grant a membership.
const GRANTING_ROLE: &str = "i.role IN ('admin', 'direct_member')";

/// `GET /orgs/{org}/memberships/{username}` → org-membership (active or
/// pending). Visible to members (pending ones to owners and the invitee).
pub async fn get_membership(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((org, username)): Path<(String, String)>,
) -> ApiResult<Json<OrgMembership>> {
    let access = OrgAccess::load(&state, Some(&auth), &org).await?;
    let user = util::find_user(&state, &username).await?;
    let is_self = user.id == auth.user.id;
    if !is_self {
        access.require_member()?;
    }
    if let Some((_, role, _)) = member_row(&state.db, access.org.id, user.id).await? {
        return Ok(Json(membership_json(
            &state, &access, &user, "active", &role,
        )));
    }
    if (is_self || access.is_admin())
        && let Some(inv) = pending_invitation(&state.db, access.org.id, user.id).await?
    {
        return Ok(Json(membership_json(
            &state,
            &access,
            &user,
            "pending",
            invitation_member_role(&inv.role),
        )));
    }
    Err(ApiError::NotFound)
}

#[derive(Debug, Deserialize)]
pub struct SetMembershipBody {
    pub role: Option<String>,
}

/// `PUT /orgs/{org}/memberships/{username} {role}` (owners): changes the
/// role of a member, or invites a non-member (pending membership).
pub async fn set_membership(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((org, username)): Path<(String, String)>,
    Json(body): Json<SetMembershipBody>,
) -> ApiResult<Json<OrgMembership>> {
    let access = OrgAccess::load(&state, Some(&auth), &org).await?;
    access.require_admin()?;
    let role = match body.role.as_deref().unwrap_or("member") {
        r @ ("admin" | "member") => r.to_string(),
        _ => {
            return Err(ApiError::invalid_field(FieldError::invalid(
                "Membership",
                "role",
            )));
        }
    };
    let user = util::find_user(&state, &username).await?;
    let mut tx = Tx::begin(&state).await?;
    let existing = sqlx::query_as::<_, (i64, String)>(
        "SELECT id, role FROM org_members WHERE org_id = $1 AND user_id = $2 FOR UPDATE",
    )
    .bind(access.org.id)
    .bind(user.id)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some((id, old_role)) = existing {
        if old_role != role {
            if old_role == "admin" && admin_count(&mut tx, access.org.id).await? <= 1 {
                return Err(ApiError::unprocessable(
                    "Cannot demote the last owner of the organization.",
                ));
            }
            sqlx::query("UPDATE org_members SET role = $2 WHERE id = $1")
                .bind(id)
                .bind(&role)
                .execute(&mut *tx)
                .await?;
            tx.sync_model(SyncModel::Membership, id, SyncAction::Update)
                .await?;
            audit::log(
                &mut *tx,
                Some(&auth.user),
                "org.update_member",
                audit::Target::Org(access.org.id),
                json!({ "user": user.login, "role": role }),
            )
            .await?;
        }
        tx.commit().await?;
        return Ok(Json(membership_json(
            &state, &access, &user, "active", &role,
        )));
    }
    let inv_role = if role == "admin" {
        "admin"
    } else {
        "direct_member"
    };
    match pending_invitation(&mut *tx, access.org.id, user.id).await? {
        Some(inv) => {
            sqlx::query("UPDATE org_invitations SET role = $2 WHERE id = $1")
                .bind(inv.id)
                .bind(inv_role)
                .execute(&mut *tx)
                .await?;
        }
        None => {
            create_invitation(
                &state,
                &mut tx,
                &access,
                &auth.user,
                Invitee::User(&user),
                inv_role,
                &[],
            )
            .await?;
        }
    }
    tx.commit().await?;
    Ok(Json(membership_json(
        &state, &access, &user, "pending", &role,
    )))
}

/// `DELETE /orgs/{org}/memberships/{username}` (owners) → 204: removes the
/// member or cancels the pending invitation.
pub async fn delete_membership(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((org, username)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    let access = OrgAccess::load(&state, Some(&auth), &org).await?;
    let user = util::find_user(&state, &username).await?;
    if user.id != auth.user.id {
        access.require_admin()?;
    }
    if remove_member(&state, &auth.user, &access.org, &user).await? {
        return Ok(StatusCode::NO_CONTENT);
    }
    let deleted = sqlx::query(
        "DELETE FROM org_invitations i WHERE i.org_id = $1 AND i.failed_at IS NULL
           AND (i.invitee_id = $2 OR lower(i.email) IN
                (SELECT lower(email) FROM user_emails WHERE user_id = $2 AND verified))",
    )
    .bind(access.org.id)
    .bind(user.id)
    .execute(&state.db)
    .await?
    .rows_affected();
    if deleted == 0 {
        return Err(ApiError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
pub struct MyMembershipsQuery {
    pub state: Option<String>,
}

/// `GET /user/memberships/orgs?state=active|pending`
pub async fn my_memberships(
    State(state): State<AppState>,
    auth: RequireUser,
    Query(q): Query<MyMembershipsQuery>,
    p: Pagination,
) -> ApiResult<Page<OrgMembership>> {
    auth.require_scope("read:org")?;
    let filter = q.state.as_deref();
    if !matches!(filter, None | Some("active") | Some("pending")) {
        return Err(ApiError::invalid_field(FieldError::invalid(
            "Membership",
            "state",
        )));
    }
    // (org_id, state, role)
    let rows: Vec<(i64, String, String)> = sqlx::query_as(&format!(
        "SELECT org_id, state, role FROM (
             SELECT m.org_id, 'active' AS state, m.role FROM org_members m WHERE m.user_id = $1
             UNION ALL
             SELECT DISTINCT ON (i.org_id) i.org_id, 'pending',
                    CASE WHEN i.role = 'admin' THEN 'admin' ELSE 'member' END
               FROM org_invitations i
              WHERE i.failed_at IS NULL AND {GRANTING_ROLE}
                AND (i.invitee_id = $1 OR lower(i.email) IN
                     (SELECT lower(email) FROM user_emails WHERE user_id = $1 AND verified))
                AND NOT EXISTS (SELECT 1 FROM org_members m2
                                 WHERE m2.org_id = i.org_id AND m2.user_id = $1)
         ) x
         JOIN users o ON o.id = x.org_id
         WHERE ($2::text IS NULL OR x.state = $2)
         ORDER BY lower(o.login), x.org_id LIMIT $3 OFFSET $4",
    ))
    .bind(auth.user.id)
    .bind(filter)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let page = p.page(rows);
    let ids: Vec<i64> = page.items.iter().map(|r| r.0).collect();
    let orgs = views::users_by_id(&state, ids.iter().map(|i| Some(*i))).await?;
    let settings: HashMap<i64, db::OrgSettings> = sqlx::query_as::<_, db::OrgSettings>(&format!(
        "SELECT {} FROM org_settings WHERE org_id = ANY($1)",
        db::OrgSettings::COLUMNS
    ))
    .bind(&ids)
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .map(|s| (s.org_id, s))
    .collect();
    let items = page
        .items
        .iter()
        .filter_map(|(org_id, st, role)| {
            let org = orgs.get(org_id)?;
            let s = settings.get(org_id)?;
            Some(OrgMembership::new(
                &state.urls,
                org,
                s.description.as_deref(),
                &auth.user,
                st,
                role,
                can_create_repository(s, role),
            ))
        })
        .collect();
    Ok(Page {
        items,
        link: page.link,
    })
}

/// `GET /user/memberships/orgs/{org}`
pub async fn my_membership(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(org): Path<String>,
) -> ApiResult<Json<OrgMembership>> {
    auth.require_scope("read:org")?;
    let access = OrgAccess::load(&state, Some(&auth), &org).await?;
    if let Some(role) = &access.role {
        return Ok(Json(membership_json(
            &state, &access, &auth.user, "active", role,
        )));
    }
    let inv = pending_invitation(&state.db, access.org.id, auth.user.id)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(membership_json(
        &state,
        &access,
        &auth.user,
        "pending",
        invitation_member_role(&inv.role),
    )))
}

#[derive(Debug, Deserialize)]
pub struct UpdateMyMembershipBody {
    pub state: Option<String>,
}

/// `PATCH /user/memberships/orgs/{org} {"state": "active"}`: accept a
/// pending invitation.
pub async fn accept_membership(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(org): Path<String>,
    Json(body): Json<UpdateMyMembershipBody>,
) -> ApiResult<Json<OrgMembership>> {
    auth.require_scope("write:org")?;
    if body.state.as_deref() != Some("active") {
        return Err(ApiError::invalid_field(FieldError::invalid(
            "Membership",
            "state",
        )));
    }
    let access = OrgAccess::load(&state, Some(&auth), &org).await?;
    if let Some(role) = &access.role {
        return Ok(Json(membership_json(
            &state, &access, &auth.user, "active", role,
        )));
    }
    let mut tx = Tx::begin(&state).await?;
    let inv = pending_invitation(&mut *tx, access.org.id, auth.user.id)
        .await?
        .ok_or_else(|| {
            ApiError::forbidden("You don't have a pending invitation to this organization.")
        })?;
    let role = invitation_member_role(&inv.role);
    add_member(
        &mut tx,
        &access.org,
        &auth.user,
        role,
        &inv.team_ids,
        inv.inviter_id.unwrap_or(auth.user.id),
    )
    .await?;
    sqlx::query(
        "DELETE FROM org_invitations i WHERE i.org_id = $1
           AND (i.invitee_id = $2 OR lower(i.email) IN
                (SELECT lower(email) FROM user_emails WHERE user_id = $2 AND verified))",
    )
    .bind(access.org.id)
    .bind(auth.user.id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(membership_json(
        &state, &access, &auth.user, "active", role,
    )))
}

/// Insert an org membership (plus team memberships) inside `tx`.
pub async fn add_member(
    tx: &mut Tx,
    org: &db::User,
    user: &db::User,
    role: &str,
    team_ids: &[i64],
    actor_id: i64,
) -> ApiResult<()> {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO org_members (org_id, user_id, role) VALUES ($1, $2, $3)
         ON CONFLICT (org_id, user_id) DO UPDATE SET role = EXCLUDED.role RETURNING id",
    )
    .bind(org.id)
    .bind(user.id)
    .bind(role)
    .fetch_one(&mut **tx)
    .await?;
    tx.sync_model(SyncModel::Membership, id, SyncAction::Insert)
        .await?;
    // The member's `user` row, now also in the org scope.
    tx.sync_user(user.id).await?;
    let added: Vec<i64> = sqlx::query_scalar(
        "INSERT INTO team_members (team_id, user_id)
         SELECT t.id, $2 FROM teams t WHERE t.id = ANY($1) AND t.org_id = $3
         ON CONFLICT DO NOTHING RETURNING team_id",
    )
    .bind(team_ids)
    .bind(user.id)
    .bind(org.id)
    .fetch_all(&mut **tx)
    .await?;
    for team_id in added {
        teams::sync_team(tx, team_id, SyncAction::Update).await?;
        tx.emit(Event::TeamMemberAdded {
            org_id: org.id,
            team_id,
            user_id: user.id,
            actor_id,
        });
    }
    audit::log(
        &mut **tx,
        Some(user),
        "org.add_member",
        audit::Target::Org(org.id),
        json!({ "user": user.login, "role": role }),
    )
    .await?;
    tx.emit(Event::OrgMemberAdded {
        org_id: org.id,
        user_id: user.id,
        actor_id,
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// Invitations
// ---------------------------------------------------------------------------

pub enum Invitee<'a> {
    User(&'a db::User),
    Email(&'a str),
}

/// Create an invitation in `tx` and queue the invitation mail.
pub async fn create_invitation(
    state: &AppState,
    tx: &mut Tx,
    access: &OrgAccess,
    inviter: &db::User,
    invitee: Invitee<'_>,
    role: &str,
    team_ids: &[i64],
) -> ApiResult<InvitationRow> {
    let (invitee_id, email) = match &invitee {
        Invitee::User(u) => (Some(u.id), None),
        Invitee::Email(e) => (None, Some(e.to_string())),
    };
    if let Some(uid) = invitee_id {
        if member_row(&mut **tx, access.org.id, uid).await?.is_some() {
            return Err(ApiError::invalid_field(FieldError::custom(
                "OrganizationInvitation",
                "invitee_id",
                "Invitee is already a part of this organization",
            )));
        }
        if social::is_blocked(&mut **tx, access.org.id, uid).await? {
            return Err(ApiError::invalid_field(FieldError::custom(
                "OrganizationInvitation",
                "invitee_id",
                "Invitee is blocked by this organization",
            )));
        }
    }
    let inv: InvitationRow = sqlx::query_as(&format!(
        "INSERT INTO org_invitations (org_id, invitee_id, email, inviter_id, role, team_ids)
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING {}",
        InvitationRow::COLUMNS
    ))
    .bind(access.org.id)
    .bind(invitee_id)
    .bind(&email)
    .bind(inviter.id)
    .bind(role)
    .bind(team_ids)
    .fetch_one(&mut **tx)
    .await
    .map_err(|e| match unique_violation(&e).as_deref() {
        Some("org_invitations_pending_user_key" | "org_invitations_pending_email_key") => {
            ApiError::invalid_field(FieldError::custom(
                "OrganizationInvitation",
                "invitee",
                "Invitee has already been invited",
            ))
        }
        _ => e.into(),
    })?;
    let to = match &invitee {
        Invitee::User(u) => util::primary_email(&mut **tx, u.id).await?,
        Invitee::Email(e) => Some(e.to_string()),
    };
    if let Some(to) = to {
        let greeting = match &invitee {
            Invitee::User(u) => u.login.clone(),
            Invitee::Email(e) => e.to_string(),
        };
        let link = state
            .urls
            .html(&format!("/orgs/{}/invitation", access.org.login));
        util::queue_mail(
            tx,
            mail::templates::org_invitation(
                &state.config.site_name,
                &to,
                &greeting,
                &inviter.login,
                &access.org.login,
                &link,
            ),
        )
        .await?;
    }
    audit::log(
        &mut **tx,
        Some(inviter),
        "org.invite_member",
        audit::Target::Org(access.org.id),
        json!({ "invitee_id": invitee_id, "email": email, "role": role }),
    )
    .await?;
    tx.emit(Event::OrgMemberInvited {
        org_id: access.org.id,
        invitation_id: inv.id,
        actor_id: inviter.id,
    });
    Ok(inv)
}

async fn render_invitations(
    state: &AppState,
    rows: Vec<InvitationRow>,
) -> ApiResult<Vec<OrgInvitation>> {
    let users = views::users_by_id(
        state,
        rows.iter().flat_map(|r| [r.invitee_id, r.inviter_id]),
    )
    .await?;
    Ok(rows
        .iter()
        .map(|r| {
            OrgInvitation::new(
                &state.urls,
                r,
                r.invitee_id.and_then(|id| users.get(&id)),
                r.inviter_id.and_then(|id| users.get(&id)),
            )
        })
        .collect())
}

#[derive(Debug, Deserialize)]
pub struct InvitationsQuery {
    pub role: Option<String>,
    pub invitation_source: Option<String>,
}

async fn list_invitations_where(
    state: &AppState,
    org: &str,
    auth: &AuthContext,
    failed: bool,
    role: Option<&str>,
    p: &Pagination,
) -> ApiResult<Page<OrgInvitation>> {
    let access = OrgAccess::load(state, Some(auth), org).await?;
    access.require_admin()?;
    let role = match role {
        None | Some("all") => None,
        Some("admin") => Some("admin"),
        Some("direct_member") => Some("direct_member"),
        Some("billing_manager") => Some("billing_manager"),
        Some(_) => {
            return Err(ApiError::invalid_field(FieldError::invalid(
                "OrganizationInvitation",
                "role",
            )));
        }
    };
    let rows: Vec<InvitationRow> = sqlx::query_as(&format!(
        "SELECT {} FROM org_invitations
          WHERE org_id = $1 AND (failed_at IS NOT NULL) = $2 AND ($3::text IS NULL OR role = $3)
          ORDER BY id LIMIT $4 OFFSET $5",
        InvitationRow::COLUMNS
    ))
    .bind(access.org.id)
    .bind(failed)
    .bind(role)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let page = p.page(rows);
    Ok(Page {
        items: render_invitations(state, page.items).await?,
        link: page.link,
    })
}

/// `GET /orgs/{org}/invitations` (owners)
pub async fn list_invitations(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(org): Path<String>,
    Query(q): Query<InvitationsQuery>,
    p: Pagination,
) -> ApiResult<Page<OrgInvitation>> {
    list_invitations_where(&state, &org, &auth, false, q.role.as_deref(), &p).await
}

/// `GET /orgs/{org}/failed_invitations` (owners)
pub async fn list_failed_invitations(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(org): Path<String>,
    p: Pagination,
) -> ApiResult<Page<OrgInvitation>> {
    list_invitations_where(&state, &org, &auth, true, None, &p).await
}

#[derive(Debug, Deserialize)]
pub struct CreateInvitationBody {
    pub invitee_id: Option<i64>,
    pub email: Option<String>,
    pub role: Option<String>,
    #[serde(default)]
    pub team_ids: Vec<i64>,
}

/// `POST /orgs/{org}/invitations` (owners) → 201 organization-invitation.
pub async fn invite(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(org): Path<String>,
    Json(body): Json<CreateInvitationBody>,
) -> ApiResult<(StatusCode, Json<OrgInvitation>)> {
    let access = OrgAccess::load(&state, Some(&auth), &org).await?;
    access.require_admin()?;
    let role = match body.role.as_deref().unwrap_or("direct_member") {
        r @ ("admin" | "direct_member") => r.to_string(),
        "reinstate" => "direct_member".to_string(),
        _ => {
            return Err(ApiError::invalid_field(FieldError::custom(
                "OrganizationInvitation",
                "role",
                "role must be one of: admin, direct_member, reinstate",
            )));
        }
    };
    let email = util::non_empty(body.email);
    let invitee_user = match (body.invitee_id, &email) {
        (Some(id), _) => Some(
            db::User::find(&state.db, id)
                .await?
                .filter(|u| !u.is_org())
                .ok_or_else(|| {
                    ApiError::invalid_field(FieldError::invalid(
                        "OrganizationInvitation",
                        "invitee_id",
                    ))
                })?,
        ),
        (None, Some(e)) => {
            if !validate::is_valid_email(e) {
                return Err(ApiError::invalid_field(FieldError::invalid(
                    "OrganizationInvitation",
                    "email",
                )));
            }
            // An address belonging to a user invites that user.
            sqlx::query_as::<_, db::User>(&format!(
                "SELECT {} FROM users u JOIN user_emails e ON e.user_id = u.id
                  WHERE lower(e.email) = lower($1) AND e.verified",
                db::prefixed("u", db::User::COLUMNS)
            ))
            .bind(e)
            .fetch_optional(&state.db)
            .await?
        }
        (None, None) => {
            return Err(ApiError::invalid_field(FieldError::missing_field(
                "OrganizationInvitation",
                "invitee_id",
            )));
        }
    };
    let team_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM teams WHERE org_id = $1 AND id = ANY($2)")
            .bind(access.org.id)
            .bind(&body.team_ids)
            .fetch_one(&state.db)
            .await?;
    if team_count != body.team_ids.len() as i64 {
        return Err(ApiError::invalid_field(FieldError::invalid(
            "OrganizationInvitation",
            "team_ids",
        )));
    }
    let mut tx = Tx::begin(&state).await?;
    let invitee = match (&invitee_user, &email) {
        (Some(u), _) => Invitee::User(u),
        (None, Some(e)) => Invitee::Email(e),
        (None, None) => unreachable!("validated above"),
    };
    let inv = create_invitation(
        &state,
        &mut tx,
        &access,
        &auth.user,
        invitee,
        &role,
        &body.team_ids,
    )
    .await?;
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        Json(OrgInvitation::new(
            &state.urls,
            &inv,
            invitee_user.as_ref(),
            Some(&auth.user),
        )),
    ))
}

/// `DELETE /orgs/{org}/invitations/{invitation_id}` (owners) → 204.
pub async fn cancel_invitation(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((org, id)): Path<(String, i64)>,
) -> ApiResult<StatusCode> {
    let access = OrgAccess::load(&state, Some(&auth), &org).await?;
    access.require_admin()?;
    let mut tx = Tx::begin(&state).await?;
    let deleted = sqlx::query("DELETE FROM org_invitations WHERE id = $1 AND org_id = $2")
        .bind(id)
        .bind(access.org.id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    if deleted == 0 {
        return Err(ApiError::NotFound);
    }
    audit::log(
        &mut *tx,
        Some(&auth.user),
        "org.cancel_invitation",
        audit::Target::Org(access.org.id),
        json!({ "invitation_id": id }),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Teams of an invitation (team list shape).
async fn invitation_teams_for(
    state: &AppState,
    access: &OrgAccess,
    id: i64,
    p: &Pagination,
) -> ApiResult<Page<api::Team>> {
    access.require_admin()?;
    let team_ids: Vec<i64> =
        sqlx::query_scalar("SELECT team_ids FROM org_invitations WHERE id = $1 AND org_id = $2")
            .bind(id)
            .bind(access.org.id)
            .fetch_optional(&state.db)
            .await?
            .ok_or(ApiError::NotFound)?;
    let rows: Vec<db::Team> = sqlx::query_as(&format!(
        "SELECT {} FROM teams WHERE id = ANY($1) ORDER BY lower(name), id LIMIT $2 OFFSET $3",
        db::Team::COLUMNS
    ))
    .bind(&team_ids)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let page = p.page(rows);
    Ok(Page {
        items: teams::render_teams(state, &access.org, page.items).await?,
        link: page.link,
    })
}

/// `GET /orgs/{org}/invitations/{invitation_id}/teams` (owners)
pub async fn invitation_teams(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((org, id)): Path<(String, i64)>,
    p: Pagination,
) -> ApiResult<Page<api::Team>> {
    let access = OrgAccess::load(&state, Some(&auth), &org).await?;
    invitation_teams_for(&state, &access, id, &p).await
}

/// `GET /organizations/{org_id}/invitations/{invitation_id}/teams`
pub async fn invitation_teams_by_id(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((org_id, id)): Path<(i64, i64)>,
    p: Pagination,
) -> ApiResult<Page<api::Team>> {
    let org = db::User::find(&state.db, org_id)
        .await?
        .filter(db::User::is_org)
        .ok_or(ApiError::NotFound)?;
    let access = OrgAccess::for_org(&state, Some(&auth), org).await?;
    invitation_teams_for(&state, &access, id, &p).await
}

// ---------------------------------------------------------------------------
// Outside collaborators
// ---------------------------------------------------------------------------

/// `GET /orgs/{org}/outside_collaborators` (members): collaborators on the
/// organization's repositories who aren't members.
pub async fn list_outside_collaborators(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(org): Path<String>,
    Query(q): Query<MembersQuery>,
    p: Pagination,
) -> ApiResult<Page<SimpleUser>> {
    let access = OrgAccess::load(&state, Some(&auth), &org).await?;
    access.require_member()?;
    let two_fa_disabled = q.filter.as_deref() == Some("2fa_disabled");
    let rows: Vec<db::User> = sqlx::query_as(&format!(
        "SELECT {} FROM users u
          WHERE u.id IN (SELECT c.user_id FROM collaborators c
                           JOIN repositories r ON r.id = c.repo_id WHERE r.owner_id = $1)
            AND NOT EXISTS (SELECT 1 FROM org_members m WHERE m.org_id = $1 AND m.user_id = u.id)
            AND (NOT $2 OR NOT EXISTS (SELECT 1 FROM user_two_factor t
                                        WHERE t.user_id = u.id AND t.enabled_at IS NOT NULL))
          ORDER BY u.id LIMIT $3 OFFSET $4",
        db::prefixed("u", db::User::COLUMNS)
    ))
    .bind(access.org.id)
    .bind(two_fa_disabled)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    Ok(p.page(rows).map(|u| SimpleUser::new(&state.urls, &u)))
}

/// `PUT /orgs/{org}/outside_collaborators/{username}` (owners) → 204:
/// converts a member to an outside collaborator, keeping the repository
/// access they had through teams as direct collaborator grants.
pub async fn convert_to_outside_collaborator(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((org, username)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    let access = OrgAccess::load(&state, Some(&auth), &org).await?;
    access.require_admin()?;
    let user = util::find_user(&state, &username).await?;
    let Some((_, role, _)) = member_row(&state.db, access.org.id, user.id).await? else {
        return Err(ApiError::forbidden(
            "User is not a member of the organization.",
        ));
    };
    if role == "admin" {
        return Err(ApiError::forbidden(
            "Owners can't be converted to outside collaborators.",
        ));
    }
    // Team grants (incl. inherited from parent teams) → direct grants.
    sqlx::query(
        "WITH RECURSIVE ut AS (
             SELECT t.id, t.parent_id FROM team_members tm JOIN teams t ON t.id = tm.team_id
              WHERE tm.user_id = $2 AND t.org_id = $1
             UNION
             SELECT p.id, p.parent_id FROM teams p JOIN ut ON p.id = ut.parent_id
         ), grants AS (
             SELECT tr.repo_id,
                    (array_agg(tr.permission ORDER BY array_position(
                        ARRAY['read','triage','write','maintain','admin'], tr.permission) DESC))[1] AS permission
               FROM team_repos tr WHERE tr.team_id IN (SELECT id FROM ut) GROUP BY tr.repo_id
         )
         INSERT INTO collaborators (repo_id, user_id, permission)
         SELECT repo_id, $2, permission FROM grants
         ON CONFLICT (repo_id, user_id) DO UPDATE SET permission = CASE
             WHEN array_position(ARRAY['read','triage','write','maintain','admin'], EXCLUDED.permission)
                > array_position(ARRAY['read','triage','write','maintain','admin'], collaborators.permission)
             THEN EXCLUDED.permission ELSE collaborators.permission END",
    )
    .bind(access.org.id)
    .bind(user.id)
    .execute(&state.db)
    .await?;
    remove_member(&state, &auth.user, &access.org, &user).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /orgs/{org}/outside_collaborators/{username}` (owners) → 204:
/// removes the user from every repository of the organization.
pub async fn remove_outside_collaborator(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((org, username)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    let access = OrgAccess::load(&state, Some(&auth), &org).await?;
    access.require_admin()?;
    let user = util::find_user(&state, &username).await?;
    if member_row(&state.db, access.org.id, user.id)
        .await?
        .is_some()
    {
        return Err(ApiError::unprocessable(
            "You cannot specify an organization member to remove as an outside collaborator.",
        ));
    }
    let mut tx = Tx::begin(&state).await?;
    sqlx::query(
        "DELETE FROM collaborators c USING repositories r
          WHERE c.repo_id = r.id AND r.owner_id = $1 AND c.user_id = $2",
    )
    .bind(access.org.id)
    .bind(user.id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "DELETE FROM repo_invitations i USING repositories r
          WHERE i.repo_id = r.id AND r.owner_id = $1 AND i.invitee_id = $2",
    )
    .bind(access.org.id)
    .bind(user.id)
    .execute(&mut *tx)
    .await?;
    audit::log(
        &mut *tx,
        Some(&auth.user),
        "org.remove_outside_collaborator",
        audit::Target::Org(access.org.id),
        json!({ "user": user.login }),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Blocks
// ---------------------------------------------------------------------------

/// `GET /orgs/{org}/blocks` (owners)
pub async fn list_blocks(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(org): Path<String>,
    p: Pagination,
) -> ApiResult<Page<SimpleUser>> {
    let access = OrgAccess::load(&state, Some(&auth), &org).await?;
    access.require_admin()?;
    social::list_blocked(&state, access.org.id, &p).await
}

/// `GET /orgs/{org}/blocks/{username}` (owners) → 204 / 404.
pub async fn check_block(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((org, username)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    let access = OrgAccess::load(&state, Some(&auth), &org).await?;
    access.require_admin()?;
    let user = util::find_account(&state, &username).await?;
    if social::is_blocked(&state.db, access.org.id, user.id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound)
    }
}

/// `PUT /orgs/{org}/blocks/{username}` (owners) → 204. Members can't be
/// blocked (422).
pub async fn block_user(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((org, username)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    let access = OrgAccess::load(&state, Some(&auth), &org).await?;
    access.require_admin()?;
    let user = util::find_account(&state, &username).await?;
    if member_row(&state.db, access.org.id, user.id)
        .await?
        .is_some()
    {
        return Err(ApiError::unprocessable(
            "Blocking an organization member is not allowed.",
        ));
    }
    social::block(&state, &auth.user, &access.org, &user).await?;
    sqlx::query("DELETE FROM org_invitations WHERE org_id = $1 AND invitee_id = $2")
        .bind(access.org.id)
        .bind(user.id)
        .execute(&state.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /orgs/{org}/blocks/{username}` (owners) → 204.
pub async fn unblock_user(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((org, username)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    let access = OrgAccess::load(&state, Some(&auth), &org).await?;
    access.require_admin()?;
    let user = util::find_account(&state, &username).await?;
    social::unblock(&state, access.org.id, user.id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Invitee self-service (web client, `/_bgh`)
// ---------------------------------------------------------------------------

/// The viewer's invitation to an organization, as the invitation page shows
/// it.
#[derive(Debug, Serialize)]
pub struct ViewerInvitation {
    /// `pending` | `active` (already a member)
    pub state: &'static str,
    pub organization: OrganizationSimple,
    pub organization_name: Option<String>,
    /// Membership role on acceptance: `admin` | `member`.
    pub role: String,
    pub invitation_id: Option<i64>,
    pub inviter: Option<SimpleUser>,
    pub created_at: Option<Timestamp>,
    /// Names of the teams the invitee joins on acceptance.
    pub teams: Vec<String>,
}

/// `GET /_bgh/orgs/{org}/invitation` → the viewer's pending invitation
/// (or `state: active` for members); 404 without one.
pub async fn viewer_invitation(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(org): Path<String>,
) -> ApiResult<Json<ViewerInvitation>> {
    let access = OrgAccess::load(&state, Some(&auth), &org).await?;
    let organization = OrganizationSimple::new(
        &state.urls,
        &access.org,
        access.settings.description.as_deref(),
    );
    let organization_name = access.org.name.clone();
    if let Some(role) = &access.role {
        return Ok(Json(ViewerInvitation {
            state: "active",
            organization,
            organization_name,
            role: role.clone(),
            invitation_id: None,
            inviter: None,
            created_at: None,
            teams: vec![],
        }));
    }
    let inv = pending_invitation(&state.db, access.org.id, auth.user.id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let inviter = match inv.inviter_id {
        Some(id) => db::User::find(&state.db, id).await?,
        None => None,
    };
    let teams: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM teams WHERE org_id = $1 AND id = ANY($2) ORDER BY lower(name)",
    )
    .bind(access.org.id)
    .bind(&inv.team_ids)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(ViewerInvitation {
        state: "pending",
        organization,
        organization_name,
        role: invitation_member_role(&inv.role).to_string(),
        invitation_id: Some(inv.id),
        inviter: inviter.map(|u| SimpleUser::new(&state.urls, &u)),
        created_at: Some(inv.created_at.into()),
        teams,
    }))
}

/// `DELETE /_bgh/orgs/{org}/invitation` → 204: the invitee declines every
/// pending invitation of theirs to the organization.
pub async fn decline_invitation(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(org): Path<String>,
) -> ApiResult<StatusCode> {
    let org = util::find_org(&state, &org).await?;
    let mut tx = Tx::begin(&state).await?;
    let ids: Vec<i64> = sqlx::query_scalar(
        "DELETE FROM org_invitations i WHERE i.org_id = $1 AND i.failed_at IS NULL
           AND (i.invitee_id = $2 OR lower(i.email) IN
                (SELECT lower(email) FROM user_emails WHERE user_id = $2 AND verified))
         RETURNING i.id",
    )
    .bind(org.id)
    .bind(auth.user.id)
    .fetch_all(&mut *tx)
    .await?;
    if ids.is_empty() {
        return Err(ApiError::NotFound);
    }
    audit::log(
        &mut *tx,
        Some(&auth.user),
        "org.decline_invitation",
        audit::Target::Org(org.id),
        json!({ "invitation_ids": ids }),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// One of the viewer's organization memberships (settings page).
#[derive(Debug, Serialize)]
pub struct ViewerOrganization {
    pub organization: OrganizationSimple,
    pub organization_name: Option<String>,
    /// `admin` | `member`
    pub role: String,
    /// Membership is publicized.
    pub public: bool,
    /// The viewer is the only owner, so they can't leave.
    pub sole_owner: bool,
    pub members_count: i64,
}

/// `GET /_bgh/user/organizations` → the viewer's active memberships with
/// publicity and last-owner flags, ordered by login.
pub async fn viewer_organizations(
    State(state): State<AppState>,
    auth: RequireUser,
) -> ApiResult<Json<Vec<ViewerOrganization>>> {
    // (org_id, role, is_public, admins, members, description)
    let rows: Vec<(i64, String, bool, i64, i64, Option<String>)> = sqlx::query_as(
        "SELECT m.org_id, m.role, m.is_public,
                (SELECT count(*) FROM org_members a WHERE a.org_id = m.org_id AND a.role = 'admin'),
                (SELECT count(*) FROM org_members c WHERE c.org_id = m.org_id),
                s.description
           FROM org_members m
           JOIN users o ON o.id = m.org_id
           LEFT JOIN org_settings s ON s.org_id = m.org_id
          WHERE m.user_id = $1
          ORDER BY lower(o.login)",
    )
    .bind(auth.user.id)
    .fetch_all(&state.db)
    .await?;
    let orgs = views::users_by_id(&state, rows.iter().map(|r| Some(r.0))).await?;
    Ok(Json(
        rows.into_iter()
            .filter_map(|(org_id, role, public, admins, members, description)| {
                let org = orgs.get(&org_id)?;
                Some(ViewerOrganization {
                    organization: OrganizationSimple::new(&state.urls, org, description.as_deref()),
                    organization_name: org.name.clone(),
                    sole_owner: role == "admin" && admins <= 1,
                    role,
                    public,
                    members_count: members,
                })
            })
            .collect(),
    ))
}
