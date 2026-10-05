//! Repository rulesets (GitHub "Repository rules" API subset).
//!
//! * `GET|POST /repos/{o}/{r}/rulesets`
//! * `GET|PUT|DELETE /repos/{o}/{r}/rulesets/{id}`
//! * `GET /repos/{o}/{r}/rules/branches/{*branch}`: active rules applying
//!   to a branch.
//!
//! Reading needs read access, writes need admin. Enforcement lives in
//! [`crate::protection`].

use std::collections::HashSet;

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use bgh_core::audit;
use bgh_core::prelude::*;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::protection::{Actor, RulesetRow};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/repos/{owner}/{repo}/rulesets", get(list).post(create))
        .route(
            "/repos/{owner}/{repo}/rulesets/{id}",
            get(get_one).put(update).delete(destroy),
        )
        .route(
            "/repos/{owner}/{repo}/rules/branches/{*branch}",
            get(rules_for_branch),
        )
}

const RULE_TYPES: &[&str] = &[
    "creation",
    "update",
    "deletion",
    "required_linear_history",
    "required_signatures",
    "non_fast_forward",
    "pull_request",
    "required_status_checks",
];

// ----- rendering -------------------------------------------------------------------

fn node_id(r: &RulesetRow) -> String {
    format!(
        "RRS_{}",
        URL_SAFE_NO_PAD.encode(format!("Ruleset:{}:{}", r.repo_id, r.id))
    )
}

/// List item shape (`repository-ruleset` without rules / conditions).
fn summary(state: &AppState, access: &RepoAccess, r: &RulesetRow) -> Value {
    let (o, n) = (&access.owner.login, &access.repo.name);
    json!({
        "id": r.id,
        "name": r.name,
        "target": r.target,
        "source_type": "Repository",
        "source": access.full_name(),
        "enforcement": r.enforcement,
        "node_id": node_id(r),
        "_links": {
            "self": {"href": format!("{}/rulesets/{}", state.urls.repo(o, n), r.id)},
            "html": {"href": format!("{}/rules/{}", state.urls.repo_html(o, n), r.id)},
        },
        "created_at": Timestamp::from(r.created_at),
        "updated_at": Timestamp::from(r.updated_at),
    })
}

/// Full `repository-ruleset`. `bypass_actors` only for admins, like GitHub.
fn full(state: &AppState, access: &RepoAccess, r: &RulesetRow, can_bypass: &str) -> Value {
    let mut v = summary(state, access, r);
    if access.permission >= Permission::Admin {
        v["bypass_actors"] = r.bypass_actors.clone();
    }
    v["conditions"] = r.conditions.clone();
    v["rules"] = r.rules.clone();
    v["current_user_can_bypass"] = json!(can_bypass);
    v
}

/// `"always"`, `"pull_requests_only"` or `"never"` for `actor` (mirrors
/// the bypass evaluation of the rules engine).
fn bypass_mode(r: &RulesetRow, actor: Option<&Actor>) -> &'static str {
    actor.map_or("never", |a| r.bypass_mode(a))
}

async fn load_actor(
    state: &AppState,
    auth: Option<&AuthContext>,
    access: &RepoAccess,
) -> ApiResult<Option<Actor>> {
    match auth {
        Some(a) => Ok(Some(Actor::load(state, access, &a.user).await?)),
        None => Ok(None),
    }
}

// ----- validation --------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
struct RulesetInput {
    name: Option<String>,
    target: Option<String>,
    enforcement: Option<String>,
    bypass_actors: Option<Vec<Value>>,
    conditions: Option<Value>,
    rules: Option<Vec<Value>>,
}

fn invalid(field: &str, msg: impl Into<String>) -> ApiError {
    ApiError::invalid_field(FieldError::custom("Ruleset", field, msg))
}

fn check_enum(field: &str, value: &str, allowed: &[&str]) -> ApiResult<()> {
    if allowed.contains(&value) {
        Ok(())
    } else {
        Err(invalid(
            field,
            format!(
                "Invalid {field} '{value}'. Expected one of: {}.",
                allowed.join(", ")
            ),
        ))
    }
}

fn string_array(v: &Value, field: &str) -> ApiResult<Vec<String>> {
    match v {
        Value::Null => Ok(vec![]),
        Value::Array(a) => a
            .iter()
            .map(|x| {
                x.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| invalid(field, format!("{field} must be an array of strings")))
            })
            .collect(),
        _ => Err(invalid(
            field,
            format!("{field} must be an array of strings"),
        )),
    }
}

