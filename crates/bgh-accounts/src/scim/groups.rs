//! SCIM `Groups` (enterprise) and the teams that follow them.

use std::collections::BTreeSet;

use axum::extract::State;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bgh_core::audit;
use bgh_core::prelude::*;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use uuid::Uuid;

use super::{
    GROUP_SCHEMA, ListQuery, PROVIDER, ScimError, ScimResult, Tenant, enterprise_auth,
    list_response, patch_operations, scim_json,
};
use crate::group_sync;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ScimGroupRow {
    pub id: Uuid,
    pub external_id: Option<String>,
    pub display_name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

const COLUMNS: &str = "id, external_id, display_name, created_at, updated_at";

/// `(scim user id, userName)` members of each group.
async fn members(state: &AppState, ids: &[Uuid]) -> ScimResult<Vec<(Uuid, Uuid, String)>> {
    Ok(sqlx::query_as(
        "SELECT m.group_id, u.id, u.user_name FROM scim_group_members m
           JOIN scim_users u ON u.id = m.scim_user_id
          WHERE m.group_id = ANY($1) ORDER BY u.created_at, u.id",
    )
    .bind(ids)
    .fetch_all(&state.db)
    .await?)
}

fn resource(base: &str, g: &ScimGroupRow, members: &[(Uuid, Uuid, String)]) -> Value {
    json!({
        "schemas": [GROUP_SCHEMA],
        "id": g.id.to_string(),
        "externalId": g.external_id,
        "displayName": g.display_name,
        "members": members
            .iter()
            .filter(|(gid, _, _)| *gid == g.id)
            .map(|(_, uid, name)| json!({
                "value": uid.to_string(),
                "$ref": format!("{base}/Users/{uid}"),
                "display": name,
            }))
            .collect::<Vec<_>>(),
        "meta": {
            "resourceType": "Group",
            "created": Timestamp::from(g.created_at),
            "lastModified": Timestamp::from(g.updated_at),
            "location": format!("{base}/Groups/{}", g.id),
        },
    })
}

async fn load(state: &AppState, id: &str) -> ScimResult<ScimGroupRow> {
    let id = Uuid::parse_str(id).map_err(|_| ScimError::not_found())?;
    sqlx::query_as::<_, ScimGroupRow>(&format!("SELECT {COLUMNS} FROM scim_groups WHERE id = $1"))
        .bind(id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(ScimError::not_found)
}

async fn render(state: &AppState, base: &str, g: &ScimGroupRow) -> ScimResult<Value> {
    let m = members(state, &[g.id]).await?;
    Ok(resource(base, g, &m))
}

/// SCIM user ids from `[{value}]` (enterprise users only).
async fn member_ids(state: &AppState, v: &Value) -> ScimResult<Vec<Uuid>> {
    let items: Vec<&Value> = match v {
        Value::Array(a) => a.iter().collect(),
        Value::Object(_) => vec![v],
        Value::Null => Vec::new(),
        _ => return Err(ScimError::bad("invalidValue", "members must be a list")),
    };
    let mut ids = Vec::new();
    for item in items {
        let id = item["value"]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok())
            .ok_or_else(|| ScimError::bad("invalidValue", "member value must be a SCIM user id"))?;
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    let known: Vec<Uuid> =
        sqlx::query_scalar("SELECT id FROM scim_users WHERE id = ANY($1) AND org_id IS NULL")
            .bind(&ids)
            .fetch_all(&state.db)
            .await?;
    if let Some(missing) = ids.iter().find(|i| !known.contains(i)) {
        return Err(ScimError::bad(
            "invalidValue",
            format!("Unknown member {missing}"),
        ));
    }
    Ok(ids)
}

/// Group identifiers teams can be mapped by.
#[derive(Debug, Clone)]
pub struct GroupKey {
    pub id: Uuid,
    pub display_name: String,
    pub external_id: Option<String>,
}

impl From<&ScimGroupRow> for GroupKey {
    fn from(g: &ScimGroupRow) -> Self {
        Self {
            id: g.id,
            display_name: g.display_name.clone(),
            external_id: g.external_id.clone(),
        }
    }
}

/// The groups a SCIM user belongs to.
pub async fn groups_of(state: &AppState, scim_user_id: Uuid) -> ApiResult<Vec<GroupKey>> {
    let rows: Vec<ScimGroupRow> = sqlx::query_as(&format!(
        "SELECT {} FROM scim_groups g JOIN scim_group_members m ON m.group_id = g.id
          WHERE m.scim_user_id = $1",
        db::prefixed("g", COLUMNS)
    ))
    .bind(scim_user_id)
    .fetch_all(&state.db)
    .await?;
    Ok(rows.iter().map(GroupKey::from).collect())
}

/// Re-sync the teams of a user's groups (after (de)activation).
pub async fn sync_user_teams(state: &AppState, scim_user_id: Uuid) -> ApiResult<()> {
    let groups = groups_of(state, scim_user_id).await?;
    sync_teams(state, &groups).await
}

/// Teams mapped (provider `scim`) to a group, by display name
/// (case-insensitive), id or externalId.
const MAPPING_MATCH: &str =
    "m.provider = 'scim' AND (lower(m.external_group_id) = lower(g.display_name)
       OR m.external_group_id = g.id::text OR m.external_group_id = g.external_id)";

/// Set the members of every team mapped to one of `groups`: the active,
/// unsuspended accounts of all SCIM groups mapped to the team.
pub async fn sync_teams(state: &AppState, groups: &[GroupKey]) -> ApiResult<()> {
    if groups.is_empty() {
        return Ok(());
    }
    let names: Vec<String> = groups
        .iter()
        .map(|g| g.display_name.to_lowercase())
        .collect();
    let ids: Vec<String> = groups.iter().map(|g| g.id.to_string()).collect();
    let ext: Vec<String> = groups
        .iter()
        .filter_map(|g| g.external_id.clone())
        .collect();
    let teams: Vec<i64> = sqlx::query_scalar(
        "SELECT DISTINCT team_id FROM external_group_mappings
          WHERE provider = $1 AND (lower(external_group_id) = ANY($2)
                OR external_group_id = ANY($3) OR external_group_id = ANY($4))",
    )
    .bind(PROVIDER)
    .bind(&names)
    .bind(&ids)
    .bind(&ext)
    .fetch_all(&state.db)
    .await?;
    for team_id in teams {
        sync_team(state, team_id).await?;
    }
    Ok(())
}

/// Set a mapped team's members from its SCIM groups (no-op when no SCIM
/// group matches its mappings, so teams mapped for other providers keep
/// their members).
pub async fn sync_team(state: &AppState, team_id: i64) -> ApiResult<()> {
    let matched: bool = sqlx::query_scalar(&format!(
        "SELECT EXISTS (SELECT 1 FROM external_group_mappings m, scim_groups g
                         WHERE m.team_id = $1 AND {MAPPING_MATCH})"
    ))
    .bind(team_id)
    .fetch_one(&state.db)
    .await?;
    if !matched {
        return Ok(());
    }
    let users: BTreeSet<i64> = sqlx::query_scalar::<_, i64>(&format!(
        "SELECT DISTINCT su.user_id
           FROM external_group_mappings m
           JOIN scim_groups g ON {MAPPING_MATCH}
           JOIN scim_group_members gm ON gm.group_id = g.id
           JOIN scim_users su ON su.id = gm.scim_user_id
           JOIN users u ON u.id = su.user_id
          WHERE m.team_id = $1 AND su.active AND u.suspended_at IS NULL"
    ))
    .bind(team_id)
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .collect();
    group_sync::set_team_members(state, team_id, PROVIDER, &users).await?;
    Ok(())
}

