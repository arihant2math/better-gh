//! Webhooks: repository and organization hooks (GitHub REST), delivery
//! log, dispatch from domain events and delivery through the job queue.
//!
//! * `/repos/{owner}/{repo}/hooks[/{id}[/config|/pings|/tests|/deliveries…]]`
//! * `/orgs/{org}/hooks[/{id}[/config|/pings|/deliveries…]]`

pub mod deliver;
pub mod deliveries;
pub mod dispatch;
pub mod ssrf;

use axum::extract::State;
use axum::http::StatusCode;
use bgh_core::audit;
use bgh_core::prelude::*;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// `webhooks` row.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct HookRow {
    pub id: i64,
    pub repo_id: Option<i64>,
    pub org_id: Option<i64>,
    pub name: String,
    pub url: String,
    pub content_type: String,
    pub secret: Option<String>,
    pub insecure_ssl: bool,
    pub events: Vec<String>,
    pub active: bool,
    pub last_response: Value,
    pub creator_id: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl HookRow {
    pub const COLUMNS: &'static str = "id, repo_id, org_id, name, url, content_type, secret, \
        insecure_ssl, events, active, last_response, creator_id, created_at, updated_at";

    pub fn wants(&self, event: &str) -> bool {
        self.events.iter().any(|e| e == "*" || e == event)
    }
}

/// Every webhook event name GitHub accepts in `events` (plus `*`).
pub const EVENTS: &[&str] = &[
    "*",
    "branch_protection_configuration",
    "branch_protection_rule",
    "check_run",
    "check_suite",
    "code_scanning_alert",
    "commit_comment",
    "create",
    "custom_property",
    "custom_property_values",
    "delete",
    "dependabot_alert",
    "deploy_key",
    "deployment",
    "deployment_protection_rule",
    "deployment_review",
    "deployment_status",
    "discussion",
    "discussion_comment",
    "fork",
    "github_app_authorization",
    "gollum",
    "installation",
    "installation_repositories",
    "installation_target",
    "issue_comment",
    "issue_dependencies",
    "issues",
    "label",
    "marketplace_purchase",
    "member",
    "membership",
    "merge_group",
    "meta",
    "milestone",
    "org_block",
    "organization",
    "package",
    "page_build",
    "personal_access_token_request",
    "ping",
    "project",
    "project_card",
    "project_column",
    "projects_v2",
    "projects_v2_item",
    "public",
    "pull_request",
    "pull_request_review",
    "pull_request_review_comment",
    "pull_request_review_thread",
    "push",
    "registry_package",
    "release",
    "repository",
    "repository_advisory",
    "repository_dispatch",
    "repository_import",
    "repository_ruleset",
    "repository_vulnerability_alert",
    "secret_scanning_alert",
    "secret_scanning_alert_location",
    "security_advisory",
    "security_and_analysis",
    "sponsorship",
    "star",
    "status",
    "sub_issues",
    "team",
    "team_add",
    "watch",
    "workflow_dispatch",
    "workflow_job",
    "workflow_run",
];

// ---------------------------------------------------------------------------
// Owners: repository or organization
// ---------------------------------------------------------------------------

/// Where a hook lives.
#[derive(Debug, Clone)]
pub enum Owner {
    Repo(Box<RepoAccess>),
    Org(Box<db::User>),
}

impl Owner {
    pub fn repo_id(&self) -> Option<i64> {
        match self {
            Self::Repo(a) => Some(a.repo.id),
            Self::Org(_) => None,
        }
    }

    pub fn org_id(&self) -> Option<i64> {
        match self {
            Self::Repo(_) => None,
            Self::Org(o) => Some(o.id),
        }
    }

    /// `{api}/repos/{o}/{r}/hooks` or `{api}/orgs/{org}/hooks`
    pub fn hooks_url(&self, state: &AppState) -> String {
        match self {
            Self::Repo(a) => format!("{}/hooks", state.urls.repo(&a.owner.login, &a.repo.name)),
            Self::Org(o) => format!("{}/hooks", state.urls.org(&o.login)),
        }
    }