fn normalize_conditions(v: &Value) -> ApiResult<Value> {
    if !(v.is_object() || v.is_null()) {
        return Err(invalid("conditions", "conditions must be an object"));
    }
    let ref_name = &v["ref_name"];
    if !(ref_name.is_object() || ref_name.is_null()) {
        return Err(invalid(
            "conditions",
            "conditions.ref_name must be an object",
        ));
    }
    let include = string_array(&ref_name["include"], "conditions.ref_name.include")?;
    let exclude = string_array(&ref_name["exclude"], "conditions.ref_name.exclude")?;
    Ok(json!({"ref_name": {"include": include, "exclude": exclude}}))
}

fn bool_param(p: &Value, rule: &str, key: &str) -> ApiResult<bool> {
    match &p[key] {
        Value::Null => Ok(false),
        Value::Bool(b) => Ok(*b),
        _ => Err(invalid(
            "rules",
            format!("Invalid parameter '{key}' for rule '{rule}': expected a boolean"),
        )),
    }
}

fn normalize_rule(v: &Value) -> ApiResult<(String, Value)> {
    let Some(kind) = v["type"].as_str() else {
        return Err(invalid("rules", "Each rule needs a 'type'"));
    };
    if !RULE_TYPES.contains(&kind) {
        return Err(invalid("rules", format!("Invalid rule '{kind}'")));
    }
    let p = &v["parameters"];
    if !(p.is_object() || p.is_null()) {
        return Err(invalid(
            "rules",
            format!("Invalid parameters for rule '{kind}'"),
        ));
    }
    let rule = match kind {
        "pull_request" => {
            let count = match &p["required_approving_review_count"] {
                Value::Null => 0,
                n => n.as_i64().filter(|n| (0..=10).contains(n)).ok_or_else(|| {
                    invalid(
                        "rules",
                        "required_approving_review_count must be an integer between 0 and 10",
                    )
                })?,
            };
            json!({"type": kind, "parameters": {
                "required_approving_review_count": count,
                "dismiss_stale_reviews_on_push": bool_param(p, kind, "dismiss_stale_reviews_on_push")?,
                "require_code_owner_review": bool_param(p, kind, "require_code_owner_review")?,
                "require_last_push_approval": bool_param(p, kind, "require_last_push_approval")?,
                "required_review_thread_resolution":
                    bool_param(p, kind, "required_review_thread_resolution")?,
            }})
        }
        "required_status_checks" => {
            let Some(list) = p["required_status_checks"].as_array() else {
                return Err(invalid(
                    "rules",
                    "Rule 'required_status_checks' needs parameters.required_status_checks",
                ));
            };
            let mut checks = Vec::new();
            for c in list {
                let Some(context) = c["context"].as_str().filter(|s| !s.is_empty()) else {
                    return Err(invalid(
                        "rules",
                        "Each required status check needs a 'context'",
                    ));
                };
                let mut check = json!({"context": context});
                match &c["integration_id"] {
                    Value::Null => {}
                    Value::Number(n) if n.is_i64() => check["integration_id"] = json!(n),
                    _ => return Err(invalid("rules", "integration_id must be an integer")),
                }
                checks.push(check);
            }
            json!({"type": kind, "parameters": {
                "required_status_checks": checks,
                "strict_required_status_checks_policy":
                    bool_param(p, kind, "strict_required_status_checks_policy")?,
            }})
        }
        _ => json!({"type": kind}),
    };
    Ok((kind.to_string(), rule))
}

fn normalize_rules(rules: &[Value]) -> ApiResult<Value> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for r in rules {
        let (kind, rule) = normalize_rule(r)?;
        if !seen.insert(kind.clone()) {
            return Err(invalid("rules", format!("Duplicate rule '{kind}'")));
        }
        out.push(rule);
    }
    Ok(Value::Array(out))
}

