//! Workflows-lite: configuration endpoint and the domain event listener.

use std::sync::Arc;

use axum::extract::State;
use bgh_core::prelude::*;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::access::{ProjectAccess, Role};
use crate::filter::{self, Candidate};
use crate::model::*;
use crate::service;

#[derive(Debug, Deserialize)]
pub struct WorkflowBody {
    pub enabled: Option<bool>,
    pub config: Option<Value>,
}

fn invalid(field: &str, msg: &str) -> ApiError {
    ApiError::invalid_field(FieldError::custom("ProjectV2Workflow", field, msg))
}

/// `PUT /_bgh/projects/{id}/workflows/{kind}`
pub async fn put(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((id, kind)): Path<(i64, String)>,
    Json(body): Json<WorkflowBody>,
) -> ApiResult<Json<Value>> {
    let access = ProjectAccess::load(&state, Some(&auth), id).await?;
    access.require(Role::Write)?;
    if !WORKFLOW_KINDS.contains(&kind.as_str()) {
        return Err(ApiError::NotFound);
    }
    let mut tx = Tx::begin(&state).await?;
    let current: Option<WorkflowRow> = sqlx::query_as(&format!(
        "SELECT {} FROM project_workflows WHERE project_id = $1 AND kind = $2 FOR UPDATE",
        WorkflowRow::COLUMNS
    ))
    .bind(id)
    .bind(&kind)
    .fetch_optional(&mut *tx)
    .await?;
    let mut config = body
        .config
        .or_else(|| current.as_ref().map(|c| c.config.clone()))
        .unwrap_or_else(|| json!({}));
    let config_obj = config
        .as_object_mut()
        .ok_or_else(|| invalid("config", "config must be an object"))?;
    let mut repo_ids: Vec<i64> = Vec::new();
    match kind.as_str() {
        "auto_add" => {
            let ids = config_obj.get("repoIds").cloned().unwrap_or(json!([]));
            repo_ids = serde_json::from_value(ids)
                .map_err(|_| invalid("config", "repoIds must be an array of repository ids"))?;
            repo_ids.sort_unstable();
            repo_ids.dedup();
            if repo_ids.len() > 20 {
                return Err(invalid("config", "at most 20 repositories"));
            }
            // Only repositories the caller can read may be watched.
            let readable = crate::compact::readable_repos(&state, Some(&auth), &repo_ids).await?;
            if readable.len() != repo_ids.len() {
                return Err(invalid("config", "unknown repository"));
            }
            let filter = config_obj.get("filter").cloned().unwrap_or(json!(""));
            match filter.as_str() {
                Some(f) if f.chars().count() <= 1024 => {}
                _ => return Err(invalid("config", "filter must be a string")),
            }
            config_obj.insert("repoIds".into(), json!(repo_ids));
            config_obj.insert("filter".into(), filter);
        }
        "item_added" | "item_reopened" | "item_closed" | "pr_merged" => {
            if let Some(opt) = config_obj.get("statusOptionId") {
                let opt = opt
                    .as_str()
                    .ok_or_else(|| invalid("config", "statusOptionId must be a string"))?;
                let status = service::status_field(&mut tx, id).await?;
                if !status.is_some_and(|f| f.option_ids().iter().any(|o| o == opt)) {
                    return Err(invalid("config", "unknown status option"));
                }
            }
        }
        _ => {}
    }
    let enabled = body
        .enabled
        .or(current.as_ref().map(|c| c.enabled))
        .unwrap_or(false);
    let wf: WorkflowRow = sqlx::query_as(&format!(
        "INSERT INTO project_workflows (project_id, kind, enabled, config, repo_ids)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (project_id, kind) DO UPDATE SET enabled = EXCLUDED.enabled,
             config = EXCLUDED.config, repo_ids = EXCLUDED.repo_ids, updated_at = now()
         RETURNING {}",
        WorkflowRow::COLUMNS
    ))
    .bind(id)
    .bind(&kind)
    .bind(enabled)
    .bind(&config)
    .bind(&repo_ids)
    .fetch_one(&mut *tx)
    .await?;
    let action = if current.is_some() {
        SyncAction::Update
    } else {
        SyncAction::Insert
    };
    tx.sync(&access.scope(), M_WORKFLOW, wf.id, action, &wf.sync_json())
        .await?;
    tx.commit().await?;
    Ok(Json(wf.sync_json()))
}

/// What happened to an issue / PR.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Change {
    Opened,
    Edited,
    Closed,
    Merged,
    Reopened,
}

/// Event listener `projects.workflows`.
pub async fn on_event(state: AppState, event: Arc<Event>) -> anyhow::Result<()> {
    let (repo_id, issue_id, change) = match &*event {
        Event::IssueOpened {
            repo_id, issue_id, ..
        } => (*repo_id, *issue_id, Change::Opened),
        Event::IssueEdited {
            repo_id, issue_id, ..
        } => (*repo_id, *issue_id, Change::Edited),
        Event::IssueClosed {
            repo_id, issue_id, ..
        } => (*repo_id, *issue_id, Change::Closed),
        Event::IssueReopened {
            repo_id, issue_id, ..
        } => (*repo_id, *issue_id, Change::Reopened),
        Event::PullRequestOpened {
            repo_id, pull_id, ..
        } => (*repo_id, *pull_id, Change::Opened),
        Event::PullRequestClosed {
            repo_id, pull_id, ..
        } => (*repo_id, *pull_id, Change::Closed),
        Event::PullRequestReopened {
            repo_id, pull_id, ..
        } => (*repo_id, *pull_id, Change::Reopened),
        Event::PullRequestMerged {
            repo_id, pull_id, ..
        } => (*repo_id, *pull_id, Change::Merged),
        _ => return Ok(()),
    };
    match change {
        Change::Opened | Change::Edited => auto_add(&state, repo_id, issue_id).await?,
        _ => on_state_change(&state, issue_id, change).await?,
    }
    Ok(())
}

