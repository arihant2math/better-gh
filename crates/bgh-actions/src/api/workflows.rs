//! `/repos/{owner}/{repo}/actions/workflows[/{workflow_id}[/enable|disable|dispatches|timing]]`

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use bgh_core::pagination::Pagination;
use bgh_core::prelude::*;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::wrapped;
use crate::json::workflow_json;
use crate::models::WorkflowRow;
use crate::trigger::{self, DispatchError};

/// Resolve `{workflow_id}`: numeric id or file name (`ci.yml`).
pub async fn find_workflow(
    state: &AppState,
    access: &RepoAccess,
    id: &str,
) -> ApiResult<WorkflowRow> {
    let row: Option<WorkflowRow> = match id.parse::<i64>() {
        Ok(n) => {
            sqlx::query_as(&format!(
                "SELECT {} FROM actions_workflows WHERE repo_id = $1 AND id = $2",
                WorkflowRow::COLUMNS
            ))
            .bind(access.repo.id)
            .bind(n)
            .fetch_optional(&state.db)
            .await?
        }
        Err(_) => {
            let path = if id.contains('/') {
                id.to_string()
            } else {
                format!("{}/{id}", trigger::WORKFLOWS_DIR)
            };
            sqlx::query_as(&format!(
                "SELECT {} FROM actions_workflows WHERE repo_id = $1 AND path = $2",
                WorkflowRow::COLUMNS
            ))
            .bind(access.repo.id)
            .bind(path)
            .fetch_optional(&state.db)
            .await?
        }
    };
    row.ok_or(ApiError::NotFound)
}

async fn sync(state: &AppState, access: &RepoAccess) {
    if let Err(err) = trigger::sync_workflows_if_stale(state, &access.repo).await {
        tracing::warn!(?err, repo_id = access.repo.id, "workflow sync failed");
    }
}

/// `GET /repos/{owner}/{repo}/actions/workflows`
pub async fn list(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<Response> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    sync(&state, &access).await;
    let total: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM actions_workflows WHERE repo_id = $1 AND state <> 'deleted'",
    )
    .bind(access.repo.id)
    .fetch_one(&state.db)
    .await?;
    let rows: Vec<WorkflowRow> = sqlx::query_as(&format!(
        "SELECT {} FROM actions_workflows WHERE repo_id = $1 AND state <> 'deleted'
          ORDER BY id LIMIT $2 OFFSET $3",
        WorkflowRow::COLUMNS
    ))
    .bind(access.repo.id)
    .bind(p.limit())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let items: Vec<Value> = rows
        .iter()
        .map(|w| workflow_json(&state, &access, w))
        .collect();
    Ok(wrapped(&p, total, "workflows", items))
}

/// `GET /repos/{owner}/{repo}/actions/workflows/{workflow_id}`
pub async fn get(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, id)): Path<(String, String, String)>,
) -> ApiResult<Json<Value>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    sync(&state, &access).await;
    let wf = find_workflow(&state, &access, &id).await?;
    Ok(Json(workflow_json(&state, &access, &wf)))
}

async fn set_state(
    state: &AppState,
    auth: &AuthContext,
    owner: &str,
    repo: &str,
    id: &str,
    new_state: &str,
) -> ApiResult<StatusCode> {
    let access = RepoAccess::load(state, Some(auth), owner, repo).await?;
    access.require(Permission::Write)?;
    access.require_not_archived()?;
    sync(state, &access).await;
    let wf = find_workflow(state, &access, id).await?;
    sqlx::query("UPDATE actions_workflows SET state = $2, updated_at = now() WHERE id = $1")
        .bind(wf.id)
        .bind(new_state)
        .execute(&state.db)
        .await?;
    bgh_core::audit::log(
        &state.db,
        Some(&auth.user),
        if new_state == "active" {
            "workflows.enable_workflow"
        } else {
            "workflows.disable_workflow"
        },
        bgh_core::audit::Target::Repo {
            id: access.repo.id,
            org_id: access.owner.is_org().then_some(access.owner.id),
        },
        json!({"workflow": wf.path}),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `PUT .../workflows/{workflow_id}/enable`
pub async fn enable(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, id)): Path<(String, String, String)>,
) -> ApiResult<StatusCode> {
    set_state(&state, &auth, &owner, &repo, &id, "active").await
}

/// `PUT .../workflows/{workflow_id}/disable`
pub async fn disable(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, id)): Path<(String, String, String)>,
) -> ApiResult<StatusCode> {
    set_state(&state, &auth, &owner, &repo, &id, "disabled_manually").await
}

#[derive(Deserialize)]
pub struct DispatchBody {
    #[serde(rename = "ref")]
    pub git_ref: Option<String>,
    #[serde(default)]
    pub inputs: Option<Map<String, Value>>,
    #[serde(default)]
    pub return_run_details: bool,
}

/// `POST .../workflows/{workflow_id}/dispatches` → 204 (or 200 with
/// `return_run_details`).
pub async fn dispatch(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, id)): Path<(String, String, String)>,
    Json(body): Json<DispatchBody>,
) -> ApiResult<Response> {
    use axum::response::IntoResponse;
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    access.require(Permission::Write)?;
    access.require_not_archived()?;
    sync(&state, &access).await;
    let wf = find_workflow(&state, &access, &id).await?;
    if wf.state != "active" {
        return Err(ApiError::unprocessable(
            "Cannot trigger a 'workflow_dispatch' on a disabled workflow",
        ));
    }
    let git_ref = body
        .git_ref
        .filter(|r| !r.is_empty())
        .ok_or_else(|| ApiError::invalid_field(FieldError::missing_field("Workflow", "ref")))?;
    let run_id = trigger::dispatch(
        &state,
        &access.repo,
        &access.owner,
        &wf,
        &git_ref,
        &body.inputs.unwrap_or_default(),
        &auth.user,
    )
    .await
    .map_err(|e| match e {
        DispatchError::Other(err) => ApiError::internal(err),
        other => ApiError::unprocessable(other.to_string()),
    })?;
    if body.return_run_details {
        let api = state.urls.repo(&access.owner.login, &access.repo.name);
        let html = state.urls.repo_html(&access.owner.login, &access.repo.name);
        return Ok(Json(json!({
            "workflow_run_id": run_id,
            "run_url": format!("{api}/actions/runs/{run_id}"),
            "html_url": format!("{html}/actions/runs/{run_id}"),
        }))
        .into_response());
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// `GET .../workflows/{workflow_id}/timing` (self-hosted: nothing billable).
pub async fn timing(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, id)): Path<(String, String, String)>,
) -> ApiResult<Json<Value>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    find_workflow(&state, &access, &id).await?;
    Ok(Json(json!({"billable": {}})))
}
