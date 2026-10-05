//! Branch protection REST API (classic rules, `branch_protections`).
//!
//! Branch names contain `/`, so these endpoints are not routed here: the
//! branches module owns `/repos/{owner}/{repo}/branches/{*rest}` and hands
//! `{branch}/protection[/...]` paths (see [`split_protection_path`]) to
//! [`dispatch`] (or the per-method `dispatch_*` wrappers, or the ready-made
//! axum handler [`handle`]).
//!
//! * `GET|PUT|DELETE .../branches/{branch}/protection`
//! * `GET|PATCH|DELETE .../protection/required_status_checks`
//! * `GET|POST|PUT|DELETE .../protection/required_status_checks/contexts`
//! * `GET|POST|DELETE .../protection/enforce_admins`
//! * `GET|PATCH|DELETE .../protection/required_pull_request_reviews`
//! * `GET|POST|DELETE .../protection/required_signatures`
//! * `GET|DELETE .../protection/restrictions`
//! * `GET|POST|PUT|DELETE .../protection/restrictions/{users,teams,apps}`
//!
//! All endpoints require admin rights; rows are keyed by `pattern` = the
//! branch name, and the branch must exist. Users and teams are stored as id
//! arrays (see `migrations/0002_repositories.sql`).

use std::collections::HashMap;

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use bgh_core::audit;
use bgh_core::extract::parse_json;
use bgh_core::models::api::{SimpleUser, Team, TeamSimple};
use bgh_core::prelude::*;
use bgh_core::urls::encode_path;
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};

use crate::protection::ProtectionRow;

/// No routes of its own: see the module docs.
pub fn routes() -> Router<AppState> {
    Router::new()
}

// ----- path parsing ------------------------------------------------------------

fn valid_suffix(s: &[&str]) -> bool {
    matches!(
        s,
        [] | ["required_status_checks"]
            | ["required_status_checks", "contexts"]
            | ["enforce_admins"]
            | ["required_pull_request_reviews"]
            | ["required_signatures"]
            | ["restrictions"]
            | ["restrictions", "users" | "teams" | "apps"]
    )
}

/// Split the `{*rest}` of `/repos/{o}/{r}/branches/{*rest}` into the branch
/// name and the path after `protection` (`[]` for the protection itself).
/// `None` when `rest` is not a protection path. With ambiguous names (a
/// branch containing a `protection` segment), the longest branch wins.
pub fn split_protection_path(rest: &str) -> Option<(String, Vec<String>)> {
    let segs: Vec<&str> = rest.trim_end_matches('/').split('/').collect();
    (1..segs.len())
        .rev()
        .filter(|&i| segs[i] == "protection" && valid_suffix(&segs[i + 1..]))
        .map(|i| {
            (
                segs[..i].join("/"),
                segs[i + 1..].iter().map(|s| s.to_string()).collect(),
            )
        })
        .find(|(branch, _)| !branch.is_empty())
}

// ----- entry points ------------------------------------------------------------

/// Axum handler for `/repos/{owner}/{repo}/branches/{*rest}` when `rest` is
/// a protection path (404 otherwise). Convenience for the branches module.
pub async fn handle(
    State(state): State<AppState>,
    auth: MaybeUser,
    method: Method,
    Path((owner, repo, rest)): Path<(String, String, String)>,
    body: Bytes,
) -> ApiResult<Response> {
    let (branch, sub) = split_protection_path(&rest).ok_or(ApiError::NotFound)?;
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let sub: Vec<&str> = sub.iter().map(String::as_str).collect();
    dispatch(
        &state,
        auth.as_ref(),
        &access,
        &method,
        &branch,
        &sub,
        &body,
    )
    .await
}

/// Route a protection request. `sub` is the path after `protection`.
pub async fn dispatch(
    state: &AppState,
    auth: Option<&AuthContext>,
    access: &RepoAccess,
    method: &Method,
    branch: &str,
    sub: &[&str],
    body: &[u8],
) -> ApiResult<Response> {
    if !valid_suffix(sub) {
        return Err(ApiError::NotFound);
    }
    access.require(Permission::Admin)?;
    let write = *method != Method::GET && *method != Method::HEAD;
    let user = match auth {
        Some(a) => &a.user,
        None => return Err(ApiError::NotFound),
    };
    if write {
        access.require_not_archived()?;
    }
    require_branch(state, access, branch).await?;
    let cx = Cx {
        state,
        access,
        user,
        branch,
    };
    match (method.as_str(), sub) {
        ("GET" | "HEAD", []) => cx.get_protection().await,
        ("PUT", []) => cx.put_protection(body).await,
        ("DELETE", []) => cx.delete_protection().await,

        ("GET" | "HEAD", ["required_status_checks"]) => cx.get_status_checks().await,
        ("PATCH", ["required_status_checks"]) => cx.patch_status_checks(body).await,
        ("DELETE", ["required_status_checks"]) => cx.delete_status_checks().await,

        ("GET" | "HEAD", ["required_status_checks", "contexts"]) => cx.get_contexts().await,
        ("POST" | "PUT" | "DELETE", ["required_status_checks", "contexts"]) => {
            cx.write_contexts(method, body).await
        }

        ("GET" | "HEAD", ["enforce_admins"]) => cx.get_flag(Flag::EnforceAdmins).await,
        ("POST", ["enforce_admins"]) => cx.set_flag(Flag::EnforceAdmins, true).await,
        ("DELETE", ["enforce_admins"]) => cx.set_flag(Flag::EnforceAdmins, false).await,

        ("GET" | "HEAD", ["required_signatures"]) => cx.get_flag(Flag::Signatures).await,
        ("POST", ["required_signatures"]) => cx.set_flag(Flag::Signatures, true).await,
        ("DELETE", ["required_signatures"]) => cx.set_flag(Flag::Signatures, false).await,

        ("GET" | "HEAD", ["required_pull_request_reviews"]) => cx.get_reviews().await,
        ("PATCH", ["required_pull_request_reviews"]) => cx.patch_reviews(body).await,
        ("DELETE", ["required_pull_request_reviews"]) => cx.delete_reviews().await,

        ("GET" | "HEAD", ["restrictions"]) => cx.get_restrictions().await,
        ("DELETE", ["restrictions"]) => cx.delete_restrictions().await,
        ("GET" | "HEAD", ["restrictions", kind]) => cx.get_restriction_list(kind).await,
        ("POST" | "PUT" | "DELETE", ["restrictions", kind]) => {
            cx.write_restriction_list(method, kind, body).await
        }
        _ => Err(ApiError::NotFound),
    }
}

