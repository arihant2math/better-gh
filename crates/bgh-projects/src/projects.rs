//! Project CRUD, listing and snapshots.

use std::collections::HashMap;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use bgh_core::prelude::*;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::access::{ProjectAccess, Role, owner_role, project_role};
use crate::compact;
use crate::model::*;
use crate::service;
use crate::util::{nullable, validate_text};

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    pub state: Option<String>,
    pub q: Option<String>,
}

/// `GET /_bgh/owners/{owner}/projects`
pub async fn list_for_owner(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path(owner): Path<String>,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    let owner = db::User::find_by_login(&state.db, &owner)
        .await?
        .ok_or(ApiError::NotFound)?;
    let role = owner_role(&state, auth.as_ref(), &owner).await?;
    let closed = match q.state.as_deref().unwrap_or("open") {
        "open" => Some(false),
        "closed" => Some(true),
        "all" => None,
        _ => {
            return Err(ApiError::invalid_field(FieldError::invalid(
                "Project", "state",
            )));
        }
    };
    let rows: Vec<ProjectRow> = sqlx::query_as(&format!(
        "{} WHERE p.owner_id = $1 AND ($2::bool OR p.public)
           AND ($3::bool IS NULL OR p.closed = $3)
           AND ($4::text IS NULL OR p.title ILIKE '%' || $4 || '%')
         ORDER BY p.updated_at DESC, p.id DESC LIMIT 500",
        ProjectRow::SELECT
    ))
    .bind(owner.id)
    .bind(role.is_some())
    .bind(closed)
    .bind(q.q.as_deref().map(str::trim).filter(|s| !s.is_empty()))
    .fetch_all(&state.db)
    .await?;
    let users = compact::users_json(&state, rows.iter().map(|p| p.creator_id)).await?;
    Ok(Json(json!({
        "projects": rows.iter().map(ProjectRow::sync_json).collect::<Vec<_>>(),
        "users": users,
    })))
}

/// `GET /_bgh/repos/{owner}/{repo}/projects`: projects linked to the repo
/// or containing its issues, visible to the caller.
pub async fn list_for_repo(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let rows: Vec<ProjectRow> = sqlx::query_as(&format!(
        "{} WHERE p.id IN (
             SELECT project_id FROM project_linked_repos WHERE repo_id = $1
             UNION
             SELECT pi.project_id FROM project_items pi JOIN issues iss ON iss.id = pi.issue_id
              WHERE iss.repo_id = $1)
         ORDER BY p.closed, p.updated_at DESC, p.id DESC",
        ProjectRow::SELECT
    ))
    .bind(access.repo.id)
    .fetch_all(&state.db)
    .await?;
    let owners =
        bgh_core::views::users_by_id(&state, rows.iter().map(|p| Some(p.owner_id))).await?;
    let mut roles: HashMap<i64, Option<Role>> = HashMap::new();
    for (id, owner) in &owners {
        roles.insert(*id, owner_role(&state, auth.as_ref(), owner).await?);
    }
    let visible: Vec<&ProjectRow> = rows
        .iter()
        .filter(|p| project_role(roles.get(&p.owner_id).copied().flatten(), p.public).is_some())
        .collect();
    let owner_rows: Vec<Value> = owners
        .values()
        .filter(|o| visible.iter().any(|p| p.owner_id == o.id))
        .map(|o| json!({"id": o.id, "login": o.login, "type": o.kind}))
        .collect();
    Ok(Json(json!({
        "projects": visible.iter().map(|p| p.sync_json()).collect::<Vec<_>>(),
        "owners": owner_rows,
    })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateBody {
    pub owner: Option<String>,
    pub title: Option<String>,
    pub short_description: Option<String>,
    #[serde(default)]
    pub public: bool,
}

/// `POST /_bgh/projects`
pub async fn create(
    State(state): State<AppState>,
    auth: RequireUser,
    Json(body): Json<CreateBody>,
) -> ApiResult<impl IntoResponse> {
    auth.require_scope("project")?;
    let owner_login = body
        .owner
        .ok_or_else(|| ApiError::invalid_field(FieldError::missing_field("Project", "owner")))?;
    let title = validate_text("Project", "title", body.title.as_deref(), 256)?;
    let owner = db::User::find_by_login(&state.db, &owner_login)
        .await?
        .ok_or_else(|| ApiError::invalid_field(FieldError::invalid("Project", "owner")))?;
    match owner_role(&state, Some(&auth), &owner).await? {
        Some(r) if r >= Role::Write => {}
        _ => {
            return Err(ApiError::forbidden(
                "You don't have permission to create projects for this owner.",
            ));
        }
    }
    let scope = owner_scope(owner.id, owner.is_org());
    let mut tx = Tx::begin(&state).await?;
    let number: i64 = sqlx::query_scalar(
        "INSERT INTO project_counters (owner_id, next_number) VALUES ($1, 2)
         ON CONFLICT (owner_id) DO UPDATE SET next_number = project_counters.next_number + 1
         RETURNING next_number - 1",
    )
    .bind(owner.id)
    .fetch_one(&mut *tx)
    .await?;
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO projects (owner_id, number, title, short_description, public, creator_id)
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
    )
    .bind(owner.id)
    .bind(number)
    .bind(&title)
    .bind(body.short_description.as_deref().filter(|s| !s.is_empty()))
    .bind(body.public)
    .bind(auth.user.id)
    .fetch_one(&mut *tx)
    .await?;
    let project = ProjectRow::find(&mut *tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    tx.sync(
        &scope,
        M_PROJECT,
        id,
        SyncAction::Insert,
        &project.sync_json(),
    )
    .await?;
    service::create_defaults(&mut tx, &scope, id).await?;
    bgh_core::audit::log(
        &mut *tx,
        Some(&auth.user),
        "project.create",
        if owner.is_org() {
            bgh_core::audit::Target::Org(owner.id)
        } else {
            bgh_core::audit::Target::User(owner.id)
        },
        json!({"project_id": id, "number": number, "title": title}),
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(project.sync_json())))
}

/// `GET /_bgh/projects/{id}`
pub async fn get(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path(id): Path<i64>,
) -> ApiResult<Json<Value>> {
    let access = ProjectAccess::load(&state, auth.as_ref(), id).await?;
    Ok(Json(snapshot(&state, auth.as_ref(), &access).await?))
}

/// `GET /_bgh/owners/{owner}/projects/{number}`
pub async fn get_by_number(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, number)): Path<(String, i64)>,
) -> ApiResult<Json<Value>> {
    let access = ProjectAccess::load_by_number(&state, auth.as_ref(), &owner, number).await?;
    Ok(Json(snapshot(&state, auth.as_ref(), &access).await?))
}