async fn set_members(state: &AppState, group: Uuid, ids: &[Uuid]) -> ScimResult<()> {
    let mut tx = state.db.begin().await?;
    sqlx::query(
        "DELETE FROM scim_group_members WHERE group_id = $1 AND NOT (scim_user_id = ANY($2))",
    )
    .bind(group)
    .bind(ids)
    .execute(&mut *tx)
    .await?;
    add_members(&mut tx, group, ids).await?;
    tx.commit().await?;
    Ok(())
}

async fn add_members(
    tx: &mut sqlx::PgConnection,
    group: Uuid,
    ids: &[Uuid],
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO scim_group_members (group_id, scim_user_id)
         SELECT $1, u FROM unnest($2::uuid[]) u ON CONFLICT DO NOTHING",
    )
    .bind(group)
    .bind(ids)
    .execute(tx)
    .await?;
    Ok(())
}

fn display_name(v: &Value) -> ScimResult<String> {
    v.as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty() && s.len() <= 255)
        .map(str::to_string)
        .ok_or_else(|| ScimError::bad("invalidValue", "displayName is required"))
}

fn unique(e: sqlx::Error) -> ScimError {
    match bgh_core::db::unique_violation(&e).as_deref() {
        Some(_) => ScimError::conflict("A group with this displayName already exists"),
        None => e.into(),
    }
}

