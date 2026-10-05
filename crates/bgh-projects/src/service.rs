//! Write helpers shared by handlers and workflows. Every helper records the
//! resulting sync actions in the caller's [`Tx`].

use bgh_core::prelude::*;
use serde_json::{Value, json};

use crate::model::*;
use crate::position;

/// Reload an item and record it (`Insert` / `Update`).
pub async fn sync_item(
    tx: &mut Tx,
    scope: &str,
    item_id: i64,
    action: SyncAction,
) -> ApiResult<ItemRow> {
    let item = ItemRow::find(&mut **tx, item_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    tx.sync(scope, M_ITEM, item.id, action, &item.sync_json())
        .await?;
    Ok(item)
}

/// Record updates for several items.
pub async fn sync_items(tx: &mut Tx, scope: &str, ids: &[i64]) -> ApiResult<()> {
    if ids.is_empty() {
        return Ok(());
    }
    for item in ItemRow::find_many(&mut **tx, ids).await? {
        tx.sync(
            scope,
            M_ITEM,
            item.id,
            SyncAction::Update,
            &item.sync_json(),
        )
        .await?;
    }
    Ok(())
}

/// Bump `projects.updated_at` and record the project.
pub async fn touch_project(tx: &mut Tx, scope: &str, project_id: i64) -> ApiResult<ProjectRow> {
    sqlx::query("UPDATE projects SET updated_at = now() WHERE id = $1")
        .bind(project_id)
        .execute(&mut **tx)
        .await?;
    let p = ProjectRow::find(&mut **tx, project_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    tx.sync(scope, M_PROJECT, p.id, SyncAction::Update, &p.sync_json())
        .await?;
    Ok(p)
}

/// Position after the last item of the project.
pub async fn next_position(tx: &mut Tx, project_id: i64) -> ApiResult<String> {
    let max: Option<String> =
        sqlx::query_scalar("SELECT max(position) FROM project_items WHERE project_id = $1")
            .bind(project_id)
            .fetch_one(&mut **tx)
            .await?;
    Ok(position::between(max.as_deref(), None))
}

/// The project's Status field, if it still has one.
pub async fn status_field(tx: &mut Tx, project_id: i64) -> ApiResult<Option<FieldRow>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {} FROM project_fields WHERE project_id = $1 AND data_type = 'status' ORDER BY id LIMIT 1",
        FieldRow::COLUMNS
    ))
    .bind(project_id)
    .fetch_optional(&mut **tx)
    .await?)
}

/// Enabled workflow of `kind`, if any.
pub async fn workflow(tx: &mut Tx, project_id: i64, kind: &str) -> ApiResult<Option<WorkflowRow>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {} FROM project_workflows WHERE project_id = $1 AND kind = $2 AND enabled",
        WorkflowRow::COLUMNS
    ))
    .bind(project_id)
    .bind(kind)
    .fetch_optional(&mut **tx)
    .await?)
}

/// Set the item's Status to the option configured on `workflow`. Returns
/// whether a value changed (the caller records the item).
pub async fn apply_status_workflow(
    tx: &mut Tx,
    project_id: i64,
    item_id: i64,
    workflow: &WorkflowRow,
) -> ApiResult<bool> {
    let Some(option_id) = workflow
        .config
        .get("statusOptionId")
        .and_then(Value::as_str)
    else {
        return Ok(false);
    };
    let Some(field) = status_field(tx, project_id).await? else {
        return Ok(false);
    };
    if !field.option_ids().iter().any(|o| o == option_id) {
        return Ok(false);
    }
    let changed = sqlx::query(
        "INSERT INTO project_item_values (item_id, field_id, value) VALUES ($1, $2, $3)
         ON CONFLICT (item_id, field_id) DO UPDATE SET value = EXCLUDED.value, updated_at = now()
         WHERE project_item_values.value IS DISTINCT FROM EXCLUDED.value",
    )
    .bind(item_id)
    .bind(field.id)
    .bind(json!(option_id))
    .execute(&mut **tx)
    .await?
    .rows_affected()
        > 0;
    if changed {
        sqlx::query("UPDATE project_items SET updated_at = now() WHERE id = $1")
            .bind(item_id)
            .execute(&mut **tx)
            .await?;
    }
    Ok(changed)
}

/// Add an issue / PR to a project (no-op when already present). Applies the
/// `item_added` workflow and records the item. Returns `(item, created)`.
pub async fn add_issue_item(
    tx: &mut Tx,
    scope: &str,
    project_id: i64,
    issue_id: i64,
    is_pr: bool,
    creator_id: Option<i64>,
    position: Option<String>,
) -> ApiResult<(ItemRow, bool)> {
    let position = match position {
        Some(p) => p,
        None => next_position(tx, project_id).await?,
    };
    let id: Option<i64> = sqlx::query_scalar(
        "INSERT INTO project_items (project_id, content_type, issue_id, position, creator_id)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (project_id, issue_id) WHERE issue_id IS NOT NULL DO NOTHING
         RETURNING id",
    )
    .bind(project_id)
    .bind(if is_pr { "PullRequest" } else { "Issue" })
    .bind(issue_id)
    .bind(&position)
    .bind(creator_id)
    .fetch_optional(&mut **tx)
    .await?;
    match id {
        Some(id) => {
            after_item_added(tx, project_id, id).await?;
            Ok((sync_item(tx, scope, id, SyncAction::Insert).await?, true))
        }
        None => {
            let id: i64 = sqlx::query_scalar(
                "SELECT id FROM project_items WHERE project_id = $1 AND issue_id = $2",
            )
            .bind(project_id)
            .bind(issue_id)
            .fetch_one(&mut **tx)
            .await?;
            let item = ItemRow::find(&mut **tx, id)
                .await?
                .ok_or(ApiError::NotFound)?;
            Ok((item, false))
        }
    }
}