async fn normalize_bypass(
    state: &AppState,
    access: &RepoAccess,
    actors: &[Value],
) -> ApiResult<Value> {
    let mut out = Vec::new();
    let (mut team_ids, mut user_ids) = (Vec::new(), Vec::new());
    for a in actors {
        let Some(kind) = a["actor_type"].as_str() else {
            return Err(invalid(
                "bypass_actors",
                "Each bypass actor needs an 'actor_type'",
            ));
        };
        check_enum(
            "bypass_actors",
            kind,
            &["RepositoryRole", "OrganizationAdmin", "Team", "User"],
        )?;
        let mode = match &a["bypass_mode"] {
            Value::Null => "always",
            m => m.as_str().unwrap_or(""),
        };
        check_enum("bypass_mode", mode, &["always", "pull_request"])?;
        let id = a["actor_id"].as_i64();
        let id = match kind {
            "RepositoryRole" => Some(id.filter(|i| (1..=5).contains(i)).ok_or_else(|| {
                invalid(
                    "bypass_actors",
                    "RepositoryRole actor_id must be between 1 and 5",
                )
            })?),
            "OrganizationAdmin" => {
                if !access.owner.is_org() {
                    return Err(invalid(
                        "bypass_actors",
                        "OrganizationAdmin is not applicable for personal repositories",
                    ));
                }
                Some(id.unwrap_or(1))
            }
            "Team" => {
                let id = id.ok_or_else(|| invalid("bypass_actors", "Team actor_id is required"))?;
                team_ids.push(id);
                Some(id)
            }
            _ => {
                let id = id.ok_or_else(|| invalid("bypass_actors", "User actor_id is required"))?;
                user_ids.push(id);
                Some(id)
            }
        };
        out.push(json!({"actor_id": id, "actor_type": kind, "bypass_mode": mode}));
    }
    if !team_ids.is_empty() {
        let found: Vec<i64> =
            sqlx::query_scalar("SELECT id FROM teams WHERE id = ANY($1) AND org_id = $2")
                .bind(&team_ids)
                .bind(access.owner.id)
                .fetch_all(&state.db)
                .await?;
        if let Some(missing) = team_ids.iter().find(|t| !found.contains(t)) {
            return Err(invalid(
                "bypass_actors",
                format!("Unknown team id {missing}"),
            ));
        }
    }
    if !user_ids.is_empty() {
        let found: Vec<i64> =
            sqlx::query_scalar("SELECT id FROM users WHERE id = ANY($1) AND type = 'User'")
                .bind(&user_ids)
                .fetch_all(&state.db)
                .await?;
        if let Some(missing) = user_ids.iter().find(|u| !found.contains(u)) {
            return Err(invalid(
                "bypass_actors",
                format!("Unknown user id {missing}"),
            ));
        }
    }
    Ok(Value::Array(out))
}

/// Validated column values.
struct Fields {
    name: String,
    target: String,
    enforcement: String,
    conditions: Value,
    rules: Value,
    bypass_actors: Value,
}

async fn validate(
    state: &AppState,
    access: &RepoAccess,
    input: RulesetInput,
    base: Option<&RulesetRow>,
) -> ApiResult<Fields> {
    let name = match (input.name, base) {
        (Some(n), _) => n.trim().to_string(),
        (None, Some(b)) => b.name.clone(),
        (None, None) => {
            return Err(ApiError::invalid_field(FieldError::missing_field(
                "Ruleset", "name",
            )));
        }
    };
    if name.is_empty() {
        return Err(invalid("name", "name can't be blank"));
    }
    let target = input
        .target
        .or_else(|| base.map(|b| b.target.clone()))
        .unwrap_or_else(|| "branch".into());
    check_enum("target", &target, &["branch", "tag"])?;
    let enforcement = match (input.enforcement, base) {
        (Some(e), _) => e,
        (None, Some(b)) => b.enforcement.clone(),
        (None, None) => {
            return Err(ApiError::invalid_field(FieldError::missing_field(
                "Ruleset",
                "enforcement",
            )));
        }
    };
    check_enum(
        "enforcement",
        &enforcement,
        &["disabled", "active", "evaluate"],
    )?;
    let conditions = match (input.conditions, base) {
        (Some(c), _) => normalize_conditions(&c)?,
        (None, Some(b)) => b.conditions.clone(),
        (None, None) => normalize_conditions(&Value::Null)?,
    };
    let rules = match (input.rules, base) {
        (Some(r), _) => normalize_rules(&r)?,
        (None, Some(b)) => b.rules.clone(),
        (None, None) => json!([]),
    };
    let bypass_actors = match (input.bypass_actors, base) {
        (Some(a), _) => normalize_bypass(state, access, &a).await?,
        (None, Some(b)) => b.bypass_actors.clone(),
        (None, None) => json!([]),
    };
    Ok(Fields {
        name,
        target,
        enforcement,
        conditions,
        rules,
        bypass_actors,
    })
}

