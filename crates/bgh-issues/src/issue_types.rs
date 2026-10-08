//! Issue types: organization-defined (`/orgs/{org}/issue-types`), set on
//! issues with `type` (REST) / `issueTypeId` (GraphQL).
//!
//! Every organization starts with GitHub's defaults (Task, Bug, Feature),
//! seeded by a trigger on organization insert (migration 5300). Issues in
//! user-owned repositories can't have a type.

use std::collections::HashMap;

use axum::extract::State;
use axum::http::StatusCode;
use bgh_core::node_id::{self, NodeType};
use bgh_core::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::issues::double;
use crate::service;

/// GitHub's per-organization limit.
pub const MAX_TYPES: i64 = 25;
pub const COLORS: &[&str] = &[
    "gray", "blue", "green", "yellow", "orange", "red", "pink", "purple",
];

/// `issue_types` row.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct IssueTypeRow {
    pub id: i64,
    pub org_id: i64,
    pub name: String,
    pub description: Option<String>,
    pub color: Option<String>,
    pub is_enabled: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl IssueTypeRow {
    pub const COLUMNS: &'static str =
        "id, org_id, name, description, color, is_enabled, created_at, updated_at";

    /// Compact form stored in timeline event data.
    pub fn event_json(&self) -> serde_json::Value {
        json!({ "id": self.id, "name": self.name, "color": self.color })
    }
}

/// `issue-type`.
#[derive(Debug, Clone, Serialize)]
pub struct IssueType {
    pub id: i64,
    pub node_id: String,
    pub name: String,
    pub description: Option<String>,
    pub color: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub is_enabled: bool,
}

impl From<&IssueTypeRow> for IssueType {
    fn from(r: &IssueTypeRow) -> Self {
        Self {
            id: r.id,
            node_id: node_id::encode(NodeType::IssueType, r.id),
            name: r.name.clone(),
            description: r.description.clone(),
            color: r.color.clone(),
            created_at: r.created_at.into(),
            updated_at: r.updated_at.into(),
            is_enabled: r.is_enabled,
        }
    }
}

/// Types of the given issues (issues without a type are absent).
pub async fn for_issues(
    db: impl sqlx::PgExecutor<'_>,
    issue_ids: &[i64],
) -> ApiResult<HashMap<i64, IssueTypeRow>> {
    if issue_ids.is_empty() {
        return Ok(HashMap::new());
    }
    #[derive(sqlx::FromRow)]
    struct TypedIssue {
        issue_id: i64,
        #[sqlx(flatten)]
        t: IssueTypeRow,
    }
    let rows: Vec<TypedIssue> = sqlx::query_as(&format!(
        "SELECT i.id AS issue_id, {} FROM issues i JOIN issue_types t ON t.id = i.issue_type_id
          WHERE i.id = ANY($1)",
        db::prefixed("t", IssueTypeRow::COLUMNS)
    ))
    .bind(issue_ids)
    .fetch_all(db)
    .await?;
    Ok(rows.into_iter().map(|r| (r.issue_id, r.t)).collect())
}

/// Type by id.
pub async fn by_id(db: impl sqlx::PgExecutor<'_>, id: i64) -> ApiResult<Option<IssueTypeRow>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {} FROM issue_types WHERE id = $1",
        IssueTypeRow::COLUMNS
    ))
    .bind(id)
    .fetch_optional(db)
    .await?)
}

/// Resolve an enabled type by name for an issue in `repo` (422 when the
/// repository isn't owned by an organization or the name is unknown).
pub async fn resolve(
    db: impl sqlx::PgExecutor<'_>,
    repo: &db::Repository,
    name: &str,
) -> ApiResult<IssueTypeRow> {
    let row: Option<IssueTypeRow> = sqlx::query_as(&format!(
        "SELECT {} FROM issue_types WHERE org_id = $1 AND lower(name) = lower($2) AND is_enabled",
        IssueTypeRow::COLUMNS
    ))
    .bind(repo.owner_id)
    .bind(name.trim())
    .fetch_optional(db)
    .await?;
    row.ok_or_else(|| ApiError::invalid_field(FieldError::invalid("Issue", "type")))
}

