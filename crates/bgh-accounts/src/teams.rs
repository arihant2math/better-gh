//! Teams: CRUD, nested teams, members/memberships, team repositories.
//!
//! Every endpoint is reachable as `/orgs/{org}/teams/{team_slug}/...`,
//! `/organizations/{org_id}/team/{team_id}/...` (the form used in `url`
//! fields) and the legacy `/teams/{team_id}/...`; the [`TeamPath`]
//! extractor resolves any of them.
//!
//! Visibility: organization members see `closed` teams; `secret` teams are
//! visible to owners and the team's members. Owners and team maintainers
//! manage a team (`write:org` scope for tokens).

use std::collections::HashMap;

use axum::extract::{FromRequestParts, RawPathParams, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bgh_core::audit;
use bgh_core::error::unique_violation;
use bgh_core::models::api::{
    MinimalRepository, Repository, RepositoryExtras, SimpleUser, TeamSimple,
};
use bgh_core::perms;
use bgh_core::prelude::*;
use bgh_core::sync;
use bgh_core::views;
use serde::Deserialize;
use serde_json::json;

use crate::json::{TeamFull, TeamMembership, TeamRepository};
use crate::orgs::{self, Invitee, OrgAccess};
use crate::util::{self, Patch};

// ---------------------------------------------------------------------------
// Sync
// ---------------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct TeamSyncRow {
    #[sqlx(flatten)]
    team: db::Team,
    member_ids: Vec<i64>,
    repo_ids: Vec<i64>,
}

