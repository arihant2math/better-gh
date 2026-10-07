//! Organization-level payloads: `team`, `team_add`, `membership` and
//! `organization` `member_removed` / `member_invited`.
//!
//! These are delivered to the organization's hooks and to global hooks;
//! `team_add` (and `team` repository grants) also reach the repository's
//! hooks.

use bgh_core::AppState;
use bgh_core::models::{api, db};
use bgh_core::node_id::{self, NodeType};
use bgh_core::time::Timestamp;
use serde_json::{Map, Value, json};

use super::HookEvent;
use super::common::{RepoCtx, organization, sender, user_json};

/// Org events need the organization row; `None` when it is gone.
async fn org_row(state: &AppState, org_id: i64) -> anyhow::Result<Option<(db::User, Value)>> {
    let Some(org) = db::User::find(&state.db, org_id).await? else {
        return Ok(None);
    };
    let Some(json) = organization(state, org_id).await? else {
        return Ok(None);
    };
    Ok(Some((org, json)))
}

async fn team_row(state: &AppState, team_id: i64) -> anyhow::Result<Option<db::Team>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {} FROM teams WHERE id = $1",
        db::Team::COLUMNS
    ))
    .bind(team_id)
    .fetch_optional(&state.db)
    .await?)
}

/// Webhook `team` object: team-simple plus `parent`.
pub async fn team_object(state: &AppState, org_login: &str, t: &db::Team) -> anyhow::Result<Value> {
    let parent = match t.parent_id {
        Some(p) => team_row(state, p).await?,
        None => None,
    };
    Ok(serde_json::to_value(api::Team {
        team: api::TeamSimple::new(&state.urls, org_login, t),
        parent: parent.map(|p| api::TeamSimple::new(&state.urls, org_login, &p)),
    })?)
}

fn org_event(event: &'static str, action: Option<&str>, org_id: i64, payload: Value) -> HookEvent {
    HookEvent {
        event,
        action: action.map(str::to_string),
        repo_id: None,
        org_id: Some(org_id),
        payload,
    }
}

fn payload(action: Option<&str>, entries: Vec<(&str, Value)>) -> Map<String, Value> {
    let mut m = Map::new();
    if let Some(a) = action {
        m.insert("action".into(), json!(a));
    }
    for (k, v) in entries {
        m.insert(k.to_string(), v);
    }
    m
}

/// `team` created / edited / deleted. `snapshot` is the deleted team's
/// JSON from the event (the row is gone).
pub async fn team(
    state: &AppState,
    org_id: i64,
    team_id: i64,
    action: &str,
    changes: Option<&Value>,
    snapshot: Option<&Value>,
    actor_id: i64,
) -> anyhow::Result<Vec<HookEvent>> {
    let Some((org, org_json)) = org_row(state, org_id).await? else {
        return Ok(Vec::new());
    };
    let team = match snapshot {
        Some(t) if !t.is_null() => t.clone(),
        _ => match team_row(state, team_id).await? {
            Some(t) => team_object(state, &org.login, &t).await?,
            None => return Ok(Vec::new()),
        },
    };
    let mut entries = vec![("team", team)];
    if let Some(c) = changes {
        entries.push(("changes", c.clone()));
    }
    let mut m = payload(Some(action), entries);
    m.insert("organization".into(), org_json);
    m.insert("sender".into(), sender(state, Some(actor_id)).await?);
    Ok(vec![org_event(
        "team",
        Some(action),
        org_id,
        Value::Object(m),
    )])
}

/// `team` added_to_repository / removed_from_repository, plus `team_add`
/// (to the repository's hooks too) when added.
pub async fn team_repo(
    state: &AppState,
    org_id: i64,
    team_id: i64,
    repo_id: i64,
    added: bool,
    actor_id: i64,
) -> anyhow::Result<Vec<HookEvent>> {
    let Some((org, org_json)) = org_row(state, org_id).await? else {
        return Ok(Vec::new());
    };
    let Some(t) = team_row(state, team_id).await? else {
        return Ok(Vec::new());
    };
    let Some(ctx) = RepoCtx::load(state, repo_id).await? else {
        return Ok(Vec::new());
    };
    let team = team_object(state, &org.login, &t).await?;
    let mut repository = ctx.repository(&state.urls);
    if added {
        let permission: Option<String> = sqlx::query_scalar(
            "SELECT permission FROM team_repos WHERE team_id = $1 AND repo_id = $2",
        )
        .bind(team_id)
        .bind(repo_id)
        .fetch_optional(&state.db)
        .await?;
        if let Some(p) = permission
            .as_deref()
            .and_then(bgh_core::perms::Permission::parse)
        {
            repository["permissions"] = permissions_json(p);
            repository["role_name"] = json!(p.as_str());
        }
    }
    let sender = sender(state, Some(actor_id)).await?;
    let action = if added {
        "added_to_repository"
    } else {
        "removed_from_repository"
    };
    let mut m = payload(
        Some(action),
        vec![("team", team.clone()), ("repository", repository.clone())],
    );
    m.insert("organization".into(), org_json.clone());
    m.insert("sender".into(), sender.clone());
    let mut out = vec![org_event("team", Some(action), org_id, Value::Object(m))];
    if added {
        let mut m = payload(None, vec![("team", team), ("repository", repository)]);
        m.insert("organization".into(), org_json);
        m.insert("sender".into(), sender);
        out.push(HookEvent {
            event: "team_add",
            action: None,
            repo_id: Some(repo_id),
            org_id: Some(org_id),
            payload: Value::Object(m),
        });
    }
    Ok(out)
}