/// Set (or clear) an issue's type, writing the `issue_type_added` /
/// `issue_type_changed` / `issue_type_removed` timeline event. The caller
/// syncs the issue. Returns whether anything changed.
pub async fn set_for_issue(
    tx: &mut Tx,
    issue: &db::Issue,
    actor_id: i64,
    new: Option<&IssueTypeRow>,
) -> ApiResult<bool> {
    let current: Option<i64> = sqlx::query_scalar("SELECT issue_type_id FROM issues WHERE id = $1")
        .bind(issue.id)
        .fetch_one(&mut **tx)
        .await?;
    if current == new.map(|t| t.id) {
        return Ok(false);
    }
    let old = match current {
        Some(id) => by_id(&mut **tx, id).await?,
        None => None,
    };
    sqlx::query("UPDATE issues SET issue_type_id = $2 WHERE id = $1")
        .bind(issue.id)
        .bind(new.map(|t| t.id))
        .execute(&mut **tx)
        .await?;
    let (event, data) = match (&old, new) {
        (None, Some(n)) => ("issue_type_added", json!({ "issue_type": n.event_json() })),
        (Some(o), Some(n)) => (
            "issue_type_changed",
            json!({ "issue_type": n.event_json(), "prev_issue_type": o.event_json() }),
        ),
        (Some(o), None) => (
            "issue_type_removed",
            json!({ "issue_type": o.event_json() }),
        ),
        (None, None) => return Ok(true),
    };
    service::add_event(tx, issue, Some(actor_id), event, None, data).await?;
    Ok(true)
}

// ---------------------------------------------------------------------------
// /orgs/{org}/issue-types
// ---------------------------------------------------------------------------

async fn load_org(state: &AppState, org: &str) -> ApiResult<db::User> {
    db::User::find_by_login(&state.db, org)
        .await?
        .filter(db::User::is_org)
        .ok_or(ApiError::NotFound)
}

/// Organization owners (and site admins) manage types: 404 for
/// non-members, 403 for members.
async fn require_org_admin(state: &AppState, auth: &AuthContext, org: &db::User) -> ApiResult<()> {
    if auth.user.site_admin {
        return Ok(());
    }
    match bgh_core::perms::org_role(&state.db, org.id, auth.user.id).await? {
        Some(r) if r.is_admin() => auth.require_scope("admin:org"),
        Some(_) => Err(ApiError::forbidden("Must be an organization owner.")),
        None => Err(ApiError::NotFound),
    }
}

/// `GET /orgs/{org}/issue-types` (not paginated, like GitHub).
pub async fn list(
    State(state): State<AppState>,
    _auth: MaybeUser,
    Path(org): Path<String>,
) -> ApiResult<Json<Vec<IssueType>>> {
    let org = load_org(&state, &org).await?;
    let rows: Vec<IssueTypeRow> = sqlx::query_as(&format!(
        "SELECT {} FROM issue_types WHERE org_id = $1 ORDER BY id",
        IssueTypeRow::COLUMNS
    ))
    .bind(org.id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(rows.iter().map(IssueType::from).collect()))
}

#[derive(Debug, Default, Deserialize)]
pub struct TypeBody {
    pub name: Option<String>,
    pub is_enabled: Option<bool>,
    #[serde(default, deserialize_with = "double")]
    pub description: Option<Option<String>>,
    #[serde(default, deserialize_with = "double")]
    pub color: Option<Option<String>>,
    /// Accepted for compatibility (GitHub's private types); ignored.
    pub is_private: Option<bool>,
}

struct Validated {
    name: String,
    is_enabled: bool,
    description: Option<String>,
    color: Option<String>,
}

fn validate(body: TypeBody) -> ApiResult<Validated> {
    let name = body.name.map(|n| n.trim().to_string()).unwrap_or_default();
    if name.is_empty() {
        return Err(ApiError::invalid_field(FieldError::missing_field(
            "IssueType",
            "name",
        )));
    }
    if name.chars().count() > 255 {
        return Err(ApiError::invalid_field(FieldError::custom(
            "IssueType",
            "name",
            "name is too long (maximum is 255 characters)",
        )));
    }
    let is_enabled = body.is_enabled.ok_or_else(|| {
        ApiError::invalid_field(FieldError::missing_field("IssueType", "is_enabled"))
    })?;
    let color = body.color.flatten().map(|c| c.to_lowercase());
    if let Some(c) = &color
        && !COLORS.contains(&c.as_str())
    {
        return Err(ApiError::invalid_field(FieldError::invalid(
            "IssueType",
            "color",
        )));
    }
    let description = body.description.flatten().filter(|d| !d.trim().is_empty());
    Ok(Validated {
        name,
        is_enabled,
        description,
        color,
    })
}