fn map_unique(e: sqlx::Error) -> ApiError {
    match bgh_core::db::unique_violation(&e).as_deref() {
        Some("repo_rulesets_name_key") => {
            ApiError::invalid_field(FieldError::already_exists("Ruleset", "name"))
        }
        _ => e.into(),
    }
}

fn sync_json(r: &RulesetRow) -> Value {
    json!({
        "id": r.id,
        "repo_id": r.repo_id,
        "name": r.name,
        "target": r.target,
        "enforcement": r.enforcement,
        "conditions": r.conditions,
        "rules": r.rules,
        "bypass_actors": r.bypass_actors,
        "updated_at": Timestamp::from(r.updated_at),
    })
}

fn audit_target(access: &RepoAccess) -> audit::Target {
    audit::Target::Repo {
        id: access.repo.id,
        org_id: access.owner.is_org().then_some(access.owner.id),
    }
}

async fn find(state: &AppState, access: &RepoAccess, id: i64) -> ApiResult<RulesetRow> {
    sqlx::query_as(&format!(
        "SELECT {} FROM repo_rulesets WHERE id = $1 AND repo_id = $2",
        RulesetRow::COLUMNS
    ))
    .bind(id)
    .bind(access.repo.id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)
}

fn user_of(auth: &MaybeUser) -> ApiResult<&db::User> {
    auth.as_ref()
        .map(|a| &a.user)
        .ok_or_else(ApiError::requires_auth)
}

// ----- handlers ----------------------------------------------------------------------

/// `GET /repos/{owner}/{repo}/rulesets`
async fn list(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<Page<Value>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let rows: Vec<RulesetRow> = sqlx::query_as(&format!(
        "SELECT {} FROM repo_rulesets WHERE repo_id = $1 ORDER BY id LIMIT $2 OFFSET $3",
        RulesetRow::COLUMNS
    ))
    .bind(access.repo.id)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    Ok(p.page(rows).map(|r| summary(&state, &access, &r)))
}

/// `GET /repos/{owner}/{repo}/rulesets/{id}`
async fn get_one(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, id)): Path<(String, String, i64)>,
) -> ApiResult<Json<Value>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let row = find(&state, &access, id).await?;
    let actor = load_actor(&state, auth.as_ref(), &access).await?;
    Ok(Json(full(
        &state,
        &access,
        &row,
        bypass_mode(&row, actor.as_ref()),
    )))
}

/// `POST /repos/{owner}/{repo}/rulesets`
async fn create(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo)): Path<(String, String)>,
    Json(input): Json<RulesetInput>,
) -> ApiResult<Response> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    access.require(Permission::Admin)?;
    access.require_not_archived()?;
    let user = user_of(&auth)?;
    let f = validate(&state, &access, input, None).await?;

    let mut tx = Tx::begin(&state).await?;
    let row: RulesetRow = sqlx::query_as(&format!(
        "INSERT INTO repo_rulesets (repo_id, name, target, enforcement, conditions, rules,
                                    bypass_actors, created_by_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING {}",
        RulesetRow::COLUMNS
    ))
    .bind(access.repo.id)
    .bind(&f.name)
    .bind(&f.target)
    .bind(&f.enforcement)
    .bind(&f.conditions)
    .bind(&f.rules)
    .bind(&f.bypass_actors)
    .bind(user.id)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_unique)?;
    tx.sync(
        &access.scope(),
        "ruleset",
        row.id,
        SyncAction::Insert,
        &sync_json(&row),
    )
    .await?;
    audit::log(
        &mut *tx,
        Some(user),
        "repository_ruleset.create",
        audit_target(&access),
        json!({"ruleset_id": row.id, "name": row.name}),
    )
    .await?;
    tx.emit(Event::RepositoryUpdated {
        repo_id: access.repo.id,
        actor_id: user.id,
    });
    tx.commit().await?;

    let actor = load_actor(&state, auth.as_ref(), &access).await?;
    let body = full(&state, &access, &row, bypass_mode(&row, actor.as_ref()));
    Ok((StatusCode::CREATED, Json(body)).into_response())
}