/// Record the `team` sync model (full row with member/repo ids) in `tx`.
pub async fn sync_team(tx: &mut Tx, team_id: i64, action: SyncAction) -> ApiResult<()> {
    let row: Option<TeamSyncRow> = sqlx::query_as(&format!(
        "SELECT {},
                ARRAY(SELECT user_id FROM team_members WHERE team_id = t.id ORDER BY user_id) AS member_ids,
                ARRAY(SELECT repo_id FROM team_repos WHERE team_id = t.id ORDER BY repo_id) AS repo_ids
           FROM teams t WHERE t.id = $1",
        db::prefixed("t", db::Team::COLUMNS)
    ))
    .bind(team_id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some(r) = row else {
        return Ok(());
    };
    let t = &r.team;
    tx.sync(
        &sync::org_scope(t.org_id),
        "team",
        t.id,
        action,
        &json!({
            "id": t.id,
            "orgId": t.org_id,
            "slug": t.slug,
            "name": t.name,
            "description": t.description,
            "privacy": t.privacy,
            "parentId": t.parent_id,
            "memberIds": r.member_ids,
            "repoIds": r.repo_ids,
        }),
    )
    .await
}

// ---------------------------------------------------------------------------
// Resolution & access
// ---------------------------------------------------------------------------

/// A team resolved for the caller.
#[derive(Debug, Clone)]
pub struct TeamCtx {
    pub access: OrgAccess,
    pub team: db::Team,
    /// The caller's direct role in the team: `member` | `maintainer`.
    pub team_role: Option<String>,
}

impl TeamCtx {
    pub fn can_manage(&self) -> bool {
        self.access.is_admin() || self.team_role.as_deref() == Some("maintainer")
    }

    /// Owners and maintainers (`write:org` for tokens).
    pub fn require_manage(&self) -> ApiResult<&AuthContext> {
        let auth = self.access.user()?;
        auth.require_scope("write:org")?;
        if !self.can_manage() {
            return Err(ApiError::forbidden(
                "You must be an organization owner or team maintainer to do that.",
            ));
        }
        Ok(auth)
    }
}

/// Whether the caller may see `team` (`role`: their direct team role).
fn visible(access: &OrgAccess, team: &db::Team, role: Option<&str>) -> bool {
    if !access.can_view_private() {
        return false;
    }
    team.privacy == "closed" || access.is_admin() || role.is_some()
}

async fn team_role(
    state: &AppState,
    team_id: i64,
    user_id: Option<i64>,
) -> ApiResult<Option<String>> {
    let Some(uid) = user_id else {
        return Ok(None);
    };
    Ok(
        sqlx::query_scalar("SELECT role FROM team_members WHERE team_id = $1 AND user_id = $2")
            .bind(team_id)
            .bind(uid)
            .fetch_optional(&state.db)
            .await?,
    )
}

/// Resolve a team for the caller (404 when invisible).
pub async fn resolve_team(
    state: &AppState,
    auth: Option<&AuthContext>,
    access: OrgAccess,
    team: db::Team,
) -> ApiResult<TeamCtx> {
    let role = team_role(state, team.id, auth.map(|a| a.user.id)).await?;
    if !visible(&access, &team, role.as_deref()) {
        return Err(ApiError::NotFound);
    }
    if let Some(a) = auth {
        a.require_scope("read:org")?;
    }
    Ok(TeamCtx {
        access,
        team,
        team_role: role,
    })
}

async fn team_by_id(state: &AppState, id: i64) -> ApiResult<db::Team> {
    sqlx::query_as(&format!(
        "SELECT {} FROM teams WHERE id = $1",
        db::Team::COLUMNS
    ))
    .bind(id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)
}

/// Extractor resolving `{org}/{team_slug}`, `{org_id}/{team_id}` or
/// `{team_id}` path params to a [`TeamCtx`]. Other params are kept in
/// `params`.
pub struct TeamPath {
    pub ctx: TeamCtx,
    pub params: HashMap<String, String>,
    pub auth: MaybeUser,
}

impl FromRequestParts<AppState> for TeamPath {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> ApiResult<Self> {
        let raw = RawPathParams::from_request_parts(parts, state)
            .await
            .map_err(|_| ApiError::NotFound)?;
        let params: HashMap<String, String> = raw
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let auth = MaybeUser::from_request_parts(parts, state).await?;
        let id = |k: &str| -> ApiResult<i64> {
            params
                .get(k)
                .and_then(|v| v.parse().ok())
                .ok_or(ApiError::NotFound)
        };
        let (access, team) =
            if let (Some(org), Some(slug)) = (params.get("org"), params.get("team_slug")) {
                let access = OrgAccess::load(state, auth.as_ref(), org).await?;
                let team: db::Team = sqlx::query_as(&format!(
                    "SELECT {} FROM teams WHERE org_id = $1 AND lower(slug) = lower($2)",
                    db::Team::COLUMNS
                ))
                .bind(access.org.id)
                .bind(slug)
                .fetch_optional(&state.db)
                .await?
                .ok_or(ApiError::NotFound)?;
                (access, team)
            } else {
                let team = team_by_id(state, id("team_id")?).await?;
                if params.contains_key("org_id") && id("org_id")? != team.org_id {
                    return Err(ApiError::NotFound);
                }
                let org = db::User::find(&state.db, team.org_id)
                    .await?
                    .ok_or(ApiError::NotFound)?;
                (OrgAccess::for_org(state, auth.as_ref(), org).await?, team)
            };
        let ctx = resolve_team(state, auth.as_ref(), access, team).await?;
        Ok(Self { ctx, params, auth })
    }
}

impl TeamPath {
    fn param(&self, k: &str) -> ApiResult<&str> {
        self.params
            .get(k)
            .map(String::as_str)
            .ok_or(ApiError::NotFound)
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// Render `team` list items (team-simple + parent) for `org`, batching
/// parent lookups.
pub async fn render_teams(
    state: &AppState,
    org: &db::User,
    teams: Vec<db::Team>,
) -> ApiResult<Vec<api::Team>> {
    let parent_ids: Vec<i64> = teams.iter().filter_map(|t| t.parent_id).collect();
    let parents: HashMap<i64, db::Team> = if parent_ids.is_empty() {
        HashMap::new()
    } else {
        sqlx::query_as::<_, db::Team>(&format!(
            "SELECT {} FROM teams WHERE id = ANY($1)",
            db::Team::COLUMNS
        ))
        .bind(&parent_ids)
        .fetch_all(&state.db)
        .await?
        .into_iter()
        .map(|t| (t.id, t))
        .collect()
    };
    Ok(teams
        .iter()
        .map(|t| api::Team {
            team: TeamSimple::new(&state.urls, &org.login, t),
            parent: t
                .parent_id
                .and_then(|p| parents.get(&p))
                .map(|p| TeamSimple::new(&state.urls, &org.login, p)),
        })
        .collect())
}

/// Render team-full.
pub async fn team_full(
    state: &AppState,
    org: &db::User,
    settings: &db::OrgSettings,
    t: &db::Team,
) -> ApiResult<TeamFull> {
    let (members, repos): (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM team_members WHERE team_id = $1),
                (SELECT count(*) FROM team_repos WHERE team_id = $1)",
    )
    .bind(t.id)
    .fetch_one(&state.db)
    .await?;
    let parent = match t.parent_id {
        Some(p) => Some(TeamSimple::new(
            &state.urls,
            &org.login,
            &team_by_id(state, p).await?,
        )),
        None => None,
    };
    Ok(TeamFull {
        team: TeamSimple::new(&state.urls, &org.login, t),
        parent,
        members_count: members,
        repos_count: repos,
        created_at: t.created_at.into(),
        updated_at: t.updated_at.into(),
        organization: orgs::org_full(state, org, settings, true).await?,
    })
}

async fn full(state: &AppState, ctx: &TeamCtx, team: &db::Team) -> ApiResult<TeamFull> {
    team_full(state, &ctx.access.org, &ctx.access.settings, team).await
}

/// GitHub's slug: lowercase, runs of other characters → `-`.
pub fn slugify(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '.' {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

fn team_conflict(e: sqlx::Error) -> ApiError {
    match unique_violation(&e).as_deref() {
        Some("teams_org_slug_key" | "teams_org_name_key") => ApiError::invalid_field(
            FieldError::custom("Team", "name", "Name must be unique for this org"),
        ),
        _ => e.into(),
    }
}

// ---------------------------------------------------------------------------
// CRUD
// ---------------------------------------------------------------------------

/// `GET /orgs/{org}/teams` → teams visible to the caller.
pub async fn list(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(org): Path<String>,
    p: Pagination,
) -> ApiResult<Page<api::Team>> {
    let access = OrgAccess::load(&state, Some(&auth), &org).await?;
    access.require_member()?;
    let rows: Vec<db::Team> = sqlx::query_as(&format!(
        "SELECT {} FROM teams t
          WHERE t.org_id = $1
            AND (t.privacy = 'closed' OR $2
                 OR EXISTS (SELECT 1 FROM team_members tm WHERE tm.team_id = t.id AND tm.user_id = $3))
          ORDER BY lower(t.name), t.id LIMIT $4 OFFSET $5",
        db::prefixed("t", db::Team::COLUMNS)
    ))
    .bind(access.org.id)
    .bind(access.is_admin())
    .bind(auth.user.id)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let page = p.page(rows);
    Ok(Page {
        items: render_teams(&state, &access.org, page.items).await?,
        link: page.link,
    })
}

#[derive(Debug, Deserialize)]
pub struct CreateTeamBody {
    #[serde(default)]
    pub name: String,
    pub description: Option<String>,
    #[serde(default)]
    pub maintainers: Vec<String>,
    #[serde(default)]
    pub repo_names: Vec<String>,
    pub privacy: Option<String>,
    pub notification_setting: Option<String>,
    pub permission: Option<String>,
    pub parent_team_id: Option<i64>,
}

fn parse_privacy(p: Option<&str>, nested: bool) -> ApiResult<String> {
    match p {
        None => Ok(if nested { "closed" } else { "secret" }.into()),
        Some("closed") => Ok("closed".into()),
        Some("secret") if !nested => Ok("secret".into()),
        Some("secret") => Err(ApiError::invalid_field(FieldError::custom(
            "Team",
            "privacy",
            "secret teams can't be nested",
        ))),
        Some(_) => Err(ApiError::invalid_field(FieldError::invalid(
            "Team", "privacy",
        ))),
    }
}

fn parse_notification(n: Option<&str>) -> ApiResult<Option<String>> {
    match n {
        None => Ok(None),
        Some(v @ ("notifications_enabled" | "notifications_disabled")) => Ok(Some(v.into())),
        Some(_) => Err(ApiError::invalid_field(FieldError::invalid(
            "Team",
            "notification_setting",
        ))),
    }
}

fn parse_team_permission(p: Option<&str>) -> ApiResult<Option<Permission>> {
    match p {
        None => Ok(None),
        Some(v) => match Permission::parse(v) {
            Some(Permission::None) | None => Err(ApiError::invalid_field(FieldError::invalid(
                "Team",
                "permission",
            ))),
            Some(p) => Ok(Some(p)),
        },
    }
}

/// Validate a parent team for `team_id` (None when creating).
async fn check_parent(
    state: &AppState,
    org_id: i64,
    parent_id: i64,
    team_id: Option<i64>,
) -> ApiResult<db::Team> {
    let parent = team_by_id(state, parent_id)
        .await
        .ok()
        .filter(|p| p.org_id == org_id)
        .ok_or_else(|| ApiError::invalid_field(FieldError::invalid("Team", "parent_team_id")))?;
    if parent.privacy == "secret" {
        return Err(ApiError::invalid_field(FieldError::custom(
            "Team",
            "parent_team_id",
            "secret teams can't be parents",
        )));
    }
    if let Some(tid) = team_id {
        // The new parent must not be the team itself or a descendant.
        let cycle: bool = sqlx::query_scalar(
            "WITH RECURSIVE anc AS (
                 SELECT id, parent_id FROM teams WHERE id = $1
                 UNION
                 SELECT t.id, t.parent_id FROM teams t JOIN anc ON t.id = anc.parent_id
             ) SELECT EXISTS (SELECT 1 FROM anc WHERE id = $2)",
        )
        .bind(parent_id)
        .bind(tid)
        .fetch_one(&state.db)
        .await?;
        if cycle {
            return Err(ApiError::invalid_field(FieldError::custom(
                "Team",
                "parent_team_id",
                "a team can't be nested under itself or its child teams",
            )));
        }
    }
    Ok(parent)
}

/// `POST /orgs/{org}/teams` → 201 team-full. The creator becomes a
/// maintainer.
pub async fn create(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(org): Path<String>,
    Json(body): Json<CreateTeamBody>,
) -> ApiResult<(StatusCode, Json<TeamFull>)> {
    auth.require_scope("write:org")?;
    let access = OrgAccess::load(&state, Some(&auth), &org).await?;
    let members_can_create: bool =
        sqlx::query_scalar("SELECT members_can_create_teams FROM org_settings WHERE org_id = $1")
            .bind(access.org.id)
            .fetch_one(&state.db)
            .await?;
    if !(access.is_admin() || (access.is_member() && members_can_create)) {
        return Err(ApiError::forbidden(
            "You must be an organization owner to create teams.",
        ));
    }
    let name = body.name.trim().to_string();
    if name.is_empty() {
        return Err(ApiError::invalid_field(FieldError::missing_field(
            "Team", "name",
        )));
    }
    let slug = slugify(&name);
    if slug.is_empty() {
        return Err(ApiError::invalid_field(FieldError::invalid("Team", "name")));
    }
    if let Some(pid) = body.parent_team_id {
        check_parent(&state, access.org.id, pid, None).await?;
    }
    let privacy = parse_privacy(body.privacy.as_deref(), body.parent_team_id.is_some())?;
    let notification = parse_notification(body.notification_setting.as_deref())?
        .unwrap_or_else(|| "notifications_enabled".into());
    let permission = parse_team_permission(body.permission.as_deref())?.unwrap_or(Permission::Read);

    // Maintainers must be organization members.
    let mut maintainers: Vec<db::User> = Vec::new();
    for login in &body.maintainers {
        let user = util::find_user(&state, login)
            .await
            .map_err(|_| ApiError::invalid_field(FieldError::invalid("Team", "maintainers")))?;
        if perms::org_role(&state.db, access.org.id, user.id)
            .await?
            .is_none()
        {
            return Err(ApiError::invalid_field(FieldError::custom(
                "Team",
                "maintainers",
                format!("{} is not a member of the organization", user.login),
            )));
        }
        maintainers.push(user);
    }
    if access.is_member() && !maintainers.iter().any(|m| m.id == auth.user.id) {
        maintainers.push(auth.user.clone());
    }
    // Repositories must belong to the organization.
    let mut repo_ids = Vec::new();
    for full_name in &body.repo_names {
        let name = match full_name.split_once('/') {
            Some((owner, name)) if owner.eq_ignore_ascii_case(&access.org.login) => name,
            Some(_) => {
                return Err(ApiError::invalid_field(FieldError::invalid(
                    "Team",
                    "repo_names",
                )));
            }
            None => full_name.as_str(),
        };
        let repo = db::Repository::find_by_name(&state.db, access.org.id, name)
            .await?
            .ok_or_else(|| ApiError::invalid_field(FieldError::invalid("Team", "repo_names")))?;
        repo_ids.push(repo.id);
    }

    let mut tx = Tx::begin(&state).await?;
    let team: db::Team = sqlx::query_as(&format!(
        "INSERT INTO teams (org_id, parent_id, name, slug, description, privacy,
                            notification_setting, permission)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING {}",
        db::Team::COLUMNS
    ))
    .bind(access.org.id)
    .bind(body.parent_team_id)
    .bind(&name)
    .bind(&slug)
    .bind(util::non_empty(body.description))
    .bind(&privacy)
    .bind(&notification)
    .bind(permission.as_str())
    .fetch_one(&mut *tx)
    .await
    .map_err(team_conflict)?;
    for m in &maintainers {
        sqlx::query(
            "INSERT INTO team_members (team_id, user_id, role) VALUES ($1, $2, 'maintainer')
             ON CONFLICT DO NOTHING",
        )
        .bind(team.id)
        .bind(m.id)
        .execute(&mut *tx)
        .await?;
    }
    for rid in &repo_ids {
        sqlx::query(
            "INSERT INTO team_repos (team_id, repo_id, permission) VALUES ($1, $2, $3)
             ON CONFLICT DO NOTHING",
        )
        .bind(team.id)
        .bind(rid)
        .bind(permission.as_str())
        .execute(&mut *tx)
        .await?;
    }
    sync_team(&mut tx, team.id, SyncAction::Insert).await?;
    audit::log(
        &mut *tx,
        Some(&auth.user),
        "team.create",
        audit::Target::Team {
            id: team.id,
            org_id: access.org.id,
        },
        json!({ "name": name }),
    )
    .await?;
    tx.emit(Event::TeamCreated {
        org_id: access.org.id,
        team_id: team.id,
        actor_id: auth.user.id,
    });
    for m in &maintainers {
        tx.emit(Event::TeamMemberAdded {
            org_id: access.org.id,
            team_id: team.id,
            user_id: m.id,
            actor_id: auth.user.id,
        });
    }
    for rid in &repo_ids {
        tx.emit(Event::TeamRepoAdded {
            org_id: access.org.id,
            team_id: team.id,
            repo_id: *rid,
            actor_id: auth.user.id,
        });
    }
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        Json(team_full(&state, &access.org, &access.settings, &team).await?),
    ))
}

