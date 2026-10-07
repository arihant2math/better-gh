//! Deployments REST API:
//!
//! * `GET`/`POST /repos/{o}/{r}/deployments` (filters `sha`, `ref`, `task`,
//!   `environment`);
//! * `GET`/`DELETE /repos/{o}/{r}/deployments/{id}` (only inactive
//!   deployments, or the last one of an environment, can be deleted);
//! * `GET`/`POST /repos/{o}/{r}/deployments/{id}/statuses` and
//!   `GET …/statuses/{status_id}`.
//!
//! Creation follows GitHub: `auto_merge` (default true) merges the default
//! branch into a branch `ref` that is behind it and answers 202 instead of
//! creating the deployment (409 on conflicts); `required_contexts`
//! (default: every context posted to the commit) must all be successful,
//! else 409. The writes go through [`crate::deployments`].

use std::collections::{BTreeMap, HashMap};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use bgh_core::deployments::{
    self as dep, DeploymentRow, DeploymentStatusRow, deployment_json, status_json,
};
use bgh_core::pagination::Pagination;
use bgh_core::prelude::*;
use bgh_core::views;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::deployments::{self, NewDeployment, NewStatus, valid_environment};

/// Max length of a status description (GitHub truncates at 140).
const MAX_DESCRIPTION: usize = 140;

async fn users(
    state: &AppState,
    ids: impl IntoIterator<Item = Option<i64>>,
) -> ApiResult<HashMap<i64, db::User>> {
    views::users_by_id(state, ids).await
}

fn render(
    state: &AppState,
    a: &RepoAccess,
    d: &DeploymentRow,
    u: &HashMap<i64, db::User>,
) -> Value {
    deployment_json(
        &state.urls,
        &a.owner.login,
        &a.repo.name,
        d,
        d.creator_id.and_then(|i| u.get(&i)),
    )
}

fn render_status(
    state: &AppState,
    a: &RepoAccess,
    s: &DeploymentStatusRow,
    u: &HashMap<i64, db::User>,
) -> Value {
    status_json(
        &state.urls,
        &a.owner.login,
        &a.repo.name,
        s,
        s.creator_id.and_then(|i| u.get(&i)),
    )
}

async fn load(state: &AppState, a: &RepoAccess, id: i64) -> ApiResult<DeploymentRow> {
    DeploymentRow::find(&state.db, a.repo.id, id)
        .await?
        .ok_or(ApiError::NotFound)
}

// ----- deployments -----------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct ListQuery {
    pub sha: Option<String>,
    #[serde(rename = "ref")]
    pub git_ref: Option<String>,
    pub task: Option<String>,
    pub environment: Option<String>,
}

/// `GET /repos/{o}/{r}/deployments`, newest first.
pub async fn list(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo)): Path<(String, String)>,
    Query(q): Query<ListQuery>,
) -> ApiResult<Page<Value>> {
    let a = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let rows: Vec<DeploymentRow> = sqlx::query_as(&format!(
        "SELECT {} FROM deployments
          WHERE repo_id = $1
            AND ($2::text IS NULL OR sha = $2)
            AND ($3::text IS NULL OR ref = $3)
            AND ($4::text IS NULL OR task = $4)
            AND ($5::text IS NULL OR lower(environment) = lower($5))
          ORDER BY id DESC LIMIT $6 OFFSET $7",
        DeploymentRow::COLUMNS
    ))
    .bind(a.repo.id)
    .bind(q.sha.as_deref().filter(|s| !s.is_empty()))
    .bind(q.git_ref.as_deref().filter(|s| !s.is_empty()))
    .bind(q.task.as_deref().filter(|s| !s.is_empty()))
    .bind(q.environment.as_deref().filter(|s| !s.is_empty()))
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let u = users(&state, rows.iter().map(|d| d.creator_id)).await?;
    Ok(p.page(rows).map(|d| render(&state, &a, &d, &u)))
}

/// `GET /repos/{o}/{r}/deployments/{id}`.
pub async fn get(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, id)): Path<(String, String, i64)>,
) -> ApiResult<Json<Value>> {
    let a = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let d = load(&state, &a, id).await?;
    let u = users(&state, [d.creator_id]).await?;
    Ok(Json(render(&state, &a, &d, &u)))
}

