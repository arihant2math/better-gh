//! Project views (table / board / roadmap) and their persisted settings.

use std::collections::HashSet;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use bgh_core::prelude::*;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::access::{ProjectAccess, Role};
use crate::model::*;
use crate::service;
use crate::util::{nullable, validate_text};

const LAYOUTS: &[&str] = &["table", "board", "roadmap"];
const MAX_VIEWS: i64 = 50;

#[derive(Debug, Deserialize)]
pub struct SortInput {
    #[serde(rename = "fieldId")]
    pub field_id: i64,
    #[serde(default = "asc")]
    pub direction: String,
}

fn asc() -> String {
    "asc".into()
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewBody {
    pub name: Option<String>,
    pub layout: Option<String>,
    pub position: Option<i32>,
    pub filter: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    pub group_by_field_id: Option<Option<i64>>,
    #[serde(default, deserialize_with = "nullable")]
    pub column_field_id: Option<Option<i64>>,
    #[serde(default, deserialize_with = "nullable")]
    pub date_field_id: Option<Option<i64>>,
    pub sort_by: Option<Vec<SortInput>>,
    pub visible_field_ids: Option<Vec<i64>>,
    pub hidden_column_ids: Option<Vec<String>>,
}

fn invalid(field: &str) -> ApiError {
    ApiError::invalid_field(FieldError::invalid("ProjectV2View", field))
}

/// Validate references against the project's fields.
async fn validate(tx: &mut Tx, project_id: i64, body: &ViewBody) -> ApiResult<()> {
    if let Some(l) = &body.layout
        && !LAYOUTS.contains(&l.as_str())
    {
        return Err(invalid("layout"));
    }
    if let Some(f) = &body.filter
        && f.chars().count() > 1024
    {
        return Err(invalid("filter"));
    }
    let fields: Vec<(i64, String)> =
        sqlx::query_as("SELECT id, data_type FROM project_fields WHERE project_id = $1")
            .bind(project_id)
            .fetch_all(&mut **tx)
            .await?;
    let kind = |id: i64| {
        fields
            .iter()
            .find(|(f, _)| *f == id)
            .map(|(_, k)| k.as_str())
    };
    if let Some(Some(id)) = body.group_by_field_id
        && !matches!(
            kind(id),
            Some(
                "status"
                    | "single_select"
                    | "iteration"
                    | "assignees"
                    | "repository"
                    | "milestone"
                    | "text"
                    | "number"
                    | "date"
                    | "labels"
            )
        )
    {
        return Err(invalid("groupByFieldId"));
    }
    if let Some(Some(id)) = body.column_field_id
        && !matches!(kind(id), Some("status" | "single_select" | "iteration"))
    {
        return Err(invalid("columnFieldId"));
    }
    if let Some(Some(id)) = body.date_field_id
        && !matches!(kind(id), Some("date" | "iteration"))
    {
        return Err(invalid("dateFieldId"));
    }
    if let Some(sort) = &body.sort_by {
        if sort.len() > 5 {
            return Err(invalid("sortBy"));
        }
        for s in sort {
            if kind(s.field_id).is_none() || !matches!(s.direction.as_str(), "asc" | "desc") {
                return Err(invalid("sortBy"));
            }
        }
    }
    if let Some(ids) = &body.visible_field_ids {
        let mut seen = HashSet::new();
        if ids
            .iter()
            .any(|id| kind(*id).is_none() || !seen.insert(*id))
        {
            return Err(invalid("visibleFieldIds"));
        }
    }
    if let Some(cols) = &body.hidden_column_ids
        && (cols.len() > 200 || cols.iter().any(|c| c.len() > 64))
    {
        return Err(invalid("hiddenColumnIds"));
    }
    Ok(())
}

fn sort_json(sort: &[SortInput]) -> Value {
    Value::Array(
        sort.iter()
            .map(|s| json!({"fieldId": s.field_id, "direction": s.direction}))
            .collect(),
    )
}

/// Apply `body` to view `view_id`.
async fn apply(tx: &mut Tx, view_id: i64, body: &ViewBody) -> ApiResult<ViewRow> {
    Ok(sqlx::query_as(&format!(
        "UPDATE project_views SET
            name = COALESCE($2, name),
            layout = COALESCE($3, layout),
            position = COALESCE($4, position),
            filter = COALESCE($5, filter),
            group_by_field_id = CASE WHEN $6 THEN $7 ELSE group_by_field_id END,
            column_field_id = CASE WHEN $8 THEN $9 ELSE column_field_id END,
            date_field_id = CASE WHEN $10 THEN $11 ELSE date_field_id END,
            sort_by = COALESCE($12, sort_by),
            visible_field_ids = COALESCE($13, visible_field_ids),
            hidden_column_ids = COALESCE($14, hidden_column_ids),
            updated_at = now()
         WHERE id = $1 RETURNING {}",
        ViewRow::COLUMNS
    ))
    .bind(view_id)
    .bind(body.name.as_deref().map(str::trim))
    .bind(body.layout.as_deref())
    .bind(body.position)
    .bind(body.filter.as_deref())
    .bind(body.group_by_field_id.is_some())
    .bind(body.group_by_field_id.flatten())
    .bind(body.column_field_id.is_some())
    .bind(body.column_field_id.flatten())
    .bind(body.date_field_id.is_some())
    .bind(body.date_field_id.flatten())
    .bind(body.sort_by.as_deref().map(sort_json))
    .bind(body.visible_field_ids.as_deref())
    .bind(body.hidden_column_ids.as_deref())
    .fetch_one(&mut **tx)
    .await?)
}

/// `POST /_bgh/projects/{id}/views`
pub async fn create(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(id): Path<i64>,
    Json(body): Json<ViewBody>,
) -> ApiResult<impl IntoResponse> {
    let access = ProjectAccess::load(&state, Some(&auth), id).await?;
    access.require(Role::Write)?;
    let mut tx = Tx::begin(&state).await?;
    validate(&mut tx, id, &body).await?;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM project_views WHERE project_id = $1")
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    if count >= MAX_VIEWS {
        return Err(ApiError::invalid_field(FieldError::custom(
            "ProjectV2View",
            "name",
            format!("a project can have at most {MAX_VIEWS} views"),
        )));
    }
    let name = match &body.name {
        Some(n) => validate_text("ProjectV2View", "name", Some(n), 256)?,
        None => format!("View {}", count + 1),
    };
    let layout = body.layout.as_deref().unwrap_or("table");
    // Defaults: Title/Assignees/Status columns, Status as board column field.
    let defaults: Vec<(i64, String)> = sqlx::query_as(
        "SELECT id, data_type FROM project_fields WHERE project_id = $1
           AND data_type IN ('title', 'assignees', 'status') ORDER BY position, id",
    )
    .bind(id)
    .fetch_all(&mut *tx)
    .await?;
    let visible: Vec<i64> = defaults.iter().map(|(i, _)| *i).collect();
    let status = defaults
        .iter()
        .find(|(_, k)| k == "status")
        .map(|(i, _)| *i);
    let view = service::insert_view(&mut tx, id, &name, layout, &visible, status).await?;
    let view = apply(
        &mut tx,
        view.id,
        &ViewBody {
            name: None,
            layout: None,
            ..body
        },
    )
    .await?;
    tx.sync(
        &access.scope(),
        M_VIEW,
        view.id,
        SyncAction::Insert,
        &view.sync_json(),
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(view.sync_json())))
}

