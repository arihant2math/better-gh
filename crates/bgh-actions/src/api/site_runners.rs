//! Site administration of self-hosted runners (`/_bgh/admin/actions/...`,
//! site admins only): every runner of the instance, site runner
//! registration (both `repo_id` and `org_id` NULL), JIT configs, the job
//! queue, and site runner groups ([`super::runner_groups::site_routes`]).

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::{delete, get, post};
use bgh_core::pagination::Pagination;
use bgh_core::prelude::*;
use bgh_core::time::Timestamp;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::FromRow;

use super::runners::{JitBody, jit_inner, mint_token};
use super::wrapped;
use crate::json::runner_json;
use crate::models::RunnerRow;

pub fn routes() -> Router<AppState> {
    const A: &str = "/_bgh/admin/actions";
    Router::new()
        .route(&format!("{A}/runners"), get(list))
        .route(
            &format!("{A}/runners/registration-token"),
            post(registration_token),
        )
        .route(&format!("{A}/runners/remove-token"), post(remove_token))
        .route(
            &format!("{A}/runners/generate-jitconfig"),
            post(generate_jitconfig),
        )
        .route(&format!("{A}/runners/{{runner_id}}"), delete(remove))
        .route(&format!("{A}/queue"), get(queue))
        .merge(super::runner_groups::site_routes())
}

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    /// `online` | `offline` | `busy` | `idle`
    pub status: Option<String>,
    /// Substring of the runner name or a label.
    pub q: Option<String>,
}

#[derive(FromRow)]
struct SiteRunnerRow {
    #[sqlx(flatten)]
    runner: RunnerRow,
    owner_login: Option<String>,
    repo_name: Option<String>,
    group_name: Option<String>,
}

/// Seen within the last two minutes (as [`RunnerRow::online`]).
const ONLINE: &str = "a.last_seen_at > now() - interval '2 minutes'";

pub async fn list(
    State(state): State<AppState>,
    _admin: RequireSiteAdmin,
    p: Pagination,
    Query(q): Query<ListQuery>,
) -> ApiResult<Response> {
    let status = match q.status.as_deref().unwrap_or("") {
        "" | "all" => "true".to_string(),
        "online" => ONLINE.to_string(),
        "offline" => format!("NOT coalesce({ONLINE}, false)"),
        "busy" => "a.busy".to_string(),
        "idle" => format!("NOT a.busy AND {ONLINE}"),
        _ => {
            return Err(ApiError::invalid_field(FieldError::invalid(
                "Runner", "status",
            )));
        }
    };
    let search = q.q.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let where_ = format!(
        "{status} AND ($1::text IS NULL OR strpos(lower(a.name), lower($1)) > 0
             OR lower($1) = ANY(a.labels) OR lower($1) = ANY(a.system_labels))"
    );
    let total: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM actions_runners a WHERE {where_}"
    ))
    .bind(search)
    .fetch_one(&state.db)
    .await?;
    let rows: Vec<SiteRunnerRow> = sqlx::query_as(&format!(
        "SELECT {cols}, coalesce(ou.login, ru.login) AS owner_login, r.name AS repo_name,
                g.name AS group_name
           FROM actions_runners a
           LEFT JOIN repositories r ON r.id = a.repo_id
           LEFT JOIN users ru ON ru.id = r.owner_id
           LEFT JOIN users ou ON ou.id = a.org_id
           LEFT JOIN actions_runner_groups g ON g.id = a.runner_group_id
          WHERE {where_}
          ORDER BY a.id LIMIT $2 OFFSET $3",
        cols = bgh_core::models::db::prefixed("a", RunnerRow::COLUMNS)
    ))
    .bind(search)
    .bind(p.limit())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let items: Vec<Value> = rows
        .iter()
        .map(|row| {
            let r = &row.runner;
            let mut v = runner_json(r);
            let scope = match (r.repo_id, r.org_id) {
                (Some(_), _) => "repo",
                (None, Some(_)) => "org",
                (None, None) => "site",
            };
            v["scope"] = json!(scope);
            v["owner"] = json!(row.owner_login);
            v["repository"] = json!(match (&row.owner_login, &row.repo_name) {
                (Some(o), Some(n)) => Some(format!("{o}/{n}")),
                _ => None,
            });
            v["builtin"] = json!(r.builtin);
            v["arch"] = json!(r.arch);
            v["runner_group_name"] = json!(row.group_name);
            v["last_seen_at"] = json!(r.last_seen_at.map(Timestamp));
            v["created_at"] = json!(Timestamp(r.created_at));
            v
        })
        .collect();
    Ok(wrapped(&p, total, "runners", items))
}

