//! Actions variables at repository, organization and environment level.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use bgh_core::pagination::Pagination;
use bgh_core::prelude::*;
use serde::Deserialize;
use serde_json::{Value, json};

use super::secrets::{Scope, SelectedBody, env_id, repo_scope, selected, set_links};
use super::{load_org, require_org_admin, valid_name, wrapped};
use crate::json::variable_json;
use crate::models::VariableRow;

fn selected_url(state: &AppState, org: &str, name: &str) -> String {
    state.urls.api(&format!(
        "/orgs/{org}/actions/variables/{}/repositories",
        bgh_core::urls::encode_segment(name)
    ))
}

async fn list_scope(
    state: &AppState,
    p: &Pagination,
    scope: Scope,
    org_login: Option<&str>,
) -> ApiResult<Response> {
    let col = scope.column();
    let total: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM actions_variables WHERE {col} = $1"
    ))
    .bind(scope.id())
    .fetch_one(&state.db)
    .await?;
    let rows: Vec<VariableRow> = sqlx::query_as(&format!(
        "SELECT {} FROM actions_variables WHERE {col} = $1 ORDER BY upper(name) LIMIT $2 OFFSET $3",
        VariableRow::COLUMNS
    ))
    .bind(scope.id())
    .bind(p.limit())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let items: Vec<Value> = rows
        .iter()
        .map(|v| variable_json(v, org_login.map(|o| selected_url(state, o, &v.name))))
        .collect();
    Ok(wrapped(p, total, "variables", items))
}

async fn get_scope(state: &AppState, scope: Scope, name: &str) -> ApiResult<VariableRow> {
    sqlx::query_as(&format!(
        "SELECT {} FROM actions_variables WHERE {} = $1 AND upper(name) = upper($2)",
        VariableRow::COLUMNS,
        scope.column()
    ))
    .bind(scope.id())
    .bind(name)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)
}

#[derive(Debug, Deserialize)]
pub struct VariableBody {
    pub name: Option<String>,
    pub value: Option<String>,
    pub visibility: Option<String>,
    pub selected_repository_ids: Option<Vec<i64>>,
}

fn check_visibility(scope: Scope, v: Option<&str>, required: bool) -> ApiResult<Option<String>> {
    match (scope, v) {
        (Scope::Org(_), Some(v)) if matches!(v, "all" | "private" | "selected") => {
            Ok(Some(v.into()))
        }
        (Scope::Org(_), Some(_)) => Err(ApiError::invalid_field(FieldError::invalid(
            "Variable",
            "visibility",
        ))),
        (Scope::Org(_), None) if required => Err(ApiError::invalid_field(
            FieldError::missing_field("Variable", "visibility"),
        )),
        _ => Ok(None),
    }
}

async fn create_scope(
    state: &AppState,
    scope: Scope,
    body: &VariableBody,
) -> ApiResult<StatusCode> {
    let name = body
        .name
        .as_deref()
        .ok_or_else(|| ApiError::invalid_field(FieldError::missing_field("Variable", "name")))?;
    let value = body
        .value
        .as_deref()
        .ok_or_else(|| ApiError::invalid_field(FieldError::missing_field("Variable", "value")))?;
    if !valid_name(name) {
        return Err(ApiError::invalid_field(FieldError::invalid(
            "Variable", "name",
        )));
    }
    let visibility = check_visibility(scope, body.visibility.as_deref(), true)?;
    let mut tx = state.db.begin().await?;
    let id: i64 = sqlx::query_scalar(&format!(
        "INSERT INTO actions_variables ({}, name, value, visibility) VALUES ($1, $2, $3, $4)
         RETURNING id",
        scope.column()
    ))
    .bind(scope.id())
    .bind(name)
    .bind(value)
    .bind(&visibility)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| match bgh_core::db::unique_violation(&e) {
        Some(_) => ApiError::conflict("Already exists - Variable already exists"),
        None => e.into(),
    })?;
    if let (Scope::Org(org), Some(ids)) = (scope, &body.selected_repository_ids) {
        set_links(
            &mut tx,
            "actions_variable_repos",
            "variable_id",
            id,
            org,
            ids,
        )
        .await?;
    }
    tx.commit().await?;
    Ok(StatusCode::CREATED)
}