/// `PATCH /_bgh/projects/{id}/views/{view_id}`
pub async fn update(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((id, view_id)): Path<(i64, i64)>,
    Json(body): Json<ViewBody>,
) -> ApiResult<Json<Value>> {
    let access = ProjectAccess::load(&state, Some(&auth), id).await?;
    access.require(Role::Write)?;
    if let Some(n) = &body.name {
        validate_text("ProjectV2View", "name", Some(n), 256)?;
    }
    let mut tx = Tx::begin(&state).await?;
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM project_views WHERE id = $1 AND project_id = $2)",
    )
    .bind(view_id)
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    if !exists {
        return Err(ApiError::NotFound);
    }
    validate(&mut tx, id, &body).await?;
    let view = apply(&mut tx, view_id, &body).await?;
    tx.sync(
        &access.scope(),
        M_VIEW,
        view.id,
        SyncAction::Update,
        &view.sync_json(),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(view.sync_json()))
}

/// `DELETE /_bgh/projects/{id}/views/{view_id}`
pub async fn delete(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((id, view_id)): Path<(i64, i64)>,
) -> ApiResult<StatusCode> {
    let access = ProjectAccess::load(&state, Some(&auth), id).await?;
    access.require(Role::Write)?;
    let scope = access.scope();
    let mut tx = Tx::begin(&state).await?;
    let ids: Vec<i64> =
        sqlx::query_scalar("SELECT id FROM project_views WHERE project_id = $1 FOR UPDATE")
            .bind(id)
            .fetch_all(&mut *tx)
            .await?;
    if !ids.contains(&view_id) {
        return Err(ApiError::NotFound);
    }
    if ids.len() == 1 {
        return Err(ApiError::invalid_field(FieldError::custom(
            "ProjectV2View",
            "id",
            "a project needs at least one view",
        )));
    }
    sqlx::query("DELETE FROM project_views WHERE id = $1")
        .bind(view_id)
        .execute(&mut *tx)
        .await?;
    tx.sync(
        &scope,
        M_VIEW,
        view_id,
        SyncAction::Delete,
        &json!({"id": view_id}),
    )
    .await?;
    // Drop the view's per-item positions.
    let affected: Vec<i64> = sqlx::query_scalar(
        "UPDATE project_items SET view_positions = view_positions - $2::text
          WHERE project_id = $1 AND view_positions ? $2::text RETURNING id",
    )
    .bind(id)
    .bind(view_id.to_string())
    .fetch_all(&mut *tx)
    .await?;
    service::sync_items(&mut tx, &scope, &affected).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
