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

/// Rule types accepted by the API (GitHub's `repository-rule` variants).
pub(crate) const RULE_TYPES: &[&str] = &[
    "creation",
    "update",
    "deletion",
    "required_linear_history",
    "merge_queue",
    "required_deployments",
    "required_signatures",
    "pull_request",
    "required_status_checks",
    "non_fast_forward",
    "commit_message_pattern",
    "commit_author_email_pattern",
    "committer_email_pattern",
    "branch_name_pattern",
    "tag_name_pattern",
    "file_path_restriction",
    "max_file_path_length",
    "file_extension_restriction",
    "max_file_size",
    "workflows",
    "code_scanning",
];

/// Rule types of `push` rulesets (the only ones they accept).
pub(crate) const PUSH_RULE_TYPES: &[&str] = &[
    "file_path_restriction",
    "max_file_path_length",
    "file_extension_restriction",
    "max_file_size",
];

// ----- rendering -------------------------------------------------------------------

fn node_id(r: &RulesetRow) -> String {
    format!(
        "RRS_{}",
        URL_SAFE_NO_PAD.encode(format!(
            "Ruleset:{}:{}",
            r.org_id.unwrap_or(r.repo_id),
            r.id
        ))
    )
}

/// List item shape (`repository-ruleset` without rules / conditions).
/// `owner` is the organization of an org ruleset or the repository owner;
/// `repo` the repository name of a repository ruleset.
pub(crate) fn summary(state: &AppState, owner: &str, repo: Option<&str>, r: &RulesetRow) -> Value {
    let (source, href, html) = match (r.org_id, repo) {
        (None, Some(n)) => (
            format!("{owner}/{n}"),
            format!("{}/rulesets/{}", state.urls.repo(owner, n), r.id),
            format!("{}/rules/{}", state.urls.repo_html(owner, n), r.id),
        ),
        _ => (
            owner.to_string(),
            format!("{}/rulesets/{}", state.urls.org(owner), r.id),
            state
                .urls
                .html(&format!("/organizations/{owner}/settings/rules/{}", r.id)),
        ),
    };
    json!({
        "id": r.id,
        "name": r.name,
        "target": r.target,
        "source_type": r.source_type(),
        "source": source,
        "enforcement": r.enforcement,
        "node_id": node_id(r),
        "_links": {
            "self": {"href": href},
            "html": {"href": html},
        },
        "created_at": Timestamp::from(r.created_at),
        "updated_at": Timestamp::from(r.updated_at),
    })
}

/// Full `repository-ruleset`. `bypass_actors` only for admins, like GitHub.
pub(crate) fn full(
    state: &AppState,
    owner: &str,
    repo: Option<&str>,
    r: &RulesetRow,
    admin: bool,
    can_bypass: &str,
) -> Value {
    let mut v = summary(state, owner, repo, r);
    if admin {
        v["bypass_actors"] = r.bypass_actors.clone();
    }
    v["conditions"] = r.conditions.clone();
    v["rules"] = r.rules.clone();
    v["current_user_can_bypass"] = json!(can_bypass);
    v
}