fn body_value(body: &axum::body::Bytes) -> ScimResult<Value> {
    serde_json::from_slice(body)
        .map_err(|_| ScimError::bad("invalidSyntax", "Problems parsing JSON"))
}

/// `members[value eq "id"]` → the id.
fn member_filter(path: &str) -> Option<String> {
    let lower = path.to_ascii_lowercase();
    let rest = lower.strip_prefix("members[value eq \"")?;
    let end = rest.find("\"]")?;
    Some(path[18..18 + end].to_string())
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `GET /scim/v2/enterprises/{enterprise}/Groups`
pub async fn list(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path(enterprise): Path<String>,
    Query(q): Query<ListQuery>,
) -> ScimResult<Response> {
    enterprise_auth(&state, &auth).await?;
    let base = Tenant::Enterprise.base(&state, &enterprise);
    let mut conds = vec!["true".to_string()];
    let mut binds = Vec::new();
    for (attr, value) in q.filter()? {
        let n = binds.len() + 1;
        conds.push(match attr.as_str() {
            "displayname" => format!("lower(display_name) = lower(${n})"),
            "externalid" => format!("external_id = ${n}"),
            "id" => format!("id::text = ${n}"),
            _ => {
                return Err(ScimError::bad(
                    "invalidFilter",
                    format!("Unsupported filter attribute {attr:?}"),
                ));
            }
        });
        binds.push(value);
    }
    let where_ = conds.join(" AND ");
    let count_sql = format!("SELECT count(*) FROM scim_groups WHERE {where_}");
    let mut count = sqlx::query_scalar::<_, i64>(&count_sql);
    for b in &binds {
        count = count.bind(b);
    }
    let total = count.fetch_one(&state.db).await?;
    let n = binds.len() + 1;
    let rows_sql = format!(
        "SELECT {COLUMNS} FROM scim_groups WHERE {where_} ORDER BY created_at, id LIMIT ${n} OFFSET ${}",
        n + 1
    );
    let mut rows = sqlx::query_as::<_, ScimGroupRow>(&rows_sql);
    for b in &binds {
        rows = rows.bind(b);
    }
    let rows = rows
        .bind(q.limit())
        .bind(q.offset())
        .fetch_all(&state.db)
        .await?;
    let ids: Vec<Uuid> = rows.iter().map(|g| g.id).collect();
    let m = members(&state, &ids).await?;
    Ok(list_response(
        total,
        q.offset(),
        rows.iter().map(|g| resource(&base, g, &m)).collect(),
    ))
}

/// `POST /scim/v2/enterprises/{enterprise}/Groups`
pub async fn create(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path(enterprise): Path<String>,
    body: axum::body::Bytes,
) -> ScimResult<Response> {
    let a = enterprise_auth(&state, &auth).await?;
    let base = Tenant::Enterprise.base(&state, &enterprise);
    let v = body_value(&body)?;
    let name = display_name(&v["displayName"])?;
    let external_id = v["externalId"].as_str().map(str::to_string);
    let ids = member_ids(&state, &v["members"]).await?;
    let mut tx = state.db.begin().await?;
    let g: ScimGroupRow = sqlx::query_as(&format!(
        "INSERT INTO scim_groups (id, external_id, display_name) VALUES ($1, $2, $3) RETURNING {COLUMNS}"
    ))
    .bind(Uuid::new_v4())
    .bind(&external_id)
    .bind(&name)
    .fetch_one(&mut *tx)
    .await
    .map_err(unique)?;
    add_members(&mut tx, g.id, &ids).await?;
    tx.commit().await?;
    audit::log(
        &state.db,
        Some(&a.user),
        "scim.group_create",
        audit::Target::Site,
        json!({ "group": name, "members": ids.len() }),
    )
    .await?;
    sync_teams(&state, &[GroupKey::from(&g)]).await?;
    let mut resp = scim_json(StatusCode::CREATED, render(&state, &base, &g).await?);
    if let Ok(loc) = HeaderValue::from_str(&format!("{base}/Groups/{}", g.id)) {
        resp.headers_mut().insert(header::LOCATION, loc);
    }
    Ok(resp)
}

/// `GET /scim/v2/enterprises/{enterprise}/Groups/{scim_group_id}`
pub async fn get(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((enterprise, id)): Path<(String, String)>,
) -> ScimResult<Response> {
    enterprise_auth(&state, &auth).await?;
    let base = Tenant::Enterprise.base(&state, &enterprise);
    let g = load(&state, &id).await?;
    Ok(scim_json(StatusCode::OK, render(&state, &base, &g).await?))
}

async fn rename(
    state: &AppState,
    g: &ScimGroupRow,
    name: &str,
    external_id: Option<String>,
) -> ScimResult<()> {
    sqlx::query(
        "UPDATE scim_groups SET display_name = $2, external_id = $3, updated_at = now() WHERE id = $1",
    )
    .bind(g.id)
    .bind(name)
    .bind(external_id)
    .execute(&state.db)
    .await
    .map_err(unique)?;
    Ok(())
}

/// `PUT /scim/v2/enterprises/{enterprise}/Groups/{scim_group_id}`
pub async fn put(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((enterprise, id)): Path<(String, String)>,
    body: axum::body::Bytes,
) -> ScimResult<Response> {
    enterprise_auth(&state, &auth).await?;
    let base = Tenant::Enterprise.base(&state, &enterprise);
    let g = load(&state, &id).await?;
    let v = body_value(&body)?;
    let name = display_name(&v["displayName"])?;
    let ids = member_ids(&state, &v["members"]).await?;
    let before = GroupKey::from(&g);
    rename(
        &state,
        &g,
        &name,
        v["externalId"].as_str().map(str::to_string),
    )
    .await?;
    set_members(&state, g.id, &ids).await?;
    let g = load(&state, &id).await?;
    sync_teams(&state, &[before, GroupKey::from(&g)]).await?;
    Ok(scim_json(StatusCode::OK, render(&state, &base, &g).await?))
}

/// `PATCH /scim/v2/enterprises/{enterprise}/Groups/{scim_group_id}`:
/// `add` / `remove` / `replace` of `members` (also
/// `members[value eq "id"]`), `displayName` and `externalId`.
pub async fn patch(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((enterprise, id)): Path<(String, String)>,
    body: axum::body::Bytes,
) -> ScimResult<Response> {
    enterprise_auth(&state, &auth).await?;
    let base = Tenant::Enterprise.base(&state, &enterprise);
    let g = load(&state, &id).await?;
    let before = GroupKey::from(&g);
    let ops = patch_operations(&body_value(&body)?)?;
    let mut name = g.display_name.clone();
    let mut external_id = g.external_id.clone();
    let mut current: Vec<Uuid> = members(&state, &[g.id])
        .await?
        .into_iter()
        .map(|(_, u, _)| u)
        .collect();
    for o in &ops {
        let path = o.path.as_deref().map(str::to_ascii_lowercase);
        match (path.as_deref(), o.op.as_str()) {
            (Some("members"), "add") => {
                for id in member_ids(&state, &o.value).await? {
                    if !current.contains(&id) {
                        current.push(id);
                    }
                }
            }
            (Some("members"), "replace") => current = member_ids(&state, &o.value).await?,
            (Some("members"), "remove") => {
                if o.value.is_null() {
                    current.clear();
                } else {
                    let gone = member_ids(&state, &o.value).await.unwrap_or_default();
                    // Unknown ids are already gone.
                    let raw: Vec<String> = match &o.value {
                        Value::Array(a) => a
                            .iter()
                            .filter_map(|x| x["value"].as_str().map(str::to_string))
                            .collect(),
                        v => v["value"]
                            .as_str()
                            .map(str::to_string)
                            .into_iter()
                            .collect(),
                    };
                    current.retain(|c| !gone.contains(c) && !raw.contains(&c.to_string()));
                }
            }
            (Some(p), "remove") if p.starts_with("members[") => {
                let target =
                    o.path.as_deref().and_then(member_filter).ok_or_else(|| {
                        ScimError::bad("invalidPath", "Unsupported members filter")
                    })?;
                current.retain(|c| c.to_string() != target);
            }
            (Some("displayname"), "add" | "replace") => name = display_name(&o.value)?,
            (Some("externalid"), "add" | "replace") => {
                external_id = o.value.as_str().map(str::to_string)
            }
            (Some("externalid"), "remove") => external_id = None,
            (None, "add" | "replace") => {
                let obj = o.value.as_object().ok_or_else(|| {
                    ScimError::bad("invalidValue", "value must be an object without a path")
                })?;
                for (k, v) in obj {
                    match k.to_ascii_lowercase().as_str() {
                        "displayname" => name = display_name(v)?,
                        "externalid" => external_id = v.as_str().map(str::to_string),
                        "members" if o.op == "replace" => current = member_ids(&state, v).await?,
                        "members" => {
                            for id in member_ids(&state, v).await? {
                                if !current.contains(&id) {
                                    current.push(id);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {
                return Err(ScimError::bad(
                    "invalidPath",
                    format!("Unsupported operation {} {:?}", o.op, o.path),
                ));
            }
        }
    }
    if name != g.display_name || external_id != g.external_id {
        rename(&state, &g, &name, external_id).await?;
    }
    set_members(&state, g.id, &current).await?;
    sqlx::query("UPDATE scim_groups SET updated_at = now() WHERE id = $1")
        .bind(g.id)
        .execute(&state.db)
        .await?;
    let g = load(&state, &id).await?;
    sync_teams(&state, &[before, GroupKey::from(&g)]).await?;
    Ok(scim_json(StatusCode::OK, render(&state, &base, &g).await?))
}

/// `DELETE /scim/v2/enterprises/{enterprise}/Groups/{scim_group_id}`:
/// mapped teams lose the group's members.
pub async fn delete(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((_enterprise, id)): Path<(String, String)>,
) -> ScimResult<Response> {
    let a = enterprise_auth(&state, &auth).await?;
    let g = load(&state, &id).await?;
    // Teams matched only by this group: empty them before it disappears.
    let teams: Vec<i64> = sqlx::query_scalar(&format!(
        "SELECT DISTINCT m.team_id FROM external_group_mappings m, scim_groups g
          WHERE g.id = $1 AND {MAPPING_MATCH}"
    ))
    .bind(g.id)
    .fetch_all(&state.db)
    .await?;
    sqlx::query("DELETE FROM scim_groups WHERE id = $1")
        .bind(g.id)
        .execute(&state.db)
        .await?;
    for team_id in teams {
        let still: bool = sqlx::query_scalar(&format!(
            "SELECT EXISTS (SELECT 1 FROM external_group_mappings m, scim_groups g
                             WHERE m.team_id = $1 AND {MAPPING_MATCH})"
        ))
        .bind(team_id)
        .fetch_one(&state.db)
        .await?;
        if still {
            sync_team(&state, team_id).await?;
        } else {
            group_sync::set_team_members(&state, team_id, PROVIDER, &BTreeSet::new()).await?;
        }
    }
    audit::log(
        &state.db,
        Some(&a.user),
        "scim.group_delete",
        audit::Target::Site,
        json!({ "group": g.display_name }),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn member_filters() {
        assert_eq!(
            member_filter(r#"members[value eq "AbC-1"]"#).as_deref(),
            Some("AbC-1")
        );
        assert_eq!(member_filter("members"), None);
    }
}