    fn audit_target(&self) -> audit::Target {
        match self {
            Self::Repo(a) => audit::Target::Repo {
                id: a.repo.id,
                org_id: a.owner.is_org().then_some(a.owner.id),
            },
            Self::Org(o) => audit::Target::Org(o.id),
        }
    }
}

/// Level of hook access a request needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Need {
    Read,
    Write,
    Admin,
}

/// Repository hooks: repo admins; tokens need `repo` or `{read,write,admin}:repo_hook`.
pub async fn repo_owner(
    state: &AppState,
    auth: &AuthContext,
    owner: &str,
    repo: &str,
    need: Need,
) -> ApiResult<Owner> {
    let access = RepoAccess::load(state, Some(auth), owner, repo).await?;
    access.require(Permission::Admin)?;
    let scope = match need {
        Need::Read => "read:repo_hook",
        Need::Write => "write:repo_hook",
        Need::Admin => "admin:repo_hook",
    };
    if !auth.has_scope("repo") && !auth.has_scope(scope) {
        auth.require_scope(scope)?;
    }
    Ok(Owner::Repo(Box::new(access)))
}

/// Organization hooks: org admins with the `admin:org_hook` scope.
pub async fn org_owner(state: &AppState, auth: &AuthContext, org: &str) -> ApiResult<Owner> {
    let org = db::User::find_by_login(&state.db, org)
        .await?
        .filter(|u| u.is_org())
        .ok_or(ApiError::NotFound)?;
    let role = bgh_core::perms::org_role(&state.db, org.id, auth.id()).await?;
    if role.as_deref() != Some("admin") && !auth.user.site_admin {
        return Err(if role.is_some() {
            ApiError::forbidden("Must be an organization owner.")
        } else {
            ApiError::NotFound
        });
    }
    auth.require_scope("admin:org_hook")?;
    Ok(Owner::Org(Box::new(org)))
}

pub async fn load_hook(state: &AppState, owner: &Owner, id: i64) -> ApiResult<HookRow> {
    sqlx::query_as(&format!(
        "SELECT {} FROM webhooks WHERE id = $1
            AND repo_id IS NOT DISTINCT FROM $2 AND org_id IS NOT DISTINCT FROM $3",
        HookRow::COLUMNS
    ))
    .bind(id)
    .bind(owner.repo_id())
    .bind(owner.org_id())
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)
}

// ---------------------------------------------------------------------------
// JSON
// ---------------------------------------------------------------------------

/// `webhook-config`.
#[derive(Debug, Clone, Serialize)]
pub struct HookConfig {
    pub content_type: String,
    pub insecure_ssl: String,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
}

pub fn config_json(h: &HookRow) -> HookConfig {
    HookConfig {
        content_type: h.content_type.clone(),
        insecure_ssl: if h.insecure_ssl { "1" } else { "0" }.into(),
        url: h.url.clone(),
        secret: h.secret.as_ref().map(|_| "********".into()),
    }
}

/// GitHub `hook` / `org-hook`.
#[derive(Debug, Clone, Serialize)]
pub struct Hook {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub id: i64,
    pub name: String,
    pub active: bool,
    pub events: Vec<String>,
    pub config: HookConfig,
    pub updated_at: Timestamp,
    pub created_at: Timestamp,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test_url: Option<String>,
    pub ping_url: String,
    pub deliveries_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_response: Option<Value>,
}