/// `GET .../teams/{team}` → team-full.
pub async fn get(State(state): State<AppState>, tp: TeamPath) -> ApiResult<Json<TeamFull>> {
    Ok(Json(full(&state, &tp.ctx, &tp.ctx.team).await?))
}

#[derive(Debug, Deserialize)]
pub struct UpdateTeamBody {
    pub name: Option<String>,
    #[serde(default)]
    pub description: Patch<String>,
    pub privacy: Option<String>,
    pub notification_setting: Option<String>,
    pub permission: Option<String>,
    #[serde(default)]
    pub parent_team_id: Patch<i64>,
}

/// `PATCH .../teams/{team}` (owners, maintainers) → team-full.
pub async fn update(
    State(state): State<AppState>,
    tp: TeamPath,
    Json(body): Json<UpdateTeamBody>,
) -> ApiResult<Json<TeamFull>> {
    let actor = tp.ctx.require_manage()?.user.clone();
    let team = &tp.ctx.team;
    let org_id = tp.ctx.access.org.id;
    let (name, slug) = match body.name.as_deref().map(str::trim) {
        Some("") => return Err(ApiError::invalid_field(FieldError::invalid("Team", "name"))),
        Some(n) => (n.to_string(), slugify(n)),
        None => (team.name.clone(), team.slug.clone()),
    };
    let parent_id = match body.parent_team_id.clone() {
        Patch::Absent => team.parent_id,
        Patch::Null => None,
        Patch::Value(pid) => {
            check_parent(&state, org_id, pid, Some(team.id)).await?;
            Some(pid)
        }
    };
    let has_children: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM teams WHERE parent_id = $1)")
            .bind(team.id)
            .fetch_one(&state.db)
            .await?;
    let privacy = match body.privacy.as_deref() {
        None if parent_id.is_some() && team.privacy == "secret" => "closed".to_string(),
        None => team.privacy.clone(),
        Some(p) => parse_privacy(Some(p), parent_id.is_some() || has_children)?,
    };
    let notification = parse_notification(body.notification_setting.as_deref())?
        .unwrap_or_else(|| team.notification_setting.clone());
    let permission = parse_team_permission(body.permission.as_deref())?
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| team.permission.clone());
    let description = match body.description.into_option() {
        None => team.description.clone(),
        Some(d) => util::non_empty(d),
    };
    let mut tx = Tx::begin(&state).await?;
    let updated: db::Team = sqlx::query_as(&format!(
        "UPDATE teams SET name = $2, slug = $3, description = $4, privacy = $5,
                          notification_setting = $6, permission = $7, parent_id = $8,
                          updated_at = now()
          WHERE id = $1 RETURNING {}",
        db::Team::COLUMNS
    ))
    .bind(team.id)
    .bind(&name)
    .bind(&slug)
    .bind(&description)
    .bind(&privacy)
    .bind(&notification)
    .bind(&permission)
    .bind(parent_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(team_conflict)?;
    sync_team(&mut tx, team.id, SyncAction::Update).await?;
    let mut changes = serde_json::Map::new();
    if updated.name != team.name {
        changes.insert("name".into(), json!({ "from": team.name }));
    }
    if updated.description != team.description {
        changes.insert("description".into(), json!({ "from": team.description }));
    }
    if updated.privacy != team.privacy {
        changes.insert("privacy".into(), json!({ "from": team.privacy }));
    }
    audit::log(
        &mut *tx,
        Some(&actor),
        "team.update",
        audit::Target::Team {
            id: team.id,
            org_id,
        },
        serde_json::Value::Object(changes.clone()),
    )
    .await?;
    tx.emit(Event::TeamEdited {
        org_id,
        team_id: team.id,
        actor_id: actor.id,
        changes: serde_json::Value::Object(changes),
    });
    tx.commit().await?;
    Ok(Json(full(&state, &tp.ctx, &updated).await?))
}