/// `{"admin", "maintain", "push", "triage", "pull"}` booleans for a role.
fn permissions_json(p: bgh_core::perms::Permission) -> Value {
    use bgh_core::perms::Permission as P;
    json!({
        "admin": p >= P::Admin,
        "maintain": p >= P::Maintain,
        "push": p >= P::Write,
        "triage": p >= P::Triage,
        "pull": p >= P::Read,
    })
}

/// `membership` added / removed (team membership).
pub async fn membership(
    state: &AppState,
    org_id: i64,
    team_id: i64,
    user_id: i64,
    added: bool,
    actor_id: i64,
) -> anyhow::Result<Vec<HookEvent>> {
    let Some((org, org_json)) = org_row(state, org_id).await? else {
        return Ok(Vec::new());
    };
    let Some(t) = team_row(state, team_id).await? else {
        return Ok(Vec::new());
    };
    let action = if added { "added" } else { "removed" };
    let mut m = payload(
        Some(action),
        vec![
            ("scope", json!("team")),
            ("member", sender(state, Some(user_id)).await?),
            ("team", team_object(state, &org.login, &t).await?),
        ],
    );
    m.insert("organization".into(), org_json);
    m.insert("sender".into(), sender(state, Some(actor_id)).await?);
    Ok(vec![org_event(
        "membership",
        Some(action),
        org_id,
        Value::Object(m),
    )])
}

/// `organization` `member_removed` (the membership row is gone: role is
/// reported as `member`).
pub async fn member_removed(
    state: &AppState,
    org_id: i64,
    user_id: i64,
    actor_id: i64,
) -> anyhow::Result<Vec<HookEvent>> {
    let Some((org, org_json)) = org_row(state, org_id).await? else {
        return Ok(Vec::new());
    };
    let Some(member) = db::User::find(&state.db, user_id).await? else {
        return Ok(Vec::new());
    };
    let urls = &state.urls;
    let org_url = urls.org(&org.login);
    let m = payload(
        Some("member_removed"),
        vec![
            (
                "membership",
                json!({
                    "url": format!("{org_url}/memberships/{}", member.login),
                    "state": "active",
                    "role": "member",
                    "organization_url": org_url,
                    "user": user_json(urls, &member),
                }),
            ),
            ("organization", org_json),
            ("sender", sender(state, Some(actor_id)).await?),
        ],
    );
    Ok(vec![org_event(
        "organization",
        Some("member_removed"),
        org_id,
        Value::Object(m),
    )])
}

#[derive(sqlx::FromRow)]
struct InvitationRow {
    id: i64,
    invitee_id: Option<i64>,
    email: Option<String>,
    inviter_id: Option<i64>,
    role: String,
    team_ids: Vec<i64>,
    created_at: chrono::DateTime<chrono::Utc>,
    failed_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// `organization` `member_invited`.
pub async fn member_invited(
    state: &AppState,
    org_id: i64,
    invitation_id: i64,
    actor_id: i64,
) -> anyhow::Result<Vec<HookEvent>> {
    let Some((org, org_json)) = org_row(state, org_id).await? else {
        return Ok(Vec::new());
    };
    let inv: Option<InvitationRow> = sqlx::query_as(
        "SELECT id, invitee_id, email, inviter_id, role, team_ids, created_at, failed_at
           FROM org_invitations WHERE id = $1 AND org_id = $2",
    )
    .bind(invitation_id)
    .bind(org_id)
    .fetch_optional(&state.db)
    .await?;
    let Some(inv) = inv else {
        return Ok(Vec::new());
    };
    let invitee = match inv.invitee_id {
        Some(id) => db::User::find(&state.db, id).await?,
        None => None,
    };
    let urls = &state.urls;
    let invitation = json!({
        "id": inv.id,
        "node_id": node_id::encode(NodeType::OrganizationInvitation, inv.id),
        "login": invitee.as_ref().map(|u| u.login.clone()),
        "email": inv.email,
        "role": inv.role,
        "created_at": Timestamp(inv.created_at),
        "failed_at": inv.failed_at.map(Timestamp),
        "failed_reason": null,
        "inviter": sender(state, inv.inviter_id).await?,
        "team_count": inv.team_ids.len(),
        "invitation_teams_url": format!("{}/invitations/{}/teams", urls.org(&org.login), inv.id),
        "invitation_source": "member",
    });
    let mut entries = vec![("invitation", invitation)];
    if let Some(u) = &invitee {
        entries.push(("user", user_json(urls, u)));
    }
    let mut m = payload(Some("member_invited"), entries);
    m.insert("organization".into(), org_json);
    m.insert("sender".into(), sender(state, Some(actor_id)).await?);
    Ok(vec![org_event(
        "organization",
        Some("member_invited"),
        org_id,
        Value::Object(m),
    )])
}