/// `"always"`, `"pull_requests_only"` or `"never"` for `actor` (mirrors
/// the bypass evaluation of the rules engine).
pub(crate) fn bypass_mode(r: &RulesetRow, actor: Option<&Actor>) -> &'static str {
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
pub(crate) struct RulesetInput {
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

fn include_exclude(v: &Value, field: &str) -> ApiResult<Value> {
    if !(v.is_object() || v.is_null()) {
        return Err(invalid("conditions", format!("{field} must be an object")));
    }
    let include = string_array(&v["include"], &format!("{field}.include"))?;
    let exclude = string_array(&v["exclude"], &format!("{field}.exclude"))?;
    Ok(json!({"include": include, "exclude": exclude}))
}

/// Normalized `conditions`: `ref_name` (branch and tag rulesets) plus, for
/// organization rulesets, one of `repository_name`, `repository_id` or
/// `repository_property`.
fn normalize_conditions(v: &Value, target: &str, org: bool) -> ApiResult<Value> {
    if !(v.is_object() || v.is_null()) {
        return Err(invalid("conditions", "conditions must be an object"));
    }
    let mut out = Map::new();
    if target != "push" {
        out.insert(
            "ref_name".into(),
            include_exclude(&v["ref_name"], "conditions.ref_name")?,
        );
    }
    if org {
        if !v["repository_name"].is_null() {
            let mut c = include_exclude(&v["repository_name"], "conditions.repository_name")?;
            c["protected"] = json!(bool_value(
                &v["repository_name"]["protected"],
                "conditions.repository_name.protected"
            )?);
            out.insert("repository_name".into(), c);
        } else if !v["repository_id"].is_null() {
            let ids = match &v["repository_id"]["repository_ids"] {
                Value::Array(a) => a
                    .iter()
                    .map(|x| {
                        x.as_i64().ok_or_else(|| {
                            invalid("conditions", "repository_ids must be an array of integers")
                        })
                    })
                    .collect::<ApiResult<Vec<i64>>>()?,
                Value::Null => vec![],
                _ => {
                    return Err(invalid(
                        "conditions",
                        "repository_ids must be an array of integers",
                    ));
                }
            };
            out.insert("repository_id".into(), json!({"repository_ids": ids}));
        } else if v["repository_property"].is_object() {
            // Custom properties are not implemented: stored for round trips,
            // never matching a repository.
            out.insert(
                "repository_property".into(),
                v["repository_property"].clone(),
            );
        } else {
            return Err(invalid(
                "conditions",
                "Organization rulesets need a repository_name, repository_id or \
                 repository_property condition",
            ));
        }
    }
    Ok(Value::Object(out))
}

fn bool_value(v: &Value, what: &str) -> ApiResult<bool> {
    match v {
        Value::Null => Ok(false),
        Value::Bool(b) => Ok(*b),
        _ => Err(invalid(
            "rules",
            format!("Invalid {what}: expected a boolean"),
        )),
    }
}

fn bool_param(p: &Value, rule: &str, key: &str) -> ApiResult<bool> {
    bool_value(&p[key], &format!("parameter '{key}' for rule '{rule}'"))
}

fn int_param(
    p: &Value,
    rule: &str,
    key: &str,
    range: (i64, i64),
    default: Option<i64>,
) -> ApiResult<i64> {
    match (&p[key], default) {
        (Value::Null, Some(d)) => Ok(d),
        (v, _) => v
            .as_i64()
            .filter(|n| (range.0..=range.1).contains(n))
            .ok_or_else(|| {
                invalid(
                    "rules",
                    format!(
                        "Invalid parameter '{key}' for rule '{rule}': expected an integer \
                         between {} and {}",
                        range.0, range.1
                    ),
                )
            }),
    }
}

fn enum_param(
    p: &Value,
    rule: &str,
    key: &str,
    allowed: &[&str],
    default: Option<&str>,
) -> ApiResult<String> {
    match (&p[key], default) {
        (Value::Null, Some(d)) => Ok(d.to_string()),
        (v, _) => v
            .as_str()
            .filter(|s| allowed.contains(s))
            .map(str::to_string)
            .ok_or_else(|| {
                invalid(
                    "rules",
                    format!(
                        "Invalid parameter '{key}' for rule '{rule}': expected one of {}",
                        allowed.join(", ")
                    ),
                )
            }),
    }
}

fn strings_param(p: &Value, rule: &str, key: &str, required: bool) -> ApiResult<Vec<String>> {
    if required && !p[key].is_array() {
        return Err(invalid(
            "rules",
            format!("Rule '{rule}' needs parameters.{key}"),
        ));
    }
    string_array(&p[key], &format!("parameters.{key}"))
}

fn normalize_rule(v: &Value, target: &str) -> ApiResult<(String, Value)> {
    let Some(kind) = v["type"].as_str() else {
        return Err(invalid("rules", "Each rule needs a 'type'"));
    };
    if !RULE_TYPES.contains(&kind) {
        return Err(invalid("rules", format!("Invalid rule '{kind}'")));
    }
    if target == "push" && !PUSH_RULE_TYPES.contains(&kind) {
        return Err(invalid(
            "rules",
            format!("Invalid rule '{kind}' for a push ruleset"),
        ));
    }
    let p = &v["parameters"];
    if !(p.is_object() || p.is_null()) {
        return Err(invalid(
            "rules",
            format!("Invalid parameters for rule '{kind}'"),
        ));
    }
    let params = match kind {
        "update" => Some(json!({
            "update_allows_fetch_and_merge": bool_param(p, kind, "update_allows_fetch_and_merge")?,
        })),
        "pull_request" => {
            let methods = match &p["allowed_merge_methods"] {
                Value::Null => vec!["merge".to_string(), "squash".into(), "rebase".into()],
                m => {
                    let m = string_array(m, "parameters.allowed_merge_methods")?;
                    for x in &m {
                        check_enum("allowed_merge_methods", x, &["merge", "squash", "rebase"])?;
                    }
                    m
                }
            };
            Some(json!({
                "allowed_merge_methods": methods,
                "required_approving_review_count":
                    int_param(p, kind, "required_approving_review_count", (0, 10), Some(0))?,
                "dismiss_stale_reviews_on_push": bool_param(p, kind, "dismiss_stale_reviews_on_push")?,
                "require_code_owner_review": bool_param(p, kind, "require_code_owner_review")?,
                "require_last_push_approval": bool_param(p, kind, "require_last_push_approval")?,
                "required_review_thread_resolution":
                    bool_param(p, kind, "required_review_thread_resolution")?,
            }))
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
            let mut out = json!({
                "required_status_checks": checks,
                "strict_required_status_checks_policy":
                    bool_param(p, kind, "strict_required_status_checks_policy")?,
            });
            if !p["do_not_enforce_on_create"].is_null() {
                out["do_not_enforce_on_create"] =
                    json!(bool_param(p, kind, "do_not_enforce_on_create")?);
            }
            Some(out)
        }
        "commit_message_pattern"
        | "commit_author_email_pattern"
        | "committer_email_pattern"
        | "branch_name_pattern"
        | "tag_name_pattern" => {
            let operator = enum_param(
                p,
                kind,
                "operator",
                &["starts_with", "ends_with", "contains", "regex"],
                None,
            )?;
            let Some(pattern) = p["pattern"].as_str() else {
                return Err(invalid(
                    "rules",
                    format!("Rule '{kind}' needs parameters.pattern"),
                ));
            };
            if operator == "regex" && regex::Regex::new(pattern).is_err() {
                return Err(invalid(
                    "rules",
                    format!("Invalid regular expression for rule '{kind}': {pattern}"),
                ));
            }
            let mut out = json!({
                "operator": operator,
                "pattern": pattern,
                "negate": bool_param(p, kind, "negate")?,
            });
            match &p["name"] {
                Value::Null => {}
                Value::String(n) => out["name"] = json!(n),
                _ => return Err(invalid("rules", "name must be a string")),
            }
            Some(out)
        }
        "file_path_restriction" => Some(json!({
            "restricted_file_paths": strings_param(p, kind, "restricted_file_paths", true)?,
        })),
        "file_extension_restriction" => Some(json!({
            "restricted_file_extensions":
                strings_param(p, kind, "restricted_file_extensions", true)?,
        })),
        "max_file_path_length" => Some(json!({
            "max_file_path_length": int_param(p, kind, "max_file_path_length", (1, 256), None)?,
        })),
        "max_file_size" => Some(json!({
            "max_file_size": int_param(p, kind, "max_file_size", (1, 100), None)?,
        })),
        "required_deployments" => Some(json!({
            "required_deployment_environments":
                strings_param(p, kind, "required_deployment_environments", false)?,
        })),
        "merge_queue" => Some(json!({
            "check_response_timeout_minutes":
                int_param(p, kind, "check_response_timeout_minutes", (1, 360), Some(60))?,
            "grouping_strategy":
                enum_param(p, kind, "grouping_strategy", &["ALLGREEN", "HEADGREEN"], Some("ALLGREEN"))?,
            "max_entries_to_build": int_param(p, kind, "max_entries_to_build", (0, 100), Some(5))?,
            "max_entries_to_merge": int_param(p, kind, "max_entries_to_merge", (0, 100), Some(5))?,
            "merge_method":
                enum_param(p, kind, "merge_method", &["MERGE", "SQUASH", "REBASE"], Some("MERGE"))?,
            "min_entries_to_merge": int_param(p, kind, "min_entries_to_merge", (0, 100), Some(1))?,
            "min_entries_to_merge_wait_minutes":
                int_param(p, kind, "min_entries_to_merge_wait_minutes", (0, 360), Some(5))?,
        })),
        "workflows" => {
            let mut list = Vec::new();
            for w in p["workflows"].as_array().into_iter().flatten() {
                let (Some(path), Some(repo_id)) = (w["path"].as_str(), w["repository_id"].as_i64())
                else {
                    return Err(invalid(
                        "rules",
                        "Each workflow needs a 'path' and a 'repository_id'",
                    ));
                };
                let mut item = json!({"path": path, "repository_id": repo_id});
                for k in ["ref", "sha"] {
                    if let Some(x) = w[k].as_str() {
                        item[k] = json!(x);
                    }
                }
                list.push(item);
            }
            Some(json!({
                "do_not_enforce_on_create": bool_param(p, kind, "do_not_enforce_on_create")?,
                "workflows": list,
            }))
        }
        "code_scanning" => {
            let mut tools = Vec::new();
            for t in p["code_scanning_tools"].as_array().into_iter().flatten() {
                let Some(tool) = t["tool"].as_str().filter(|s| !s.is_empty()) else {
                    return Err(invalid("rules", "Each code scanning tool needs a 'tool'"));
                };
                tools.push(json!({
                    "tool": tool,
                    "alerts_threshold": enum_param(t, kind, "alerts_threshold",
                        &["none", "errors", "errors_and_warnings", "all"], Some("errors"))?,
                    "security_alerts_threshold": enum_param(t, kind, "security_alerts_threshold",
                        &["none", "critical", "high_or_higher", "medium_or_higher", "all"],
                        Some("high_or_higher"))?,
                }));
            }
            Some(json!({"code_scanning_tools": tools}))
        }
        _ => None,
    };
    let rule = match params {
        Some(params) => json!({"type": kind, "parameters": params}),
        None => json!({"type": kind}),
    };
    Ok((kind.to_string(), rule))
}

pub(crate) fn normalize_rules(rules: &[Value], target: &str) -> ApiResult<Value> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for r in rules {
        let (kind, rule) = normalize_rule(r, target)?;
        if !seen.insert(kind.clone()) {
            return Err(invalid("rules", format!("Duplicate rule '{kind}'")));
        }
        out.push(rule);
    }
    Ok(Value::Array(out))
}