pub fn hook_json(state: &AppState, owner: &Owner, h: &HookRow) -> Hook {
    let url = format!("{}/{}", owner.hooks_url(state), h.id);
    let is_repo = matches!(owner, Owner::Repo(_));
    Hook {
        kind: if is_repo {
            "Repository"
        } else {
            "Organization"
        },
        id: h.id,
        name: h.name.clone(),
        active: h.active,
        events: h.events.clone(),
        config: config_json(h),
        updated_at: h.updated_at.into(),
        created_at: h.created_at.into(),
        test_url: is_repo.then(|| format!("{url}/test")),
        ping_url: format!("{url}/pings"),
        deliveries_url: format!("{url}/deliveries"),
        last_response: is_repo.then(|| h.last_response.clone()),
        url,
    }
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct ConfigBody {
    pub url: Option<String>,
    pub content_type: Option<String>,
    pub secret: Option<String>,
    pub insecure_ssl: Option<Value>,
}

fn invalid(msg: impl Into<String>) -> ApiError {
    ApiError::validation(vec![FieldError {
        resource: "Hook".into(),
        field: String::new(),
        code: "custom".into(),
        message: Some(msg.into()),
    }])
}

fn parse_insecure(v: &Value) -> ApiResult<bool> {
    match v {
        Value::String(s) if s == "0" => Ok(false),
        Value::String(s) if s == "1" => Ok(true),
        Value::Number(n) if n.as_i64() == Some(0) => Ok(false),
        Value::Number(n) if n.as_i64() == Some(1) => Ok(true),
        Value::Bool(b) => Ok(*b),
        _ => Err(invalid("Config insecure_ssl must be \"0\" or \"1\"")),
    }
}

/// Apply a config body onto `h` (fields present override).
async fn apply_config(state: &AppState, h: &mut HookRow, c: ConfigBody) -> ApiResult<()> {
    if let Some(url) = c.url {
        let policy = ssrf::Policy::load(state).await;
        let parsed = ssrf::validate_url(&policy, &url).map_err(invalid)?;
        h.url = parsed.to_string();
    }
    if let Some(ct) = c.content_type {
        h.content_type = match ct.as_str() {
            "json" | "application/json" => "json".into(),
            "form" | "application/x-www-form-urlencoded" => "form".into(),
            _ => return Err(invalid("Config content_type must be json or form")),
        };
    }
    if let Some(secret) = c.secret {
        h.secret = (!secret.is_empty()).then_some(secret);
    }
    if let Some(v) = &c.insecure_ssl {
        h.insecure_ssl = parse_insecure(v)?;
    }
    Ok(())
}

fn validate_events(events: &[String]) -> ApiResult<Vec<String>> {
    let mut out: Vec<String> = Vec::with_capacity(events.len());
    for e in events {
        if !EVENTS.contains(&e.as_str()) {
            return Err(invalid(format!("Invalid event: {e:?}")));
        }
        if !out.contains(e) {
            out.push(e.clone());
        }
    }
    if out.is_empty() {
        return Err(invalid("Hook must subscribe to at least one event"));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// CRUD
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct CreateBody {
    pub name: Option<String>,
    pub config: Option<ConfigBody>,
    pub events: Option<Vec<String>>,
    pub active: Option<bool>,
}

async fn create(
    state: &AppState,
    auth: &AuthContext,
    owner: Owner,
    body: CreateBody,
) -> ApiResult<(StatusCode, Json<Hook>)> {
    if let Some(name) = &body.name
        && name != "web"
    {
        return Err(invalid(format!(
            "Name {name:?} is not a valid hook name; use \"web\""
        )));
    }
    let config = body
        .config
        .ok_or_else(|| ApiError::invalid_field(FieldError::missing_field("Hook", "config")))?;
    if config.url.as_deref().is_none_or(|u| u.trim().is_empty()) {
        return Err(invalid("Config must contain url"));
    }
    let now = Utc::now();
    let mut h = HookRow {
        id: 0,
        repo_id: owner.repo_id(),
        org_id: owner.org_id(),
        name: "web".into(),
        url: String::new(),
        content_type: "form".into(),
        secret: None,
        insecure_ssl: false,
        events: validate_events(&body.events.unwrap_or_else(|| vec!["push".into()]))?,
        active: body.active.unwrap_or(true),
        last_response: json!({"code": null, "status": "unused", "message": null}),
        creator_id: Some(auth.id()),
        created_at: now,
        updated_at: now,
    };
    apply_config(state, &mut h, config).await?;

    let mut tx = Tx::begin(state).await?;
    let dup: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM webhooks WHERE url = $1
            AND repo_id IS NOT DISTINCT FROM $2 AND org_id IS NOT DISTINCT FROM $3)",
    )
    .bind(&h.url)
    .bind(h.repo_id)
    .bind(h.org_id)
    .fetch_one(&mut *tx)
    .await?;
    if dup {
        return Err(invalid(match owner {
            Owner::Repo(_) => "Hook already exists on this repository",
            Owner::Org(_) => "Hook already exists on this organization",
        }));
    }
    let row: HookRow = sqlx::query_as(&format!(
        "INSERT INTO webhooks (repo_id, org_id, name, url, content_type, secret, insecure_ssl,
                               events, active, creator_id)
         VALUES ($1, $2, 'web', $3, $4, $5, $6, $7, $8, $9) RETURNING {}",
        HookRow::COLUMNS
    ))
    .bind(h.repo_id)
    .bind(h.org_id)
    .bind(&h.url)
    .bind(&h.content_type)
    .bind(&h.secret)
    .bind(h.insecure_ssl)
    .bind(&h.events)
    .bind(h.active)
    .bind(h.creator_id)
    .fetch_one(&mut *tx)
    .await?;
    audit::log(
        &mut *tx,
        Some(&auth.user),
        "hook.create",
        owner.audit_target(),
        json!({ "hook_id": row.id, "url": row.url, "events": row.events }),
    )
    .await?;
    // GitHub pings a new hook right away.
    if row.active {
        dispatch::queue_ping(state, &mut tx, &owner, &row, auth.id()).await?;
    }
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(hook_json(state, &owner, &row))))
}

