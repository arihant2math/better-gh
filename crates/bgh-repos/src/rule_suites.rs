//! Rule suites ("rule insights"): recorded ruleset evaluations of pushes
//! and pull request merges ([`crate::rule_eval::record`]).
//!
//! * `GET /repos/{o}/{r}/rulesets/rule-suites[/{id}]` (repository admins)
//! * `GET /orgs/{org}/rulesets/rule-suites[/{id}]` (organization owners)
//!
//! Filters: `ref` (full name, or a branch / tag name), `time_period`
//! (`hour`, `day` (default), `week`, `month`), `actor_name`,
//! `rule_suite_result` (`pass`, `fail`, `bypass`, `all`) and, for
//! organizations, `repository_name`.

use axum::Router;
use axum::extract::State;
use axum::routing::get;
use bgh_core::prelude::*;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::org_rulesets::{load_org, require_org_admin};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/repos/{owner}/{repo}/rulesets/rule-suites", get(list_repo))
        .route(
            "/repos/{owner}/{repo}/rulesets/rule-suites/{id}",
            get(get_repo),
        )
        .route("/orgs/{org}/rulesets/rule-suites", get(list_org))
        .route("/orgs/{org}/rulesets/rule-suites/{id}", get(get_org))
}

#[derive(Debug, sqlx::FromRow)]
struct SuiteRow {
    id: i64,
    repo_id: i64,
    repository_name: String,
    actor_id: Option<i64>,
    actor_name: Option<String>,
    r#ref: String,
    before_sha: String,
    after_sha: String,
    result: String,
    evaluation_result: Option<String>,
    rule_evaluations: Value,
    pushed_at: DateTime<Utc>,
}

const COLUMNS: &str = "s.id, s.repo_id, r.name AS repository_name, s.actor_id, s.actor_name, \
    s.ref, s.before_sha, s.after_sha, s.result, s.evaluation_result, s.rule_evaluations, \
    s.pushed_at";

/// `rule-suite` (with `rule_evaluations` when `full`).
fn render(s: &SuiteRow, full: bool) -> Value {
    let mut v = json!({
        "id": s.id,
        "actor_id": s.actor_id,
        "actor_name": s.actor_name,
        "before_sha": s.before_sha,
        "after_sha": s.after_sha,
        "ref": s.r#ref,
        "repository_id": s.repo_id,
        "repository_name": s.repository_name,
        "pushed_at": Timestamp::from(s.pushed_at),
        "result": s.result,
        "evaluation_result": s.evaluation_result,
    });
    if full {
        v["rule_evaluations"] = s.rule_evaluations.clone();
    }
    v
}

#[derive(Debug, Default, Deserialize)]
struct Filters {
    #[serde(rename = "ref")]
    refname: Option<String>,
    time_period: Option<String>,
    actor_name: Option<String>,
    rule_suite_result: Option<String>,
    repository_name: Option<String>,
}

fn invalid(field: &str, msg: &str) -> ApiError {
    ApiError::invalid_field(FieldError::custom("RuleSuite", field, msg))
}

impl Filters {
    fn hours(&self) -> ApiResult<i64> {
        Ok(match self.time_period.as_deref().unwrap_or("day") {
            "hour" => 1,
            "day" => 24,
            "week" => 24 * 7,
            "month" => 24 * 30,
            _ => {
                return Err(invalid(
                    "time_period",
                    "time_period must be one of hour, day, week, month",
                ));
            }
        })
    }

    fn result(&self) -> ApiResult<Option<&str>> {
        match self.rule_suite_result.as_deref() {
            None | Some("all") => Ok(None),
            Some(r @ ("pass" | "fail" | "bypass")) => Ok(Some(r)),
            Some(_) => Err(invalid(
                "rule_suite_result",
                "rule_suite_result must be one of pass, fail, bypass, all",
            )),
        }
    }
}

