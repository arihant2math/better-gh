//! Runner groups: which repositories (organization groups) or which
//! organizations (site groups) may use a set of self-hosted runners, with
//! an optional workflow allowlist.
//!
//! * `/orgs/{org}/actions/runner-groups[...]`: GitHub REST shapes.
//! * `/_bgh/admin/actions/runner-groups[...]`: site groups for site admins,
//!   in the GHES enterprise shape (`selected_organizations_url`,
//!   `/organizations` sub-resource).
//!
//! Every scope has one default group (created on first use; the site one
//! has id 1). Job matching lives in [`crate::server::try_acquire`].

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::{get, put};
use bgh_core::pagination::Pagination;
use bgh_core::prelude::*;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{FromRow, PgConnection};

use super::{load_org, require_org_admin, wrapped};
use crate::json::runner_json;
use crate::models::RunnerRow;

/// REST routes (relative to `/api/v3`).
pub fn routes() -> Router<AppState> {
    const G: &str = "/orgs/{org}/actions/runner-groups";
    Router::new()
        .route(G, get(org::list).post(org::create))
        .route(
            &format!("{G}/{{group_id}}"),
            get(org::get).patch(org::update).delete(org::delete),
        )
        .route(
            &format!("{G}/{{group_id}}/repositories"),
            get(org::list_repos).put(org::set_repos),
        )
        .route(
            &format!("{G}/{{group_id}}/repositories/{{repository_id}}"),
            put(org::add_repo).delete(org::remove_repo),
        )
        .route(
            &format!("{G}/{{group_id}}/runners"),
            get(org::list_runners).put(org::set_runners),
        )
        .route(
            &format!("{G}/{{group_id}}/runners/{{runner_id}}"),
            put(org::add_runner).delete(org::remove_runner),
        )
}

/// Site routes (absolute paths, `/_bgh/admin/actions/runner-groups`).
pub fn site_routes() -> Router<AppState> {
    const G: &str = "/_bgh/admin/actions/runner-groups";
    Router::new()
        .route(G, get(site::list).post(site::create))
        .route(
            &format!("{G}/{{group_id}}"),
            get(site::get).patch(site::update).delete(site::delete),
        )
        .route(
            &format!("{G}/{{group_id}}/organizations"),
            get(site::list_orgs).put(site::set_orgs),
        )
        .route(
            &format!("{G}/{{group_id}}/organizations/{{org_id}}"),
            put(site::add_org).delete(site::remove_org),
        )
        .route(
            &format!("{G}/{{group_id}}/runners"),
            get(site::list_runners).put(site::set_runners),
        )
        .route(
            &format!("{G}/{{group_id}}/runners/{{runner_id}}"),
            put(site::add_runner).delete(site::remove_runner),
        )
}

// ---------------------------------------------------------------------------
// Model
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, FromRow)]
pub struct GroupRow {
    pub id: i64,
    pub org_id: Option<i64>,
    pub name: String,
    pub visibility: String,
    pub is_default: bool,
    pub allows_public_repositories: bool,
    pub restricted_to_workflows: bool,
    pub selected_workflows: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl GroupRow {
    pub const COLUMNS: &'static str = "id, org_id, name, visibility, is_default, \
        allows_public_repositories, restricted_to_workflows, selected_workflows, created_at, \
        updated_at";
}

/// Owner of a runner group.
#[derive(Clone)]
pub enum Scope {
    Org { id: i64, login: String },
    Site,
}

impl Scope {
    pub fn org_id(&self) -> Option<i64> {
        match self {
            Self::Org { id, .. } => Some(*id),
            Self::Site => None,
        }
    }

    fn base_url(&self, state: &AppState) -> String {
        match self {
            Self::Org { login, .. } => state
                .urls
                .api(&format!("/orgs/{login}/actions/runner-groups")),
            Self::Site => state.urls.html("/_bgh/admin/actions/runner-groups"),
        }
    }

    fn audit_target(&self) -> bgh_core::audit::Target {
        match self {
            Self::Org { id, .. } => bgh_core::audit::Target::Org(*id),
            Self::Site => bgh_core::audit::Target::Site,
        }
    }

    /// SQL condition on `actions_runners` (alias-free) for runners that
    /// belong to this scope and can be grouped.
    fn runner_filter(&self) -> &'static str {
        match self {
            Self::Org { .. } => "org_id = $1",
            Self::Site => {
                "repo_id IS NULL AND org_id IS NULL AND NOT builtin AND $1::bigint IS NULL"
            }
        }
    }
}