async fn update_scope(
    state: &AppState,
    scope: Scope,
    name: &str,
    body: &VariableBody,
) -> ApiResult<StatusCode> {
    let existing = get_scope(state, scope, name).await?;
    if let Some(n) = &body.name
        && !valid_name(n)
    {
        return Err(ApiError::invalid_field(FieldError::invalid(
            "Variable", "name",
        )));
    }
    let visibility = check_visibility(scope, body.visibility.as_deref(), false)?;
    let mut tx = state.db.begin().await?;
    sqlx::query(
        "UPDATE actions_variables SET name = coalesce($2, name), value = coalesce($3, value),
                visibility = coalesce($4, visibility), updated_at = now() WHERE id = $1",
    )
    .bind(existing.id)
    .bind(&body.name)
    .bind(&body.value)
    .bind(&visibility)
    .execute(&mut *tx)
    .await
    .map_err(|e| match bgh_core::db::unique_violation(&e) {
        Some(_) => ApiError::conflict("Already exists - Variable already exists"),
        None => e.into(),
    })?;
    if let (Scope::Org(org), Some(ids)) = (scope, &body.selected_repository_ids) {
        set_links(
            &mut tx,
            "actions_variable_repos",
            "variable_id",
            existing.id,
            org,
            ids,
        )
        .await?;
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn delete_scope(state: &AppState, scope: Scope, name: &str) -> ApiResult<StatusCode> {
    let n = sqlx::query(&format!(
        "DELETE FROM actions_variables WHERE {} = $1 AND upper(name) = upper($2)",
        scope.column()
    ))
    .bind(scope.id())
    .bind(name)
    .execute(&state.db)
    .await?
    .rows_affected();
    if n == 0 {
        return Err(ApiError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}

// ----- repository ------------------------------------------------------------

pub async fn repo_list(
    State(state): State<AppState>,
    auth: RequireUser,
    p: Pagination,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<Response> {
    let a = repo_scope(&state, &auth, &owner, &repo).await?;
    list_scope(&state, &p, Scope::Repo(a.repo.id), None).await
}

pub async fn repo_create(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo)): Path<(String, String)>,
    Json(body): Json<VariableBody>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let a = repo_scope(&state, &auth, &owner, &repo).await?;
    Ok((
        create_scope(&state, Scope::Repo(a.repo.id), &body).await?,
        Json(json!({})),
    ))
}

pub async fn repo_get(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, name)): Path<(String, String, String)>,
) -> ApiResult<Json<Value>> {
    let a = repo_scope(&state, &auth, &owner, &repo).await?;
    let v = get_scope(&state, Scope::Repo(a.repo.id), &name).await?;
    Ok(Json(variable_json(&v, None)))
}

pub async fn repo_update(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, name)): Path<(String, String, String)>,
    Json(body): Json<VariableBody>,
) -> ApiResult<StatusCode> {
    let a = repo_scope(&state, &auth, &owner, &repo).await?;
    update_scope(&state, Scope::Repo(a.repo.id), &name, &body).await
}

pub async fn repo_delete(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, name)): Path<(String, String, String)>,
) -> ApiResult<StatusCode> {
    let a = repo_scope(&state, &auth, &owner, &repo).await?;
    delete_scope(&state, Scope::Repo(a.repo.id), &name).await
}

/// `GET /repos/{o}/{r}/actions/organization-variables`
pub async fn repo_org_variables(
    State(state): State<AppState>,
    auth: RequireUser,
    p: Pagination,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<Response> {
    let a = repo_scope(&state, &auth, &owner, &repo).await?;
    let sql = "FROM actions_variables s WHERE s.org_id = $1
           AND (s.visibility = 'all' OR (s.visibility = 'private' AND $3)
                OR (s.visibility = 'selected' AND EXISTS (
                     SELECT 1 FROM actions_variable_repos l WHERE l.variable_id = s.id AND l.repo_id = $2)))";
    let total: i64 = sqlx::query_scalar(&format!("SELECT count(*) {sql}"))
        .bind(a.owner.id)
        .bind(a.repo.id)
        .bind(a.repo.is_private())
        .fetch_one(&state.db)
        .await?;
    let rows: Vec<VariableRow> = sqlx::query_as(&format!(
        "SELECT {} {sql} ORDER BY upper(s.name) LIMIT $4 OFFSET $5",
        bgh_core::models::db::prefixed("s", VariableRow::COLUMNS)
    ))
    .bind(a.owner.id)
    .bind(a.repo.id)
    .bind(a.repo.is_private())
    .bind(p.limit())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let items: Vec<Value> = rows
        .iter()
        .map(|v| {
            let mut j = variable_json(v, None);
            if let Some(o) = j.as_object_mut() {
                o.remove("visibility");
                o.remove("selected_repositories_url");
            }
            j
        })
        .collect();
    Ok(wrapped(&p, total, "variables", items))
}

// ----- environment -----------------------------------------------------------

pub async fn env_list(
    State(state): State<AppState>,
    auth: RequireUser,
    p: Pagination,
    Path((owner, repo, env)): Path<(String, String, String)>,
) -> ApiResult<Response> {
    let a = repo_scope(&state, &auth, &owner, &repo).await?;
    let e = env_id(&state, &a, &env).await?;
    list_scope(&state, &p, Scope::Env(e), None).await
}

pub async fn env_create(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, env)): Path<(String, String, String)>,
    Json(body): Json<VariableBody>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let a = repo_scope(&state, &auth, &owner, &repo).await?;
    let e = env_id(&state, &a, &env).await?;
    Ok((
        create_scope(&state, Scope::Env(e), &body).await?,
        Json(json!({})),
    ))
}