/// `DELETE .../teams/{team}` (owners, maintainers) → 204; child teams are
/// deleted too.
pub async fn delete(State(state): State<AppState>, tp: TeamPath) -> ApiResult<StatusCode> {
    let actor = tp.ctx.require_manage()?.user.clone();
    let org_id = tp.ctx.access.org.id;
    let mut tx = Tx::begin(&state).await?;
    let deleted: Vec<(i64, String)> = sqlx::query_as(
        "WITH RECURSIVE tree AS (
             SELECT id FROM teams WHERE id = $1
             UNION
             SELECT t.id FROM teams t JOIN tree ON t.parent_id = tree.id
         ) DELETE FROM teams WHERE id IN (SELECT id FROM tree) RETURNING id, slug",
    )
    .bind(tp.ctx.team.id)
    .fetch_all(&mut *tx)
    .await?;
    let scope = sync::org_scope(org_id);
    for (id, slug) in &deleted {
        tx.sync(
            &scope,
            "team",
            *id,
            SyncAction::Delete,
            &json!({ "id": id }),
        )
        .await?;
        tx.emit(Event::TeamDeleted {
            org_id,
            team_id: *id,
            slug: slug.clone(),
            actor_id: actor.id,
        });
    }
    // Pending invitations no longer reference deleted teams.
    let ids: Vec<i64> = deleted.iter().map(|d| d.0).collect();
    sqlx::query(
        "UPDATE org_invitations SET team_ids = ARRAY(SELECT x FROM unnest(team_ids) x WHERE x <> ALL($2))
          WHERE org_id = $1 AND team_ids && $2",
    )
    .bind(org_id)
    .bind(&ids)
    .execute(&mut *tx)
    .await?;
    audit::log(
        &mut *tx,
        Some(&actor),
        "team.destroy",
        audit::Target::Team {
            id: tp.ctx.team.id,
            org_id,
        },
        json!({ "name": tp.ctx.team.name }),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET .../teams/{team}/teams` → child teams.
pub async fn children(
    State(state): State<AppState>,
    tp: TeamPath,
    p: Pagination,
) -> ApiResult<Page<api::Team>> {
    let rows: Vec<db::Team> = sqlx::query_as(&format!(
        "SELECT {} FROM teams t
          WHERE t.parent_id = $1
            AND (t.privacy = 'closed' OR $2
                 OR EXISTS (SELECT 1 FROM team_members tm WHERE tm.team_id = t.id AND tm.user_id = $3))
          ORDER BY lower(t.name), t.id LIMIT $4 OFFSET $5",
        db::prefixed("t", db::Team::COLUMNS)
    ))
    .bind(tp.ctx.team.id)
    .bind(tp.ctx.access.is_admin())
    .bind(tp.auth.user_id())
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let page = p.page(rows);
    Ok(Page {
        items: render_teams(&state, &tp.ctx.access.org, page.items).await?,
        link: page.link,
    })
}

// ---------------------------------------------------------------------------
// Members
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct MembersQuery {
    pub role: Option<String>,
}

/// `GET .../teams/{team}/members?role=member|maintainer|all`: members of the
/// team and its child teams.
pub async fn members(
    State(state): State<AppState>,
    tp: TeamPath,
    Query(q): Query<MembersQuery>,
    p: Pagination,
) -> ApiResult<Page<SimpleUser>> {
    let role = match q.role.as_deref() {
        None | Some("all") => None,
        Some(r @ ("member" | "maintainer")) => Some(r.to_string()),
        Some(_) => return Err(ApiError::invalid_field(FieldError::invalid("Team", "role"))),
    };
    let rows: Vec<db::User> = sqlx::query_as(&format!(
        "WITH RECURSIVE tree AS (
             SELECT id FROM teams WHERE id = $1
             UNION
             SELECT t.id FROM teams t JOIN tree ON t.parent_id = tree.id
         )
         SELECT {} FROM users u
          WHERE u.id IN (SELECT tm.user_id FROM team_members tm
                          WHERE tm.team_id IN (SELECT id FROM tree)
                            AND ($2::text IS NULL OR (tm.role = $2 AND tm.team_id = $1)))
          ORDER BY u.id LIMIT $3 OFFSET $4",
        db::prefixed("u", db::User::COLUMNS)
    ))
    .bind(tp.ctx.team.id)
    .bind(&role)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    Ok(p.page(rows).map(|u| SimpleUser::new(&state.urls, &u)))
}

/// Membership of `user` in `team`: direct role, or `member` through a
/// child team.
async fn membership_of(
    state: &AppState,
    team: &db::Team,
    user_id: i64,
) -> ApiResult<Option<String>> {
    Ok(sqlx::query_scalar(
        "WITH RECURSIVE tree AS (
             SELECT id FROM teams WHERE id = $1
             UNION
             SELECT t.id FROM teams t JOIN tree ON t.parent_id = tree.id
         )
         SELECT CASE WHEN tm.team_id = $1 THEN tm.role ELSE 'member' END
           FROM team_members tm
          WHERE tm.user_id = $2 AND tm.team_id IN (SELECT id FROM tree)
          ORDER BY (tm.team_id = $1) DESC LIMIT 1",
    )
    .bind(team.id)
    .bind(user_id)
    .fetch_optional(&state.db)
    .await?)
}

/// `GET .../teams/{team}/memberships/{username}` → team-membership.
pub async fn get_membership(
    State(state): State<AppState>,
    tp: TeamPath,
) -> ApiResult<Json<TeamMembership>> {
    let user = util::find_user(&state, tp.param("username")?).await?;
    let team = &tp.ctx.team;
    if let Some(role) = membership_of(&state, team, user.id).await? {
        return Ok(Json(TeamMembership::new(
            &state.urls,
            team,
            &user.login,
            &role,
            "active",
        )));
    }
    let pending: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM org_invitations
                         WHERE org_id = $1 AND invitee_id = $2 AND $3 = ANY(team_ids)
                           AND failed_at IS NULL)",
    )
    .bind(team.org_id)
    .bind(user.id)
    .bind(team.id)
    .fetch_one(&state.db)
    .await?;
    if pending {
        return Ok(Json(TeamMembership::new(
            &state.urls,
            team,
            &user.login,
            "member",
            "pending",
        )));
    }
    Err(ApiError::NotFound)
}

#[derive(Debug, Deserialize)]
pub struct SetMembershipBody {
    pub role: Option<String>,
}

/// `PUT .../teams/{team}/memberships/{username} {role}` (owners,
/// maintainers). Non-members of the organization are invited (owners
/// only) and the membership is `pending`.
pub async fn set_membership(
    State(state): State<AppState>,
    tp: TeamPath,
    Json(body): Json<SetMembershipBody>,
) -> ApiResult<Json<TeamMembership>> {
    let actor = tp.ctx.require_manage()?.user.clone();
    let role = match body.role.as_deref().unwrap_or("member") {
        r @ ("member" | "maintainer") => r.to_string(),
        _ => {
            return Err(ApiError::invalid_field(FieldError::invalid(
                "TeamMember",
                "role",
            )));
        }
    };
    let user = util::find_user(&state, tp.param("username")?).await?;
    let team = &tp.ctx.team;
    let access = &tp.ctx.access;
    let mut tx = Tx::begin(&state).await?;
    if perms::org_role(&mut *tx, access.org.id, user.id)
        .await?
        .is_none()
    {
        if !access.is_admin() {
            return Err(ApiError::forbidden(
                "Only organization owners can add people outside the organization to a team.",
            ));
        }
        let existing: Option<i64> = sqlx::query_scalar(
            "SELECT id FROM org_invitations WHERE org_id = $1 AND invitee_id = $2 AND failed_at IS NULL",
        )
        .bind(access.org.id)
        .bind(user.id)
        .fetch_optional(&mut *tx)
        .await?;
        match existing {
            Some(id) => {
                sqlx::query(
                    "UPDATE org_invitations SET team_ids = array_append(team_ids, $2)
                      WHERE id = $1 AND NOT ($2 = ANY(team_ids))",
                )
                .bind(id)
                .bind(team.id)
                .execute(&mut *tx)
                .await?;
            }
            None => {
                orgs::create_invitation(
                    &state,
                    &mut tx,
                    access,
                    &actor,
                    Invitee::User(&user),
                    "direct_member",
                    &[team.id],
                )
                .await?;
            }
        }
        tx.commit().await?;
        return Ok(Json(TeamMembership::new(
            &state.urls,
            team,
            &user.login,
            &role,
            "pending",
        )));
    }
    let inserted: bool = sqlx::query_scalar(
        "INSERT INTO team_members (team_id, user_id, role) VALUES ($1, $2, $3)
         ON CONFLICT (team_id, user_id) DO UPDATE SET role = EXCLUDED.role
         RETURNING (xmax = 0)",
    )
    .bind(team.id)
    .bind(user.id)
    .bind(&role)
    .fetch_one(&mut *tx)
    .await?;
    sync_team(&mut tx, team.id, SyncAction::Update).await?;
    audit::log(
        &mut *tx,
        Some(&actor),
        "team.add_member",
        audit::Target::Team {
            id: team.id,
            org_id: team.org_id,
        },
        json!({ "user": user.login, "role": role }),
    )
    .await?;
    if inserted {
        tx.emit(Event::TeamMemberAdded {
            org_id: team.org_id,
            team_id: team.id,
            user_id: user.id,
            actor_id: actor.id,
        });
    }
    tx.commit().await?;
    Ok(Json(TeamMembership::new(
        &state.urls,
        team,
        &user.login,
        &role,
        "active",
    )))
}

/// `DELETE .../teams/{team}/memberships/{username}` (owners, maintainers,
/// or the member leaving) → 204.
pub async fn delete_membership(
    State(state): State<AppState>,
    tp: TeamPath,
) -> ApiResult<StatusCode> {
    let user = util::find_user(&state, tp.param("username")?).await?;
    let actor = tp.ctx.access.user()?.user.clone();
    if user.id != actor.id {
        tp.ctx.require_manage()?;
    }
    let team = &tp.ctx.team;
    let mut tx = Tx::begin(&state).await?;
    let removed = sqlx::query("DELETE FROM team_members WHERE team_id = $1 AND user_id = $2")
        .bind(team.id)
        .bind(user.id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    if removed == 0 {
        // Maybe a pending invitation for this team.
        let updated = sqlx::query(
            "UPDATE org_invitations SET team_ids = array_remove(team_ids, $3)
              WHERE org_id = $1 AND invitee_id = $2 AND $3 = ANY(team_ids)",
        )
        .bind(team.org_id)
        .bind(user.id)
        .bind(team.id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if updated == 0 {
            return Err(ApiError::NotFound);
        }
        tx.commit().await?;
        return Ok(StatusCode::NO_CONTENT);
    }
    sync_team(&mut tx, team.id, SyncAction::Update).await?;
    audit::log(
        &mut *tx,
        Some(&actor),
        "team.remove_member",
        audit::Target::Team {
            id: team.id,
            org_id: team.org_id,
        },
        json!({ "user": user.login }),
    )
    .await?;
    tx.emit(Event::TeamMemberRemoved {
        org_id: team.org_id,
        team_id: team.id,
        user_id: user.id,
        actor_id: actor.id,
    });
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET .../teams/{team}/invitations` (owners, maintainers): pending org
/// invitations that include the team.
pub async fn invitations(
    State(state): State<AppState>,
    tp: TeamPath,
    p: Pagination,
) -> ApiResult<Page<crate::json::OrgInvitation>> {
    tp.ctx.require_manage()?;
    let rows: Vec<crate::json::InvitationRow> = sqlx::query_as(&format!(
        "SELECT {} FROM org_invitations WHERE org_id = $1 AND $2 = ANY(team_ids)
           AND failed_at IS NULL ORDER BY id LIMIT $3 OFFSET $4",
        crate::json::InvitationRow::COLUMNS
    ))
    .bind(tp.ctx.team.org_id)
    .bind(tp.ctx.team.id)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let page = p.page(rows);
    let users = views::users_by_id(
        &state,
        page.items.iter().flat_map(|r| [r.invitee_id, r.inviter_id]),
    )
    .await?;
    Ok(Page {
        items: page
            .items
            .iter()
            .map(|r| {
                crate::json::OrgInvitation::new(
                    &state.urls,
                    r,
                    r.invitee_id.and_then(|id| users.get(&id)),
                    r.inviter_id.and_then(|id| users.get(&id)),
                )
            })
            .collect(),
        link: page.link,
    })
}

// ---------------------------------------------------------------------------
// Repositories
// ---------------------------------------------------------------------------

/// `GET .../teams/{team}/repos` → repositories the team has access to
/// (minimal-repository with the team's `permissions` and `role_name`).
pub async fn repos(
    State(state): State<AppState>,
    tp: TeamPath,
    p: Pagination,
) -> ApiResult<Page<serde_json::Value>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        #[sqlx(flatten)]
        repo: db::Repository,
        team_permission: String,
    }
    let rows: Vec<Row> = sqlx::query_as(&format!(
        "SELECT {}, tr.permission AS team_permission FROM team_repos tr
           JOIN repositories r ON r.id = tr.repo_id
          WHERE tr.team_id = $1 ORDER BY lower(r.name), r.id LIMIT $2 OFFSET $3",
        db::prefixed("r", db::Repository::COLUMNS)
    ))
    .bind(tp.ctx.team.id)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let page = p.page(rows);
    let owners =
        views::users_by_id(&state, page.items.iter().map(|r| Some(r.repo.owner_id))).await?;
    let mut items = Vec::with_capacity(page.items.len());
    for r in &page.items {
        let Some(owner) = owners.get(&r.repo.owner_id) else {
            continue;
        };
        let perm = Permission::parse(&r.team_permission).unwrap_or(Permission::Read);
        let mut v = serde_json::to_value(MinimalRepository::new(
            &state.urls,
            &r.repo,
            owner,
            Some(perm),
        ))?;
        v["role_name"] = json!(perm.as_str());
        items.push(v);
    }
    Ok(Page {
        items,
        link: page.link,
    })
}

