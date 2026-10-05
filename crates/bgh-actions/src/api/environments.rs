//! Deployment environments (`/repos/{o}/{r}/environments`): the scope of
//! environment secrets and variables, with GitHub's protection rules
//! (`wait_timer`, required `reviewers` + `prevent_self_review`,
//! `deployment_branch_policy`, `can_admins_bypass`) and the custom
//! deployment branch / tag policies
//! (`/environments/{env}/deployment-branch-policies`). Enforcement lives in
//! [`crate::gates`].

use axum::extract::State;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bgh_core::pagination::Pagination;
use bgh_core::prelude::*;
use serde_json::{Value, json};

use super::wrapped;
use crate::gates::{
    BranchPolicyRow, MAX_REVIEWERS, MAX_WAIT_TIMER, branch_policy_json, reviewers_for,
    reviewers_json,
};
use crate::json::environment_json;
use crate::models::EnvironmentRow;

/// Render environments with their reviewers (one batch query).
async fn render(
    state: &AppState,
    a: &RepoAccess,
    rows: &[EnvironmentRow],
) -> ApiResult<Vec<Value>> {
    let mut conn = state.db.acquire().await?;
    let ids: Vec<i64> = rows.iter().map(|e| e.id).collect();
    let reviewers = reviewers_for(&mut conn, &ids).await?;
    let mut out = Vec::with_capacity(rows.len());
    for e in rows {
        let r = match reviewers.get(&e.id) {
            Some(r) => reviewers_json(state, &mut conn, r).await?,
            None => vec![],
        };
        out.push(environment_json(state, a, e, &r));
    }
    Ok(out)
}

pub async fn list(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<Response> {
    let a = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let total: i64 =
        sqlx::query_scalar("SELECT count(*) FROM actions_environments WHERE repo_id = $1")
            .bind(a.repo.id)
            .fetch_one(&state.db)
            .await?;
    let rows: Vec<EnvironmentRow> = sqlx::query_as(&format!(
        "SELECT {} FROM actions_environments WHERE repo_id = $1 ORDER BY lower(name), id
          LIMIT $2 OFFSET $3",
        EnvironmentRow::COLUMNS
    ))
    .bind(a.repo.id)
    .bind(p.limit())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let items = render(&state, &a, &rows).await?;
    Ok(wrapped(&p, total, "environments", items))
}

pub(crate) async fn find(
    state: &AppState,
    a: &RepoAccess,
    name: &str,
) -> ApiResult<EnvironmentRow> {
    sqlx::query_as(&format!(
        "SELECT {} FROM actions_environments WHERE repo_id = $1 AND lower(name) = lower($2)",
        EnvironmentRow::COLUMNS
    ))
    .bind(a.repo.id)
    .bind(name)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)
}

pub async fn get(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, name)): Path<(String, String, String)>,
) -> ApiResult<Json<Value>> {
    let a = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let e = find(&state, &a, &name).await?;
    let mut v = render(&state, &a, std::slice::from_ref(&e)).await?;
    Ok(Json(v.remove(0)))
}

fn invalid(field: &str, message: impl Into<String>) -> ApiError {
    ApiError::invalid_field(FieldError::custom("Environment", field, message))
}

/// Parsed protection settings of a `PUT` body (`None`: keep).
#[derive(Default)]
struct Settings {
    wait_timer: Option<i32>,
    prevent_self_review: Option<bool>,
    can_admins_bypass: Option<bool>,
    /// `Some(None)`: any ref.
    branch_policy: Option<Option<&'static str>>,
    reviewers: Option<Vec<(Option<i64>, Option<i64>)>>,
}