/// Suites of one repository (`repo_id`) or of every repository of an
/// organization (`owner_id`), newest first.
async fn query(
    state: &AppState,
    repo_id: Option<i64>,
    owner_id: Option<i64>,
    f: &Filters,
    p: &Pagination,
) -> ApiResult<Vec<SuiteRow>> {
    let hours = f.hours()?;
    let result = f.result()?;
    Ok(sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM rule_suites s JOIN repositories r ON r.id = s.repo_id
          WHERE ($1::bigint IS NULL OR s.repo_id = $1)
            AND ($2::bigint IS NULL OR r.owner_id = $2)
            AND s.pushed_at > now() - make_interval(hours => $3::int)
            AND ($4::text IS NULL OR s.ref IN ($4, 'refs/heads/' || $4, 'refs/tags/' || $4))
            AND ($5::text IS NULL OR lower(s.actor_name) = lower($5))
            AND ($6::text IS NULL OR s.result = $6)
            AND ($7::text IS NULL OR lower(r.name) = lower($7))
          ORDER BY s.pushed_at DESC, s.id DESC
          LIMIT $8 OFFSET $9"
    ))
    .bind(repo_id)
    .bind(owner_id)
    .bind(hours as i32)
    .bind(f.refname.as_deref().filter(|s| !s.is_empty()))
    .bind(f.actor_name.as_deref().filter(|s| !s.is_empty()))
    .bind(result)
    .bind(f.repository_name.as_deref().filter(|s| !s.is_empty()))
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?)
}

async fn find(
    state: &AppState,
    repo_id: Option<i64>,
    owner_id: Option<i64>,
    id: i64,
) -> ApiResult<SuiteRow> {
    sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM rule_suites s JOIN repositories r ON r.id = s.repo_id
          WHERE s.id = $1 AND ($2::bigint IS NULL OR s.repo_id = $2)
            AND ($3::bigint IS NULL OR r.owner_id = $3)"
    ))
    .bind(id)
    .bind(repo_id)
    .bind(owner_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)
}

async fn repo_admin(
    state: &AppState,
    auth: &MaybeUser,
    owner: &str,
    repo: &str,
) -> ApiResult<RepoAccess> {
    let access = RepoAccess::load(state, auth.as_ref(), owner, repo).await?;
    access.require(Permission::Admin)?;
    Ok(access)
}

/// `GET /repos/{owner}/{repo}/rulesets/rule-suites`
async fn list_repo(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Query(f): Query<Filters>,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<Page<Value>> {
    let access = repo_admin(&state, &auth, &owner, &repo).await?;
    let f = Filters {
        repository_name: None,
        ..f
    };
    let rows = query(&state, Some(access.repo.id), None, &f, &p).await?;
    Ok(p.page(rows).map(|s| render(&s, false)))
}

/// `GET /repos/{owner}/{repo}/rulesets/rule-suites/{id}`
async fn get_repo(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, id)): Path<(String, String, i64)>,
) -> ApiResult<Json<Value>> {
    let access = repo_admin(&state, &auth, &owner, &repo).await?;
    let s = find(&state, Some(access.repo.id), None, id).await?;
    Ok(Json(render(&s, true)))
}

/// `GET /orgs/{org}/rulesets/rule-suites`
async fn list_org(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Query(f): Query<Filters>,
    Path(org): Path<String>,
) -> ApiResult<Page<Value>> {
    let org = load_org(&state, &org).await?;
    require_org_admin(&state, &auth, &org).await?;
    let rows = query(&state, None, Some(org.id), &f, &p).await?;
    Ok(p.page(rows).map(|s| render(&s, false)))
}

/// `GET /orgs/{org}/rulesets/rule-suites/{id}`
async fn get_org(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((org, id)): Path<(String, i64)>,
) -> ApiResult<Json<Value>> {
    let org = load_org(&state, &org).await?;
    require_org_admin(&state, &auth, &org).await?;
    let s = find(&state, None, Some(org.id), id).await?;
    Ok(Json(render(&s, true)))
}