#[derive(Debug, Default, Deserialize)]
pub struct CreateBody {
    #[serde(rename = "ref")]
    pub git_ref: Option<String>,
    pub task: Option<String>,
    pub auto_merge: Option<bool>,
    /// `None` = every context; `Some([])` = skip the check.
    pub required_contexts: Option<Vec<String>>,
    pub payload: Option<Value>,
    pub environment: Option<String>,
    pub description: Option<String>,
    pub transient_environment: Option<bool>,
    pub production_environment: Option<bool>,
}

/// Latest state of every status context and check run on `sha`
/// (`success` for successful, neutral or skipped check runs).
async fn context_states(
    state: &AppState,
    repo_id: i64,
    sha: &str,
) -> ApiResult<BTreeMap<String, String>> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT context, state FROM (
             SELECT DISTINCT ON (context) context, state
               FROM commit_statuses WHERE repo_id = $1 AND sha = $2
              ORDER BY context, id DESC) s
         UNION ALL
         SELECT name, CASE
                  WHEN status <> 'completed' THEN 'pending'
                  WHEN conclusion IN ('success', 'neutral', 'skipped') THEN 'success'
                  ELSE 'failure' END
           FROM (SELECT DISTINCT ON (name) name, status, conclusion
                   FROM check_runs WHERE repo_id = $1 AND head_sha = $2
                  ORDER BY name, id DESC) c",
    )
    .bind(repo_id)
    .bind(sha)
    .fetch_all(&state.db)
    .await?;
    let mut out = BTreeMap::new();
    for (context, st) in rows {
        // A context reported both ways counts as successful only if both are.
        let e = out.entry(context).or_insert_with(|| st.clone());
        if st != "success" {
            *e = st;
        }
    }
    Ok(out)
}

/// 409 body for unmet `required_contexts` (GitHub's shape).
fn contexts_conflict(git_ref: &str, contexts: Vec<Value>) -> Response {
    (
        StatusCode::CONFLICT,
        Json(json!({
            "message": format!("Conflict: Commit status checks failed for {git_ref}."),
            "errors": [{
                "contexts": contexts,
                "resource": "Deployment",
                "field": "required_contexts",
                "code": "invalid",
            }],
            "documentation_url": "https://docs.github.com/rest/deployments/deployments#create-a-deployment",
        })),
    )
        .into_response()
}

/// `auto_merge`: merge the default branch into branch `git_ref` when it
/// is behind. `Ok(true)` when a merge commit was written.
async fn auto_merge(
    state: &AppState,
    a: &RepoAccess,
    user: &db::User,
    git_ref: &str,
) -> ApiResult<bool> {
    let default = a.repo.default_branch.clone();
    if git_ref == default || git_ref.starts_with("refs/") && !git_ref.starts_with("refs/heads/") {
        return Ok(false);
    }
    let branch = git_ref.strip_prefix("refs/heads/").unwrap_or(git_ref);
    if branch == default {
        return Ok(false);
    }
    let git = bgh_repos::store(state).cli(a.repo.id)?;
    let Some(head) = git.resolve_commit(&format!("refs/heads/{branch}")).await? else {
        return Ok(false); // a tag or SHA: nothing to merge into
    };
    let Some(base) = git.resolve_commit(&format!("refs/heads/{default}")).await? else {
        return Ok(false);
    };
    if git.is_ancestor(&base, &head).await? {
        return Ok(false);
    }
    let tree = match git.merge_trees(&head, &base).await? {
        bgh_git::ops::MergeOutcome::Clean { tree } => tree,
        bgh_git::ops::MergeOutcome::Conflicts { .. } => {
            return Err(ApiError::conflict(format!(
                "Conflict merging {default} into {branch}."
            )));
        }
    };
    let me = bgh_repos::identity::default_identity(state, user).await?;
    let message = format!("Auto-merged {default} into {branch} on deployment.");
    let sha = git
        .commit_tree(&tree, &[head.clone(), base], &message, &me, &me)
        .await?;
    bgh_repos::refs::write_ref(
        state,
        a,
        user,
        &format!("refs/heads/{branch}"),
        Some(&head),
        Some(&sha),
        false,
    )
    .await?;
    Ok(true)
}