/// `PUT /repos/{owner}/{repo}/rulesets/{id}` (partial update)
async fn update(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, id)): Path<(String, String, i64)>,
    Json(input): Json<RulesetInput>,
) -> ApiResult<Json<Value>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    access.require(Permission::Admin)?;
    access.require_not_archived()?;
    let user = user_of(&auth)?;
    let existing = find(&state, &access, id).await?;
    let f = validate(&state, &access, input, Some(&existing)).await?;

    let mut tx = Tx::begin(&state).await?;
    let row: RulesetRow = sqlx::query_as(&format!(
        "UPDATE repo_rulesets SET name = $3, target = $4, enforcement = $5, conditions = $6,
                rules = $7, bypass_actors = $8, updated_at = now()
          WHERE id = $1 AND repo_id = $2 RETURNING {}",
        RulesetRow::COLUMNS
    ))
    .bind(id)
    .bind(access.repo.id)
    .bind(&f.name)
    .bind(&f.target)
    .bind(&f.enforcement)
    .bind(&f.conditions)
    .bind(&f.rules)
    .bind(&f.bypass_actors)
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_unique)?
    .ok_or(ApiError::NotFound)?;
    tx.sync(
        &access.scope(),
        "ruleset",
        row.id,
        SyncAction::Update,
        &sync_json(&row),
    )
    .await?;
    audit::log(
        &mut *tx,
        Some(user),
        "repository_ruleset.update",
        audit_target(&access),
        json!({"ruleset_id": row.id, "name": row.name}),
    )
    .await?;
    tx.emit(Event::RepositoryUpdated {
        repo_id: access.repo.id,
        actor_id: user.id,
    });
    tx.commit().await?;

    let actor = load_actor(&state, auth.as_ref(), &access).await?;
    Ok(Json(full(
        &state,
        &access,
        &row,
        bypass_mode(&row, actor.as_ref()),
    )))
}

/// `DELETE /repos/{owner}/{repo}/rulesets/{id}`
async fn destroy(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, id)): Path<(String, String, i64)>,
) -> ApiResult<StatusCode> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    access.require(Permission::Admin)?;
    access.require_not_archived()?;
    let user = user_of(&auth)?;

    let mut tx = Tx::begin(&state).await?;
    let name: String = sqlx::query_scalar(
        "DELETE FROM repo_rulesets WHERE id = $1 AND repo_id = $2 RETURNING name",
    )
    .bind(id)
    .bind(access.repo.id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(ApiError::NotFound)?;
    tx.sync(
        &access.scope(),
        "ruleset",
        id,
        SyncAction::Delete,
        &json!({"id": id}),
    )
    .await?;
    audit::log(
        &mut *tx,
        Some(user),
        "repository_ruleset.destroy",
        audit_target(&access),
        json!({"ruleset_id": id, "name": name}),
    )
    .await?;
    tx.emit(Event::RepositoryUpdated {
        repo_id: access.repo.id,
        actor_id: user.id,
    });
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /repos/{owner}/{repo}/rules/branches/{*branch}`: rules of active
/// rulesets selecting the branch, flattened.
async fn rules_for_branch(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo, branch)): Path<(String, String, String)>,
) -> ApiResult<Page<Value>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let rows: Vec<RulesetRow> = sqlx::query_as(&format!(
        "SELECT {} FROM repo_rulesets WHERE repo_id = $1 AND enforcement = 'active' ORDER BY id",
        RulesetRow::COLUMNS
    ))
    .bind(access.repo.id)
    .fetch_all(&state.db)
    .await?;
    let refname = format!("refs/heads/{branch}");
    let source = access.full_name();
    let mut items: Vec<Value> = Vec::new();
    for r in rows
        .iter()
        .filter(|r| r.applies_to(&refname, &access.repo.default_branch))
    {
        for rule in r.rules.as_array().into_iter().flatten() {
            let mut item = Map::new();
            item.insert("type".into(), rule["type"].clone());
            if let Some(params) = rule.get("parameters").filter(|v| !v.is_null()) {
                item.insert("parameters".into(), params.clone());
            }
            item.insert("ruleset_source_type".into(), json!("Repository"));
            item.insert("ruleset_source".into(), json!(source));
            item.insert("ruleset_id".into(), json!(r.id));
            items.push(Value::Object(item));
        }
    }
    let total = items.len() as i64;
    let page: Vec<Value> = items
        .into_iter()
        .skip(p.offset() as usize)
        .take(p.limit() as usize)
        .collect();
    Ok(p.page_with_total(page, total))
}