macro_rules! method_wrapper {
    ($name:ident, $method:expr) => {
        #[doc = concat!("[`dispatch`] for `", stringify!($method), "`.")]
        pub async fn $name(
            state: &AppState,
            auth: Option<&AuthContext>,
            access: &RepoAccess,
            branch: &str,
            sub: &[&str],
            _headers: &HeaderMap,
            body: Bytes,
        ) -> ApiResult<Response> {
            dispatch(state, auth, access, &$method, branch, sub, &body).await
        }
    };
}

method_wrapper!(dispatch_get, Method::GET);
method_wrapper!(dispatch_put, Method::PUT);
method_wrapper!(dispatch_post, Method::POST);
method_wrapper!(dispatch_patch, Method::PATCH);
method_wrapper!(dispatch_delete, Method::DELETE);

/// GitHub's `protection` object of the branch list / branch JSON.
pub fn protection_summary(row: &ProtectionRow) -> Value {
    let checks = checks_of(row.required_status_checks.as_ref());
    let level = if row.required_status_checks.is_none() {
        "off"
    } else if row.enforce_admins {
        "everyone"
    } else {
        "non_admins"
    };
    json!({
        "enabled": true,
        "required_status_checks": {
            "enforcement_level": level,
            "contexts": checks.iter().map(|c| &c.0).collect::<Vec<_>>(),
            "checks": checks_json(&checks),
        },
    })
}

// ----- helpers -------------------------------------------------------------------

fn not_found(msg: &str) -> ApiError {
    ApiError::Status(StatusCode::NOT_FOUND, msg.to_string())
}

fn not_protected() -> ApiError {
    not_found("Branch not protected")
}

fn ok_json(v: Value) -> ApiResult<Response> {
    Ok(Json(v).into_response())
}