pub async fn env_get(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, env, name)): Path<(String, String, String, String)>,
) -> ApiResult<Json<Value>> {
    let a = repo_scope(&state, &auth, &owner, &repo).await?;
    let e = env_id(&state, &a, &env).await?;
    Ok(Json(variable_json(
        &get_scope(&state, Scope::Env(e), &name).await?,
        None,
    )))
}

pub async fn env_update(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, env, name)): Path<(String, String, String, String)>,
    Json(body): Json<VariableBody>,
) -> ApiResult<StatusCode> {
    let a = repo_scope(&state, &auth, &owner, &repo).await?;
    let e = env_id(&state, &a, &env).await?;
    update_scope(&state, Scope::Env(e), &name, &body).await
}

pub async fn env_delete(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, env, name)): Path<(String, String, String, String)>,
) -> ApiResult<StatusCode> {
    let a = repo_scope(&state, &auth, &owner, &repo).await?;
    let e = env_id(&state, &a, &env).await?;
    delete_scope(&state, Scope::Env(e), &name).await
}

// ----- organization ----------------------------------------------------------

pub async fn org_list(
    State(state): State<AppState>,
    auth: RequireUser,
    p: Pagination,
    Path(org): Path<String>,
) -> ApiResult<Response> {
    let o = load_org(&state, &org).await?;
    require_org_admin(&state, &auth, &o).await?;
    list_scope(&state, &p, Scope::Org(o.id), Some(&o.login)).await
}

pub async fn org_create(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(org): Path<String>,
    Json(body): Json<VariableBody>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let o = load_org(&state, &org).await?;
    require_org_admin(&state, &auth, &o).await?;
    Ok((
        create_scope(&state, Scope::Org(o.id), &body).await?,
        Json(json!({})),
    ))
}

pub async fn org_get(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((org, name)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let o = load_org(&state, &org).await?;
    require_org_admin(&state, &auth, &o).await?;
    let v = get_scope(&state, Scope::Org(o.id), &name).await?;
    Ok(Json(variable_json(
        &v,
        Some(selected_url(&state, &o.login, &v.name)),
    )))
}

pub async fn org_update(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((org, name)): Path<(String, String)>,
    Json(body): Json<VariableBody>,
) -> ApiResult<StatusCode> {
    let o = load_org(&state, &org).await?;
    require_org_admin(&state, &auth, &o).await?;
    update_scope(&state, Scope::Org(o.id), &name, &body).await
}

pub async fn org_delete(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((org, name)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    let o = load_org(&state, &org).await?;
    require_org_admin(&state, &auth, &o).await?;
    delete_scope(&state, Scope::Org(o.id), &name).await
}

pub async fn org_repos(
    State(state): State<AppState>,
    auth: RequireUser,
    p: Pagination,
    Path((org, name)): Path<(String, String)>,
) -> ApiResult<Response> {
    selected::list(&state, &auth, &selected::VARIABLES, &org, &name, &p).await
}

pub async fn org_set_repos(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((org, name)): Path<(String, String)>,
    Json(body): Json<SelectedBody>,
) -> ApiResult<StatusCode> {
    selected::set(
        &state,
        &auth,
        &selected::VARIABLES,
        &org,
        &name,
        &body.selected_repository_ids,
    )
    .await
}

pub async fn org_add_repo(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((org, name, repo_id)): Path<(String, String, i64)>,
) -> ApiResult<StatusCode> {
    selected::add_or_remove(
        &state,
        &auth,
        &selected::VARIABLES,
        &org,
        &name,
        repo_id,
        true,
    )
    .await
}

pub async fn org_remove_repo(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((org, name, repo_id)): Path<(String, String, i64)>,
) -> ApiResult<StatusCode> {
    selected::add_or_remove(
        &state,
        &auth,
        &selected::VARIABLES,
        &org,
        &name,
        repo_id,
        false,
    )
    .await
}
