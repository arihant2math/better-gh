//! Project fields: custom field CRUD, single-select options, iterations.

use std::collections::HashSet;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use bgh_core::prelude::*;
use chrono::{Duration, NaiveDate, Utc};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::access::{ProjectAccess, Role};
use crate::model::*;
use crate::service::{self, short_id};
use crate::util::validate_text;

pub const COLORS: &[&str] = &[
    "GRAY", "BLUE", "GREEN", "YELLOW", "ORANGE", "RED", "PINK", "PURPLE",
];
const CUSTOM_TYPES: &[&str] = &["text", "number", "date", "single_select", "iteration"];
const MAX_FIELDS: i64 = 50;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OptionInput {
    pub id: Option<String>,
    pub name: Option<String>,
    pub color: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IterationInput {
    pub id: Option<String>,
    pub title: Option<String>,
    pub start_date: Option<String>,
    pub duration: Option<i64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IterationsInput {
    pub start_date: Option<String>,
    pub duration: Option<i64>,
    /// Number of iterations to generate when `iterations` is absent.
    pub count: Option<usize>,
    pub iterations: Option<Vec<IterationInput>>,
}

fn invalid(field: &str, msg: impl Into<String>) -> ApiError {
    ApiError::invalid_field(FieldError::custom("ProjectV2Field", field, msg))
}

/// Normalize single-select options, keeping known ids and generating new ones.
fn normalize_options(input: Vec<OptionInput>, existing: &[String]) -> ApiResult<Value> {
    if input.len() > 50 {
        return Err(invalid("options", "too many options (maximum is 50)"));
    }
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(input.len());
    for o in input {
        let name = validate_text("ProjectV2Field", "options", o.name.as_deref(), 256)?;
        let color = o
            .color
            .unwrap_or_else(|| "GRAY".into())
            .to_ascii_uppercase();
        if !COLORS.contains(&color.as_str()) {
            return Err(invalid("options", format!("invalid color {color:?}")));
        }
        let id = match o.id {
            Some(id) if existing.contains(&id) && !seen.contains(&id) => id,
            _ => short_id(),
        };
        seen.insert(id.clone());
        out.push(json!({
            "id": id,
            "name": name,
            "color": color,
            "description": o.description.unwrap_or_default(),
        }));
    }
    Ok(Value::Array(out))
}

fn parse_date(field: &str, s: &str) -> ApiResult<NaiveDate> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .map_err(|_| invalid(field, format!("invalid date {s:?} (expected YYYY-MM-DD)")))
}

fn check_duration(d: i64) -> ApiResult<i64> {
    if (1..=365).contains(&d) {
        Ok(d)
    } else {
        Err(invalid(
            "iterations",
            "duration must be between 1 and 365 days",
        ))
    }
}

/// Normalize an iteration configuration. Iterations are sorted by start
/// date and may not overlap; gaps between them are breaks.
fn normalize_iterations(input: IterationsInput, existing: Option<&Value>) -> ApiResult<Value> {
    let old = existing.cloned().unwrap_or(Value::Null);
    let duration = check_duration(
        input
            .duration
            .or_else(|| old.get("duration").and_then(Value::as_i64))
            .unwrap_or(14),
    )?;
    let start = match input.start_date.or_else(|| {
        old.get("startDate")
            .and_then(Value::as_str)
            .map(String::from)
    }) {
        Some(s) => parse_date("iterations", &s)?,
        None => Utc::now().date_naive(),
    };
    let known: Vec<String> = old
        .get("iterations")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|i| i.get("id").and_then(Value::as_str).map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let mut list: Vec<(NaiveDate, i64, String, String)> = match input.iterations {
        Some(items) => {
            let mut v = Vec::with_capacity(items.len());
            let mut seen = HashSet::new();
            for it in items {
                let s = match it.start_date {
                    Some(s) => parse_date("iterations", &s)?,
                    None => return Err(invalid("iterations", "iteration startDate is required")),
                };
                let d = check_duration(it.duration.unwrap_or(duration))?;
                let id = match it.id {
                    Some(id) if known.contains(&id) && !seen.contains(&id) => id,
                    _ => short_id(),
                };
                seen.insert(id.clone());
                v.push((s, d, id, it.title.unwrap_or_default().trim().to_string()));
            }
            v
        }
        None => {
            let count = input.count.unwrap_or(3).min(100);
            (0..count)
                .map(|n| {
                    (
                        start + Duration::days(duration * n as i64),
                        duration,
                        short_id(),
                        String::new(),
                    )
                })
                .collect()
        }
    };
    if list.len() > 200 {
        return Err(invalid(
            "iterations",
            "too many iterations (maximum is 200)",
        ));
    }
    list.sort_by_key(|(s, ..)| *s);
    for w in list.windows(2) {
        if w[0].0 + Duration::days(w[0].1) > w[1].0 {
            return Err(invalid("iterations", "iterations may not overlap"));
        }
    }
    let iterations: Vec<Value> = list
        .into_iter()
        .enumerate()
        .map(|(n, (s, d, id, title))| {
            json!({
                "id": id,
                "title": if title.is_empty() { format!("Iteration {}", n + 1) } else { title },
                "startDate": s.format("%Y-%m-%d").to_string(),
                "duration": d,
            })
        })
        .collect();
    Ok(json!({
        "startDate": start.format("%Y-%m-%d").to_string(),
        "duration": duration,
        "iterations": iterations,
    }))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateBody {
    pub name: Option<String>,
    pub data_type: Option<String>,
    pub options: Option<Vec<OptionInput>>,
    pub iterations: Option<IterationsInput>,
}

/// `POST /_bgh/projects/{id}/fields`
pub async fn create(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(id): Path<i64>,
    Json(body): Json<CreateBody>,
) -> ApiResult<impl IntoResponse> {
    let field = create_field(&state, &auth, id, body).await?;
    Ok((StatusCode::CREATED, Json(field.sync_json())))
}

/// Create a custom field.
pub async fn create_field(
    state: &AppState,
    auth: &AuthContext,
    id: i64,
    body: CreateBody,
) -> ApiResult<FieldRow> {
    let access = ProjectAccess::load(state, Some(auth), id).await?;
    access.require(Role::Write)?;
    let name = validate_text("ProjectV2Field", "name", body.name.as_deref(), 256)?;
    let data_type = body.data_type.ok_or_else(|| {
        ApiError::invalid_field(FieldError::missing_field("ProjectV2Field", "dataType"))
    })?;
    if !CUSTOM_TYPES.contains(&data_type.as_str()) {
        return Err(ApiError::invalid_field(FieldError::invalid(
            "ProjectV2Field",
            "dataType",
        )));
    }
    let options = match data_type.as_str() {
        "single_select" => Some(normalize_options(body.options.unwrap_or_default(), &[])?),
        _ => None,
    };
    let iterations = match data_type.as_str() {
        "iteration" => Some(normalize_iterations(
            body.iterations.unwrap_or_default(),
            None,
        )?),
        _ => None,
    };
    let mut tx = Tx::begin(state).await?;
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM project_fields WHERE project_id = $1")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
    if count >= MAX_FIELDS {
        return Err(invalid(
            "name",
            format!("a project can have at most {MAX_FIELDS} fields"),
        ));
    }
    let field: FieldRow = sqlx::query_as(&format!(
        "INSERT INTO project_fields (project_id, name, data_type, position, options, iterations)
         VALUES ($1, $2, $3,
                 (SELECT COALESCE(max(position) + 1, 0) FROM project_fields WHERE project_id = $1),
                 $4, $5)
         RETURNING {}",
        FieldRow::COLUMNS
    ))
    .bind(id)
    .bind(&name)
    .bind(&data_type)
    .bind(options)
    .bind(iterations)
    .fetch_one(&mut *tx)
    .await
    .map_err(name_conflict)?;
    tx.sync(
        &access.scope(),
        M_FIELD,
        field.id,
        SyncAction::Insert,
        &field.sync_json(),
    )
    .await?;
    tx.commit().await?;
    Ok(field)
}

fn name_conflict(e: sqlx::Error) -> ApiError {
    match bgh_core::db::unique_violation(&e).as_deref() {
        Some("project_fields_name_key") => {
            ApiError::invalid_field(FieldError::already_exists("ProjectV2Field", "name"))
        }
        _ => e.into(),
    }
}

async fn load_field(tx: &mut Tx, project_id: i64, field_id: i64) -> ApiResult<FieldRow> {
    sqlx::query_as(&format!(
        "SELECT {} FROM project_fields WHERE id = $1 AND project_id = $2 FOR UPDATE",
        FieldRow::COLUMNS
    ))
    .bind(field_id)
    .bind(project_id)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(ApiError::NotFound)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateBody {
    pub name: Option<String>,
    pub options: Option<Vec<OptionInput>>,
    pub iterations: Option<IterationsInput>,
    pub position: Option<i32>,
}

/// `PATCH /_bgh/projects/{id}/fields/{field_id}`
pub async fn update(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((id, field_id)): Path<(i64, i64)>,
    Json(body): Json<UpdateBody>,
) -> ApiResult<Json<Value>> {
    let access = ProjectAccess::load(&state, Some(&auth), id).await?;
    access.require(Role::Write)?;
    let scope = access.scope();
    let mut tx = Tx::begin(&state).await?;
    let field = load_field(&mut tx, id, field_id).await?;
    let name = match body.name {
        Some(n) => {
            let n = validate_text("ProjectV2Field", "name", Some(&n), 256)?;
            if field.is_builtin() && n != field.name {
                return Err(invalid("name", "built-in fields can't be renamed"));
            }
            Some(n)
        }
        None => None,
    };
    let options = match body.options {
        Some(o) if field.is_select() => Some(normalize_options(o, &field.option_ids())?),
        Some(_) => return Err(invalid("options", "only single select fields have options")),
        None => None,
    };
    let iterations = match body.iterations {
        Some(i) if field.data_type == "iteration" => {
            Some(normalize_iterations(i, field.iterations.as_ref())?)
        }
        Some(_) => {
            return Err(invalid(
                "iterations",
                "only iteration fields have iterations",
            ));
        }
        None => None,
    };
    let updated: FieldRow = sqlx::query_as(&format!(
        "UPDATE project_fields SET name = COALESCE($2, name), options = COALESCE($3, options),
                iterations = COALESCE($4, iterations), position = COALESCE($5, position),
                updated_at = now()
          WHERE id = $1 RETURNING {}",
        FieldRow::COLUMNS
    ))
    .bind(field_id)
    .bind(name)
    .bind(options.as_ref())
    .bind(iterations.as_ref())
    .bind(body.position)
    .fetch_one(&mut *tx)
    .await
    .map_err(name_conflict)?;
    tx.sync(
        &scope,
        M_FIELD,
        field_id,
        SyncAction::Update,
        &updated.sync_json(),
    )
    .await?;

    // Values pointing at removed options / iterations are cleared.
    let valid = if options.is_some() {
        Some(updated.option_ids())
    } else if iterations.is_some() {
        Some(updated.iteration_ids())
    } else {
        None
    };
    if let Some(valid) = valid {
        let affected: Vec<i64> = sqlx::query_scalar(
            "DELETE FROM project_item_values WHERE field_id = $1 AND NOT (value #>> '{}' = ANY($2))
             RETURNING item_id",
        )
        .bind(field_id)
        .bind(&valid)
        .fetch_all(&mut *tx)
        .await?;
        service::sync_items(&mut tx, &scope, &affected).await?;
        // Board views hiding removed columns.
        let views: Vec<ViewRow> = sqlx::query_as(&format!(
            "UPDATE project_views SET hidden_column_ids = ARRAY(
                 SELECT c FROM unnest(hidden_column_ids) c WHERE c = ANY($2)), updated_at = now()
              WHERE project_id = $3 AND column_field_id = $1
                AND NOT (hidden_column_ids <@ $2)
              RETURNING {}",
            ViewRow::COLUMNS
        ))
        .bind(field_id)
        .bind(&valid)
        .bind(id)
        .fetch_all(&mut *tx)
        .await?;
        for v in views {
            tx.sync(&scope, M_VIEW, v.id, SyncAction::Update, &v.sync_json())
                .await?;
        }
    }
    tx.commit().await?;
    Ok(Json(updated.sync_json()))
}

/// `DELETE /_bgh/projects/{id}/fields/{field_id}`
pub async fn delete(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((id, field_id)): Path<(i64, i64)>,
) -> ApiResult<StatusCode> {
    delete_field(&state, &auth, id, field_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Delete a custom field (built-ins: 422). Returns the deleted row.
pub async fn delete_field(
    state: &AppState,
    auth: &AuthContext,
    id: i64,
    field_id: i64,
) -> ApiResult<FieldRow> {
    let access = ProjectAccess::load(state, Some(auth), id).await?;
    access.require(Role::Write)?;
    let scope = access.scope();
    let mut tx = Tx::begin(state).await?;
    let field = load_field(&mut tx, id, field_id).await?;
    if field.is_builtin() {
        return Err(invalid("id", "built-in fields can't be deleted"));
    }
    let affected: Vec<i64> =
        sqlx::query_scalar("SELECT item_id FROM project_item_values WHERE field_id = $1")
            .bind(field_id)
            .fetch_all(&mut *tx)
            .await?;
    let view_ids: Vec<i64> = sqlx::query_scalar(
        "SELECT id FROM project_views WHERE project_id = $1 AND (
             $2 = ANY(visible_field_ids) OR group_by_field_id = $2 OR column_field_id = $2
             OR date_field_id = $2 OR sort_by @> jsonb_build_array(jsonb_build_object('fieldId', $2)))",
    )
    .bind(id)
    .bind(field_id)
    .fetch_all(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM project_fields WHERE id = $1")
        .bind(field_id)
        .execute(&mut *tx)
        .await?;
    tx.sync(
        &scope,
        M_FIELD,
        field_id,
        SyncAction::Delete,
        &json!({"id": field_id}),
    )
    .await?;
    let views: Vec<ViewRow> = sqlx::query_as(&format!(
        "UPDATE project_views SET visible_field_ids = array_remove(visible_field_ids, $2),
                sort_by = (SELECT COALESCE(jsonb_agg(e), '[]'::jsonb) FROM jsonb_array_elements(sort_by) e
                            WHERE (e->>'fieldId')::bigint IS DISTINCT FROM $2),
                updated_at = now()
          WHERE id = ANY($1) RETURNING {}",
        ViewRow::COLUMNS
    ))
    .bind(&view_ids)
    .bind(field_id)
    .fetch_all(&mut *tx)
    .await?;
    for v in views {
        tx.sync(&scope, M_VIEW, v.id, SyncAction::Update, &v.sync_json())
            .await?;
    }
    service::sync_items(&mut tx, &scope, &affected).await?;
    tx.commit().await?;
    Ok(field)
}

/// Validate a value for `field`; `Null` clears it.
pub fn validate_value(field: &FieldRow, value: &Value) -> ApiResult<()> {
    let bad = |msg: &str| {
        Err(ApiError::invalid_field(FieldError::custom(
            "ProjectV2ItemFieldValue",
            "value",
            format!("{}: {msg}", field.name),
        )))
    };
    match field.data_type.as_str() {
        "text" => match value.as_str() {
            Some(s) if s.chars().count() <= 1024 => Ok(()),
            _ => bad("expected a string of at most 1024 characters"),
        },
        "number" => match value.as_f64() {
            Some(n) if n.is_finite() => Ok(()),
            _ => bad("expected a number"),
        },
        "date" => match value
            .as_str()
            .map(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d"))
        {
            Some(Ok(_)) => Ok(()),
            _ => bad("expected a date (YYYY-MM-DD)"),
        },
        "single_select" | "status" => match value.as_str() {
            Some(s) if field.option_ids().iter().any(|o| o == s) => Ok(()),
            _ => bad("unknown option"),
        },
        "iteration" => match value.as_str() {
            Some(s) if field.iteration_ids().iter().any(|o| o == s) => Ok(()),
            _ => bad("unknown iteration"),
        },
        _ => bad("this field's value comes from the issue and can't be set on the item"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_iterations() {
        let v = normalize_iterations(
            IterationsInput {
                start_date: Some("2024-01-01".into()),
                duration: Some(7),
                count: Some(2),
                iterations: None,
            },
            None,
        )
        .unwrap();
        let its = v["iterations"].as_array().unwrap();
        assert_eq!(its.len(), 2);
        assert_eq!(its[1]["startDate"], "2024-01-08");
        assert_eq!(its[1]["title"], "Iteration 2");
    }

    #[test]
    fn rejects_overlap_but_allows_breaks() {
        let mk = |a: &str, b: &str| IterationsInput {
            duration: Some(7),
            iterations: Some(vec![
                IterationInput {
                    id: None,
                    title: None,
                    start_date: Some(a.into()),
                    duration: None,
                },
                IterationInput {
                    id: None,
                    title: None,
                    start_date: Some(b.into()),
                    duration: None,
                },
            ]),
            ..Default::default()
        };
        assert!(normalize_iterations(mk("2024-01-01", "2024-01-05"), None).is_err());
        assert!(normalize_iterations(mk("2024-01-01", "2024-01-15"), None).is_ok());
    }
}