fn no_content() -> ApiResult<Response> {
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn require_branch(state: &AppState, access: &RepoAccess, branch: &str) -> ApiResult<()> {
    let refname = format!("refs/heads/{branch}");
    let exists = crate::store(state)
        .read(access.repo.id, move |r| {
            Ok(r.find_ref(&refname).ok().flatten().is_some())
        })
        .await?;
    if exists {
        Ok(())
    } else {
        Err(not_found("Branch not found"))
    }
}

fn ids(v: Option<&Value>, key: &str) -> Vec<i64> {
    v.and_then(|v| v[key].as_array())
        .into_iter()
        .flatten()
        .filter_map(Value::as_i64)
        .collect()
}

/// `(context, app_id)` pairs of a stored `required_status_checks` value.
fn checks_of(v: Option<&Value>) -> Vec<(String, Option<i64>)> {
    let mut out: Vec<(String, Option<i64>)> = Vec::new();
    let Some(v) = v else { return out };
    for c in v["checks"].as_array().into_iter().flatten() {
        if let Some(ctx) = c["context"].as_str()
            && !out.iter().any(|(x, _)| x == ctx)
        {
            out.push((ctx.to_string(), c["app_id"].as_i64()));
        }
    }
    for c in v["contexts"].as_array().into_iter().flatten() {
        if let Some(ctx) = c.as_str()
            && !out.iter().any(|(x, _)| x == ctx)
        {
            out.push((ctx.to_string(), None));
        }
    }
    out
}

fn checks_json(checks: &[(String, Option<i64>)]) -> Value {
    checks
        .iter()
        .map(|(c, a)| json!({"context": c, "app_id": a}))
        .collect()
}

/// Stored `required_status_checks` value.
fn status_checks_value(strict: bool, checks: Vec<(String, Option<i64>)>) -> Value {
    let mut uniq: Vec<(String, Option<i64>)> = Vec::new();
    for (c, a) in checks {
        if !uniq.iter().any(|(x, _)| *x == c) {
            uniq.push((c, a));
        }
    }
    json!({
        "strict": strict,
        "contexts": uniq.iter().map(|c| &c.0).collect::<Vec<_>>(),
        "checks": checks_json(&uniq),
    })
}

/// Contexts keeping the `app_id` of existing checks.
fn checks_from_contexts(
    contexts: Vec<String>,
    existing: &[(String, Option<i64>)],
) -> Vec<(String, Option<i64>)> {
    contexts
        .into_iter()
        .map(|c| {
            let app = existing.iter().find(|(x, _)| *x == c).and_then(|e| e.1);
            (c, app)
        })
        .collect()
}

/// `Some(None)` for an explicit `null`, `None` when the key is missing.
fn double<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

#[derive(Debug, Deserialize)]
struct CheckInput {
    context: String,
    #[serde(default)]
    app_id: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct StatusChecksInput {
    strict: Option<bool>,
    contexts: Option<Vec<String>>,
    checks: Option<Vec<CheckInput>>,
}

#[derive(Debug, Default, Deserialize)]
struct PeopleInput {
    users: Option<Vec<String>>,
    teams: Option<Vec<String>>,
    apps: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct ReviewsInput {
    dismiss_stale_reviews: Option<bool>,
    require_code_owner_reviews: Option<bool>,
    required_approving_review_count: Option<i64>,
    require_last_push_approval: Option<bool>,
    #[serde(default, deserialize_with = "double")]
    dismissal_restrictions: Option<Option<PeopleInput>>,
    #[serde(default, deserialize_with = "double")]
    bypass_pull_request_allowances: Option<Option<PeopleInput>>,
}

#[derive(Debug, Deserialize)]
struct PutBody {
    #[serde(default, deserialize_with = "double")]
    required_status_checks: Option<Option<StatusChecksInput>>,
    #[serde(default, deserialize_with = "double")]
    enforce_admins: Option<Option<bool>>,
    #[serde(default, deserialize_with = "double")]
    required_pull_request_reviews: Option<Option<ReviewsInput>>,
    #[serde(default, deserialize_with = "double")]
    restrictions: Option<Option<PeopleInput>>,
    required_linear_history: Option<bool>,
    allow_force_pushes: Option<bool>,
    allow_deletions: Option<bool>,
    block_creations: Option<bool>,
    required_conversation_resolution: Option<bool>,
    lock_branch: Option<bool>,
    allow_fork_syncing: Option<bool>,
}

/// Login / slug → id lookups for one request (one query each).
#[derive(Default)]
struct Resolver {
    users: HashMap<String, i64>,
    teams: HashMap<String, i64>,
}

impl Resolver {
    async fn load(
        state: &AppState,
        access: &RepoAccess,
        logins: Vec<String>,
        slugs: Vec<String>,
    ) -> ApiResult<Self> {
        let mut out = Self::default();
        let logins: Vec<String> = logins.iter().map(|l| l.to_lowercase()).collect();
        let slugs: Vec<String> = slugs.iter().map(|s| s.to_lowercase()).collect();
        if !logins.is_empty() {
            let rows: Vec<(i64, String)> = sqlx::query_as(
                "SELECT id, lower(login) FROM users WHERE lower(login) = ANY($1) AND type = 'User'",
            )
            .bind(&logins)
            .fetch_all(&state.db)
            .await?;
            out.users = rows.into_iter().map(|(id, l)| (l, id)).collect();
        }
        if !slugs.is_empty() && access.owner.is_org() {
            let rows: Vec<(i64, String)> = sqlx::query_as(
                "SELECT id, lower(slug) FROM teams WHERE org_id = $1 AND lower(slug) = ANY($2)",
            )
            .bind(access.owner.id)
            .bind(&slugs)
            .fetch_all(&state.db)
            .await?;
            out.teams = rows.into_iter().map(|(id, s)| (s, id)).collect();
        }
        Ok(out)
    }

    fn user_ids(&self, logins: &[String]) -> ApiResult<Vec<i64>> {
        let mut out = Vec::new();
        for l in logins {
            let id = self.users.get(&l.to_lowercase()).ok_or_else(|| {
                ApiError::invalid_field(FieldError::custom(
                    "ProtectedBranch",
                    "users",
                    format!("Could not resolve to a User with the login of '{l}'."),
                ))
            })?;
            if !out.contains(id) {
                out.push(*id);
            }
        }
        Ok(out)
    }

    fn team_ids(&self, slugs: &[String]) -> ApiResult<Vec<i64>> {
        let mut out = Vec::new();
        for s in slugs {
            let id = self.teams.get(&s.to_lowercase()).ok_or_else(|| {
                ApiError::invalid_field(FieldError::custom(
                    "ProtectedBranch",
                    "teams",
                    format!("Could not resolve to a Team with the slug of '{s}'."),
                ))
            })?;
            if !out.contains(id) {
                out.push(*id);
            }
        }
        Ok(out)
    }
}

fn check_apps(apps: Option<&Vec<String>>) -> ApiResult<()> {
    match apps.and_then(|a| a.first()) {
        Some(a) => Err(ApiError::invalid_field(FieldError::custom(
            "ProtectedBranch",
            "apps",
            format!("Could not resolve to an App with the slug of '{a}'."),
        ))),
        None => Ok(()),
    }
}

fn people_names(p: Option<&PeopleInput>, logins: &mut Vec<String>, slugs: &mut Vec<String>) {
    if let Some(p) = p {
        logins.extend(p.users.iter().flatten().cloned());
        slugs.extend(p.teams.iter().flatten().cloned());
    }
}

/// `{"users": [ids], "teams": [ids]}` (+ `"apps": []` when `with_apps`).
fn people_value(p: &PeopleInput, r: &Resolver, with_apps: bool) -> ApiResult<Value> {
    check_apps(p.apps.as_ref())?;
    let users = r.user_ids(p.users.as_deref().unwrap_or_default())?;
    let teams = r.team_ids(p.teams.as_deref().unwrap_or_default())?;
    Ok(if with_apps {
        json!({"users": users, "teams": teams, "apps": []})
    } else {
        json!({"users": users, "teams": teams})
    })
}

fn org_only(access: &RepoAccess) -> ApiResult<()> {
    if access.owner.is_org() {
        Ok(())
    } else {
        Err(ApiError::unprocessable(
            "Only organization repositories can have users and team restrictions",
        ))
    }
}

fn review_count(n: i64) -> ApiResult<i64> {
    if (0..=6).contains(&n) {
        Ok(n)
    } else {
        Err(ApiError::invalid_field(FieldError::custom(
            "ProtectedBranch",
            "required_approving_review_count",
            "required_approving_review_count must be between 0 and 6",
        )))
    }
}

/// Apply a reviews input on top of `base` (stored value or `{}`).
fn reviews_value(base: Option<&Value>, input: &ReviewsInput, r: &Resolver) -> ApiResult<Value> {
    let b = base.cloned().unwrap_or_else(|| json!({}));
    let flag = |new: Option<bool>, k: &str| new.unwrap_or_else(|| b[k].as_bool().unwrap_or(false));
    let count = match input.required_approving_review_count {
        Some(n) => review_count(n)?,
        None => b["required_approving_review_count"].as_i64().unwrap_or(1),
    };
    let mut v = json!({
        "dismiss_stale_reviews": flag(input.dismiss_stale_reviews, "dismiss_stale_reviews"),
        "require_code_owner_reviews":
            flag(input.require_code_owner_reviews, "require_code_owner_reviews"),
        "required_approving_review_count": count,
        "require_last_push_approval":
            flag(input.require_last_push_approval, "require_last_push_approval"),
    });
    for (key, new) in [
        ("dismissal_restrictions", &input.dismissal_restrictions),
        (
            "bypass_pull_request_allowances",
            &input.bypass_pull_request_allowances,
        ),
    ] {
        let value = match new {
            None => b.get(key).filter(|x| !x.is_null()).cloned(),
            Some(None) => None,
            // `{}` disables the setting.
            Some(Some(p)) if p.users.is_none() && p.teams.is_none() && p.apps.is_none() => None,
            Some(Some(p)) => Some(people_value(p, r, true)?),
        };
        if let Some(value) = value {
            v[key] = value;
        }
    }
    Ok(v)
}

/// Mutable columns of a protection rule.
#[derive(Debug, Clone, Default)]
struct Draft {
    required_status_checks: Option<Value>,
    required_pull_request_reviews: Option<Value>,
    restrictions: Option<Value>,
    enforce_admins: bool,
    required_linear_history: bool,
    allow_force_pushes: bool,
    allow_deletions: bool,
    block_creations: bool,
    required_conversation_resolution: bool,
    required_signatures: bool,
    lock_branch: bool,
    allow_fork_syncing: bool,
}

impl From<&ProtectionRow> for Draft {
    fn from(r: &ProtectionRow) -> Self {
        Self {
            required_status_checks: r.required_status_checks.clone(),
            required_pull_request_reviews: r.required_pull_request_reviews.clone(),
            restrictions: r.restrictions.clone(),
            enforce_admins: r.enforce_admins,
            required_linear_history: r.required_linear_history,
            allow_force_pushes: r.allow_force_pushes,
            allow_deletions: r.allow_deletions,
            block_creations: r.block_creations,
            required_conversation_resolution: r.required_conversation_resolution,
            required_signatures: r.required_signatures,
            lock_branch: r.lock_branch,
            allow_fork_syncing: r.allow_fork_syncing,
        }
    }
}

/// Compact client shape for sync model `branch_protection`.
fn sync_json(r: &ProtectionRow) -> Value {
    json!({
        "id": r.id,
        "repo_id": r.repo_id,
        "pattern": r.pattern,
        "required_status_checks": r.required_status_checks,
        "required_pull_request_reviews": r.required_pull_request_reviews,
        "restrictions": r.restrictions,
        "enforce_admins": r.enforce_admins,
        "required_linear_history": r.required_linear_history,
        "allow_force_pushes": r.allow_force_pushes,
        "allow_deletions": r.allow_deletions,
        "block_creations": r.block_creations,
        "required_conversation_resolution": r.required_conversation_resolution,
        "required_signatures": r.required_signatures,
        "lock_branch": r.lock_branch,
        "allow_fork_syncing": r.allow_fork_syncing,
        "updated_at": Timestamp::from(r.updated_at),
    })
}

#[derive(Clone, Copy)]
enum Flag {
    EnforceAdmins,
    Signatures,
}

impl Flag {
    fn path(self) -> &'static str {
        match self {
            Self::EnforceAdmins => "enforce_admins",
            Self::Signatures => "required_signatures",
        }
    }

    fn get(self, r: &ProtectionRow) -> bool {
        match self {
            Self::EnforceAdmins => r.enforce_admins,
            Self::Signatures => r.required_signatures,
        }
    }

    fn set(self, d: &mut Draft, on: bool) {
        match self {
            Self::EnforceAdmins => d.enforce_admins = on,
            Self::Signatures => d.required_signatures = on,
        }
    }
}

/// Users and teams referenced by rules, loaded in one query each.
#[derive(Default)]
struct People {
    users: HashMap<i64, db::User>,
    teams: HashMap<i64, Team>,
}

impl People {
    async fn load(
        state: &AppState,
        access: &RepoAccess,
        values: &[Option<&Value>],
    ) -> ApiResult<Self> {
        let user_ids: Vec<i64> = values.iter().flat_map(|v| ids(*v, "users")).collect();
        let mut team_ids: Vec<i64> = values.iter().flat_map(|v| ids(*v, "teams")).collect();
        let users = bgh_core::views::users_by_id(state, user_ids.into_iter().map(Some)).await?;
        team_ids.sort_unstable();
        team_ids.dedup();
        let mut teams = HashMap::new();
        if !team_ids.is_empty() {
            let rows: Vec<db::Team> = sqlx::query_as(&format!(
                "SELECT {} FROM teams WHERE id = ANY($1) AND org_id = $2",
                db::Team::COLUMNS
            ))
            .bind(&team_ids)
            .bind(access.owner.id)
            .fetch_all(&state.db)
            .await?;
            let parent_ids: Vec<i64> = rows.iter().filter_map(|t| t.parent_id).collect();
            let parents: HashMap<i64, db::Team> = if parent_ids.is_empty() {
                HashMap::new()
            } else {
                sqlx::query_as::<_, db::Team>(&format!(
                    "SELECT {} FROM teams WHERE id = ANY($1)",
                    db::Team::COLUMNS
                ))
                .bind(&parent_ids)
                .fetch_all(&state.db)
                .await?
                .into_iter()
                .map(|t| (t.id, t))
                .collect()
            };
            let org = &access.owner.login;
            for t in rows {
                let parent = t
                    .parent_id
                    .and_then(|p| parents.get(&p))
                    .map(|p| TeamSimple::new(&state.urls, org, p));
                teams.insert(
                    t.id,
                    Team {
                        team: TeamSimple::new(&state.urls, org, &t),
                        parent,
                    },
                );
            }
        }
        Ok(Self { users, teams })
    }

    fn users(&self, state: &AppState, v: Option<&Value>) -> Vec<SimpleUser> {
        ids(v, "users")
            .iter()
            .filter_map(|id| self.users.get(id))
            .map(|u| SimpleUser::new(&state.urls, u))
            .collect()
    }

    fn teams(&self, v: Option<&Value>) -> Vec<Team> {
        ids(v, "teams")
            .iter()
            .filter_map(|id| self.teams.get(id).cloned())
            .collect()
    }
}

// ----- handlers ------------------------------------------------------------------

struct Cx<'a> {
    state: &'a AppState,
    access: &'a RepoAccess,
    user: &'a db::User,
    branch: &'a str,
}

impl Cx<'_> {
    fn url(&self, suffix: &str) -> String {
        format!(
            "{}/branches/{}/protection{suffix}",
            self.state
                .urls
                .repo(&self.access.owner.login, &self.access.repo.name),
            encode_path(self.branch)
        )
    }

    async fn row(&self) -> ApiResult<ProtectionRow> {
        sqlx::query_as(&format!(
            "SELECT {} FROM branch_protections WHERE repo_id = $1 AND pattern = $2",
            ProtectionRow::COLUMNS
        ))
        .bind(self.access.repo.id)
        .bind(self.branch)
        .fetch_optional(&self.state.db)
        .await?
        .ok_or_else(not_protected)
    }

    async fn lock_row(&self, tx: &mut Tx) -> ApiResult<Option<ProtectionRow>> {
        Ok(sqlx::query_as(&format!(
            "SELECT {} FROM branch_protections WHERE repo_id = $1 AND pattern = $2 FOR UPDATE",
            ProtectionRow::COLUMNS
        ))
        .bind(self.access.repo.id)
        .bind(self.branch)
        .fetch_optional(&mut **tx)
        .await?)
    }

    fn audit_target(&self) -> audit::Target {
        audit::Target::Repo {
            id: self.access.repo.id,
            org_id: self.access.owner.is_org().then_some(self.access.owner.id),
        }
    }

    /// Upsert the rule within `tx`, with sync, audit and event.
    async fn persist(&self, tx: &mut Tx, existed: bool, d: &Draft) -> ApiResult<ProtectionRow> {
        let row: ProtectionRow = sqlx::query_as(&format!(
            "INSERT INTO branch_protections (repo_id, pattern, required_status_checks,
                 required_pull_request_reviews, restrictions, enforce_admins,
                 required_linear_history, allow_force_pushes, allow_deletions, block_creations,
                 required_conversation_resolution, required_signatures, lock_branch,
                 allow_fork_syncing)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
             ON CONFLICT (repo_id, pattern) DO UPDATE SET
                 required_status_checks = EXCLUDED.required_status_checks,
                 required_pull_request_reviews = EXCLUDED.required_pull_request_reviews,
                 restrictions = EXCLUDED.restrictions,
                 enforce_admins = EXCLUDED.enforce_admins,
                 required_linear_history = EXCLUDED.required_linear_history,
                 allow_force_pushes = EXCLUDED.allow_force_pushes,
                 allow_deletions = EXCLUDED.allow_deletions,
                 block_creations = EXCLUDED.block_creations,
                 required_conversation_resolution = EXCLUDED.required_conversation_resolution,
                 required_signatures = EXCLUDED.required_signatures,
                 lock_branch = EXCLUDED.lock_branch,
                 allow_fork_syncing = EXCLUDED.allow_fork_syncing,
                 updated_at = now()
             RETURNING {}",
            ProtectionRow::COLUMNS
        ))
        .bind(self.access.repo.id)
        .bind(self.branch)
        .bind(&d.required_status_checks)
        .bind(&d.required_pull_request_reviews)
        .bind(&d.restrictions)
        .bind(d.enforce_admins)
        .bind(d.required_linear_history)
        .bind(d.allow_force_pushes)
        .bind(d.allow_deletions)
        .bind(d.block_creations)
        .bind(d.required_conversation_resolution)
        .bind(d.required_signatures)
        .bind(d.lock_branch)
        .bind(d.allow_fork_syncing)
        .fetch_one(&mut **tx)
        .await?;
        let (action, verb) = if existed {
            (SyncAction::Update, "update")
        } else {
            (SyncAction::Insert, "create")
        };
        tx.sync(
            &self.access.scope(),
            "branch_protection",
            row.id,
            action,
            &sync_json(&row),
        )
        .await?;
        audit::log(
            &mut **tx,
            Some(self.user),
            &format!("protected_branch.{verb}"),
            self.audit_target(),
            json!({"branch": self.branch, "protection_id": row.id}),
        )
        .await?;
        tx.emit(Event::RepositoryUpdated {
            repo_id: self.access.repo.id,
            actor_id: self.user.id,
        });
        Ok(row)
    }

    /// Read-modify-write of an existing rule (404 "Branch not protected").
    async fn modify(
        &self,
        f: impl FnOnce(&mut Draft) -> ApiResult<()>,
    ) -> ApiResult<ProtectionRow> {
        let mut tx = Tx::begin(self.state).await?;
        let row = self.lock_row(&mut tx).await?.ok_or_else(not_protected)?;
        let mut d = Draft::from(&row);
        f(&mut d)?;
        let row = self.persist(&mut tx, true, &d).await?;
        tx.commit().await?;
        Ok(row)
    }

    // --- protection --------------------------------------------------------------

    async fn render(&self, row: &ProtectionRow) -> ApiResult<Value> {
        let rpr = row.required_pull_request_reviews.as_ref();
        let people = People::load(
            self.state,
            self.access,
            &[
                row.restrictions.as_ref(),
                rpr.map(|v| &v["dismissal_restrictions"]),
                rpr.map(|v| &v["bypass_pull_request_allowances"]),
            ],
        )
        .await?;
        let mut v = json!({
            "url": self.url(""),
            "required_signatures": self.flag_json(Flag::Signatures, row),
            "enforce_admins": self.flag_json(Flag::EnforceAdmins, row),
            "required_linear_history": {"enabled": row.required_linear_history},
            "allow_force_pushes": {"enabled": row.allow_force_pushes},
            "allow_deletions": {"enabled": row.allow_deletions},
            "block_creations": {"enabled": row.block_creations},
            "required_conversation_resolution": {"enabled": row.required_conversation_resolution},
            "lock_branch": {"enabled": row.lock_branch},
            "allow_fork_syncing": {"enabled": row.allow_fork_syncing},
        });
        if row.required_status_checks.is_some() {
            v["required_status_checks"] = self.status_checks_json(row);
        }
        if let Some(rpr) = rpr {
            v["required_pull_request_reviews"] = self.reviews_json(rpr, &people)?;
        }
        if let Some(r) = &row.restrictions {
            v["restrictions"] = self.restrictions_json(r, &people)?;
        }
        Ok(v)
    }

    async fn get_protection(&self) -> ApiResult<Response> {
        let row = self.row().await?;
        ok_json(self.render(&row).await?)
    }

    async fn put_protection(&self, body: &[u8]) -> ApiResult<Response> {
        let raw: Value = parse_json(body)?;
        let Some(obj) = raw.as_object() else {
            return Err(ApiError::unprocessable(
                "Invalid request.\n\nThe request body must be a JSON object.",
            ));
        };
        let missing: Vec<String> = [
            "required_status_checks",
            "enforce_admins",
            "required_pull_request_reviews",
            "restrictions",
        ]
        .iter()
        .filter(|k| !obj.contains_key(**k))
        .map(|k| format!("{k:?}"))
        .collect();
        if !missing.is_empty() {
            let verb = if missing.len() == 1 {
                "wasn't"
            } else {
                "weren't"
            };
            return Err(ApiError::unprocessable(format!(
                "Invalid request.\n\n{} {verb} supplied.",
                missing.join(", ")
            )));
        }
        let b: PutBody = serde_json::from_value(raw.clone())
            .map_err(|e| ApiError::unprocessable(format!("Invalid request.\n\n{e}")))?;

        let status = match b.required_status_checks.flatten() {
            None => None,
            Some(sc) => {
                let strict = sc.strict.ok_or_else(|| {
                    ApiError::invalid_field(FieldError::missing_field(
                        "ProtectedBranch",
                        "required_status_checks.strict",
                    ))
                })?;
                let checks = match (sc.checks, sc.contexts) {
                    (Some(checks), _) => {
                        checks.into_iter().map(|c| (c.context, c.app_id)).collect()
                    }
                    (None, Some(ctx)) => ctx.into_iter().map(|c| (c, None)).collect(),
                    (None, None) => {
                        return Err(ApiError::invalid_field(FieldError::missing_field(
                            "ProtectedBranch",
                            "required_status_checks.contexts",
                        )));
                    }
                };
                Some(status_checks_value(strict, checks))
            }
        };
        let reviews = b.required_pull_request_reviews.flatten();
        let restrictions = b.restrictions.flatten();
        if let Some(r) = &restrictions {
            org_only(self.access)?;
            if r.users.is_none() || r.teams.is_none() {
                return Err(ApiError::unprocessable(
                    "Invalid request.\n\n\"users\", \"teams\" weren't supplied.",
                ));
            }
        }
        let (mut logins, mut slugs) = (Vec::new(), Vec::new());
        people_names(restrictions.as_ref(), &mut logins, &mut slugs);
        if let Some(r) = &reviews {
            people_names(
                r.dismissal_restrictions.as_ref().and_then(Option::as_ref),
                &mut logins,
                &mut slugs,
            );
            people_names(
                r.bypass_pull_request_allowances
                    .as_ref()
                    .and_then(Option::as_ref),
                &mut logins,
                &mut slugs,
            );
        }
        let resolver = Resolver::load(self.state, self.access, logins, slugs).await?;
        let d = Draft {
            required_status_checks: status,
            required_pull_request_reviews: reviews
                .as_ref()
                .map(|r| reviews_value(None, r, &resolver))
                .transpose()?,
            restrictions: restrictions
                .as_ref()
                .map(|r| people_value(r, &resolver, true))
                .transpose()?,
            enforce_admins: b.enforce_admins.flatten().unwrap_or(false),
            required_linear_history: b.required_linear_history.unwrap_or(false),
            allow_force_pushes: b.allow_force_pushes.unwrap_or(false),
            allow_deletions: b.allow_deletions.unwrap_or(false),
            block_creations: b.block_creations.unwrap_or(false),
            required_conversation_resolution: b.required_conversation_resolution.unwrap_or(false),
            required_signatures: false,
            lock_branch: b.lock_branch.unwrap_or(false),
            allow_fork_syncing: b.allow_fork_syncing.unwrap_or(false),
        };

        let mut tx = Tx::begin(self.state).await?;
        let existing = self.lock_row(&mut tx).await?;
        // `required_signatures` is managed by its own endpoint.
        let d = Draft {
            required_signatures: existing.as_ref().is_some_and(|r| r.required_signatures),
            ..d
        };
        let row = self.persist(&mut tx, existing.is_some(), &d).await?;
        tx.commit().await?;
        ok_json(self.render(&row).await?)
    }

    async fn delete_protection(&self) -> ApiResult<Response> {
        let mut tx = Tx::begin(self.state).await?;
        let row = self.lock_row(&mut tx).await?.ok_or_else(not_protected)?;
        sqlx::query("DELETE FROM branch_protections WHERE id = $1")
            .bind(row.id)
            .execute(&mut *tx)
            .await?;
        tx.sync(
            &self.access.scope(),
            "branch_protection",
            row.id,
            SyncAction::Delete,
            &json!({"id": row.id}),
        )
        .await?;
        audit::log(
            &mut *tx,
            Some(self.user),
            "protected_branch.destroy",
            self.audit_target(),
            json!({"branch": self.branch, "protection_id": row.id}),
        )
        .await?;
        tx.emit(Event::RepositoryUpdated {
            repo_id: self.access.repo.id,
            actor_id: self.user.id,
        });
        tx.commit().await?;
        no_content()
    }

    // --- required status checks --------------------------------------------------

    fn status_checks_json(&self, row: &ProtectionRow) -> Value {
        let v = row.required_status_checks.as_ref();
        let checks = checks_of(v);
        json!({
            "url": self.url("/required_status_checks"),
            "strict": v.and_then(|v| v["strict"].as_bool()).unwrap_or(false),
            "contexts": checks.iter().map(|c| &c.0).collect::<Vec<_>>(),
            "contexts_url": self.url("/required_status_checks/contexts"),
            "checks": checks_json(&checks),
            "enforcement_level": if row.enforce_admins { "everyone" } else { "non_admins" },
        })
    }

    fn require_status_checks(row: &ProtectionRow) -> ApiResult<&Value> {
        row.required_status_checks
            .as_ref()
            .ok_or_else(|| not_found("Required status checks not enabled"))
    }

    async fn get_status_checks(&self) -> ApiResult<Response> {
        let row = self.row().await?;
        Self::require_status_checks(&row)?;
        ok_json(self.status_checks_json(&row))
    }

    async fn patch_status_checks(&self, body: &[u8]) -> ApiResult<Response> {
        let input: StatusChecksInput = parse_json(body)?;
        let row = self
            .modify(|d| {
                let cur = d
                    .required_status_checks
                    .as_ref()
                    .ok_or_else(|| not_found("Required status checks not enabled"))?;
                let existing = checks_of(Some(cur));
                let strict = input
                    .strict
                    .unwrap_or_else(|| cur["strict"].as_bool().unwrap_or(false));
                let checks = match (input.checks, input.contexts) {
                    (Some(checks), _) => {
                        checks.into_iter().map(|c| (c.context, c.app_id)).collect()
                    }
                    (None, Some(ctx)) => checks_from_contexts(ctx, &existing),
                    (None, None) => existing,
                };
                d.required_status_checks = Some(status_checks_value(strict, checks));
                Ok(())
            })
            .await?;
        ok_json(self.status_checks_json(&row))
    }

    async fn delete_status_checks(&self) -> ApiResult<Response> {
        self.modify(|d| {
            d.required_status_checks
                .take()
                .ok_or_else(|| not_found("Required status checks not enabled"))?;
            Ok(())
        })
        .await?;
        no_content()
    }

    async fn get_contexts(&self) -> ApiResult<Response> {
        let row = self.row().await?;
        let v = Self::require_status_checks(&row)?;
        let checks = checks_of(Some(v));
        ok_json(json!(checks.iter().map(|c| &c.0).collect::<Vec<_>>()))
    }

    async fn write_contexts(&self, method: &Method, body: &[u8]) -> ApiResult<Response> {
        let given = string_list(body, "contexts")?;
        let row = self
            .modify(|d| {
                let cur = d
                    .required_status_checks
                    .as_ref()
                    .ok_or_else(|| not_found("Required status checks not enabled"))?;
                let existing = checks_of(Some(cur));
                let strict = cur["strict"].as_bool().unwrap_or(false);
                let mut names: Vec<String> = existing.iter().map(|c| c.0.clone()).collect();
                apply_list(method, &mut names, given);
                d.required_status_checks = Some(status_checks_value(
                    strict,
                    checks_from_contexts(names, &existing),
                ));
                Ok(())
            })
            .await?;
        let checks = checks_of(row.required_status_checks.as_ref());
        ok_json(json!(checks.iter().map(|c| &c.0).collect::<Vec<_>>()))
    }

    // --- enforce_admins / required_signatures -----------------------------------

    fn flag_json(&self, flag: Flag, row: &ProtectionRow) -> Value {
        json!({"url": self.url(&format!("/{}", flag.path())), "enabled": flag.get(row)})
    }

    async fn get_flag(&self, flag: Flag) -> ApiResult<Response> {
        let row = self.row().await?;
        ok_json(self.flag_json(flag, &row))
    }

    async fn set_flag(&self, flag: Flag, on: bool) -> ApiResult<Response> {
        let row = self
            .modify(|d| {
                flag.set(d, on);
                Ok(())
            })
            .await?;
        if on {
            ok_json(self.flag_json(flag, &row))
        } else {
            no_content()
        }
    }

    // --- required pull request reviews --------------------------------------------

    fn reviews_json(&self, v: &Value, people: &People) -> ApiResult<Value> {
        let mut out = json!({
            "url": self.url("/required_pull_request_reviews"),
            "dismiss_stale_reviews": v["dismiss_stale_reviews"].as_bool().unwrap_or(false),
            "require_code_owner_reviews": v["require_code_owner_reviews"].as_bool().unwrap_or(false),
            "required_approving_review_count":
                v["required_approving_review_count"].as_i64().unwrap_or(0),
            "require_last_push_approval": v["require_last_push_approval"].as_bool().unwrap_or(false),
        });
        if let Some(dr) = v.get("dismissal_restrictions").filter(|x| x.is_object()) {
            out["dismissal_restrictions"] = json!({
                "url": self.url("/dismissal_restrictions"),
                "users_url": self.url("/dismissal_restrictions/users"),
                "teams_url": self.url("/dismissal_restrictions/teams"),
                "users": people.users(self.state, Some(dr)),
                "teams": people.teams(Some(dr)),
                "apps": [],
            });
        }
        if let Some(bp) = v
            .get("bypass_pull_request_allowances")
            .filter(|x| x.is_object())
        {
            out["bypass_pull_request_allowances"] = json!({
                "users": people.users(self.state, Some(bp)),
                "teams": people.teams(Some(bp)),
                "apps": [],
            });
        }
        Ok(out)
    }

    async fn render_reviews(&self, row: &ProtectionRow) -> ApiResult<Value> {
        let v = row
            .required_pull_request_reviews
            .as_ref()
            .ok_or_else(|| not_found("Required pull request reviews not enabled"))?;
        let people = People::load(
            self.state,
            self.access,
            &[
                Some(&v["dismissal_restrictions"]),
                Some(&v["bypass_pull_request_allowances"]),
            ],
        )
        .await?;
        self.reviews_json(v, &people)
    }

    async fn get_reviews(&self) -> ApiResult<Response> {
        let row = self.row().await?;
        ok_json(self.render_reviews(&row).await?)
    }

    async fn patch_reviews(&self, body: &[u8]) -> ApiResult<Response> {
        let input: ReviewsInput = parse_json(body)?;
        let (mut logins, mut slugs) = (Vec::new(), Vec::new());
        for p in [
            &input.dismissal_restrictions,
            &input.bypass_pull_request_allowances,
        ] {
            people_names(p.as_ref().and_then(Option::as_ref), &mut logins, &mut slugs);
        }
        let resolver = Resolver::load(self.state, self.access, logins, slugs).await?;
        let row = self
            .modify(|d| {
                let cur = d
                    .required_pull_request_reviews
                    .as_ref()
                    .ok_or_else(|| not_found("Required pull request reviews not enabled"))?;
                d.required_pull_request_reviews =
                    Some(reviews_value(Some(cur), &input, &resolver)?);
                Ok(())
            })
            .await?;
        ok_json(self.render_reviews(&row).await?)
    }

    async fn delete_reviews(&self) -> ApiResult<Response> {
        self.modify(|d| {
            d.required_pull_request_reviews
                .take()
                .ok_or_else(|| not_found("Required pull request reviews not enabled"))?;
            Ok(())
        })
        .await?;
        no_content()
    }

    // --- restrictions --------------------------------------------------------------

    fn restrictions_json(&self, v: &Value, people: &People) -> ApiResult<Value> {
        Ok(json!({
            "url": self.url("/restrictions"),
            "users_url": self.url("/restrictions/users"),
            "teams_url": self.url("/restrictions/teams"),
            "apps_url": self.url("/restrictions/apps"),
            "users": people.users(self.state, Some(v)),
            "teams": people.teams(Some(v)),
            "apps": [],
        }))
    }

    fn require_restrictions(row: &ProtectionRow) -> ApiResult<&Value> {
        row.restrictions
            .as_ref()
            .ok_or_else(|| not_found("Push restrictions not enabled"))
    }

    async fn get_restrictions(&self) -> ApiResult<Response> {
        let row = self.row().await?;
        let v = Self::require_restrictions(&row)?;
        let people = People::load(self.state, self.access, &[Some(v)]).await?;
        ok_json(self.restrictions_json(v, &people)?)
    }

    async fn delete_restrictions(&self) -> ApiResult<Response> {
        self.modify(|d| {
            d.restrictions
                .take()
                .ok_or_else(|| not_found("Push restrictions not enabled"))?;
            Ok(())
        })
        .await?;
        no_content()
    }

    async fn restriction_list_json(&self, kind: &str, v: &Value) -> ApiResult<Value> {
        let people = People::load(self.state, self.access, &[Some(v)]).await?;
        Ok(match kind {
            "users" => serde_json::to_value(people.users(self.state, Some(v)))?,
            "teams" => serde_json::to_value(people.teams(Some(v)))?,
            _ => json!([]),
        })
    }

    async fn get_restriction_list(&self, kind: &str) -> ApiResult<Response> {
        let row = self.row().await?;
        let v = Self::require_restrictions(&row)?;
        ok_json(self.restriction_list_json(kind, v).await?)
    }

    async fn write_restriction_list(
        &self,
        method: &Method,
        kind: &str,
        body: &[u8],
    ) -> ApiResult<Response> {
        let given = string_list(body, kind)?;
        let resolver = match kind {
            "users" => Resolver::load(self.state, self.access, given.clone(), vec![]).await?,
            "teams" => Resolver::load(self.state, self.access, vec![], given.clone()).await?,
            _ => Resolver::default(),
        };
        let given_ids = match kind {
            "users" => resolver.user_ids(&given)?,
            "teams" => resolver.team_ids(&given)?,
            _ => {
                check_apps(Some(&given))?;
                vec![]
            }
        };
        let row = self
            .modify(|d| {
                let cur = d
                    .restrictions
                    .as_mut()
                    .ok_or_else(|| not_found("Push restrictions not enabled"))?;
                if kind != "apps" {
                    let mut list = ids(Some(cur), kind);
                    apply_list(method, &mut list, given_ids);
                    cur[kind] = json!(list);
                }
                Ok(())
            })
            .await?;
        let v = Self::require_restrictions(&row)?;
        ok_json(self.restriction_list_json(kind, v).await?)
    }
}