async fn parse_settings(state: &AppState, a: &RepoAccess, body: &Value) -> ApiResult<Settings> {
    let mut s = Settings::default();
    if body.is_null() {
        return Ok(s);
    }
    if !body.is_object() {
        return Err(ApiError::bad_request("Problems parsing JSON"));
    }
    let bool_field = |key: &str| -> ApiResult<Option<bool>> {
        match &body[key] {
            Value::Null => Ok(None),
            Value::Bool(b) => Ok(Some(*b)),
            _ => Err(invalid(key, format!("{key} must be a boolean"))),
        }
    };
    s.prevent_self_review = bool_field("prevent_self_review")?;
    s.can_admins_bypass = bool_field("can_admins_bypass")?;
    match &body["wait_timer"] {
        Value::Null => {}
        v => {
            let n = v
                .as_i64()
                .filter(|n| (0..=MAX_WAIT_TIMER).contains(n))
                .ok_or_else(|| {
                    invalid(
                        "wait_timer",
                        format!("wait_timer must be an integer between 0 and {MAX_WAIT_TIMER}"),
                    )
                })?;
            s.wait_timer = Some(n as i32);
        }
    }
    if let Some(obj) = body.as_object()
        && obj.contains_key("deployment_branch_policy")
    {
        s.branch_policy = Some(match &body["deployment_branch_policy"] {
            Value::Null => None,
            Value::Object(p) => {
                let protected = p.get("protected_branches").and_then(Value::as_bool);
                let custom = p.get("custom_branch_policies").and_then(Value::as_bool);
                match (protected, custom) {
                    (Some(true), Some(false)) => Some("protected"),
                    (Some(false), Some(true)) => Some("custom"),
                    _ => {
                        return Err(invalid(
                            "deployment_branch_policy",
                            "Exactly one of protected_branches and custom_branch_policies must be true",
                        ));
                    }
                }
            }
            _ => {
                return Err(invalid(
                    "deployment_branch_policy",
                    "deployment_branch_policy must be an object or null",
                ));
            }
        });
    }
    match &body["reviewers"] {
        Value::Null => {}
        Value::Array(list) => {
            if list.len() > MAX_REVIEWERS {
                return Err(invalid(
                    "reviewers",
                    format!("An environment can have at most {MAX_REVIEWERS} reviewers"),
                ));
            }
            let mut out: Vec<(Option<i64>, Option<i64>)> = Vec::new();
            for r in list {
                let id = r["id"]
                    .as_i64()
                    .ok_or_else(|| invalid("reviewers", "Each reviewer needs an integer id"))?;
                let entry = match r["type"].as_str() {
                    Some("User") => {
                        let user = db::User::find(&state.db, id)
                            .await?
                            .filter(|u| u.kind == "User");
                        let Some(user) = user else {
                            return Err(invalid("reviewers", format!("User {id} does not exist")));
                        };
                        let perm =
                            bgh_core::perms::repo_permission(&state.db, Some(user.id), &a.repo)
                                .await?;
                        if perm < Permission::Read {
                            return Err(invalid(
                                "reviewers",
                                format!("{} does not have access to this repository", user.login),
                            ));
                        }
                        (Some(id), None)
                    }
                    Some("Team") => {
                        let ok: bool = sqlx::query_scalar(
                            "SELECT EXISTS (SELECT 1 FROM teams WHERE id = $1 AND org_id = $2)",
                        )
                        .bind(id)
                        .bind(a.repo.owner_id)
                        .fetch_one(&state.db)
                        .await?;
                        if !ok {
                            return Err(invalid(
                                "reviewers",
                                format!("Team {id} does not exist in this organization"),
                            ));
                        }
                        (None, Some(id))
                    }
                    _ => {
                        return Err(invalid(
                            "reviewers",
                            "Reviewer type must be either User or Team",
                        ));
                    }
                };
                if !out.contains(&entry) {
                    out.push(entry);
                }
            }
            s.reviewers = Some(out);
        }
        _ => return Err(invalid("reviewers", "reviewers must be an array or null")),
    }
    Ok(s)
}