/// The id of the default group of `org_id` (NULL: site), created on demand.
pub async fn ensure_default_group(
    conn: &mut PgConnection,
    org_id: Option<i64>,
) -> sqlx::Result<i64> {
    sqlx::query(
        "INSERT INTO actions_runner_groups (org_id, name, is_default, allows_public_repositories)
         VALUES ($1, 'Default', true, true) ON CONFLICT DO NOTHING",
    )
    .bind(org_id)
    .execute(&mut *conn)
    .await?;
    sqlx::query_scalar(
        "SELECT id FROM actions_runner_groups WHERE org_id IS NOT DISTINCT FROM $1 AND is_default",
    )
    .bind(org_id)
    .fetch_one(&mut *conn)
    .await
}

/// Resolve the group a new runner of `org_id` (NULL: site) joins:
/// `requested` must be a group of that scope, else the default group.
pub async fn group_for_new_runner(
    conn: &mut PgConnection,
    org_id: Option<i64>,
    requested: Option<i64>,
) -> ApiResult<i64> {
    let default = ensure_default_group(conn, org_id).await?;
    match requested {
        None => Ok(default),
        Some(id) => {
            let ok: Option<i64> = sqlx::query_scalar(
                "SELECT id FROM actions_runner_groups WHERE id = $1 AND org_id IS NOT DISTINCT FROM $2",
            )
            .bind(id)
            .bind(org_id)
            .fetch_optional(&mut *conn)
            .await?;
            ok.ok_or_else(|| {
                ApiError::invalid_field(FieldError::invalid("Runner", "runner_group_id"))
            })
        }
    }
}

pub fn group_json(state: &AppState, scope: &Scope, g: &GroupRow) -> Value {
    let base = format!("{}/{}", scope.base_url(state), g.id);
    let mut v = json!({
        "id": g.id,
        "name": g.name,
        "visibility": g.visibility,
        "default": g.is_default,
        "runners_url": format!("{base}/runners"),
        "hosted_runners_url": format!("{base}/hosted-runners"),
        "network_configuration_id": null,
        "inherited": false,
        "allows_public_repositories": g.allows_public_repositories,
        "restricted_to_workflows": g.restricted_to_workflows,
        "selected_workflows": g.selected_workflows,
        "workflow_restrictions_read_only": false,
    });
    if g.visibility == "selected" {
        match scope {
            Scope::Org { .. } => {
                v["selected_repositories_url"] = json!(format!("{base}/repositories"))
            }
            Scope::Site => v["selected_organizations_url"] = json!(format!("{base}/organizations")),
        }
    }
    v
}

async fn find_group(state: &AppState, scope: &Scope, id: i64) -> ApiResult<GroupRow> {
    sqlx::query_as(&format!(
        "SELECT {} FROM actions_runner_groups WHERE id = $1 AND org_id IS NOT DISTINCT FROM $2",
        GroupRow::COLUMNS
    ))
    .bind(id)
    .bind(scope.org_id())
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)
}

// ---------------------------------------------------------------------------
// Shared implementation
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct GroupBody {
    pub name: Option<String>,
    pub visibility: Option<String>,
    pub selected_repository_ids: Option<Vec<i64>>,
    pub selected_organization_ids: Option<Vec<i64>>,
    pub runners: Option<Vec<i64>>,
    pub allows_public_repositories: Option<bool>,
    pub restricted_to_workflows: Option<bool>,
    pub selected_workflows: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    pub visible_to_repository: Option<String>,
    pub visible_to_organization: Option<String>,
}

fn check_visibility(scope: &Scope, v: &str) -> ApiResult<()> {
    let ok = match scope {
        Scope::Org { .. } => ["all", "selected", "private"].contains(&v),
        Scope::Site => ["all", "selected"].contains(&v),
    };
    if ok {
        Ok(())
    } else {
        Err(ApiError::invalid_field(FieldError::invalid(
            "RunnerGroup",
            "visibility",
        )))
    }
}

fn clean_name(name: &str) -> ApiResult<String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 255 {
        return Err(ApiError::invalid_field(FieldError::invalid(
            "RunnerGroup",
            "name",
        )));
    }
    Ok(name.to_string())
}

fn clean_workflows(list: &[String]) -> ApiResult<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for w in list {
        let w = w.trim();
        if w.is_empty() {
            continue;
        }
        // owner/repo/path/to/workflow.yml[@ref]
        if w.split('@').next().is_none_or(|p| p.split('/').count() < 3) {
            return Err(ApiError::invalid_field(FieldError::invalid(
                "RunnerGroup",
                "selected_workflows",
            )));
        }
        if !out.iter().any(|x| x == w) {
            out.push(w.to_string());
        }
    }
    Ok(out)
}

