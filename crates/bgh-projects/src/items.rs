//! Project items: issues, pull requests and draft issues; field values and
//! ordering.

use std::collections::{HashMap, HashSet};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use bgh_core::perms;
use bgh_core::prelude::*;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::access::{ProjectAccess, Role};
use crate::fields::validate_value;
use crate::model::*;
use crate::position;
use crate::service;
use crate::util::{nullable, validate_text};

const MAX_ITEMS: i64 = 10_000;

#[derive(Debug, Default, Deserialize)]
pub struct DraftInput {
    pub title: Option<String>,
    pub body: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateBody {
    pub issue_id: Option<i64>,
    pub owner: Option<String>,
    pub repo: Option<String>,
    pub number: Option<i64>,
    pub draft: Option<DraftInput>,
    pub position: Option<String>,
}

fn check_position(p: &str) -> ApiResult<()> {
    if position::is_valid(p) {
        Ok(())
    } else {
        Err(ApiError::invalid_field(FieldError::invalid(
            "ProjectV2Item",
            "position",
        )))
    }
}

/// Resolve the issue an item request refers to, checking read access.
async fn resolve_issue(
    state: &AppState,
    auth: &AuthContext,
    body: &CreateBody,
) -> ApiResult<(i64, bool)> {
    let row: Option<(i64, bool, i64)> = match (body.issue_id, &body.owner, &body.repo, body.number)
    {
        (Some(id), ..) => {
            sqlx::query_as("SELECT id, is_pull_request, repo_id FROM issues WHERE id = $1")
                .bind(id)
                .fetch_optional(&state.db)
                .await?
        }
        (None, Some(owner), Some(repo), Some(number)) => {
            sqlx::query_as(
                "SELECT i.id, i.is_pull_request, i.repo_id FROM issues i
                   JOIN repositories r ON r.id = i.repo_id JOIN users u ON u.id = r.owner_id
                  WHERE lower(u.login) = lower($1) AND lower(r.name) = lower($2) AND i.number = $3",
            )
            .bind(owner)
            .bind(repo)
            .bind(number)
            .fetch_optional(&state.db)
            .await?
        }
        _ => {
            return Err(ApiError::invalid_field(FieldError::missing_field(
                "ProjectV2Item",
                "issueId",
            )));
        }
    };
    let not_found = || {
        ApiError::invalid_field(FieldError::custom(
            "ProjectV2Item",
            "issueId",
            "Could not resolve to an issue or pull request",
        ))
    };
    let (issue_id, is_pr, repo_id) = row.ok_or_else(not_found)?;
    let repo = db::Repository::find(&state.db, repo_id)
        .await?
        .ok_or_else(not_found)?;
    let raw = perms::repo_permission(&state.db, Some(auth.user.id), &repo).await?;
    if perms::effective(Some(auth), &repo, raw) < Permission::Read {
        return Err(not_found());
    }
    Ok((issue_id, is_pr))
}

/// `POST /_bgh/projects/{id}/items`
pub async fn create(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(id): Path<i64>,
    Json(body): Json<CreateBody>,
) -> ApiResult<axum::response::Response> {
    let (item, created) = add_item(&state, &auth, id, body).await?;
    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(item.sync_json())).into_response())
}

