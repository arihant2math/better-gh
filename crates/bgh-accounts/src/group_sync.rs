//! Team membership driven by external groups (`external_group_mappings`):
//! LDAP group DNs (provider `ldap`, see [`crate::ldap`]) and OIDC `groups`
//! claims (provider `oidc`, applied at sign-in by [`crate::sso`]). Reused by
//! SAML/SCIM group sync.
//!
//! A mapped team's membership is managed by its provider: members of a
//! mapped group are added (and made organization members when needed),
//! everyone else is removed.

use std::collections::{BTreeSet, HashSet};

use axum::extract::State;
use bgh_core::audit;
use bgh_core::prelude::*;
use serde_json::json;

use crate::{orgs, teams};

/// Provider name of OIDC group mappings (all OIDC providers share it).
pub const OIDC: &str = "oidc";

/// Providers whose groups GitHub's team-sync REST maps (IdP group names:
/// the OIDC groups claim, the SAML groups attribute, SCIM groups).
pub const IDP_PROVIDERS: &[&str] = &[OIDC, crate::saml::PROVIDER, crate::scim::PROVIDER];

/// `(team_id, external_group_id)` mappings of `provider`.
pub async fn mappings(state: &AppState, provider: &str) -> ApiResult<Vec<(i64, String)>> {
    Ok(sqlx::query_as(
        "SELECT team_id, external_group_id FROM external_group_mappings
          WHERE provider = $1 ORDER BY team_id, id",
    )
    .bind(provider)
    .fetch_all(&state.db)
    .await?)
}

/// External groups of `provider` mapped to a team.
pub async fn team_groups(state: &AppState, provider: &str, team_id: i64) -> ApiResult<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT external_group_id FROM external_group_mappings
          WHERE provider = $1 AND team_id = $2 ORDER BY id",
    )
    .bind(provider)
    .bind(team_id)
    .fetch_all(&state.db)
    .await?)
}

/// Replace the groups of `provider` mapped to a team.
pub async fn set_team_groups(
    tx: &mut Tx,
    provider: &str,
    team_id: i64,
    groups: &[String],
) -> ApiResult<()> {
    sqlx::query("DELETE FROM external_group_mappings WHERE provider = $1 AND team_id = $2")
        .bind(provider)
        .bind(team_id)
        .execute(&mut **tx)
        .await?;
    sqlx::query(
        "INSERT INTO external_group_mappings (provider, external_group_id, team_id)
         SELECT $1, g, $2 FROM unnest($3::text[]) g ON CONFLICT DO NOTHING",
    )
    .bind(provider)
    .bind(team_id)
    .bind(groups)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn add(tx: &mut Tx, team: &db::Team, user: &db::User, provider: &str) -> ApiResult<()> {
    let in_org: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM org_members WHERE org_id = $1 AND user_id = $2)",
    )
    .bind(team.org_id)
    .bind(user.id)
    .fetch_one(&mut **tx)
    .await?;
    if !in_org {
        let Some(org) = db::User::find(&mut **tx, team.org_id).await? else {
            return Ok(());
        };
        // Adds the team membership too.
        orgs::add_member(
            tx,
            &org,
            user,
            bgh_core::perms::OrgRole::Member,
            &[team.id],
            user.id,
        )
        .await?;
    } else {
        let inserted = sqlx::query(
            "INSERT INTO team_members (team_id, user_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
        )
        .bind(team.id)
        .bind(user.id)
        .execute(&mut **tx)
        .await?
        .rows_affected();
        if inserted == 0 {
            return Ok(());
        }
        teams::sync_team(tx, team.id, SyncAction::Update).await?;
        tx.emit(Event::TeamMemberAdded {
            org_id: team.org_id,
            team_id: team.id,
            user_id: user.id,
            actor_id: user.id,
        });
    }
    audit::log(
        &mut **tx,
        None,
        "team.add_member",
        audit::Target::Team {
            id: team.id,
            org_id: team.org_id,
        },
        json!({ "user": user.login, "role": "member", "group_sync": provider }),
    )
    .await?;
    Ok(())
}