async fn list(state: &AppState, owner: &Owner, p: &Pagination) -> ApiResult<Page<Hook>> {
    let rows: Vec<HookRow> = sqlx::query_as(&format!(
        "SELECT {} FROM webhooks
          WHERE repo_id IS NOT DISTINCT FROM $1 AND org_id IS NOT DISTINCT FROM $2
          ORDER BY id LIMIT $3 OFFSET $4",
        HookRow::COLUMNS
    ))
    .bind(owner.repo_id())
    .bind(owner.org_id())
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    Ok(p.page(rows).map(|h| hook_json(state, owner, &h)))
}

#[derive(Debug, Default, Deserialize)]
pub struct UpdateBody {
    pub config: Option<ConfigBody>,
    pub events: Option<Vec<String>>,
    pub add_events: Option<Vec<String>>,
    pub remove_events: Option<Vec<String>>,
    pub active: Option<bool>,
}

async fn save(
    state: &AppState,
    auth: &AuthContext,
    owner: &Owner,
    h: &HookRow,
    action: &str,
) -> ApiResult<HookRow> {
    let mut tx = Tx::begin(state).await?;
    let row: HookRow = sqlx::query_as(&format!(
        "UPDATE webhooks SET url = $2, content_type = $3, secret = $4, insecure_ssl = $5,
                events = $6, active = $7, updated_at = now()
          WHERE id = $1 RETURNING {}",
        HookRow::COLUMNS
    ))
    .bind(h.id)
    .bind(&h.url)
    .bind(&h.content_type)
    .bind(&h.secret)
    .bind(h.insecure_ssl)
    .bind(&h.events)
    .bind(h.active)
    .fetch_one(&mut *tx)
    .await?;
    audit::log(
        &mut *tx,
        Some(&auth.user),
        action,
        owner.audit_target(),
        json!({ "hook_id": row.id, "url": row.url, "events": row.events, "active": row.active }),
    )
    .await?;
    tx.commit().await?;
    Ok(row)
}