/// `POST /repos/{o}/{r}/deployments`: 201 deployment, 202 after an
/// auto-merge, 409 on merge conflicts or failed required contexts.
pub async fn create(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo)): Path<(String, String)>,
    Json(body): Json<CreateBody>,
) -> ApiResult<Response> {
    let a = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    a.require(Permission::Write)?;
    a.require_not_archived()?;
    let git_ref = body
        .git_ref
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .ok_or_else(|| ApiError::invalid_field(FieldError::missing_field("Deployment", "ref")))?
        .to_string();
    let environment = body
        .environment
        .clone()
        .unwrap_or_else(|| "production".into());
    if !valid_environment(&environment) {
        return Err(ApiError::invalid_field(FieldError::invalid(
            "Deployment",
            "environment",
        )));
    }
    let task = body
        .task
        .clone()
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| "deploy".into());
    if task.len() > 255 {
        return Err(ApiError::invalid_field(FieldError::invalid(
            "Deployment",
            "task",
        )));
    }
    let payload = match body.payload.clone() {
        None | Some(Value::Null) => json!({}),
        // A JSON string holding an object is stored as that object.
        Some(Value::String(s)) => match serde_json::from_str::<Value>(&s) {
            Ok(v @ Value::Object(_)) => v,
            _ => Value::String(s),
        },
        Some(v @ Value::Object(_)) => v,
        Some(_) => {
            return Err(ApiError::invalid_field(FieldError::invalid(
                "Deployment",
                "payload",
            )));
        }
    };

    let git = bgh_repos::store(&state).cli(a.repo.id)?;
    let resolve = |r: String| {
        let git = &git;
        async move {
            if let Some(b) = r.strip_prefix("refs/") {
                return git.resolve_commit(&format!("refs/{b}")).await;
            }
            for cand in [format!("refs/heads/{r}"), format!("refs/tags/{r}")] {
                if let Some(sha) = git.resolve_commit(&cand).await? {
                    return Ok(Some(sha));
                }
            }
            if r.len() >= 7 && r.len() <= 40 && r.bytes().all(|b| b.is_ascii_hexdigit()) {
                return git.resolve_commit(&r).await;
            }
            Ok(None)
        }
    };
    let not_found = || ApiError::unprocessable(format!("No ref found for: {git_ref}"));
    resolve(git_ref.clone()).await?.ok_or_else(not_found)?;

    if body.auto_merge.unwrap_or(true) && auto_merge(&state, &a, &auth.user, &git_ref).await? {
        let default = &a.repo.default_branch;
        let branch = git_ref.strip_prefix("refs/heads/").unwrap_or(&git_ref);
        return Ok((
            StatusCode::ACCEPTED,
            Json(json!({
                "message": format!("Auto-merged {default} into {branch} on deployment.")
            })),
        )
            .into_response());
    }
    let sha = resolve(git_ref.clone()).await?.ok_or_else(not_found)?;

    let states = context_states(&state, a.repo.id, &sha).await?;
    let required: Vec<String> = match &body.required_contexts {
        Some(list) => list.clone(),
        None => states.keys().cloned().collect(),
    };
    if required
        .iter()
        .any(|c| states.get(c).map(String::as_str) != Some("success"))
    {
        let contexts = required
            .iter()
            .map(|c| {
                json!({
                    "context": c,
                    "state": states.get(c).map(String::as_str).unwrap_or("missing"),
                })
            })
            .collect();
        return Ok(contexts_conflict(&git_ref, contexts));
    }

    let production = body
        .production_environment
        .unwrap_or(environment == "production");
    let mut tx = Tx::begin(&state).await?;
    let d = deployments::create_deployment(
        &mut tx,
        a.repo.id,
        Some(auth.user.id),
        NewDeployment {
            git_ref,
            sha,
            task,
            payload,
            environment,
            description: body.description.clone(),
            transient_environment: body.transient_environment.unwrap_or(false),
            production_environment: production,
            run_id: None,
            job_id: None,
        },
    )
    .await?;
    tx.commit().await?;
    let u = HashMap::from([(auth.user.id, auth.user.clone())]);
    Ok((StatusCode::CREATED, Json(render(&state, &a, &d, &u))).into_response())
}