/// Add an issue, pull request or draft issue to a project. Returns the item
/// and whether it was created (an issue already on the project is returned
/// as is).
pub async fn add_item(
    state: &AppState,
    auth: &AuthContext,
    id: i64,
    body: CreateBody,
) -> ApiResult<(ItemRow, bool)> {
    let state = state.clone();
    let access = ProjectAccess::load(&state, Some(auth), id).await?;
    access.require(Role::Write)?;
    if let Some(p) = &body.position {
        check_position(p)?;
    }
    let scope = access.scope();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM project_items WHERE project_id = $1")
        .bind(id)
        .fetch_one(&state.db)
        .await?;
    if count >= MAX_ITEMS {
        return Err(ApiError::invalid_field(FieldError::custom(
            "ProjectV2Item",
            "projectId",
            format!("a project can have at most {MAX_ITEMS} items"),
        )));
    }

    if let Some(draft) = &body.draft {
        let title = validate_text("DraftIssue", "title", draft.title.as_deref(), 256)?;
        let mut tx = Tx::begin(&state).await?;
        let position = match body.position.clone() {
            Some(p) => p,
            None => service::next_position(&mut tx, id).await?,
        };
        let item_id: i64 = sqlx::query_scalar(
            "INSERT INTO project_items (project_id, content_type, title, body, position, creator_id)
             VALUES ($1, 'DraftIssue', $2, $3, $4, $5) RETURNING id",
        )
        .bind(id)
        .bind(&title)
        .bind(draft.body.as_deref())
        .bind(&position)
        .bind(auth.user.id)
        .fetch_one(&mut *tx)
        .await?;
        service::after_item_added(&mut tx, id, item_id).await?;
        let item = service::sync_item(&mut tx, &scope, item_id, SyncAction::Insert).await?;
        service::touch_project(&mut tx, &scope, id).await?;
        tx.commit().await?;
        return Ok((item, true));
    }

    let (issue_id, is_pr) = resolve_issue(&state, auth, &body).await?;
    let mut tx = Tx::begin(&state).await?;
    let (item, created) = service::add_issue_item(
        &mut tx,
        &scope,
        id,
        issue_id,
        is_pr,
        Some(auth.user.id),
        body.position.clone(),
    )
    .await?;
    if created {
        service::touch_project(&mut tx, &scope, id).await?;
    }
    tx.commit().await?;
    Ok((item, created))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateBody {
    pub archived: Option<bool>,
    pub position: Option<String>,
    pub view_id: Option<i64>,
    #[serde(default, deserialize_with = "nullable")]
    pub view_position: Option<Option<String>>,
    pub title: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    pub body: Option<Option<String>>,
    pub assignee_ids: Option<Vec<i64>>,
    pub values: Option<HashMap<String, Value>>,
}

/// `PATCH /_bgh/projects/{id}/items/{item_id}`
pub async fn update(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((id, item_id)): Path<(i64, i64)>,
    Json(body): Json<UpdateBody>,
) -> ApiResult<Json<Value>> {
    let item = update_item(&state, &auth, id, item_id, body).await?;
    Ok(Json(item.sync_json()))
}

/// Update an item (archive state, positions, draft content, field values).
pub async fn update_item(
    state: &AppState,
    auth: &AuthContext,
    id: i64,
    item_id: i64,
    body: UpdateBody,
) -> ApiResult<ItemRow> {
    let access = ProjectAccess::load(state, Some(auth), id).await?;
    access.require(Role::Write)?;
    let scope = access.scope();
    let mut tx = Tx::begin(state).await?;
    let item: ItemRow = sqlx::query_as(&format!(
        "{} WHERE i.id = $1 AND i.project_id = $2 FOR UPDATE OF i",
        ItemRow::SELECT
    ))
    .bind(item_id)
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(ApiError::NotFound)?;

    if (body.title.is_some() || body.body.is_some() || body.assignee_ids.is_some())
        && !item.is_draft()
    {
        return Err(ApiError::invalid_field(FieldError::custom(
            "ProjectV2Item",
            "title",
            "only draft issues can be edited on the project; edit the issue instead",
        )));
    }
    let title = match &body.title {
        Some(t) => Some(validate_text("DraftIssue", "title", Some(t), 256)?),
        None => None,
    };
    if let Some(p) = &body.position {
        check_position(p)?;
    }
    let mut assignees = body.assignee_ids.clone();
    if let Some(a) = &mut assignees {
        a.sort_unstable();
        a.dedup();
        if a.len() > 10 {
            return Err(ApiError::invalid_field(FieldError::custom(
                "DraftIssue",
                "assigneeIds",
                "at most 10 assignees",
            )));
        }
        let found: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM users WHERE id = ANY($1) AND type <> 'Organization'",
        )
        .bind(&*a)
        .fetch_one(&mut *tx)
        .await?;
        if found != a.len() as i64 {
            return Err(ApiError::invalid_field(FieldError::invalid(
                "DraftIssue",
                "assigneeIds",
            )));
        }
    }
    // Per-view position.
    let view_positions = match (body.view_id, &body.view_position) {
        (Some(view_id), Some(pos)) => {
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM project_views WHERE id = $1 AND project_id = $2)",
            )
            .bind(view_id)
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
            if !exists {
                return Err(ApiError::invalid_field(FieldError::invalid(
                    "ProjectV2Item",
                    "viewId",
                )));
            }
            let mut map = item.view_positions.as_object().cloned().unwrap_or_default();
            match pos {
                Some(p) => {
                    check_position(p)?;
                    map.insert(view_id.to_string(), json!(p));
                }
                None => {
                    map.remove(&view_id.to_string());
                }
            }
            Some(Value::Object(map))
        }
        (None, Some(_)) => {
            return Err(ApiError::invalid_field(FieldError::missing_field(
                "ProjectV2Item",
                "viewId",
            )));
        }
        _ => None,
    };

    sqlx::query(
        "UPDATE project_items SET
            archived_at = CASE WHEN $2::bool IS NULL THEN archived_at
                               WHEN $2 AND NOT archived THEN now()
                               WHEN NOT $2 THEN NULL ELSE archived_at END,
            archived = COALESCE($2, archived),
            position = COALESCE($3, position),
            view_positions = COALESCE($4, view_positions),
            title = COALESCE($5, title),
            body = CASE WHEN $6 THEN $7 ELSE body END,
            assignee_ids = COALESCE($8, assignee_ids),
            updated_at = now()
         WHERE id = $1",
    )
    .bind(item_id)
    .bind(body.archived)
    .bind(body.position.as_deref())
    .bind(view_positions)
    .bind(title)
    .bind(body.body.is_some())
    .bind(body.body.clone().flatten())
    .bind(assignees)
    .execute(&mut *tx)
    .await?;

    if let Some(values) = &body.values {
        let ids: Vec<i64> = values
            .keys()
            .map(|k| k.parse::<i64>())
            .collect::<Result<_, _>>()
            .map_err(|_| ApiError::invalid_field(FieldError::invalid("ProjectV2Item", "values")))?;
        let fields: Vec<FieldRow> = sqlx::query_as(&format!(
            "SELECT {} FROM project_fields WHERE project_id = $1 AND id = ANY($2)",
            FieldRow::COLUMNS
        ))
        .bind(id)
        .bind(&ids)
        .fetch_all(&mut *tx)
        .await?;
        let by_id: HashMap<i64, &FieldRow> = fields.iter().map(|f| (f.id, f)).collect();
        let mut seen = HashSet::new();
        for (key, value) in values {
            let fid: i64 = key.parse().unwrap_or_default();
            let field = by_id.get(&fid).ok_or_else(|| {
                ApiError::invalid_field(FieldError::custom(
                    "ProjectV2Item",
                    "values",
                    format!("unknown field {key}"),
                ))
            })?;
            seen.insert(fid);
            if value.is_null() {
                sqlx::query("DELETE FROM project_item_values WHERE item_id = $1 AND field_id = $2")
                    .bind(item_id)
                    .bind(fid)
                    .execute(&mut *tx)
                    .await?;
            } else {
                validate_value(field, value)?;
                sqlx::query(
                    "INSERT INTO project_item_values (item_id, field_id, value) VALUES ($1, $2, $3)
                     ON CONFLICT (item_id, field_id) DO UPDATE SET value = EXCLUDED.value, updated_at = now()",
                )
                .bind(item_id)
                .bind(fid)
                .bind(value)
                .execute(&mut *tx)
                .await?;
            }
        }
    }

    // Unarchiving / archiving through the API is also what auto-archive does.
    let item = service::sync_item(&mut tx, &scope, item_id, SyncAction::Update).await?;
    tx.commit().await?;
    Ok(item)
}

