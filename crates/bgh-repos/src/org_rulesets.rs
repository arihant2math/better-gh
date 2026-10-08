//! Organization rulesets (GitHub "Organization rules" API).
//!
//! * `GET|POST /orgs/{org}/rulesets`
//! * `GET|PUT|DELETE /orgs/{org}/rulesets/{id}`
//!
//! Organization rulesets live in `repo_rulesets` (`org_id` set) and apply
//! to the organization's repositories selected by their
//! `repository_name` / `repository_id` conditions
//! ([`RulesetRow::applies_to_repo`]); [`crate::protection::RepoRules`]
//! loads them with the repository's own rulesets, so pushes, merges and
//! `rules/branches` enforce and report them. Every endpoint needs an
//! organization owner (`admin:org`), like GitHub.

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use bgh_core::audit;
use bgh_core::prelude::*;
use serde_json::{Value, json};

use crate::protection::{Actor, RulesetRow};
use crate::rulesets::{
    ListQuery, RulesetInput, bypass_mode, full, map_unique, summary, sync_json, user_of, validate,
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/orgs/{org}/rulesets", get(list).post(create))
        .route(
            "/orgs/{org}/rulesets/{id}",
            get(get_one).put(update).delete(destroy),
        )
}

/// Load an organization by login (404 unless it is one).
pub(crate) async fn load_org(state: &AppState, org: &str) -> ApiResult<db::User> {
    db::User::find_by_login(&state.db, org)
        .await?
        .filter(|u| u.is_org())
        .ok_or(ApiError::NotFound)
}

/// Require an organization owner (404 for non-members, 403 for members).
pub(crate) async fn require_org_admin(
    state: &AppState,
    auth: &MaybeUser,
    org: &db::User,
) -> ApiResult<AuthContext> {
    let auth = auth.0.clone().ok_or_else(ApiError::requires_auth)?;
    if auth.user.site_admin {
        return Ok(auth);
    }
    match bgh_core::perms::org_role(&state.db, org.id, auth.user.id).await? {
        Some(r) if r.is_admin() => {
            auth.require_scope("admin:org")?;
            Ok(auth)
        }
        Some(_) => Err(ApiError::forbidden("Must be an organization owner.")),
        None => Err(ApiError::NotFound),
    }
}

async fn org_actor(state: &AppState, org: &db::User, auth: &AuthContext) -> ApiResult<Actor> {
    Actor::for_user(state, org, auth.user.id, Permission::Admin).await
}

async fn find(state: &AppState, org: &db::User, id: i64) -> ApiResult<RulesetRow> {
    sqlx::query_as(&format!(
        "SELECT {} FROM repo_rulesets WHERE id = $1 AND org_id = $2",
        RulesetRow::COLUMNS
    ))
    .bind(id)
    .bind(org.id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)
}

fn render(state: &AppState, org: &db::User, r: &RulesetRow, actor: &Actor) -> Value {
    full(
        state,
        &org.login,
        None,
        r,
        true,
        bypass_mode(r, Some(actor)),
    )
}

/// `GET /orgs/{org}/rulesets`
async fn list(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Query(q): Query<ListQuery>,
    Path(org): Path<String>,
) -> ApiResult<Page<Value>> {
    let org = load_org(&state, &org).await?;
    require_org_admin(&state, &auth, &org).await?;
    let rows: Vec<RulesetRow> = sqlx::query_as(&format!(
        "SELECT {} FROM repo_rulesets WHERE org_id = $1 ORDER BY id",
        RulesetRow::COLUMNS
    ))
    .bind(org.id)
    .fetch_all(&state.db)
    .await?;
    let rows: Vec<&RulesetRow> = rows.iter().filter(|r| q.wants(&r.target)).collect();
    let total = rows.len() as i64;
    let page: Vec<Value> = rows
        .into_iter()
        .skip(p.offset() as usize)
        .take(p.limit() as usize)
        .map(|r| summary(&state, &org.login, None, r))
        .collect();
    Ok(p.page_with_total(page, total))
}

/// `GET /orgs/{org}/rulesets/{id}`
async fn get_one(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((org, id)): Path<(String, i64)>,
) -> ApiResult<Json<Value>> {
    let org = load_org(&state, &org).await?;
    let a = require_org_admin(&state, &auth, &org).await?;
    let row = find(&state, &org, id).await?;
    let actor = org_actor(&state, &org, &a).await?;
    Ok(Json(render(&state, &org, &row, &actor)))
}

/// `POST /orgs/{org}/rulesets`
async fn create(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path(org): Path<String>,
    Json(input): Json<RulesetInput>,
) -> ApiResult<Response> {
    let org = load_org(&state, &org).await?;
    let a = require_org_admin(&state, &auth, &org).await?;
    let user = user_of(&auth)?;
    let f = validate(&state, &org, true, input, None).await?;

    let mut tx = Tx::begin(&state).await?;
    let row: RulesetRow = sqlx::query_as(&format!(
        "INSERT INTO repo_rulesets (org_id, name, target, enforcement, conditions, rules,
                                    bypass_actors, created_by_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING {}",
        RulesetRow::COLUMNS
    ))
    .bind(org.id)
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
        &bgh_core::sync::org_scope(org.id),
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
        audit::Target::Org(org.id),
        json!({"ruleset_id": row.id, "name": row.name, "source_type": "Organization"}),
    )
    .await?;
    tx.commit().await?;

    let actor = org_actor(&state, &org, &a).await?;
    Ok((
        StatusCode::CREATED,
        Json(render(&state, &org, &row, &actor)),
    )
        .into_response())
}

/// `PUT /orgs/{org}/rulesets/{id}` (partial update)
async fn update(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((org, id)): Path<(String, i64)>,
    Json(input): Json<RulesetInput>,
) -> ApiResult<Json<Value>> {
    let org = load_org(&state, &org).await?;
    let a = require_org_admin(&state, &auth, &org).await?;
    let user = user_of(&auth)?;
    let existing = find(&state, &org, id).await?;
    let f = validate(&state, &org, true, input, Some(&existing)).await?;

    let mut tx = Tx::begin(&state).await?;
    let row: RulesetRow = sqlx::query_as(&format!(
        "UPDATE repo_rulesets SET name = $3, target = $4, enforcement = $5, conditions = $6,
                rules = $7, bypass_actors = $8, updated_at = now()
          WHERE id = $1 AND org_id = $2 RETURNING {}",
        RulesetRow::COLUMNS
    ))
    .bind(id)
    .bind(org.id)
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
        &bgh_core::sync::org_scope(org.id),
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
        audit::Target::Org(org.id),
        json!({"ruleset_id": row.id, "name": row.name, "source_type": "Organization"}),
    )
    .await?;
    tx.commit().await?;

    let actor = org_actor(&state, &org, &a).await?;
    Ok(Json(render(&state, &org, &row, &actor)))
}

/// `DELETE /orgs/{org}/rulesets/{id}`
async fn destroy(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((org, id)): Path<(String, i64)>,
) -> ApiResult<StatusCode> {
    let org = load_org(&state, &org).await?;
    require_org_admin(&state, &auth, &org).await?;
    let user = user_of(&auth)?;

    let mut tx = Tx::begin(&state).await?;
    let name: String = sqlx::query_scalar(
        "DELETE FROM repo_rulesets WHERE id = $1 AND org_id = $2 RETURNING name",
    )
    .bind(id)
    .bind(org.id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(ApiError::NotFound)?;
    tx.sync(
        &bgh_core::sync::org_scope(org.id),
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
        audit::Target::Org(org.id),
        json!({"ruleset_id": id, "name": name, "source_type": "Organization"}),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