async fn update(
    state: &AppState,
    auth: &AuthContext,
    owner: Owner,
    id: i64,
    body: UpdateBody,
) -> ApiResult<Json<Hook>> {
    let mut h = load_hook(state, &owner, id).await?;
    if let Some(c) = body.config {
        apply_config(state, &mut h, c).await?;
    }
    if let Some(events) = &body.events {
        h.events = validate_events(events)?;
    }
    if let Some(add) = &body.add_events {
        let mut events = h.events.clone();
        events.extend(add.iter().cloned());
        h.events = validate_events(&events)?;
    }
    if let Some(remove) = &body.remove_events {
        let events: Vec<String> = h
            .events
            .iter()
            .filter(|e| !remove.contains(e))
            .cloned()
            .collect();
        h.events = validate_events(&events)?;
    }
    if let Some(active) = body.active {
        h.active = active;
    }
    let row = save(state, auth, &owner, &h, "hook.config_changed").await?;
    Ok(Json(hook_json(state, &owner, &row)))
}

async fn update_config(
    state: &AppState,
    auth: &AuthContext,
    owner: Owner,
    id: i64,
    body: ConfigBody,
) -> ApiResult<Json<HookConfig>> {
    let mut h = load_hook(state, &owner, id).await?;
    apply_config(state, &mut h, body).await?;
    let row = save(state, auth, &owner, &h, "hook.config_changed").await?;
    Ok(Json(config_json(&row)))
}