/// `DELETE /repos/{o}/{r}/deployments/{id}`: 204; 422 for an active
/// deployment unless it is the only one in its environment.
pub async fn delete(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, id)): Path<(String, String, i64)>,
) -> ApiResult<StatusCode> {
    let a = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    a.require(Permission::Write)?;
    a.require_not_archived()?;
    let d = load(&state, &a, id).await?;
    if d.state.as_deref() != Some("inactive") {
        let others: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM deployments
              WHERE repo_id = $1 AND lower(environment) = lower($2) AND id <> $3",
        )
        .bind(a.repo.id)
        .bind(&d.environment)
        .bind(d.id)
        .fetch_one(&state.db)
        .await?;
        if others > 0 {
            return Err(ApiError::unprocessable(
                "We cannot delete an active deployment unless it is the only deployment in a given environment.",
            ));
        }
    }
    sqlx::query("DELETE FROM deployments WHERE id = $1")
        .bind(d.id)
        .execute(&state.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

// ----- statuses ----------------------------------------------------------------

/// `GET /repos/{o}/{r}/deployments/{id}/statuses`, newest first.
pub async fn list_statuses(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo, id)): Path<(String, String, i64)>,
) -> ApiResult<Page<Value>> {
    let a = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let d = load(&state, &a, id).await?;
    let rows: Vec<DeploymentStatusRow> = sqlx::query_as(&format!(
        "SELECT {} FROM deployment_statuses WHERE deployment_id = $1
          ORDER BY id DESC LIMIT $2 OFFSET $3",
        DeploymentStatusRow::COLUMNS
    ))
    .bind(d.id)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let u = users(&state, rows.iter().map(|s| s.creator_id)).await?;
    Ok(p.page(rows).map(|s| render_status(&state, &a, &s, &u)))
}

/// `GET /repos/{o}/{r}/deployments/{id}/statuses/{status_id}`.
pub async fn get_status(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, id, status_id)): Path<(String, String, i64, i64)>,
) -> ApiResult<Json<Value>> {
    let a = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let d = load(&state, &a, id).await?;
    let s = DeploymentStatusRow::find(&state.db, d.id, status_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let u = users(&state, [s.creator_id]).await?;
    Ok(Json(render_status(&state, &a, &s, &u)))
}

#[derive(Debug, Default, Deserialize)]
pub struct StatusBody {
    pub state: Option<String>,
    pub target_url: Option<String>,
    pub log_url: Option<String>,
    pub description: Option<String>,
    pub environment: Option<String>,
    pub environment_url: Option<String>,
    pub auto_inactive: Option<bool>,
}

/// `POST /repos/{o}/{r}/deployments/{id}/statuses` → 201.
pub async fn create_status(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, id)): Path<(String, String, i64)>,
    Json(body): Json<StatusBody>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let a = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    a.require(Permission::Write)?;
    a.require_not_archived()?;
    let d = load(&state, &a, id).await?;
    let st = body.state.as_deref().ok_or_else(|| {
        ApiError::invalid_field(FieldError::missing_field("DeploymentStatus", "state"))
    })?;
    if !dep::STATES.contains(&st) {
        return Err(ApiError::invalid_field(FieldError::invalid(
            "DeploymentStatus",
            "state",
        )));
    }
    if let Some(env) = body.environment.as_deref()
        && !valid_environment(env)
    {
        return Err(ApiError::invalid_field(FieldError::invalid(
            "DeploymentStatus",
            "environment",
        )));
    }
    let mut description = body.description.clone().unwrap_or_default();
    if description.chars().count() > MAX_DESCRIPTION {
        description = description.chars().take(MAX_DESCRIPTION).collect();
    }
    // `log_url` replaces `target_url`; each defaults to the other.
    let target_url = body
        .target_url
        .clone()
        .or_else(|| body.log_url.clone())
        .unwrap_or_default();
    let log_url = body
        .log_url
        .clone()
        .or_else(|| body.target_url.clone())
        .unwrap_or_default();
    let mut tx = Tx::begin(&state).await?;
    let s = deployments::create_status(
        &mut tx,
        a.repo.id,
        d.id,
        Some(auth.user.id),
        NewStatus {
            state: st.to_string(),
            description,
            environment: body.environment.clone(),
            target_url,
            log_url,
            environment_url: body.environment_url.clone().unwrap_or_default(),
            auto_inactive: body.auto_inactive.unwrap_or(true),
        },
    )
    .await?;
    tx.commit().await?;
    let u = HashMap::from([(auth.user.id, auth.user.clone())]);
    Ok((StatusCode::CREATED, Json(render_status(&state, &a, &s, &u))))
}

// ----- web client ----------------------------------------------------------------

/// A deployment with its latest status, as the deployments page shows it.
#[derive(Debug, serde::Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
struct WebDeploymentRow {
    id: i64,
    environment: String,
    #[sqlx(rename = "ref")]
    #[serde(rename = "ref")]
    git_ref: String,
    sha: String,
    task: String,
    description: Option<String>,
    state: Option<String>,
    creator_id: Option<i64>,
    production_environment: bool,
    transient_environment: bool,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
    environment_url: Option<String>,
    log_url: Option<String>,
    status_description: Option<String>,
}