/// Everything the client needs to render a project.
async fn snapshot(
    state: &AppState,
    auth: Option<&AuthContext>,
    access: &ProjectAccess,
) -> ApiResult<Value> {
    let pid = access.id();
    let fields: Vec<FieldRow> = sqlx::query_as(&format!(
        "SELECT {} FROM project_fields WHERE project_id = $1 ORDER BY position, id",
        FieldRow::COLUMNS
    ))
    .bind(pid)
    .fetch_all(&state.db)
    .await?;
    let views: Vec<ViewRow> = sqlx::query_as(&format!(
        "SELECT {} FROM project_views WHERE project_id = $1 ORDER BY position, id",
        ViewRow::COLUMNS
    ))
    .bind(pid)
    .fetch_all(&state.db)
    .await?;
    let items: Vec<ItemRow> = sqlx::query_as(&format!(
        "{} WHERE i.project_id = $1 ORDER BY i.position, i.id",
        ItemRow::SELECT
    ))
    .bind(pid)
    .fetch_all(&state.db)
    .await?;
    let workflows: Vec<WorkflowRow> = sqlx::query_as(&format!(
        "SELECT {} FROM project_workflows WHERE project_id = $1 ORDER BY id",
        WorkflowRow::COLUMNS
    ))
    .bind(pid)
    .fetch_all(&state.db)
    .await?;
    let issue_ids: Vec<i64> = items.iter().filter_map(|i| i.issue_id).collect();
    let refs = compact::load_refs(state, auth, &issue_ids).await?;
    let mut user_ids: Vec<Option<i64>> = vec![access.project.creator_id];
    for i in &items {
        user_ids.push(i.creator_id);
        user_ids.extend(i.assignee_ids.iter().copied().map(Some));
    }
    user_ids.extend(refs.user_ids.iter().copied().map(Some));
    let users = compact::users_json(state, user_ids).await?;
    let o = &access.owner;
    Ok(json!({
        "project": access.project.sync_json(),
        "owner": {
            "id": o.id,
            "login": o.login,
            "name": o.name,
            "type": o.kind,
            "avatarUrl": state.urls.avatar(o.id, o.avatar_url.as_deref()),
        },
        "role": access.role.as_str(),
        "fields": fields.iter().map(FieldRow::sync_json).collect::<Vec<_>>(),
        "views": views.iter().map(ViewRow::sync_json).collect::<Vec<_>>(),
        "items": items.iter().map(ItemRow::sync_json).collect::<Vec<_>>(),
        "workflows": workflows.iter().map(WorkflowRow::sync_json).collect::<Vec<_>>(),
        "issues": refs.issues,
        "repos": refs.repos,
        "labels": refs.labels,
        "milestones": refs.milestones,
        "users": users,
    }))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateBody {
    pub title: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    pub short_description: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    pub readme: Option<Option<String>>,
    pub public: Option<bool>,
    pub closed: Option<bool>,
}

/// `PATCH /_bgh/projects/{id}`
pub async fn update(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(id): Path<i64>,
    Json(body): Json<UpdateBody>,
) -> ApiResult<Json<Value>> {
    let access = ProjectAccess::load(&state, Some(&auth), id).await?;
    access.require(Role::Write)?;
    if body.public.is_some() || body.closed.is_some() {
        access.require(Role::Admin)?;
    }
    let title = match &body.title {
        Some(t) => Some(validate_text("Project", "title", Some(t), 256)?),
        None => None,
    };
    if let Some(Some(d)) = &body.short_description
        && d.chars().count() > 1024
    {
        return Err(ApiError::invalid_field(FieldError::invalid(
            "Project",
            "shortDescription",
        )));
    }
    let mut tx = Tx::begin(&state).await?;
    sqlx::query(
        "UPDATE projects SET
            title = COALESCE($2, title),
            short_description = CASE WHEN $3 THEN $4 ELSE short_description END,
            readme = CASE WHEN $5 THEN $6 ELSE readme END,
            public = COALESCE($7, public),
            closed_at = CASE WHEN $8::bool IS NULL THEN closed_at
                             WHEN $8 AND NOT closed THEN now()
                             WHEN NOT $8 THEN NULL ELSE closed_at END,
            closed = COALESCE($8, closed),
            updated_at = now()
         WHERE id = $1",
    )
    .bind(id)
    .bind(title)
    .bind(body.short_description.is_some())
    .bind(body.short_description.flatten().filter(|s| !s.is_empty()))
    .bind(body.readme.is_some())
    .bind(body.readme.flatten())
    .bind(body.public)
    .bind(body.closed)
    .execute(&mut *tx)
    .await?;
    let project = ProjectRow::find(&mut *tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    tx.sync(
        &access.scope(),
        M_PROJECT,
        id,
        SyncAction::Update,
        &project.sync_json(),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(project.sync_json()))
}

/// `DELETE /_bgh/projects/{id}`
pub async fn delete(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    let access = ProjectAccess::load(&state, Some(&auth), id).await?;
    access.require(Role::Admin)?;
    let scope = access.scope();
    let mut tx = Tx::begin(&state).await?;
    // Children are removed by cascade; record their deletes so clients that
    // don't cascade locally stay consistent.
    for (model, table) in [
        (M_ITEM, "project_items"),
        (M_VIEW, "project_views"),
        (M_FIELD, "project_fields"),
        (M_WORKFLOW, "project_workflows"),
    ] {
        let ids: Vec<i64> = sqlx::query_scalar(&format!(
            "SELECT id FROM {table} WHERE project_id = $1 ORDER BY id"
        ))
        .bind(id)
        .fetch_all(&mut *tx)
        .await?;
        for mid in ids {
            tx.sync(&scope, model, mid, SyncAction::Delete, &json!({"id": mid}))
                .await?;
        }
    }
    sqlx::query("DELETE FROM projects WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.sync(
        &scope,
        M_PROJECT,
        id,
        SyncAction::Delete,
        &json!({"id": id}),
    )
    .await?;
    bgh_core::audit::log(
        &mut *tx,
        Some(&auth.user),
        "project.delete",
        if access.owner.is_org() {
            bgh_core::audit::Target::Org(access.owner.id)
        } else {
            bgh_core::audit::Target::User(access.owner.id)
        },
        json!({"project_id": id, "title": access.project.title}),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `PUT /_bgh/projects/{id}/repos/{repo_id}`: link a repository (requires
/// write access to it).
pub async fn link_repo(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((id, repo_id)): Path<(i64, i64)>,
) -> ApiResult<Json<Value>> {
    set_link(&state, &auth, id, repo_id, true).await
}

/// `DELETE /_bgh/projects/{id}/repos/{repo_id}`
pub async fn unlink_repo(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((id, repo_id)): Path<(i64, i64)>,
) -> ApiResult<Json<Value>> {
    set_link(&state, &auth, id, repo_id, false).await
}

async fn set_link(
    state: &AppState,
    auth: &AuthContext,
    id: i64,
    repo_id: i64,
    link: bool,
) -> ApiResult<Json<Value>> {
    let access = ProjectAccess::load(state, Some(auth), id).await?;
    access.require(Role::Write)?;
    let repo = db::Repository::find(&state.db, repo_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let owner = db::User::find(&state.db, repo.owner_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let repo_access = RepoAccess::for_repo(state, Some(auth), repo, owner).await?;
    if link {
        repo_access.require(Permission::Write)?;
    }
    let mut tx = Tx::begin(state).await?;
    if link {
        sqlx::query(
            "INSERT INTO project_linked_repos (project_id, repo_id) VALUES ($1, $2)
             ON CONFLICT DO NOTHING",
        )
        .bind(id)
        .bind(repo_id)
        .execute(&mut *tx)
        .await?;
    } else {
        sqlx::query("DELETE FROM project_linked_repos WHERE project_id = $1 AND repo_id = $2")
            .bind(id)
            .bind(repo_id)
            .execute(&mut *tx)
            .await?;
    }
    let project = service::touch_project(&mut tx, &access.scope(), id).await?;
    tx.commit().await?;
    Ok(Json(project.sync_json()))
}