fn map_unique(e: sqlx::Error) -> ApiError {
    match bgh_core::db::unique_violation(&e).as_deref() {
        Some("issue_types_org_name_key") => {
            ApiError::invalid_field(FieldError::already_exists("IssueType", "name"))
        }
        _ => e.into(),
    }
}

/// Re-sync every issue of type `type_id` (its compact row embeds the type).
async fn sync_typed_issues(tx: &mut Tx, type_id: i64) -> ApiResult<()> {
    let ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM issues WHERE issue_type_id = $1")
        .bind(type_id)
        .fetch_all(&mut **tx)
        .await?;
    if !ids.is_empty() {
        tx.sync_models(SyncModel::Issue, &ids, SyncAction::Update)
            .await?;
    }
    Ok(())
}

/// `POST /orgs/{org}/issue-types` → 200 (GitHub answers 200, not 201).
pub async fn create(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(org): Path<String>,
    Json(body): Json<TypeBody>,
) -> ApiResult<Json<IssueType>> {
    let org = load_org(&state, &org).await?;
    require_org_admin(&state, &auth, &org).await?;
    let v = validate(body)?;
    let mut tx = Tx::begin(&state).await?;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM issue_types WHERE org_id = $1")
        .bind(org.id)
        .fetch_one(&mut *tx)
        .await?;
    if count >= MAX_TYPES {
        return Err(ApiError::unprocessable(format!(
            "Organizations can have at most {MAX_TYPES} issue types"
        )));
    }
    let row: IssueTypeRow = sqlx::query_as(&format!(
        "INSERT INTO issue_types (org_id, name, description, color, is_enabled)
         VALUES ($1, $2, $3, $4, $5) RETURNING {}",
        IssueTypeRow::COLUMNS
    ))
    .bind(org.id)
    .bind(&v.name)
    .bind(&v.description)
    .bind(&v.color)
    .bind(v.is_enabled)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_unique)?;
    bgh_core::audit::log(
        &mut *tx,
        Some(&auth.user),
        "org.issue_type_create",
        bgh_core::audit::Target::Org(org.id),
        json!({ "name": v.name }),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(IssueType::from(&row)))
}

async fn load_type(state: &AppState, org: &db::User, id: i64) -> ApiResult<IssueTypeRow> {
    by_id(&state.db, id)
        .await?
        .filter(|t| t.org_id == org.id)
        .ok_or(ApiError::NotFound)
}

/// `PUT /orgs/{org}/issue-types/{issue_type_id}`
pub async fn update(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((org, id)): Path<(String, i64)>,
    Json(body): Json<TypeBody>,
) -> ApiResult<Json<IssueType>> {
    let org = load_org(&state, &org).await?;
    require_org_admin(&state, &auth, &org).await?;
    load_type(&state, &org, id).await?;
    let v = validate(body)?;
    let mut tx = Tx::begin(&state).await?;
    let row: IssueTypeRow = sqlx::query_as(&format!(
        "UPDATE issue_types SET name = $2, description = $3, color = $4, is_enabled = $5,
                updated_at = now()
          WHERE id = $1 RETURNING {}",
        IssueTypeRow::COLUMNS
    ))
    .bind(id)
    .bind(&v.name)
    .bind(&v.description)
    .bind(&v.color)
    .bind(v.is_enabled)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_unique)?;
    sync_typed_issues(&mut tx, id).await?;
    bgh_core::audit::log(
        &mut *tx,
        Some(&auth.user),
        "org.issue_type_update",
        bgh_core::audit::Target::Org(org.id),
        json!({ "id": id, "name": v.name }),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(IssueType::from(&row)))
}

/// `DELETE /orgs/{org}/issue-types/{issue_type_id}` → 204. Issues of this
/// type lose it.
pub async fn delete(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((org, id)): Path<(String, i64)>,
) -> ApiResult<StatusCode> {
    let org = load_org(&state, &org).await?;
    require_org_admin(&state, &auth, &org).await?;
    let t = load_type(&state, &org, id).await?;
    let mut tx = Tx::begin(&state).await?;
    let ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM issues WHERE issue_type_id = $1")
        .bind(id)
        .fetch_all(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM issue_types WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    if !ids.is_empty() {
        tx.sync_models(SyncModel::Issue, &ids, SyncAction::Update)
            .await?;
    }
    bgh_core::audit::log(
        &mut *tx,
        Some(&auth.user),
        "org.issue_type_delete",
        bgh_core::audit::Target::Org(org.id),
        json!({ "id": id, "name": t.name }),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