/// `item_added` workflow for a freshly inserted item.
pub async fn after_item_added(tx: &mut Tx, project_id: i64, item_id: i64) -> ApiResult<()> {
    if let Some(wf) = workflow(tx, project_id, "item_added").await? {
        apply_status_workflow(tx, project_id, item_id, &wf).await?;
    }
    Ok(())
}

/// A fresh option / iteration id.
pub fn short_id() -> String {
    format!("{:08x}", rand::random::<u32>())
}

/// Default fields, view and workflows of a new project.
pub async fn create_defaults(tx: &mut Tx, scope: &str, project_id: i64) -> ApiResult<()> {
    let todo = short_id();
    let in_progress = short_id();
    let done = short_id();
    let status_options = json!([
        {"id": todo, "name": "Todo", "color": "GRAY", "description": "This item hasn't been started"},
        {"id": in_progress, "name": "In Progress", "color": "YELLOW", "description": "This is actively being worked on"},
        {"id": done, "name": "Done", "color": "PURPLE", "description": "This has been completed"},
    ]);
    let defaults: [(&str, &str); 6] = [
        ("Title", "title"),
        ("Assignees", "assignees"),
        ("Status", "status"),
        ("Labels", "labels"),
        ("Repository", "repository"),
        ("Milestone", "milestone"),
    ];
    let mut ids = Vec::new();
    for (pos, (name, kind)) in defaults.iter().enumerate() {
        let options = (*kind == "status").then(|| status_options.clone());
        let f: FieldRow = sqlx::query_as(&format!(
            "INSERT INTO project_fields (project_id, name, data_type, position, options)
             VALUES ($1, $2, $3, $4, $5) RETURNING {}",
            FieldRow::COLUMNS
        ))
        .bind(project_id)
        .bind(name)
        .bind(kind)
        .bind(pos as i32)
        .bind(options)
        .fetch_one(&mut **tx)
        .await?;
        tx.sync(scope, M_FIELD, f.id, SyncAction::Insert, &f.sync_json())
            .await?;
        ids.push(f.id);
    }
    let view = insert_view(
        tx,
        project_id,
        "View 1",
        "table",
        &[ids[0], ids[1], ids[2]],
        Some(ids[2]),
    )
    .await?;
    tx.sync(
        scope,
        M_VIEW,
        view.id,
        SyncAction::Insert,
        &view.sync_json(),
    )
    .await?;
    for kind in WORKFLOW_KINDS {
        let (enabled, config) = match *kind {
            "item_closed" | "pr_merged" => (true, json!({"statusOptionId": done})),
            "item_added" | "item_reopened" => (false, json!({"statusOptionId": todo})),
            "auto_add" => (
                false,
                json!({"repoIds": [], "filter": "is:issue,pr is:open"}),
            ),
            _ => (false, json!({})),
        };
        let wf: WorkflowRow = sqlx::query_as(&format!(
            "INSERT INTO project_workflows (project_id, kind, enabled, config)
             VALUES ($1, $2, $3, $4) RETURNING {}",
            WorkflowRow::COLUMNS
        ))
        .bind(project_id)
        .bind(kind)
        .bind(enabled)
        .bind(&config)
        .fetch_one(&mut **tx)
        .await?;
        tx.sync(
            scope,
            M_WORKFLOW,
            wf.id,
            SyncAction::Insert,
            &wf.sync_json(),
        )
        .await?;
    }
    Ok(())
}

/// Insert a view with the next number and position.
pub async fn insert_view(
    tx: &mut Tx,
    project_id: i64,
    name: &str,
    layout: &str,
    visible: &[i64],
    column_field: Option<i64>,
) -> ApiResult<ViewRow> {
    let number: i64 = sqlx::query_scalar(
        "UPDATE projects SET next_view_number = next_view_number + 1 WHERE id = $1
         RETURNING next_view_number - 1",
    )
    .bind(project_id)
    .fetch_one(&mut **tx)
    .await?;
    Ok(sqlx::query_as(&format!(
        "INSERT INTO project_views (project_id, number, name, layout, position, visible_field_ids, column_field_id)
         VALUES ($1, $2, $3, $4,
                 (SELECT COALESCE(max(position) + 1, 0) FROM project_views WHERE project_id = $1),
                 $5, $6)
         RETURNING {}",
        ViewRow::COLUMNS
    ))
    .bind(project_id)
    .bind(number)
    .bind(name)
    .bind(layout)
    .bind(visible)
    .bind(column_field)
    .fetch_one(&mut **tx)
    .await?)
}