/// POST adds, PUT replaces, DELETE removes (order kept, no duplicates).
fn apply_list<T: PartialEq>(method: &Method, list: &mut Vec<T>, given: Vec<T>) {
    match *method {
        Method::PUT => {
            list.clear();
            for g in given {
                if !list.contains(&g) {
                    list.push(g);
                }
            }
        }
        Method::DELETE => list.retain(|x| !given.contains(x)),
        _ => {
            for g in given {
                if !list.contains(&g) {
                    list.push(g);
                }
            }
        }
    }
}

/// A body that is a JSON array of strings or `{"<key>": [...]}`.
fn string_list(body: &[u8], key: &str) -> ApiResult<Vec<String>> {
    let v: Value = parse_json(body)?;
    let arr = match &v {
        Value::Array(a) => Some(a),
        Value::Object(o) => o.get(key).and_then(Value::as_array),
        _ => None,
    };
    let invalid = || {
        ApiError::unprocessable(format!(
            "Invalid request.\n\nExpected an array of strings or {{\"{key}\": [...]}}."
        ))
    };
    arr.ok_or_else(invalid)?
        .iter()
        .map(|x| x.as_str().map(str::to_string).ok_or_else(invalid))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::split_protection_path as split;

    #[test]
    fn splits_paths() {
        assert_eq!(split("main/protection"), Some(("main".into(), vec![])));
        assert_eq!(
            split("feature/x/protection/required_status_checks/contexts"),
            Some((
                "feature/x".into(),
                vec!["required_status_checks".into(), "contexts".into()]
            ))
        );
        assert_eq!(
            split("a/protection/restrictions/users"),
            Some(("a".into(), vec!["restrictions".into(), "users".into()]))
        );
        assert_eq!(
            split("protection/protection"),
            Some(("protection".into(), vec![]))
        );
        assert_eq!(split("main"), None);
        assert_eq!(split("protection"), None);
        assert_eq!(split("main/protection/bogus"), None);
        assert_eq!(split("main/protection/restrictions/robots"), None);
    }
}
