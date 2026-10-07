//! GitHub App payloads (P46): `installation` and `installation_repositories`
//! (delivered only to the app's own hook), and the `installation` object
//! added to every app hook delivery.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use bgh_core::AppState;
use bgh_core::apps::{AppRow, Installation, InstallationRow};
use bgh_core::models::db;
use bgh_core::node_id::{self, NodeType};
use serde_json::{Value, json};

use super::HookEvent;
use super::common::sender;

/// `node_id` of an installation (`IntegrationInstallation`).
pub fn installation_node_id(id: i64) -> String {
    STANDARD.encode(format!("023:IntegrationInstallation{id}"))
}

/// The `installation` object of app hook deliveries.
pub fn installation_ref(id: i64) -> Value {
    json!({ "id": id, "node_id": installation_node_id(id) })
}

/// Repositories in installation payloads: `{id, node_id, name, full_name,
/// private}` (one query, ordered by id).
pub async fn repository_list(state: &AppState, repo_ids: &[i64]) -> anyhow::Result<Vec<Value>> {
    if repo_ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows: Vec<(i64, String, String, String)> = sqlx::query_as(
        "SELECT r.id, r.name, u.login, r.visibility FROM repositories r
           JOIN users u ON u.id = r.owner_id
          WHERE r.id = ANY($1) ORDER BY r.id",
    )
    .bind(repo_ids)
    .fetch_all(&state.db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, name, owner, visibility)| {
            json!({
                "id": id,
                "node_id": node_id::encode(NodeType::Repository, id),
                "name": name,
                "full_name": format!("{owner}/{name}"),
                "private": visibility != "public",
            })
        })
        .collect())
}

/// Every repository an installation covers (the account's repositories for
/// `all`).
pub async fn installation_repo_ids(
    state: &AppState,
    inst: &InstallationRow,
) -> anyhow::Result<Vec<i64>> {
    Ok(sqlx::query_scalar(
        "SELECT r.id FROM repositories r
          WHERE ($2 AND r.owner_id = $1)
             OR (NOT $2 AND r.id IN (SELECT repo_id FROM app_installation_repos
                                      WHERE installation_id = $3))
          ORDER BY r.id",
    )
    .bind(inst.account_id)
    .bind(inst.all_repositories())
    .bind(inst.id)
    .fetch_all(&state.db)
    .await?)
}

/// GitHub's `installation` JSON for a live installation (`None` if gone).
pub async fn installation_json(
    state: &AppState,
    installation_id: i64,
) -> anyhow::Result<Option<(InstallationRow, Value)>> {
    let inst: Option<InstallationRow> = sqlx::query_as(&format!(
        "SELECT {} FROM app_installations WHERE id = $1",
        InstallationRow::COLUMNS
    ))
    .bind(installation_id)
    .fetch_optional(&state.db)
    .await?;
    let Some(inst) = inst else { return Ok(None) };
    let app: Option<AppRow> = sqlx::query_as(&format!(
        "SELECT {} FROM github_apps WHERE id = $1",
        AppRow::COLUMNS
    ))
    .bind(inst.app_id)
    .fetch_optional(&state.db)
    .await?;
    let Some(app) = app else { return Ok(None) };
    let Some(account) = db::User::find(&state.db, inst.account_id).await? else {
        return Ok(None);
    };
    let suspended_by = match inst.suspended_by_id {
        Some(id) => db::User::find(&state.db, id).await?,
        None => None,
    };
    let v = serde_json::to_value(Installation::new(
        &state.urls,
        &inst,
        &app,
        &account,
        suspended_by.as_ref(),
    ))?;
    Ok(Some((inst, v)))
}

/// `installation` event. `snapshot` / `repos_snapshot` describe a deleted
/// installation.
pub async fn installation(
    state: &AppState,
    installation_id: i64,
    action: &str,
    actor_id: i64,
    snapshot: &Value,
    repos_snapshot: &Value,
) -> anyhow::Result<Vec<HookEvent>> {
    let (installation, repositories) = if snapshot.is_object() {
        (snapshot.clone(), repos_snapshot.clone())
    } else {
        let Some((inst, v)) = installation_json(state, installation_id).await? else {
            return Ok(Vec::new());
        };
        let ids = installation_repo_ids(state, &inst).await?;
        (v, json!(repository_list(state, &ids).await?))
    };
    let payload = json!({
        "action": action,
        "installation": installation,
        "repositories": repositories,
        "requester": null,
        "sender": sender(state, Some(actor_id)).await?,
    });
    Ok(vec![HookEvent {
        event: "installation",
        action: Some(action.to_string()),
        repo_id: None,
        org_id: None,
        payload,
    }])
}

/// `installation_repositories` events: one `added` and/or one `removed`.
pub async fn installation_repositories(
    state: &AppState,
    installation_id: i64,
    actor_id: i64,
    selection: &str,
    added: &[i64],
    removed: &[i64],
) -> anyhow::Result<Vec<HookEvent>> {
    let Some((_, installation)) = installation_json(state, installation_id).await? else {
        return Ok(Vec::new());
    };
    let sender = sender(state, Some(actor_id)).await?;
    let mut out = Vec::new();
    for (action, ids) in [("added", added), ("removed", removed)] {
        if ids.is_empty() {
            continue;
        }
        let repos = repository_list(state, ids).await?;
        let (a, r) = if action == "added" {
            (repos, Vec::new())
        } else {
            (Vec::new(), repos)
        };
        out.push(HookEvent {
            event: "installation_repositories",
            action: Some(action.to_string()),
            repo_id: None,
            org_id: None,
            payload: json!({
                "action": action,
                "installation": installation,
                "repository_selection": selection,
                "repositories_added": a,
                "repositories_removed": r,
                "requester": null,
                "sender": sender,
            }),
        });
    }
    Ok(out)
}