#[derive(sqlx::FromRow)]
struct IssueInfo {
    title: String,
    state: String,
    is_pull_request: bool,
    merged: Option<bool>,
    labels: Vec<String>,
}

async fn issue_info(state: &AppState, issue_id: i64) -> Result<Option<IssueInfo>, sqlx::Error> {
    sqlx::query_as(
        "SELECT i.title, i.state, i.is_pull_request, pr.merged,
                ARRAY(SELECT l.name FROM issue_labels il JOIN labels l ON l.id = il.label_id
                       WHERE il.issue_id = i.id) AS labels
           FROM issues i LEFT JOIN pull_requests pr ON pr.issue_id = i.id WHERE i.id = $1",
    )
    .bind(issue_id)
    .fetch_optional(&state.db)
    .await
}

#[derive(sqlx::FromRow)]
struct AutoAdd {
    project_id: i64,
    owner_id: i64,
    owner_type: String,
    config: Value,
}

async fn auto_add(state: &AppState, repo_id: i64, issue_id: i64) -> anyhow::Result<()> {
    let workflows: Vec<AutoAdd> = sqlx::query_as(
        "SELECT w.project_id, p.owner_id, u.type AS owner_type, w.config
           FROM project_workflows w JOIN projects p ON p.id = w.project_id
           JOIN users u ON u.id = p.owner_id
          WHERE w.kind = 'auto_add' AND w.enabled AND w.repo_ids @> ARRAY[$1::bigint]
            AND NOT p.closed
            AND NOT EXISTS (SELECT 1 FROM project_items i WHERE i.project_id = w.project_id AND i.issue_id = $2)",
    )
    .bind(repo_id)
    .bind(issue_id)
    .fetch_all(&state.db)
    .await?;
    if workflows.is_empty() {
        return Ok(());
    }
    let Some(info) = issue_info(state, issue_id).await? else {
        return Ok(());
    };
    for wf in workflows {
        let filter = wf
            .config
            .get("filter")
            .and_then(Value::as_str)
            .unwrap_or("");
        let candidate = Candidate {
            is_pr: info.is_pull_request,
            open: info.state == "open",
            title: &info.title,
            labels: &info.labels,
        };
        if !filter::matches(filter, &candidate) {
            continue;
        }
        let scope = owner_scope(wf.owner_id, wf.owner_type == "Organization");
        let mut tx = Tx::begin(state).await?;
        let (_, created) = service::add_issue_item(
            &mut tx,
            &scope,
            wf.project_id,
            issue_id,
            info.is_pull_request,
            None,
            None,
        )
        .await?;
        if created {
            service::touch_project(&mut tx, &scope, wf.project_id).await?;
        }
        tx.commit().await?;
    }
    Ok(())
}

#[derive(sqlx::FromRow)]
struct ItemRef {
    item_id: i64,
    project_id: i64,
    owner_id: i64,
    owner_type: String,
}

async fn on_state_change(state: &AppState, issue_id: i64, change: Change) -> anyhow::Result<()> {
    let items: Vec<ItemRef> = sqlx::query_as(
        "SELECT i.id AS item_id, i.project_id, p.owner_id, u.type AS owner_type
           FROM project_items i JOIN projects p ON p.id = i.project_id
           JOIN users u ON u.id = p.owner_id
          WHERE i.issue_id = $1 ORDER BY i.id",
    )
    .bind(issue_id)
    .fetch_all(&state.db)
    .await?;
    if items.is_empty() {
        return Ok(());
    }
    // A merged PR may also be reported as closed: let the merge win.
    let merged = change == Change::Closed
        && issue_info(state, issue_id)
            .await?
            .is_some_and(|i| i.merged == Some(true));
    for it in items {
        let mut tx = Tx::begin(state).await?;
        let mut changed = false;
        let kind = match change {
            Change::Merged => Some("pr_merged"),
            Change::Closed if merged => {
                match service::workflow(&mut tx, it.project_id, "pr_merged").await? {
                    Some(_) => None,
                    None => Some("item_closed"),
                }
            }
            Change::Closed => Some("item_closed"),
            Change::Reopened => Some("item_reopened"),
            _ => None,
        };
        if let Some(kind) = kind
            && let Some(wf) = service::workflow(&mut tx, it.project_id, kind).await?
        {
            changed |=
                service::apply_status_workflow(&mut tx, it.project_id, it.item_id, &wf).await?;
        }
        if matches!(change, Change::Closed | Change::Merged)
            && service::workflow(&mut tx, it.project_id, "auto_archive")
                .await?
                .is_some()
        {
            changed |= sqlx::query(
                "UPDATE project_items SET archived = true, updated_at = now() WHERE id = $1 AND NOT archived",
            )
            .bind(it.item_id)
            .execute(&mut *tx)
            .await?
            .rows_affected()
                > 0;
        }
        if changed {
            let scope = owner_scope(it.owner_id, it.owner_type == "Organization");
            service::sync_item(&mut tx, &scope, it.item_id, SyncAction::Update).await?;
            tx.commit().await?;
        }
    }
    Ok(())
}