async fn team_repo(state: &AppState, tp: &TeamPath) -> ApiResult<(db::Repository, db::User)> {
    let owner = util::find_account(state, tp.param("owner")?).await?;
    let repo = db::Repository::find_by_name(&state.db, owner.id, tp.param("repo")?)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok((repo, owner))
}

/// `GET .../teams/{team}/repos/{owner}/{repo}` → 204 when the team has
/// access, 200 team-repository with
/// `Accept: application/vnd.github.v3.repository+json`, else 404.
pub async fn check_repo(
    State(state): State<AppState>,
    headers: HeaderMap,
    tp: TeamPath,
) -> ApiResult<Response> {
    let (repo, owner) = team_repo(&state, &tp).await?;
    let perm: Option<String> =
        sqlx::query_scalar("SELECT permission FROM team_repos WHERE team_id = $1 AND repo_id = $2")
            .bind(tp.ctx.team.id)
            .bind(repo.id)
            .fetch_optional(&state.db)
            .await?;
    let Some(perm) = perm.and_then(|p| Permission::parse(&p)) else {
        return Err(ApiError::NotFound);
    };
    let wants_repo = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|a| a.contains("repository+json"));
    if !wants_repo {
        return Ok(StatusCode::NO_CONTENT.into_response());
    }
    let full = Repository::new(
        &state.urls,
        &repo,
        &owner,
        Some(perm),
        RepositoryExtras::default(),
    );
    Ok(Json(TeamRepository::new(full, perm)).into_response())
}