fn name_conflict(e: sqlx::Error) -> ApiError {
    match bgh_core::db::unique_violation(&e).as_deref() {
        Some("actions_runner_groups_name_key") => {
            ApiError::invalid_field(FieldError::already_exists("RunnerGroup", "name"))
        }
        _ => e.into(),
    }
}

/// Validate that every repository id belongs to the organization.
async fn check_repos(conn: &mut PgConnection, org_id: i64, ids: &[i64]) -> ApiResult<Vec<i64>> {
    let mut ids = ids.to_vec();
    ids.sort_unstable();
    ids.dedup();
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM repositories WHERE id = ANY($1) AND owner_id = $2",
    )
    .bind(&ids)
    .bind(org_id)
    .fetch_one(&mut *conn)
    .await?;
    if n != ids.len() as i64 {
        return Err(ApiError::invalid_field(FieldError::invalid(
            "RunnerGroup",
            "selected_repository_ids",
        )));
    }
    Ok(ids)
}

/// Validate that every id is an organization.
async fn check_orgs(conn: &mut PgConnection, ids: &[i64]) -> ApiResult<Vec<i64>> {
    let mut ids = ids.to_vec();
    ids.sort_unstable();
    ids.dedup();
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM users WHERE id = ANY($1) AND type = 'Organization'",
    )
    .bind(&ids)
    .fetch_one(&mut *conn)
    .await?;
    if n != ids.len() as i64 {
        return Err(ApiError::invalid_field(FieldError::invalid(
            "RunnerGroup",
            "selected_organization_ids",
        )));
    }
    Ok(ids)
}

/// Validate that every runner id belongs to the scope.
async fn check_runners(conn: &mut PgConnection, scope: &Scope, ids: &[i64]) -> ApiResult<Vec<i64>> {
    let mut ids = ids.to_vec();
    ids.sort_unstable();
    ids.dedup();
    let n: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM actions_runners WHERE {} AND id = ANY($2)",
        scope.runner_filter()
    ))
    .bind(scope.org_id())
    .bind(&ids)
    .fetch_one(&mut *conn)
    .await?;
    if n != ids.len() as i64 {
        return Err(ApiError::invalid_field(FieldError::invalid(
            "RunnerGroup",
            "runners",
        )));
    }
    Ok(ids)
}

async fn set_selection(
    conn: &mut PgConnection,
    scope: &Scope,
    group_id: i64,
    ids: &[i64],
) -> ApiResult<()> {
    let (table, col) = match scope {
        Scope::Org { .. } => ("actions_runner_group_repos", "repo_id"),
        Scope::Site => ("actions_runner_group_orgs", "org_id"),
    };
    sqlx::query(&format!(
        "DELETE FROM {table} WHERE group_id = $1 AND NOT ({col} = ANY($2))"
    ))
    .bind(group_id)
    .bind(ids)
    .execute(&mut *conn)
    .await?;
    sqlx::query(&format!(
        "INSERT INTO {table} (group_id, {col}) SELECT $1, unnest($2::bigint[]) ON CONFLICT DO NOTHING"
    ))
    .bind(group_id)
    .bind(ids)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Move `ids` into `group_id`; with `exclusive`, runners of the group not
/// in `ids` go back to the scope's default group.
async fn set_runners_inner(
    conn: &mut PgConnection,
    scope: &Scope,
    group_id: i64,
    ids: &[i64],
    exclusive: bool,
) -> ApiResult<()> {
    if exclusive {
        let default = ensure_default_group(conn, scope.org_id()).await?;
        sqlx::query(
            "UPDATE actions_runners SET runner_group_id = $3
              WHERE runner_group_id = $1 AND NOT (id = ANY($2))",
        )
        .bind(group_id)
        .bind(ids)
        .bind(default)
        .execute(&mut *conn)
        .await?;
    }
    sqlx::query("UPDATE actions_runners SET runner_group_id = $1 WHERE id = ANY($2)")
        .bind(group_id)
        .bind(ids)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

async fn list_inner(
    state: &AppState,
    scope: &Scope,
    p: &Pagination,
    q: &ListQuery,
) -> ApiResult<Response> {
    let mut conn = state.db.acquire().await?;
    ensure_default_group(&mut conn, scope.org_id()).await?;
    drop(conn);
    // Optional visibility filter: groups usable by one repository / org.
    let (filter, repo): (&str, Option<db::Repository>) = match (scope, &q.visible_to_repository) {
        (Scope::Org { id: org_id, .. }, Some(name)) => {
            let repo: Option<db::Repository> = sqlx::query_as(&format!(
                "SELECT {} FROM repositories WHERE owner_id = $1 AND lower(name) = lower($2)",
                db::Repository::COLUMNS
            ))
            .bind(*org_id)
            .bind(name)
            .fetch_optional(&state.db)
            .await?;
            let repo = repo.ok_or(ApiError::NotFound)?;
            (
                "AND (g.allows_public_repositories OR $4 <> 'public')
                 AND (g.visibility = 'all'
                      OR (g.visibility = 'private' AND $4 <> 'public')
                      OR EXISTS (SELECT 1 FROM actions_runner_group_repos gr
                                  WHERE gr.group_id = g.id AND gr.repo_id = $3))",
                Some(repo),
            )
        }
        _ => ("AND $3::bigint IS NULL AND $4::text IS NULL", None),
    };
    let org_filter = match (scope, &q.visible_to_organization) {
        (Scope::Site, Some(login)) => {
            let org = load_org(state, login).await?;
            Some(org.id)
        }
        _ => None,
    };
    let where_ = format!(
        "g.org_id IS NOT DISTINCT FROM $1 {filter}
         AND ($2::bigint IS NULL OR g.visibility = 'all' OR EXISTS (
              SELECT 1 FROM actions_runner_group_orgs go WHERE go.group_id = g.id AND go.org_id = $2))"
    );
    let total: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM actions_runner_groups g WHERE {where_}"
    ))
    .bind(scope.org_id())
    .bind(org_filter)
    .bind(repo.as_ref().map(|r| r.id))
    .bind(repo.as_ref().map(|r| r.visibility.clone()))
    .fetch_one(&state.db)
    .await?;
    let rows: Vec<GroupRow> = sqlx::query_as(&format!(
        "SELECT {} FROM actions_runner_groups g WHERE {where_}
          ORDER BY g.is_default DESC, g.id LIMIT $5 OFFSET $6",
        bgh_core::models::db::prefixed("g", GroupRow::COLUMNS)
    ))
    .bind(scope.org_id())
    .bind(org_filter)
    .bind(repo.as_ref().map(|r| r.id))
    .bind(repo.as_ref().map(|r| r.visibility.clone()))
    .bind(p.limit())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let items: Vec<Value> = rows.iter().map(|g| group_json(state, scope, g)).collect();
    Ok(wrapped(p, total, "runner_groups", items))
}