/// `DELETE /_bgh/projects/{id}/items/{item_id}`
pub async fn delete(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((id, item_id)): Path<(i64, i64)>,
) -> ApiResult<StatusCode> {
    delete_item(&state, &auth, id, item_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Remove an item from a project.
pub async fn delete_item(
    state: &AppState,
    auth: &AuthContext,
    id: i64,
    item_id: i64,
) -> ApiResult<()> {
    let access = ProjectAccess::load(state, Some(auth), id).await?;
    access.require(Role::Write)?;
    let scope = access.scope();
    let mut tx = Tx::begin(state).await?;
    let deleted = sqlx::query("DELETE FROM project_items WHERE id = $1 AND project_id = $2")
        .bind(item_id)
        .bind(id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    if deleted == 0 {
        return Err(ApiError::NotFound);
    }
    tx.sync(
        &scope,
        M_ITEM,
        item_id,
        SyncAction::Delete,
        &json!({"id": item_id}),
    )
    .await?;
    service::touch_project(&mut tx, &scope, id).await?;
    tx.commit().await?;
    Ok(())
}

/// Move an item right after `after_id` (`None`: to the top), like
/// GitHub's `updateProjectV2ItemPosition`.
pub async fn move_item(
    state: &AppState,
    auth: &AuthContext,
    id: i64,
    item_id: i64,
    after_id: Option<i64>,
) -> ApiResult<ItemRow> {
    let positions: Vec<(i64, String)> = sqlx::query_as(
        "SELECT id, position FROM project_items WHERE project_id = $1 AND id <> $2
          ORDER BY position, id",
    )
    .bind(id)
    .bind(item_id)
    .fetch_all(&state.db)
    .await?;
    let (lo, hi) = match after_id {
        Some(after) => {
            let idx = positions
                .iter()
                .position(|(i, _)| *i == after)
                .ok_or_else(|| {
                    ApiError::invalid_field(FieldError::invalid("ProjectV2Item", "afterId"))
                })?;
            (
                Some(positions[idx].1.as_str()),
                positions.get(idx + 1).map(|p| p.1.as_str()),
            )
        }
        None => (None, positions.first().map(|p| p.1.as_str())),
    };
    // Equal neighbours (legacy duplicates) can't be split: append instead.
    let position = match (lo, hi) {
        (Some(a), Some(b)) if a >= b => position::between(Some(a), None),
        _ => position::between(lo, hi),
    };
    update_item(
        state,
        auth,
        id,
        item_id,
        UpdateBody {
            position: Some(position),
            ..Default::default()
        },
    )
    .await
}