#[derive(Debug, Deserialize, Default)]
pub struct AddRepoBody {
    pub permission: Option<String>,
}

/// `PUT .../teams/{team}/repos/{owner}/{repo} {permission}` → 204. Needs
/// team management rights and admin access to the repository, which must
/// belong to the team's organization.
pub async fn add_repo(
    State(state): State<AppState>,
    tp: TeamPath,
    body: axum::body::Bytes,
) -> ApiResult<StatusCode> {
    let actor = tp.ctx.require_manage()?.user.clone();
    let body: AddRepoBody = util::optional_json(&body)?;
    let (repo, _owner) = team_repo(&state, &tp).await?;
    let team = &tp.ctx.team;
    if repo.owner_id != team.org_id {
        return Err(ApiError::unprocessable(
            "The repository must be owned by the team's organization.",
        ));
    }
    if !tp.ctx.access.is_admin() {
        let p = perms::repo_permission(&state.db, Some(actor.id), &repo).await?;
        if p < Permission::Admin {
            return Err(ApiError::forbidden("Must have admin rights to Repository."));
        }
    }
    let permission = match body.permission.as_deref() {
        None => Permission::parse(&team.permission).unwrap_or(Permission::Read),
        Some(p) => match Permission::parse(p) {
            Some(Permission::None) | None => {
                return Err(ApiError::invalid_field(FieldError::invalid(
                    "TeamRepository",
                    "permission",
                )));
            }
            Some(p) => p,
        },
    };
    let mut tx = Tx::begin(&state).await?;
    sqlx::query(
        "INSERT INTO team_repos (team_id, repo_id, permission) VALUES ($1, $2, $3)
         ON CONFLICT (team_id, repo_id) DO UPDATE SET permission = EXCLUDED.permission",
    )
    .bind(team.id)
    .bind(repo.id)
    .bind(permission.as_str())
    .execute(&mut *tx)
    .await?;
    sync_team(&mut tx, team.id, SyncAction::Update).await?;
    audit::log(
        &mut *tx,
        Some(&actor),
        "team.add_repository",
        audit::Target::Team {
            id: team.id,
            org_id: team.org_id,
        },
        json!({ "repo": repo.name, "permission": permission.as_str() }),
    )
    .await?;
    tx.emit(Event::TeamRepoAdded {
        org_id: team.org_id,
        team_id: team.id,
        repo_id: repo.id,
        actor_id: actor.id,
    });
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE .../teams/{team}/repos/{owner}/{repo}` → 204 (team managers or
/// repository admins).
pub async fn remove_repo(State(state): State<AppState>, tp: TeamPath) -> ApiResult<StatusCode> {
    let actor = tp.ctx.access.user()?.user.clone();
    let (repo, _owner) = team_repo(&state, &tp).await?;
    let team = &tp.ctx.team;
    if tp.ctx.require_manage().is_err() {
        let p = perms::repo_permission(&state.db, Some(actor.id), &repo).await?;
        if p < Permission::Admin {
            return Err(ApiError::forbidden(
                "You must be a team maintainer or repository admin to do that.",
            ));
        }
    }
    let mut tx = Tx::begin(&state).await?;
    let removed = sqlx::query("DELETE FROM team_repos WHERE team_id = $1 AND repo_id = $2")
        .bind(team.id)
        .bind(repo.id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    if removed > 0 {
        sync_team(&mut tx, team.id, SyncAction::Update).await?;
        audit::log(
            &mut *tx,
            Some(&actor),
            "team.remove_repository",
            audit::Target::Team {
                id: team.id,
                org_id: team.org_id,
            },
            json!({ "repo": repo.name }),
        )
        .await?;
        tx.emit(Event::TeamRepoRemoved {
            org_id: team.org_id,
            team_id: team.id,
            repo_id: repo.id,
            actor_id: actor.id,
        });
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// /user/teams
// ---------------------------------------------------------------------------

/// `GET /user/teams` (scope `read:org`) → team-full for every team the
/// caller belongs to.
pub async fn my_teams(
    State(state): State<AppState>,
    auth: RequireUser,
    p: Pagination,
) -> ApiResult<Page<TeamFull>> {
    if !auth.has_scope("read:org") && !auth.has_scope("user") && !auth.has_scope("repo") {
        auth.require_scope("read:org")?;
    }
    let rows: Vec<db::Team> = sqlx::query_as(&format!(
        "SELECT {} FROM team_members tm JOIN teams t ON t.id = tm.team_id
          WHERE tm.user_id = $1 ORDER BY t.org_id, lower(t.name), t.id LIMIT $2 OFFSET $3",
        db::prefixed("t", db::Team::COLUMNS)
    ))
    .bind(auth.user.id)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let page = p.page(rows);
    let orgs = views::users_by_id(&state, page.items.iter().map(|t| Some(t.org_id))).await?;
    let mut settings: HashMap<i64, db::OrgSettings> = HashMap::new();
    let mut items = Vec::with_capacity(page.items.len());
    for t in &page.items {
        let Some(org) = orgs.get(&t.org_id) else {
            continue;
        };
        if !settings.contains_key(&org.id) {
            if let Some(s) = db::OrgSettings::find(&state.db, org.id).await? {
                settings.insert(org.id, s);
            }
        }
        let Some(s) = settings.get(&org.id) else {
            continue;
        };
        items.push(team_full(&state, org, s, t).await?);
    }
    Ok(Page {
        items,
        link: page.link,
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn slugs() {
        assert_eq!(super::slugify("Justice League"), "justice-league");
        assert_eq!(super::slugify("  A & B!! "), "a-b");
        assert_eq!(super::slugify("ops.v2_team"), "ops.v2_team");
    }
}