/// Validate `bypass_actors` of a ruleset owned by (or in a repository of)
/// `owner`.
async fn normalize_bypass(
    state: &AppState,
    owner: &db::User,
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
            &[
                "RepositoryRole",
                "OrganizationAdmin",
                "Team",
                "User",
                "Integration",
                "DeployKey",
            ],
        )?;
        let mode = match &a["bypass_mode"] {
            Value::Null => "always",
            m => m.as_str().unwrap_or(""),
        };
        check_enum("bypass_mode", mode, &["always", "pull_request", "exempt"])?;
        let id = a["actor_id"].as_i64();
        let id = match kind {
            "RepositoryRole" => Some(id.filter(|i| (1..=5).contains(i)).ok_or_else(|| {
                invalid(
                    "bypass_actors",
                    "RepositoryRole actor_id must be between 1 and 5",
                )
            })?),
            "OrganizationAdmin" => {
                if !owner.is_org() {
                    return Err(invalid(
                        "bypass_actors",
                        "OrganizationAdmin is not applicable for personal repositories",
                    ));
                }
                Some(id.unwrap_or(1))
            }
            "DeployKey" => None,
            "Integration" => Some(
                id.filter(|i| *i > 0)
                    .ok_or_else(|| invalid("bypass_actors", "Integration actor_id is required"))?,
            ),
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
                .bind(owner.id)
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
pub(crate) struct Fields {
    pub name: String,
    pub target: String,
    pub enforcement: String,
    pub conditions: Value,
    pub rules: Value,
    pub bypass_actors: Value,
}

/// Validate a create (`base: None`) or partial update of a ruleset owned
/// by `owner` (the organization for `org`, else the repository owner).
pub(crate) async fn validate(
    state: &AppState,
    owner: &db::User,
    org: bool,
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
    let target_changed = input.target.is_some();
    let target = input
        .target
        .or_else(|| base.map(|b| b.target.clone()))
        .unwrap_or_else(|| "branch".into());
    check_enum("target", &target, &["branch", "tag", "push"])?;
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
        (Some(c), _) => normalize_conditions(&c, &target, org)?,
        (None, Some(b)) if !target_changed => b.conditions.clone(),
        (None, Some(b)) => normalize_conditions(&b.conditions, &target, org)?,
        (None, None) => normalize_conditions(&Value::Null, &target, org)?,
    };
    let rules = match (input.rules, base) {
        (Some(r), _) => normalize_rules(&r, &target)?,
        (None, Some(b)) => {
            if target_changed {
                normalize_rules(b.rules.as_array().map_or(&[][..], |v| v), &target)?;
            }
            b.rules.clone()
        }
        (None, None) => json!([]),
    };
    let bypass_actors = match (input.bypass_actors, base) {
        (Some(a), _) => normalize_bypass(state, owner, &a).await?,
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

pub(crate) fn map_unique(e: sqlx::Error) -> ApiError {
    match bgh_core::db::unique_violation(&e).as_deref() {
        Some("repo_rulesets_name_key" | "org_rulesets_name_key") => {
            ApiError::invalid_field(FieldError::already_exists("Ruleset", "name"))
        }
        _ => e.into(),
    }
}

pub(crate) fn sync_json(r: &RulesetRow) -> Value {
    json!({
        "id": r.id,
        "repo_id": r.org_id.is_none().then_some(r.repo_id),
        "org_id": r.org_id,
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

/// Repository rulesets of `access`, plus (`includes_parents`) the
/// organization rulesets selecting the repository.
async fn rulesets_of(
    state: &AppState,
    access: &RepoAccess,
    includes_parents: bool,
) -> ApiResult<Vec<RulesetRow>> {
    let rows: Vec<RulesetRow> = sqlx::query_as(&format!(
        "SELECT {} FROM repo_rulesets
          WHERE repo_id = $1 OR ($3 AND org_id = $2)
          ORDER BY org_id NULLS FIRST, id",
        RulesetRow::COLUMNS
    ))
    .bind(access.repo.id)
    .bind(access.owner.id)
    .bind(includes_parents && access.owner.is_org())
    .fetch_all(&state.db)
    .await?;
    Ok(rows
        .into_iter()
        .filter(|r| r.applies_to_repo(&access.repo))
        .collect())
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

pub(crate) fn user_of(auth: &MaybeUser) -> ApiResult<&db::User> {
    auth.as_ref()
        .map(|a| &a.user)
        .ok_or_else(ApiError::requires_auth)
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct ListQuery {
    /// Include organization rulesets (default true, like GitHub).
    includes_parents: Option<bool>,
    /// Comma separated `branch`, `tag`, `push`.
    targets: Option<String>,
}

impl ListQuery {
    pub(crate) fn wants(&self, target: &str) -> bool {
        self.targets
            .as_deref()
            .is_none_or(|t| t.split(',').any(|x| x.trim() == target))
    }
}

// ----- handlers ----------------------------------------------------------------------

/// `GET /repos/{owner}/{repo}/rulesets`
async fn list(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Query(q): Query<ListQuery>,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<Page<Value>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let rows: Vec<RulesetRow> = rulesets_of(&state, &access, q.includes_parents.unwrap_or(true))
        .await?
        .into_iter()
        .filter(|r| q.wants(&r.target))
        .collect();
    let total = rows.len() as i64;
    let (o, n) = (&access.owner.login, &access.repo.name);
    let page: Vec<Value> = rows
        .iter()
        .skip(p.offset() as usize)
        .take(p.limit() as usize)
        .map(|r| summary(&state, o, Some(n), r))
        .collect();
    Ok(p.page_with_total(page, total))
}

/// `GET /repos/{owner}/{repo}/rulesets/{id}` (also organization rulesets
/// selecting the repository, unless `includes_parents=false`)
async fn get_one(
    State(state): State<AppState>,
    auth: MaybeUser,
    Query(q): Query<ListQuery>,
    Path((owner, repo, id)): Path<(String, String, i64)>,
) -> ApiResult<Json<Value>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let row = rulesets_of(&state, &access, q.includes_parents.unwrap_or(true))
        .await?
        .into_iter()
        .find(|r| r.id == id)
        .ok_or(ApiError::NotFound)?;
    let actor = load_actor(&state, auth.as_ref(), &access).await?;
    let admin = if row.org_id.is_some() {
        actor.as_ref().is_some_and(|a| a.org_admin)
            || auth.as_ref().is_some_and(|a| a.user.site_admin)
    } else {
        access.permission >= Permission::Admin
    };
    Ok(Json(full(
        &state,
        &access.owner.login,
        Some(&access.repo.name),
        &row,
        admin,
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
    let f = validate(&state, &access.owner, false, input, None).await?;
    let row = insert_repo_ruleset(&state, &access, user, &f, false).await?;
    let actor = load_actor(&state, auth.as_ref(), &access).await?;
    let body = full(
        &state,
        &access.owner.login,
        Some(&access.repo.name),
        &row,
        true,
        bypass_mode(&row, actor.as_ref()),
    );
    Ok((StatusCode::CREATED, Json(body)).into_response())
}

/// Insert a repository ruleset (sync, audit, event).
pub(crate) async fn insert_repo_ruleset(
    state: &AppState,
    access: &RepoAccess,
    user: &db::User,
    f: &Fields,
    tag_protection: bool,
) -> ApiResult<RulesetRow> {
    let mut tx = Tx::begin(state).await?;
    let row: RulesetRow = sqlx::query_as(&format!(
        "INSERT INTO repo_rulesets (repo_id, name, target, enforcement, conditions, rules,
                                    bypass_actors, created_by_id, tag_protection)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING {}",
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
    .bind(tag_protection)
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
        audit_target(access),
        json!({"ruleset_id": row.id, "name": row.name}),
    )
    .await?;
    tx.emit(Event::RepositoryUpdated {
        repo_id: access.repo.id,
        actor_id: user.id,
    });
    tx.commit().await?;
    Ok(row)
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
    let f = validate(&state, &access.owner, false, input, Some(&existing)).await?;

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
        &access.owner.login,
        Some(&access.repo.name),
        &row,
        true,
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
    delete_repo_ruleset(&state, &access, user, id, false).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Delete a repository ruleset (only legacy tag protections when
/// `tag_protection`); 404 when missing.
pub(crate) async fn delete_repo_ruleset(
    state: &AppState,
    access: &RepoAccess,
    user: &db::User,
    id: i64,
    tag_protection: bool,
) -> ApiResult<()> {
    let mut tx = Tx::begin(state).await?;
    let name: String = sqlx::query_scalar(
        "DELETE FROM repo_rulesets WHERE id = $1 AND repo_id = $2 AND (tag_protection OR NOT $3)
         RETURNING name",
    )
    .bind(id)
    .bind(access.repo.id)
    .bind(tag_protection)
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
        audit_target(access),
        json!({"ruleset_id": id, "name": name}),
    )
    .await?;
    tx.emit(Event::RepositoryUpdated {
        repo_id: access.repo.id,
        actor_id: user.id,
    });
    tx.commit().await?;
    Ok(())
}

/// `GET /repos/{owner}/{repo}/rules/branches/{*branch}`: rules of active
/// rulesets (repository and organization) selecting the branch, flattened.
async fn rules_for_branch(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo, branch)): Path<(String, String, String)>,
) -> ApiResult<Page<Value>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let rows: Vec<RulesetRow> = rulesets_of(&state, &access, true)
        .await?
        .into_iter()
        .filter(|r| r.enforcement == "active" && r.target != "push")
        .collect();
    let refname = format!("refs/heads/{branch}");
    let repo_source = access.full_name();
    let mut items: Vec<Value> = Vec::new();
    for r in rows
        .iter()
        .filter(|r| r.applies_to(&refname, &access.repo.default_branch))
    {
        let source = if r.org_id.is_some() {
            access.owner.login.clone()
        } else {
            repo_source.clone()
        };
        for rule in r.rules.as_array().into_iter().flatten() {
            let mut item = Map::new();
            item.insert("type".into(), rule["type"].clone());
            if let Some(params) = rule.get("parameters").filter(|v| !v.is_null()) {
                item.insert("parameters".into(), params.clone());
            }
            item.insert("ruleset_source_type".into(), json!(r.source_type()));
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