const WEB_COLUMNS: &str = "d.id, d.environment, d.ref, d.sha, d.task, d.description, d.state, \
    d.creator_id, d.production_environment, d.transient_environment, d.created_at, d.updated_at, \
    nullif(s.environment_url, '') AS environment_url, nullif(s.log_url, '') AS log_url, \
    nullif(s.description, '') AS status_description";

fn web_row(state: &AppState, d: WebDeploymentRow, u: &HashMap<i64, db::User>) -> Value {
    let creator = d.creator_id.and_then(|i| u.get(&i)).map(|c| {
        json!({
            "login": c.login,
            "avatarUrl": bgh_core::models::api::SimpleUser::new(&state.urls, c).avatar_url,
        })
    });
    let mut v = serde_json::to_value(&d).unwrap_or(Value::Null);
    if let Value::Object(m) = &mut v {
        m.insert("createdAt".into(), json!(Timestamp(d.created_at)));
        m.insert("updatedAt".into(), json!(Timestamp(d.updated_at)));
        m.insert("creator".into(), creator.unwrap_or(Value::Null));
        m.remove("creatorId");
    }
    v
}

#[derive(Debug, Default, Deserialize)]
pub struct WebQuery {
    pub environment: Option<String>,
    pub page: Option<i64>,
}

const WEB_PER_PAGE: i64 = 30;

/// `GET /_bgh/repos/{o}/{r}/deployments?environment=&page=`: every
/// environment with its latest deployment and count, plus one page of the
/// activity log (deployments with their latest status, newest first).
pub async fn web_summary(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo)): Path<(String, String)>,
    Query(q): Query<WebQuery>,
) -> ApiResult<Json<Value>> {
    let a = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let page = q.page.unwrap_or(1).max(1);
    let env = q.environment.as_deref().filter(|e| !e.is_empty());
    let envs: Vec<(i64, String, i64)> = sqlx::query_as(
        "SELECT e.id, e.name, (SELECT count(*) FROM deployments d
                                WHERE d.repo_id = e.repo_id AND lower(d.environment) = lower(e.name))
           FROM actions_environments e WHERE e.repo_id = $1 ORDER BY lower(e.name), e.id",
    )
    .bind(a.repo.id)
    .fetch_all(&state.db)
    .await?;
    let latest: Vec<WebDeploymentRow> = sqlx::query_as(&format!(
        "SELECT DISTINCT ON (lower(d.environment)) {WEB_COLUMNS}
           FROM deployments d LEFT JOIN deployment_statuses s ON s.id = d.latest_status_id
          WHERE d.repo_id = $1
          ORDER BY lower(d.environment), d.id DESC"
    ))
    .bind(a.repo.id)
    .fetch_all(&state.db)
    .await?;
    let mut rows: Vec<WebDeploymentRow> = sqlx::query_as(&format!(
        "SELECT {WEB_COLUMNS}
           FROM deployments d LEFT JOIN deployment_statuses s ON s.id = d.latest_status_id
          WHERE d.repo_id = $1 AND ($2::text IS NULL OR lower(d.environment) = lower($2))
          ORDER BY d.id DESC LIMIT $3 OFFSET $4"
    ))
    .bind(a.repo.id)
    .bind(env)
    .bind(WEB_PER_PAGE + 1)
    .bind((page - 1) * WEB_PER_PAGE)
    .fetch_all(&state.db)
    .await?;
    let has_more = rows.len() as i64 > WEB_PER_PAGE;
    rows.truncate(WEB_PER_PAGE as usize);
    let u = users(
        &state,
        rows.iter().chain(latest.iter()).map(|d| d.creator_id),
    )
    .await?;
    let mut latest_by_env: HashMap<String, Value> = latest
        .into_iter()
        .map(|d| (d.environment.to_lowercase(), web_row(&state, d, &u)))
        .collect();
    let environments: Vec<Value> = envs
        .into_iter()
        .map(|(id, name, count)| {
            json!({
                "id": id,
                "name": name,
                "deployments": count,
                "latest": latest_by_env.remove(&name.to_lowercase()),
            })
        })
        .collect();
    let deployments: Vec<Value> = rows.into_iter().map(|d| web_row(&state, d, &u)).collect();
    Ok(Json(json!({
        "environments": environments,
        "deployments": deployments,
        "page": page,
        "hasMore": has_more,
        "canWrite": a.permission >= Permission::Write,
    })))
}