/// `PUT /repos/{o}/{r}/environments/{name}`: create or update (200).
pub async fn put(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, name)): Path<(String, String, String)>,
    body: axum::body::Bytes,
) -> ApiResult<Json<Value>> {
    let a = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    a.require(Permission::Admin)?;
    if !crate::deployments::valid_environment(&name) {
        return Err(ApiError::invalid_field(FieldError::invalid(
            "Environment",
            "name",
        )));
    }
    // The body is optional (`PUT` with no body creates the environment).
    let body: Value = if body.iter().all(u8::is_ascii_whitespace) {
        Value::Null
    } else {
        serde_json::from_slice(&body).map_err(|_| ApiError::bad_request("Problems parsing JSON"))?
    };
    let s = parse_settings(&state, &a, &body).await?;
    let mut tx = Tx::begin(&state).await?;
    let e: EnvironmentRow = sqlx::query_as(&format!(
        "INSERT INTO actions_environments (repo_id, name) VALUES ($1, $2)
         ON CONFLICT (repo_id, lower(name)) DO UPDATE SET updated_at = now()
         RETURNING {}",
        EnvironmentRow::COLUMNS
    ))
    .bind(a.repo.id)
    .bind(&name)
    .fetch_one(&mut *tx)
    .await?;
    let (policy_set, policy) = match s.branch_policy {
        Some(p) => (true, p),
        None => (false, None),
    };
    let e: EnvironmentRow = sqlx::query_as(&format!(
        "UPDATE actions_environments SET
             wait_timer = coalesce($2, wait_timer),
             prevent_self_review = coalesce($3, prevent_self_review),
             can_admins_bypass = coalesce($4, can_admins_bypass),
             branch_policy = CASE WHEN $5 THEN $6 ELSE branch_policy END,
             updated_at = now()
          WHERE id = $1 RETURNING {}",
        EnvironmentRow::COLUMNS
    ))
    .bind(e.id)
    .bind(s.wait_timer)
    .bind(s.prevent_self_review)
    .bind(s.can_admins_bypass)
    .bind(policy_set)
    .bind(policy)
    .fetch_one(&mut *tx)
    .await?;
    if let Some(reviewers) = &s.reviewers {
        sqlx::query("DELETE FROM actions_environment_reviewers WHERE environment_id = $1")
            .bind(e.id)
            .execute(&mut *tx)
            .await?;
        for (i, (user_id, team_id)) in reviewers.iter().enumerate() {
            sqlx::query(
                "INSERT INTO actions_environment_reviewers (environment_id, position, user_id,
                                                            team_id)
                 VALUES ($1, $2, $3, $4)",
            )
            .bind(e.id)
            .bind(i as i32)
            .bind(user_id)
            .bind(team_id)
            .execute(&mut *tx)
            .await?;
        }
    }
    if !body.is_null() {
        bgh_core::audit::log(
            &mut *tx,
            Some(&auth.user),
            "environment.update_protection_rule",
            bgh_core::audit::Target::Repo {
                id: a.repo.id,
                org_id: None,
            },
            json!({"environment": e.name, "settings": body}),
        )
        .await?;
    }
    tx.commit().await?;
    let mut v = render(&state, &a, std::slice::from_ref(&e)).await?;
    Ok(Json(v.remove(0)))
}

pub async fn delete(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, name)): Path<(String, String, String)>,
) -> ApiResult<StatusCode> {
    let a = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    a.require(Permission::Admin)?;
    let e = find(&state, &a, &name).await?;
    sqlx::query("DELETE FROM actions_environments WHERE id = $1")
        .bind(e.id)
        .execute(&state.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

// ----- deployment branch policies ---------------------------------------------------

async fn find_policy(
    state: &AppState,
    env: &EnvironmentRow,
    id: i64,
) -> ApiResult<BranchPolicyRow> {
    sqlx::query_as(&format!(
        "SELECT {} FROM actions_environment_branch_policies WHERE id = $1 AND environment_id = $2",
        BranchPolicyRow::COLUMNS
    ))
    .bind(id)
    .bind(env.id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)
}

fn policy_name(body: &Value) -> ApiResult<String> {
    match body["name"].as_str().map(str::trim) {
        Some(n) if !n.is_empty() && n.len() <= 255 => Ok(n.to_string()),
        Some(_) => Err(ApiError::invalid_field(FieldError::invalid(
            "DeploymentBranchPolicy",
            "name",
        ))),
        None => Err(ApiError::invalid_field(FieldError::missing_field(
            "DeploymentBranchPolicy",
            "name",
        ))),
    }
}

/// `GET /repos/{o}/{r}/environments/{env}/deployment-branch-policies`
pub async fn policies_list(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo, name)): Path<(String, String, String)>,
) -> ApiResult<Response> {
    let a = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let e = find(&state, &a, &name).await?;
    let total: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM actions_environment_branch_policies WHERE environment_id = $1",
    )
    .bind(e.id)
    .fetch_one(&state.db)
    .await?;
    let rows: Vec<BranchPolicyRow> = sqlx::query_as(&format!(
        "SELECT {} FROM actions_environment_branch_policies WHERE environment_id = $1
          ORDER BY id LIMIT $2 OFFSET $3",
        BranchPolicyRow::COLUMNS
    ))
    .bind(e.id)
    .bind(p.limit())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let items: Vec<Value> = rows.iter().map(branch_policy_json).collect();
    Ok(wrapped(&p, total, "branch_policies", items))
}