async fn remove(tx: &mut Tx, team: &db::Team, user: &db::User, provider: &str) -> ApiResult<()> {
    let removed = sqlx::query("DELETE FROM team_members WHERE team_id = $1 AND user_id = $2")
        .bind(team.id)
        .bind(user.id)
        .execute(&mut **tx)
        .await?
        .rows_affected();
    if removed == 0 {
        return Ok(());
    }
    teams::sync_team(tx, team.id, SyncAction::Update).await?;
    audit::log(
        &mut **tx,
        None,
        "team.remove_member",
        audit::Target::Team {
            id: team.id,
            org_id: team.org_id,
        },
        json!({ "user": user.login, "group_sync": provider }),
    )
    .await?;
    tx.emit(Event::TeamMemberRemoved {
        org_id: team.org_id,
        team_id: team.id,
        user_id: user.id,
        actor_id: user.id,
    });
    Ok(())
}

async fn load_teams(state: &AppState, ids: &[i64]) -> ApiResult<Vec<db::Team>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {} FROM teams WHERE id = ANY($1) ORDER BY id",
        db::Team::COLUMNS
    ))
    .bind(ids)
    .fetch_all(&state.db)
    .await?)
}

/// Apply the groups `user` belongs to (external ids of `provider`) to the
/// teams mapped to `provider`: join the teams of their groups, leave the
/// other mapped teams. Returns `(added, removed)` team counts.
pub async fn apply_user_groups(
    state: &AppState,
    user: &db::User,
    provider: &str,
    groups: &HashSet<String>,
) -> ApiResult<(usize, usize)> {
    let maps = mappings(state, provider).await?;
    if maps.is_empty() {
        return Ok((0, 0));
    }
    let managed: BTreeSet<i64> = maps.iter().map(|(t, _)| *t).collect();
    let wanted: BTreeSet<i64> = maps
        .iter()
        .filter(|(_, g)| groups.contains(g))
        .map(|(t, _)| *t)
        .collect();
    let managed_ids: Vec<i64> = managed.iter().copied().collect();
    let current: BTreeSet<i64> = sqlx::query_scalar::<_, i64>(
        "SELECT team_id FROM team_members WHERE user_id = $1 AND team_id = ANY($2)",
    )
    .bind(user.id)
    .bind(&managed_ids)
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .collect();
    let to_add: Vec<i64> = wanted.difference(&current).copied().collect();
    let to_remove: Vec<i64> = current.difference(&wanted).copied().collect();
    if to_add.is_empty() && to_remove.is_empty() {
        return Ok((0, 0));
    }
    let all: Vec<i64> = to_add.iter().chain(&to_remove).copied().collect();
    let teams = load_teams(state, &all).await?;
    let mut tx = Tx::begin(state).await?;
    for team in &teams {
        if to_add.contains(&team.id) {
            add(&mut tx, team, user, provider).await?;
        } else {
            remove(&mut tx, team, user, provider).await?;
        }
    }
    tx.commit().await?;
    Ok((to_add.len(), to_remove.len()))
}

/// Make the team's members exactly `user_ids` (a managed team).
/// Returns `(added, removed)`.
pub async fn set_team_members(
    state: &AppState,
    team_id: i64,
    provider: &str,
    user_ids: &BTreeSet<i64>,
) -> ApiResult<(usize, usize)> {
    let Some(team) = load_teams(state, &[team_id]).await?.pop() else {
        return Ok((0, 0));
    };
    let current: BTreeSet<i64> =
        sqlx::query_scalar::<_, i64>("SELECT user_id FROM team_members WHERE team_id = $1")
            .bind(team_id)
            .fetch_all(&state.db)
            .await?
            .into_iter()
            .collect();
    sqlx::query(
        "UPDATE external_group_mappings SET synced_at = now() WHERE provider = $1 AND team_id = $2",
    )
    .bind(provider)
    .bind(team_id)
    .execute(&state.db)
    .await?;
    let to_add: Vec<i64> = user_ids.difference(&current).copied().collect();
    let to_remove: Vec<i64> = current.difference(user_ids).copied().collect();
    if to_add.is_empty() && to_remove.is_empty() {
        return Ok((0, 0));
    }
    let ids: Vec<i64> = to_add.iter().chain(&to_remove).copied().collect();
    let users: Vec<db::User> = sqlx::query_as(&format!(
        "SELECT {} FROM users WHERE id = ANY($1) ORDER BY id",
        db::User::COLUMNS
    ))
    .bind(&ids)
    .fetch_all(&state.db)
    .await?;
    let mut tx = Tx::begin(state).await?;
    for user in &users {
        if to_add.contains(&user.id) {
            add(&mut tx, &team, user, provider).await?;
        } else {
            remove(&mut tx, &team, user, provider).await?;
        }
    }
    tx.commit().await?;
    Ok((to_add.len(), to_remove.len()))
}