async fn delete(
    state: &AppState,
    auth: &AuthContext,
    owner: Owner,
    id: i64,
) -> ApiResult<StatusCode> {
    let h = load_hook(state, &owner, id).await?;
    let mut tx = Tx::begin(state).await?;
    sqlx::query("DELETE FROM webhooks WHERE id = $1")
        .bind(h.id)
        .execute(&mut *tx)
        .await?;
    audit::log(
        &mut *tx,
        Some(&auth.user),
        "hook.destroy",
        owner.audit_target(),
        json!({ "hook_id": h.id, "url": h.url }),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn ping(
    state: &AppState,
    auth: &AuthContext,
    owner: Owner,
    id: i64,
) -> ApiResult<StatusCode> {
    let h = load_hook(state, &owner, id).await?;
    let mut tx = Tx::begin(state).await?;
    dispatch::queue_ping(state, &mut tx, &owner, &h, auth.id()).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

// ----- repository hook handlers ------------------------------------------

type RepoPath = Path<(String, String)>;
type RepoHookPath = Path<(String, String, i64)>;

pub async fn repo_create(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((o, r)): RepoPath,
    Json(body): Json<CreateBody>,
) -> ApiResult<(StatusCode, Json<Hook>)> {
    let owner = repo_owner(&state, &auth, &o, &r, Need::Write).await?;
    create(&state, &auth, owner, body).await
}

pub async fn repo_list(
    State(state): State<AppState>,
    auth: RequireUser,
    p: Pagination,
    Path((o, r)): RepoPath,
) -> ApiResult<Page<Hook>> {
    let owner = repo_owner(&state, &auth, &o, &r, Need::Read).await?;
    list(&state, &owner, &p).await
}

pub async fn repo_get(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((o, r, id)): RepoHookPath,
) -> ApiResult<Json<Hook>> {
    let owner = repo_owner(&state, &auth, &o, &r, Need::Read).await?;
    let h = load_hook(&state, &owner, id).await?;
    Ok(Json(hook_json(&state, &owner, &h)))
}

pub async fn repo_update(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((o, r, id)): RepoHookPath,
    Json(body): Json<UpdateBody>,
) -> ApiResult<Json<Hook>> {
    let owner = repo_owner(&state, &auth, &o, &r, Need::Write).await?;
    update(&state, &auth, owner, id, body).await
}

pub async fn repo_delete(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((o, r, id)): RepoHookPath,
) -> ApiResult<StatusCode> {
    let owner = repo_owner(&state, &auth, &o, &r, Need::Admin).await?;
    delete(&state, &auth, owner, id).await
}

pub async fn repo_get_config(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((o, r, id)): RepoHookPath,
) -> ApiResult<Json<HookConfig>> {
    let owner = repo_owner(&state, &auth, &o, &r, Need::Read).await?;
    Ok(Json(config_json(&load_hook(&state, &owner, id).await?)))
}

pub async fn repo_update_config(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((o, r, id)): RepoHookPath,
    Json(body): Json<ConfigBody>,
) -> ApiResult<Json<HookConfig>> {
    let owner = repo_owner(&state, &auth, &o, &r, Need::Write).await?;
    update_config(&state, &auth, owner, id, body).await
}

pub async fn repo_ping(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((o, r, id)): RepoHookPath,
) -> ApiResult<StatusCode> {
    let owner = repo_owner(&state, &auth, &o, &r, Need::Read).await?;
    ping(&state, &auth, owner, id).await
}

/// `POST /repos/{owner}/{repo}/hooks/{id}/tests` — deliver a `push` for the
/// latest commit on the default branch if the hook subscribes to `push`.
pub async fn repo_test(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((o, r, id)): RepoHookPath,
) -> ApiResult<StatusCode> {
    let owner = repo_owner(&state, &auth, &o, &r, Need::Read).await?;
    let h = load_hook(&state, &owner, id).await?;
    if h.wants("push") {
        let repo_id = owner.repo_id().unwrap_or_default();
        let payload = crate::payloads::test_push(&state, repo_id, auth.id())
            .await
            .map_err(ApiError::internal)?;
        if let Some(payload) = payload {
            let mut tx = Tx::begin(&state).await?;
            dispatch::queue_delivery(&mut tx, &h, "push", None, Some(repo_id), &payload, false)
                .await?;
            tx.commit().await?;
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

// ----- organization hook handlers ----------------------------------------

type OrgPath = Path<String>;
type OrgHookPath = Path<(String, i64)>;

pub async fn org_create(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(org): OrgPath,
    Json(body): Json<CreateBody>,
) -> ApiResult<(StatusCode, Json<Hook>)> {
    let owner = org_owner(&state, &auth, &org).await?;
    create(&state, &auth, owner, body).await
}

pub async fn org_list(
    State(state): State<AppState>,
    auth: RequireUser,
    p: Pagination,
    Path(org): OrgPath,
) -> ApiResult<Page<Hook>> {
    let owner = org_owner(&state, &auth, &org).await?;
    list(&state, &owner, &p).await
}

pub async fn org_get(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((org, id)): OrgHookPath,
) -> ApiResult<Json<Hook>> {
    let owner = org_owner(&state, &auth, &org).await?;
    let h = load_hook(&state, &owner, id).await?;
    Ok(Json(hook_json(&state, &owner, &h)))
}

pub async fn org_update(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((org, id)): OrgHookPath,
    Json(body): Json<UpdateBody>,
) -> ApiResult<Json<Hook>> {
    let owner = org_owner(&state, &auth, &org).await?;
    update(&state, &auth, owner, id, body).await
}

pub async fn org_delete(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((org, id)): OrgHookPath,
) -> ApiResult<StatusCode> {
    let owner = org_owner(&state, &auth, &org).await?;
    delete(&state, &auth, owner, id).await
}

pub async fn org_get_config(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((org, id)): OrgHookPath,
) -> ApiResult<Json<HookConfig>> {
    let owner = org_owner(&state, &auth, &org).await?;
    Ok(Json(config_json(&load_hook(&state, &owner, id).await?)))
}

pub async fn org_update_config(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((org, id)): OrgHookPath,
    Json(body): Json<ConfigBody>,
) -> ApiResult<Json<HookConfig>> {
    let owner = org_owner(&state, &auth, &org).await?;
    update_config(&state, &auth, owner, id, body).await
}

pub async fn org_ping(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((org, id)): OrgHookPath,
) -> ApiResult<StatusCode> {
    let owner = org_owner(&state, &auth, &org).await?;
    ping(&state, &auth, owner, id).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_events_and_insecure_ssl() {
        assert_eq!(
            validate_events(&["push".into(), "push".into(), "issues".into()]).unwrap(),
            vec!["push".to_string(), "issues".to_string()]
        );
        assert!(validate_events(&["bogus".into()]).is_err());
        assert!(validate_events(&[]).is_err());
        assert!(parse_insecure(&json!("1")).unwrap());
        assert!(!parse_insecure(&json!(0)).unwrap());
        assert!(parse_insecure(&json!("yes")).is_err());
    }
}