async fn create_inner(
    state: &AppState,
    auth: &AuthContext,
    scope: &Scope,
    body: GroupBody,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let name = clean_name(body.name.as_deref().ok_or_else(|| {
        ApiError::invalid_field(FieldError::missing_field("RunnerGroup", "name"))
    })?)?;
    let visibility = body.visibility.clone().unwrap_or_else(|| "all".into());
    check_visibility(scope, &visibility)?;
    let workflows = clean_workflows(body.selected_workflows.as_deref().unwrap_or_default())?;
    let mut tx = Tx::begin(state).await?;
    ensure_default_group(&mut tx, scope.org_id()).await?;
    let g: GroupRow = sqlx::query_as(&format!(
        "INSERT INTO actions_runner_groups
             (org_id, name, visibility, allows_public_repositories, restricted_to_workflows,
              selected_workflows)
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING {}",
        GroupRow::COLUMNS
    ))
    .bind(scope.org_id())
    .bind(&name)
    .bind(&visibility)
    .bind(body.allows_public_repositories.unwrap_or(false))
    .bind(body.restricted_to_workflows.unwrap_or(false))
    .bind(&workflows)
    .fetch_one(&mut *tx)
    .await
    .map_err(name_conflict)?;
    let selected = match scope {
        Scope::Org { id: org_id, .. } => match &body.selected_repository_ids {
            Some(ids) => Some(check_repos(&mut tx, *org_id, ids).await?),
            None => None,
        },
        Scope::Site => match &body.selected_organization_ids {
            Some(ids) => Some(check_orgs(&mut tx, ids).await?),
            None => None,
        },
    };
    if let Some(ids) = selected {
        set_selection(&mut tx, scope, g.id, &ids).await?;
    }
    if let Some(ids) = &body.runners {
        let ids = check_runners(&mut tx, scope, ids).await?;
        set_runners_inner(&mut tx, scope, g.id, &ids, false).await?;
    }
    bgh_core::audit::log(
        &mut *tx,
        Some(&auth.user),
        "runner_group.create",
        scope.audit_target(),
        json!({"runner_group": g.name, "runner_group_id": g.id}),
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(group_json(state, scope, &g))))
}