// ---------------------------------------------------------------------------
// GitHub team sync REST (IdP groups of the OIDC groups claim)
// ---------------------------------------------------------------------------

/// GitHub's `group-mapping` item.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GroupMapping {
    pub group_id: String,
    #[serde(default)]
    pub group_name: String,
    #[serde(default)]
    pub group_description: String,
}

#[derive(Debug, serde::Serialize)]
pub struct GroupMappings {
    pub groups: Vec<GroupMapping>,
}

#[derive(Debug, serde::Deserialize)]
pub struct GroupMappingsBody {
    pub groups: Option<Vec<GroupMapping>>,
}

fn mappings_json(groups: Vec<String>) -> GroupMappings {
    GroupMappings {
        groups: groups
            .into_iter()
            .map(|g| GroupMapping {
                group_name: g.clone(),
                group_id: g,
                group_description: String::new(),
            })
            .collect(),
    }
}

/// `GET /orgs/{org}/teams/{team_slug}/team-sync/group-mappings` (and the
/// legacy `/teams/{team_id}/...`): the IdP groups (values of the OIDC groups
/// claim) whose members are synced into the team. Org owners and team
/// maintainers (`read:org`).
pub async fn get_mappings(
    State(state): State<AppState>,
    tp: teams::TeamPath,
) -> ApiResult<Json<GroupMappings>> {
    let auth = tp.ctx.access.user()?;
    auth.require_scope("read:org")?;
    if !tp.ctx.can_manage() {
        return Err(ApiError::forbidden(
            "You must be an organization owner or team maintainer to do that.",
        ));
    }
    let groups = team_groups(&state, OIDC, tp.ctx.team.id).await?;
    Ok(Json(mappings_json(groups)))
}

/// `PATCH .../team-sync/group-mappings {groups: [{group_id, ...}]}`
/// (organization owners, `admin:org`): replaces the team's groups; an
/// empty list removes the sync.
pub async fn set_mappings(
    State(state): State<AppState>,
    tp: teams::TeamPath,
    Json(body): Json<GroupMappingsBody>,
) -> ApiResult<Json<GroupMappings>> {
    let auth = tp.ctx.access.user()?;
    auth.require_scope("admin:org")?;
    if !tp.ctx.access.is_admin() {
        return Err(ApiError::forbidden(
            "You must be an organization owner to do that.",
        ));
    }
    let Some(groups) = body.groups else {
        return Err(ApiError::invalid_field(FieldError::missing_field(
            "GroupMapping",
            "groups",
        )));
    };
    let mut ids: Vec<String> = Vec::new();
    for g in &groups {
        let id = g.group_id.trim();
        if id.is_empty() {
            return Err(ApiError::invalid_field(FieldError::missing_field(
                "GroupMapping",
                "group_id",
            )));
        }
        if !ids.iter().any(|x| x == id) {
            ids.push(id.to_string());
        }
    }
    let team = &tp.ctx.team;
    let mut tx = Tx::begin(&state).await?;
    for provider in IDP_PROVIDERS {
        set_team_groups(&mut tx, provider, team.id, &ids).await?;
    }
    audit::log(
        &mut *tx,
        Some(&auth.user),
        "team.update_group_mappings",
        audit::Target::Team {
            id: team.id,
            org_id: team.org_id,
        },
        json!({ "team": team.slug, "groups": ids }),
    )
    .await?;
    tx.commit().await?;
    // SCIM groups are known now: apply them right away.
    crate::scim::groups::sync_team(&state, team.id).await?;
    Ok(Json(mappings_json(ids)))
}