/// `POST /repos/{o}/{r}/environments/{env}/deployment-branch-policies`:
/// 200 with the policy; 303 to the existing one for a duplicate; 404 when
/// the environment doesn't use custom branch policies.
pub async fn policies_create(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, name)): Path<(String, String, String)>,
    Json(body): Json<Value>,
) -> ApiResult<Response> {
    let a = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    a.require(Permission::Admin)?;
    let e = find(&state, &a, &name).await?;
    if e.branch_policy.as_deref() != Some("custom") {
        return Err(ApiError::NotFound);
    }
    let policy = policy_name(&body)?;
    let kind = match body["type"].as_str() {
        None => "branch",
        Some("branch") => "branch",
        Some("tag") => "tag",
        Some(_) => {
            return Err(ApiError::invalid_field(FieldError::invalid(
                "DeploymentBranchPolicy",
                "type",
            )));
        }
    };
    let row: Option<BranchPolicyRow> = sqlx::query_as(&format!(
        "INSERT INTO actions_environment_branch_policies (environment_id, name, type)
         VALUES ($1, $2, $3) ON CONFLICT DO NOTHING RETURNING {}",
        BranchPolicyRow::COLUMNS
    ))
    .bind(e.id)
    .bind(&policy)
    .bind(kind)
    .fetch_optional(&state.db)
    .await?;
    match row {
        Some(row) => Ok(Json(branch_policy_json(&row)).into_response()),
        None => {
            let id: i64 = sqlx::query_scalar(
                "SELECT id FROM actions_environment_branch_policies
                  WHERE environment_id = $1 AND type = $2 AND name = $3",
            )
            .bind(e.id)
            .bind(kind)
            .bind(&policy)
            .fetch_one(&state.db)
            .await?;
            let url = state.urls.api(&format!(
                "/repos/{}/{}/environments/{}/deployment-branch-policies/{id}",
                a.owner.login,
                a.repo.name,
                bgh_core::urls::encode_segment(&e.name)
            ));
            let mut resp = StatusCode::SEE_OTHER.into_response();
            if let Ok(v) = HeaderValue::from_str(&url) {
                resp.headers_mut().insert(header::LOCATION, v);
            }
            Ok(resp)
        }
    }
}

/// `GET .../deployment-branch-policies/{id}`
pub async fn policies_get(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, name, id)): Path<(String, String, String, i64)>,
) -> ApiResult<Json<Value>> {
    let a = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let e = find(&state, &a, &name).await?;
    Ok(Json(branch_policy_json(
        &find_policy(&state, &e, id).await?,
    )))
}

/// `PUT .../deployment-branch-policies/{id}` (`{name}`)
pub async fn policies_update(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, name, id)): Path<(String, String, String, i64)>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let a = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    a.require(Permission::Admin)?;
    let e = find(&state, &a, &name).await?;
    let p = find_policy(&state, &e, id).await?;
    let policy = policy_name(&body)?;
    let row: BranchPolicyRow = sqlx::query_as(&format!(
        "UPDATE actions_environment_branch_policies SET name = $2, updated_at = now()
          WHERE id = $1 RETURNING {}",
        BranchPolicyRow::COLUMNS
    ))
    .bind(p.id)
    .bind(&policy)
    .fetch_one(&state.db)
    .await
    .map_err(
        |err| match bgh_core::db::unique_violation(&err).as_deref() {
            Some("actions_environment_branch_policies_key") => ApiError::invalid_field(
                FieldError::already_exists("DeploymentBranchPolicy", "name"),
            ),
            _ => err.into(),
        },
    )?;
    Ok(Json(branch_policy_json(&row)))
}

/// `DELETE .../deployment-branch-policies/{id}`
pub async fn policies_delete(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, name, id)): Path<(String, String, String, i64)>,
) -> ApiResult<StatusCode> {
    let a = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    a.require(Permission::Admin)?;
    let e = find(&state, &a, &name).await?;
    let p = find_policy(&state, &e, id).await?;
    sqlx::query("DELETE FROM actions_environment_branch_policies WHERE id = $1")
        .bind(p.id)
        .execute(&state.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