async fn update_inner(
    state: &AppState,
    auth: &AuthContext,
    scope: &Scope,
    id: i64,
    body: GroupBody,
) -> ApiResult<Json<Value>> {
    let g = find_group(state, scope, id).await?;
    let name = match &body.name {
        Some(n) => clean_name(n)?,
        None => g.name.clone(),
    };
    if g.is_default && name != g.name {
        return Err(ApiError::unprocessable(
            "The default runner group cannot be renamed",
        ));
    }
    let visibility = body.visibility.clone().unwrap_or(g.visibility.clone());
    check_visibility(scope, &visibility)?;
    let workflows = match &body.selected_workflows {
        Some(w) => clean_workflows(w)?,
        None => g.selected_workflows.clone(),
    };
    let mut tx = Tx::begin(state).await?;
    let g: GroupRow = sqlx::query_as(&format!(
        "UPDATE actions_runner_groups
            SET name = $2, visibility = $3, allows_public_repositories = $4,
                restricted_to_workflows = $5, selected_workflows = $6, updated_at = now()
          WHERE id = $1 RETURNING {}",
        GroupRow::COLUMNS
    ))
    .bind(g.id)
    .bind(&name)
    .bind(&visibility)
    .bind(
        body.allows_public_repositories
            .unwrap_or(g.allows_public_repositories),
    )
    .bind(
        body.restricted_to_workflows
            .unwrap_or(g.restricted_to_workflows),
    )
    .bind(&workflows)
    .fetch_one(&mut *tx)
    .await
    .map_err(name_conflict)?;
    bgh_core::audit::log(
        &mut *tx,
        Some(&auth.user),
        "runner_group.update",
        scope.audit_target(),
        json!({"runner_group": g.name, "runner_group_id": g.id}),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(group_json(state, scope, &g)))
}

