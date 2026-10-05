//! Private JSON endpoints for the web UI (`/_bgh/actions/repos/...`): data
//! the GitHub REST API doesn't expose but the Actions pages need.
//!
//! * `GET …/runs/{run_id}/graph`: the run's job graph from the stored
//!   workflow definition (`needs` edges, matrix jobs) plus the job key of
//!   every job row of the run, so the client can place REST jobs on it.
//! * `GET …/workflows/{workflow_id}/dispatch?ref=`: `workflow_dispatch`
//!   inputs of the workflow file at `ref` (default branch when omitted).

use axum::Router;
use axum::extract::{Query, State};
use axum::routing::get;
use bgh_core::prelude::*;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::api::runs::load_run;
use crate::api::workflows::find_workflow;
use crate::trigger;
use crate::workflow::Workflow;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/_bgh/actions/repos/{owner}/{repo}/runs/{run_id}/graph",
            get(run_graph),
        )
        .route(
            "/_bgh/actions/repos/{owner}/{repo}/workflows/{workflow_id}/dispatch",
            get(dispatch_form),
        )
}

async fn run_graph(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, run_id)): Path<(String, String, i64)>,
) -> ApiResult<Json<Value>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let run = load_run(&state, &access, run_id).await?;
    let def: Option<Workflow> = serde_json::from_value(run.workflow_def.clone()).ok();
    let jobs: Vec<Value> = def
        .iter()
        .flat_map(|d| d.jobs.iter())
        .map(|(key, job)| {
            json!({
                "key": key,
                "name": job.name.clone().unwrap_or_else(|| key.clone()),
                "needs": job.needs,
                "matrix": job.strategy.as_ref().is_some_and(|s| s.matrix.is_some()),
                "uses": job.uses,
            })
        })
        .collect();
    let keys: Vec<(i64, String)> =
        sqlx::query_as("SELECT id, job_key FROM actions_jobs WHERE run_id = $1 ORDER BY id")
            .bind(run.id)
            .fetch_all(&state.db)
            .await?;
    let job_keys: Map<String, Value> = keys
        .into_iter()
        .map(|(id, key)| (id.to_string(), Value::String(key)))
        .collect();
    Ok(Json(json!({
        "run_id": run.id,
        "workflow_name": def.as_ref().and_then(|d| d.name.clone()).unwrap_or(run.name),
        "jobs": jobs,
        "job_keys": job_keys,
    })))
}

#[derive(Deserialize)]
struct DispatchQuery {
    #[serde(rename = "ref")]
    git_ref: Option<String>,
}

async fn dispatch_form(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, id)): Path<(String, String, String)>,
    Query(q): Query<DispatchQuery>,
) -> ApiResult<Json<Value>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let wf = find_workflow(&state, &access, &id).await?;
    let git_ref = q
        .git_ref
        .filter(|r| !r.is_empty())
        .unwrap_or_else(|| access.repo.default_branch.clone());
    let candidates = if git_ref.starts_with("refs/") {
        vec![git_ref.clone()]
    } else {
        vec![
            format!("refs/heads/{git_ref}"),
            format!("refs/tags/{git_ref}"),
        ]
    };
    let mut sha = None;
    for c in candidates {
        if let Some(s) = trigger::store(&state)
            .read(access.repo.id, move |g| Ok(g.resolve_commit(&c).ok()))
            .await
            .map_err(ApiError::internal)?
        {
            sha = Some(s);
            break;
        }
    }
    let mut out = json!({
        "ref": git_ref,
        "sha": sha,
        "path": wf.path,
        "dispatchable": false,
        "inputs": [],
        "error": null,
    });
    let Some(sha) = sha else {
        out["error"] = json!(format!("No ref found for: {git_ref}"));
        return Ok(Json(out));
    };
    let files = trigger::load_workflows(&state, access.repo.id, &sha)
        .await
        .map_err(ApiError::internal)?;
    let Some(file) = files.into_iter().find(|f| f.path == wf.path) else {
        out["error"] = json!(format!("{} does not exist at {git_ref}", wf.path));
        return Ok(Json(out));
    };
    match file.def {
        Err(e) => out["error"] = json!(e),
        Ok(def) => {
            if let Some(trig) = def.on.get("workflow_dispatch") {
                out["dispatchable"] = json!(true);
                out["inputs"] = trig
                    .inputs
                    .iter()
                    .map(|(name, i)| {
                        json!({
                            "name": name,
                            "description": i.description,
                            "required": i.required,
                            "default": i.default,
                            "type": i.r#type,
                            "options": i.options,
                        })
                    })
                    .collect();
            } else {
                out["error"] = json!("This workflow has no workflow_dispatch trigger.");
            }
        }
    }
    Ok(Json(out))
}