pub async fn remove(
    State(state): State<AppState>,
    admin: RequireSiteAdmin,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    let r: RunnerRow = sqlx::query_as(&format!(
        "SELECT {} FROM actions_runners WHERE id = $1",
        RunnerRow::COLUMNS
    ))
    .bind(id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)?;
    if r.builtin {
        return Err(ApiError::unprocessable(
            "The built-in runner cannot be removed; disable it with BGH_ACTIONS_BUILTIN_RUNNER=false",
        ));
    }
    if r.busy {
        return Err(ApiError::unprocessable(format!(
            "Bad request - Runner \"{}\" is still running a job\"",
            r.name
        )));
    }
    let mut tx = Tx::begin(&state).await?;
    sqlx::query("DELETE FROM actions_runners WHERE id = $1")
        .bind(r.id)
        .execute(&mut *tx)
        .await?;
    bgh_core::audit::log(
        &mut *tx,
        Some(&admin.user),
        "runner.remove",
        bgh_core::audit::Target::Site,
        json!({"runner": r.name, "runner_id": r.id}),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn registration_token(
    State(state): State<AppState>,
    admin: RequireSiteAdmin,
) -> ApiResult<(StatusCode, Json<Value>)> {
    bgh_core::audit::log(
        &state.db,
        Some(&admin.user),
        "runner.registration_token",
        bgh_core::audit::Target::Site,
        json!({}),
    )
    .await?;
    mint_token(&state, "registration", None, None).await
}

pub async fn remove_token(
    State(state): State<AppState>,
    _admin: RequireSiteAdmin,
) -> ApiResult<(StatusCode, Json<Value>)> {
    mint_token(&state, "remove", None, None).await
}

pub async fn generate_jitconfig(
    State(state): State<AppState>,
    _admin: RequireSiteAdmin,
    Json(body): Json<JitBody>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let url = state.urls.html("/");
    jit_inner(&state, None, None, url, body).await
}

#[derive(Debug, Deserialize)]
pub struct QueueQuery {
    /// `queued` | `in_progress` (default: both).
    pub status: Option<String>,
}

#[derive(FromRow)]
struct QueueRow {
    id: i64,
    run_id: i64,
    name: String,
    status: String,
    labels: Vec<String>,
    owner: String,
    repo: String,
    workflow_name: String,
    created_at: DateTime<Utc>,
    started_at: Option<DateTime<Utc>>,
    runner_name: Option<String>,
}

pub async fn queue(
    State(state): State<AppState>,
    _admin: RequireSiteAdmin,
    p: Pagination,
    Query(q): Query<QueueQuery>,
) -> ApiResult<Response> {
    let statuses: Vec<&str> = match q.status.as_deref() {
        None | Some("") => vec!["queued", "in_progress"],
        Some(s @ ("queued" | "in_progress")) => vec![s],
        Some(_) => {
            return Err(ApiError::invalid_field(FieldError::invalid(
                "Job", "status",
            )));
        }
    };
    let total: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM actions_jobs
          WHERE status IN ('queued', 'in_progress') AND status = ANY($1)",
    )
    .bind(&statuses)
    .fetch_one(&state.db)
    .await?;
    let rows: Vec<QueueRow> = sqlx::query_as(
        "SELECT j.id, j.run_id, j.name, j.status, j.labels, u.login AS owner, r.name AS repo,
                ru.name AS workflow_name, j.created_at, j.started_at, j.runner_name
           FROM actions_jobs j
           JOIN repositories r ON r.id = j.repo_id
           JOIN users u ON u.id = r.owner_id
           JOIN actions_runs ru ON ru.id = j.run_id
          WHERE j.status IN ('queued', 'in_progress') AND j.status = ANY($1)
          ORDER BY j.id LIMIT $2 OFFSET $3",
    )
    .bind(&statuses)
    .bind(p.limit())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let items: Vec<Value> = rows
        .iter()
        .map(|j| {
            json!({
                "id": j.id,
                "run_id": j.run_id,
                "name": j.name,
                "status": j.status,
                "labels": j.labels,
                "repository": format!("{}/{}", j.owner, j.repo),
                "workflow_name": j.workflow_name,
                "created_at": Timestamp(j.created_at),
                "started_at": j.started_at.map(Timestamp),
                "runner_name": j.runner_name,
                "html_url": state.urls.html(&format!(
                    "/{}/{}/actions/runs/{}/job/{}",
                    j.owner, j.repo, j.run_id, j.id
                )),
            })
        })
        .collect();
    Ok(wrapped(&p, total, "jobs", items))
}