async fn delete_inner(
    state: &AppState,
    auth: &AuthContext,
    scope: &Scope,
    id: i64,
) -> ApiResult<StatusCode> {
    let g = find_group(state, scope, id).await?;
    if g.is_default {
        return Err(ApiError::unprocessable(
            "The default runner group cannot be deleted",
        ));
    }
    let mut tx = Tx::begin(state).await?;
    let default = ensure_default_group(&mut tx, scope.org_id()).await?;
    // Its runners return to the default group.
    sqlx::query("UPDATE actions_runners SET runner_group_id = $2 WHERE runner_group_id = $1")
        .bind(g.id)
        .bind(default)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM actions_runner_groups WHERE id = $1")
        .bind(g.id)
        .execute(&mut *tx)
        .await?;
    bgh_core::audit::log(
        &mut *tx,
        Some(&auth.user),
        "runner_group.remove",
        scope.audit_target(),
        json!({"runner_group": g.name, "runner_group_id": g.id}),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_runners_inner(
    state: &AppState,
    scope: &Scope,
    p: &Pagination,
    id: i64,
) -> ApiResult<Response> {
    let g = find_group(state, scope, id).await?;
    let total: i64 =
        sqlx::query_scalar("SELECT count(*) FROM actions_runners WHERE runner_group_id = $1")
            .bind(g.id)
            .fetch_one(&state.db)
            .await?;
    let rows: Vec<RunnerRow> = sqlx::query_as(&format!(
        "SELECT {} FROM actions_runners WHERE runner_group_id = $1 ORDER BY id LIMIT $2 OFFSET $3",
        RunnerRow::COLUMNS
    ))
    .bind(g.id)
    .bind(p.limit())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let items: Vec<Value> = rows.iter().map(runner_json).collect();
    Ok(wrapped(p, total, "runners", items))
}

#[derive(Debug, Deserialize)]
pub struct RunnersBody {
    pub runners: Vec<i64>,
}

async fn set_runners_api(
    state: &AppState,
    auth: &AuthContext,
    scope: &Scope,
    id: i64,
    ids: &[i64],
    exclusive: bool,
) -> ApiResult<StatusCode> {
    let g = find_group(state, scope, id).await?;
    let mut tx = Tx::begin(state).await?;
    let ids = check_runners(&mut tx, scope, ids).await?;
    set_runners_inner(&mut tx, scope, g.id, &ids, exclusive).await?;
    bgh_core::audit::log(
        &mut *tx,
        Some(&auth.user),
        "runner_group.runners_added",
        scope.audit_target(),
        json!({"runner_group": g.name, "runner_group_id": g.id, "runners": ids}),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn remove_runner_inner(
    state: &AppState,
    auth: &AuthContext,
    scope: &Scope,
    id: i64,
    runner_id: i64,
) -> ApiResult<StatusCode> {
    let g = find_group(state, scope, id).await?;
    if g.is_default {
        return Err(ApiError::unprocessable(
            "Runners can't be removed from the default runner group; move them to another group",
        ));
    }
    let mut tx = Tx::begin(state).await?;
    let default = ensure_default_group(&mut tx, scope.org_id()).await?;
    let n = sqlx::query(
        "UPDATE actions_runners SET runner_group_id = $3 WHERE id = $2 AND runner_group_id = $1",
    )
    .bind(g.id)
    .bind(runner_id)
    .bind(default)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if n == 0 {
        return Err(ApiError::NotFound);
    }
    bgh_core::audit::log(
        &mut *tx,
        Some(&auth.user),
        "runner_group.runner_removed",
        scope.audit_target(),
        json!({"runner_group": g.name, "runner_group_id": g.id, "runner_id": runner_id}),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Replace (`exclusive`) or extend the selected repositories / orgs.
async fn set_selection_api(
    state: &AppState,
    auth: &AuthContext,
    scope: &Scope,
    id: i64,
    ids: &[i64],
    exclusive: bool,
) -> ApiResult<StatusCode> {
    let g = find_group(state, scope, id).await?;
    let mut tx = Tx::begin(state).await?;
    let ids = match scope {
        Scope::Org { id: org_id, .. } => check_repos(&mut tx, *org_id, ids).await?,
        Scope::Site => check_orgs(&mut tx, ids).await?,
    };
    if exclusive {
        set_selection(&mut tx, scope, g.id, &ids).await?;
    } else {
        let (table, col) = match scope {
            Scope::Org { .. } => ("actions_runner_group_repos", "repo_id"),
            Scope::Site => ("actions_runner_group_orgs", "org_id"),
        };
        sqlx::query(&format!(
            "INSERT INTO {table} (group_id, {col}) SELECT $1, unnest($2::bigint[]) ON CONFLICT DO NOTHING"
        ))
        .bind(g.id)
        .bind(&ids)
        .execute(&mut *tx)
        .await?;
    }
    bgh_core::audit::log(
        &mut *tx,
        Some(&auth.user),
        "runner_group.update_selection",
        scope.audit_target(),
        json!({"runner_group": g.name, "runner_group_id": g.id, "ids": ids}),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn remove_selection_inner(
    state: &AppState,
    auth: &AuthContext,
    scope: &Scope,
    id: i64,
    member: i64,
) -> ApiResult<StatusCode> {
    let g = find_group(state, scope, id).await?;
    let (table, col) = match scope {
        Scope::Org { .. } => ("actions_runner_group_repos", "repo_id"),
        Scope::Site => ("actions_runner_group_orgs", "org_id"),
    };
    let mut tx = Tx::begin(state).await?;
    sqlx::query(&format!(
        "DELETE FROM {table} WHERE group_id = $1 AND {col} = $2"
    ))
    .bind(g.id)
    .bind(member)
    .execute(&mut *tx)
    .await?;
    bgh_core::audit::log(
        &mut *tx,
        Some(&auth.user),
        "runner_group.update_selection",
        scope.audit_target(),
        json!({"runner_group": g.name, "runner_group_id": g.id, "removed": member}),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Organization handlers
// ---------------------------------------------------------------------------

pub mod org {
    use super::*;

    async fn scope(state: &AppState, auth: &AuthContext, org: &str) -> ApiResult<Scope> {
        let o = load_org(state, org).await?;
        require_org_admin(state, auth, &o).await?;
        Ok(Scope::Org {
            id: o.id,
            login: o.login,
        })
    }

    #[derive(Debug, Deserialize)]
    pub struct ReposBody {
        pub selected_repository_ids: Vec<i64>,
    }

    pub async fn list(
        State(state): State<AppState>,
        auth: RequireUser,
        p: Pagination,
        Path(org): Path<String>,
        Query(q): Query<ListQuery>,
    ) -> ApiResult<Response> {
        let s = scope(&state, &auth, &org).await?;
        list_inner(&state, &s, &p, &q).await
    }

    pub async fn create(
        State(state): State<AppState>,
        auth: RequireUser,
        Path(org): Path<String>,
        Json(body): Json<GroupBody>,
    ) -> ApiResult<(StatusCode, Json<Value>)> {
        let s = scope(&state, &auth, &org).await?;
        create_inner(&state, &auth, &s, body).await
    }

    pub async fn get(
        State(state): State<AppState>,
        auth: RequireUser,
        Path((org, id)): Path<(String, i64)>,
    ) -> ApiResult<Json<Value>> {
        let s = scope(&state, &auth, &org).await?;
        let g = find_group(&state, &s, id).await?;
        Ok(Json(group_json(&state, &s, &g)))
    }

    pub async fn update(
        State(state): State<AppState>,
        auth: RequireUser,
        Path((org, id)): Path<(String, i64)>,
        Json(body): Json<GroupBody>,
    ) -> ApiResult<Json<Value>> {
        let s = scope(&state, &auth, &org).await?;
        update_inner(&state, &auth, &s, id, body).await
    }

    pub async fn delete(
        State(state): State<AppState>,
        auth: RequireUser,
        Path((org, id)): Path<(String, i64)>,
    ) -> ApiResult<StatusCode> {
        let s = scope(&state, &auth, &org).await?;
        delete_inner(&state, &auth, &s, id).await
    }

    pub async fn list_repos(
        State(state): State<AppState>,
        auth: RequireUser,
        p: Pagination,
        Path((org, id)): Path<(String, i64)>,
    ) -> ApiResult<Response> {
        let s = scope(&state, &auth, &org).await?;
        let g = find_group(&state, &s, id).await?;
        let total: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM actions_runner_group_repos WHERE group_id = $1",
        )
        .bind(g.id)
        .fetch_one(&state.db)
        .await?;
        let rows: Vec<db::Repository> = sqlx::query_as(&format!(
            "SELECT {} FROM repositories r JOIN actions_runner_group_repos gr ON gr.repo_id = r.id
              WHERE gr.group_id = $1 ORDER BY lower(r.name), r.id LIMIT $2 OFFSET $3",
            bgh_core::models::db::prefixed("r", db::Repository::COLUMNS)
        ))
        .bind(g.id)
        .bind(p.limit())
        .bind(p.offset())
        .fetch_all(&state.db)
        .await?;
        let items = bgh_core::views::minimal_repos(&state, Some(&auth), rows).await?;
        Ok(wrapped(&p, total, "repositories", items))
    }

    pub async fn set_repos(
        State(state): State<AppState>,
        auth: RequireUser,
        Path((org, id)): Path<(String, i64)>,
        Json(body): Json<ReposBody>,
    ) -> ApiResult<StatusCode> {
        let s = scope(&state, &auth, &org).await?;
        set_selection_api(&state, &auth, &s, id, &body.selected_repository_ids, true).await
    }

    pub async fn add_repo(
        State(state): State<AppState>,
        auth: RequireUser,
        Path((org, id, repo_id)): Path<(String, i64, i64)>,
    ) -> ApiResult<StatusCode> {
        let s = scope(&state, &auth, &org).await?;
        set_selection_api(&state, &auth, &s, id, &[repo_id], false).await
    }

    pub async fn remove_repo(
        State(state): State<AppState>,
        auth: RequireUser,
        Path((org, id, repo_id)): Path<(String, i64, i64)>,
    ) -> ApiResult<StatusCode> {
        let s = scope(&state, &auth, &org).await?;
        remove_selection_inner(&state, &auth, &s, id, repo_id).await
    }

    pub async fn list_runners(
        State(state): State<AppState>,
        auth: RequireUser,
        p: Pagination,
        Path((org, id)): Path<(String, i64)>,
    ) -> ApiResult<Response> {
        let s = scope(&state, &auth, &org).await?;
        list_runners_inner(&state, &s, &p, id).await
    }

    pub async fn set_runners(
        State(state): State<AppState>,
        auth: RequireUser,
        Path((org, id)): Path<(String, i64)>,
        Json(body): Json<RunnersBody>,
    ) -> ApiResult<StatusCode> {
        let s = scope(&state, &auth, &org).await?;
        set_runners_api(&state, &auth, &s, id, &body.runners, true).await
    }

    pub async fn add_runner(
        State(state): State<AppState>,
        auth: RequireUser,
        Path((org, id, runner_id)): Path<(String, i64, i64)>,
    ) -> ApiResult<StatusCode> {
        let s = scope(&state, &auth, &org).await?;
        set_runners_api(&state, &auth, &s, id, &[runner_id], false).await
    }

    pub async fn remove_runner(
        State(state): State<AppState>,
        auth: RequireUser,
        Path((org, id, runner_id)): Path<(String, i64, i64)>,
    ) -> ApiResult<StatusCode> {
        let s = scope(&state, &auth, &org).await?;
        remove_runner_inner(&state, &auth, &s, id, runner_id).await
    }
}

// ---------------------------------------------------------------------------
// Site handlers
// ---------------------------------------------------------------------------

pub mod site {
    use super::*;

    #[derive(Debug, Deserialize)]
    pub struct OrgsBody {
        pub selected_organization_ids: Vec<i64>,
    }

    pub async fn list(
        State(state): State<AppState>,
        _admin: RequireSiteAdmin,
        p: Pagination,
        Query(q): Query<ListQuery>,
    ) -> ApiResult<Response> {
        list_inner(&state, &Scope::Site, &p, &q).await
    }

    pub async fn create(
        State(state): State<AppState>,
        admin: RequireSiteAdmin,
        Json(body): Json<GroupBody>,
    ) -> ApiResult<(StatusCode, Json<Value>)> {
        create_inner(&state, &admin, &Scope::Site, body).await
    }

    pub async fn get(
        State(state): State<AppState>,
        _admin: RequireSiteAdmin,
        Path(id): Path<i64>,
    ) -> ApiResult<Json<Value>> {
        let g = find_group(&state, &Scope::Site, id).await?;
        Ok(Json(group_json(&state, &Scope::Site, &g)))
    }

    pub async fn update(
        State(state): State<AppState>,
        admin: RequireSiteAdmin,
        Path(id): Path<i64>,
        Json(body): Json<GroupBody>,
    ) -> ApiResult<Json<Value>> {
        update_inner(&state, &admin, &Scope::Site, id, body).await
    }

    pub async fn delete(
        State(state): State<AppState>,
        admin: RequireSiteAdmin,
        Path(id): Path<i64>,
    ) -> ApiResult<StatusCode> {
        delete_inner(&state, &admin, &Scope::Site, id).await
    }

    pub async fn list_orgs(
        State(state): State<AppState>,
        _admin: RequireSiteAdmin,
        p: Pagination,
        Path(id): Path<i64>,
    ) -> ApiResult<Response> {
        let g = find_group(&state, &Scope::Site, id).await?;
        let total: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM actions_runner_group_orgs WHERE group_id = $1",
        )
        .bind(g.id)
        .fetch_one(&state.db)
        .await?;
        let rows: Vec<db::User> = sqlx::query_as(&format!(
            "SELECT {} FROM users u JOIN actions_runner_group_orgs go ON go.org_id = u.id
              WHERE go.group_id = $1 ORDER BY lower(u.login), u.id LIMIT $2 OFFSET $3",
            bgh_core::models::db::prefixed("u", db::User::COLUMNS)
        ))
        .bind(g.id)
        .bind(p.limit())
        .bind(p.offset())
        .fetch_all(&state.db)
        .await?;
        let items: Vec<api::OrganizationSimple> = rows
            .iter()
            .map(|o| api::OrganizationSimple::new(&state.urls, o, None))
            .collect();
        Ok(wrapped(&p, total, "organizations", items))
    }

    pub async fn set_orgs(
        State(state): State<AppState>,
        admin: RequireSiteAdmin,
        Path(id): Path<i64>,
        Json(body): Json<OrgsBody>,
    ) -> ApiResult<StatusCode> {
        set_selection_api(
            &state,
            &admin,
            &Scope::Site,
            id,
            &body.selected_organization_ids,
            true,
        )
        .await
    }

    pub async fn add_org(
        State(state): State<AppState>,
        admin: RequireSiteAdmin,
        Path((id, org_id)): Path<(i64, i64)>,
    ) -> ApiResult<StatusCode> {
        set_selection_api(&state, &admin, &Scope::Site, id, &[org_id], false).await
    }

    pub async fn remove_org(
        State(state): State<AppState>,
        admin: RequireSiteAdmin,
        Path((id, org_id)): Path<(i64, i64)>,
    ) -> ApiResult<StatusCode> {
        remove_selection_inner(&state, &admin, &Scope::Site, id, org_id).await
    }

    pub async fn list_runners(
        State(state): State<AppState>,
        _admin: RequireSiteAdmin,
        p: Pagination,
        Path(id): Path<i64>,
    ) -> ApiResult<Response> {
        list_runners_inner(&state, &Scope::Site, &p, id).await
    }

    pub async fn set_runners(
        State(state): State<AppState>,
        admin: RequireSiteAdmin,
        Path(id): Path<i64>,
        Json(body): Json<RunnersBody>,
    ) -> ApiResult<StatusCode> {
        set_runners_api(&state, &admin, &Scope::Site, id, &body.runners, true).await
    }

    pub async fn add_runner(
        State(state): State<AppState>,
        admin: RequireSiteAdmin,
        Path((id, runner_id)): Path<(i64, i64)>,
    ) -> ApiResult<StatusCode> {
        set_runners_api(&state, &admin, &Scope::Site, id, &[runner_id], false).await
    }

    pub async fn remove_runner(
        State(state): State<AppState>,
        admin: RequireSiteAdmin,
        Path((id, runner_id)): Path<(i64, i64)>,
    ) -> ApiResult<StatusCode> {
        remove_runner_inner(&state, &admin, &Scope::Site, id, runner_id).await
    }
}
