//! Labels: repository CRUD, issue labels, milestone labels, and the default
//! label set created for new repositories.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use bgh_core::error::unique_violation;
use bgh_core::models::api::Label;
use bgh_core::perms::RepoAccess;
use bgh_core::prelude::*;
use bgh_core::sync;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::issues::{self, double};
use crate::json;
use crate::service;

/// GitHub's default labels: (name, color, description).
pub const DEFAULT_LABELS: [(&str, &str, &str); 9] = [
    ("bug", "d73a4a", "Something isn't working"),
    (
        "documentation",
        "0075ca",
        "Improvements or additions to documentation",
    ),
    (
        "duplicate",
        "cfd3d7",
        "This issue or pull request already exists",
    ),
    ("enhancement", "a2eeef", "New feature or request"),
    ("good first issue", "7057ff", "Good for newcomers"),
    ("help wanted", "008672", "Extra attention is needed"),
    ("invalid", "e4e669", "This doesn't seem right"),
    ("question", "d876e3", "Further information is requested"),
    ("wontfix", "ffffff", "This will not be worked on"),
];

/// Create the default labels for a repository (idempotent).
pub async fn create_default_labels(state: &AppState, repo_id: i64) -> ApiResult<()> {
    let mut tx = Tx::begin(state).await?;
    for (name, color, description) in DEFAULT_LABELS {
        let label: Option<db::Label> = sqlx::query_as(&format!(
            "INSERT INTO labels (repo_id, name, color, description, is_default)
             VALUES ($1, $2, $3, $4, true) ON CONFLICT DO NOTHING RETURNING {}",
            db::Label::COLUMNS
        ))
        .bind(repo_id)
        .bind(name)
        .bind(color)
        .bind(description)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(l) = label {
            tx.sync(
                &sync::repo_scope(repo_id),
                "label",
                l.id,
                SyncAction::Insert,
                &json::label_sync_json(&l),
            )
            .await?;
        }
    }
    tx.commit().await?;
    Ok(())
}

/// Listener: default labels on [`Event::RepositoryCreated`] (not for
/// forks, which GitHub creates without labels).
pub async fn on_event(state: AppState, event: Arc<Event>) -> anyhow::Result<()> {
    if let Event::RepositoryCreated { repo_id, .. } = &*event {
        let fork: Option<bool> = sqlx::query_scalar("SELECT fork FROM repositories WHERE id = $1")
            .bind(repo_id)
            .fetch_optional(&state.db)
            .await?;
        if fork == Some(false) {
            create_default_labels(&state, *repo_id)
                .await
                .map_err(|e| anyhow::anyhow!("creating default labels: {e}"))?;
        }
    }
    Ok(())
}

async fn find_label(
    db: impl sqlx::PgExecutor<'_>,
    repo_id: i64,
    name: &str,
) -> ApiResult<db::Label> {
    sqlx::query_as::<_, db::Label>(&format!(
        "SELECT {} FROM labels WHERE repo_id = $1 AND lower(name) = lower($2)",
        db::Label::COLUMNS
    ))
    .bind(repo_id)
    .bind(name)
    .fetch_optional(db)
    .await?
    .ok_or(ApiError::NotFound)
}

fn render(access: &RepoAccess, state: &AppState, l: &db::Label) -> Label {
    Label::new(&state.urls, &access.owner.login, &access.repo.name, l)
}

fn map_unique(e: sqlx::Error) -> ApiError {
    match unique_violation(&e).as_deref() {
        Some("labels_repo_name_key") => {
            ApiError::invalid_field(FieldError::already_exists("Label", "name"))
        }
        _ => e.into(),
    }
}

fn validate_name(name: &str) -> ApiResult<()> {
    if name.trim().is_empty() {
        return Err(ApiError::invalid_field(FieldError::missing_field(
            "Label", "name",
        )));
    }
    if name.chars().count() > 50 {
        return Err(ApiError::invalid_field(FieldError::invalid(
            "Label", "name",
        )));
    }
    Ok(())
}

fn validate_color(c: &str) -> ApiResult<String> {
    service::normalize_color(c)
        .ok_or_else(|| ApiError::invalid_field(FieldError::invalid("Label", "color")))
}

fn validate_description(d: Option<&str>) -> ApiResult<()> {
    if d.is_some_and(|d| d.chars().count() > 100) {
        return Err(ApiError::invalid_field(FieldError::invalid(
            "Label",
            "description",
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Repository labels
// ---------------------------------------------------------------------------

/// `GET /repos/{owner}/{repo}/labels`
pub async fn list(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<Page<Label>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let rows: Vec<db::Label> = sqlx::query_as(&format!(
        "SELECT {} FROM labels WHERE repo_id = $1 ORDER BY lower(name), id LIMIT $2 OFFSET $3",
        db::Label::COLUMNS
    ))
    .bind(access.repo.id)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    Ok(p.page(rows).map(|l| render(&access, &state, &l)))
}

/// `GET /repos/{owner}/{repo}/labels/{name}`
pub async fn get(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, name)): Path<(String, String, String)>,
) -> ApiResult<Json<Label>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let l = find_label(&state.db, access.repo.id, &name).await?;
    Ok(Json(render(&access, &state, &l)))
}

#[derive(Debug, Deserialize)]
pub struct CreateLabel {
    pub name: Option<String>,
    pub color: Option<String>,
    pub description: Option<String>,
}

/// `POST /repos/{owner}/{repo}/labels`
pub async fn create(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo)): Path<(String, String)>,
    Json(body): Json<CreateLabel>,
) -> ApiResult<(StatusCode, Json<Label>)> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    access.require(Permission::Write)?;
    access.require_not_archived()?;
    let name = body.name.unwrap_or_default();
    validate_name(&name)?;
    let color = match body.color.as_deref() {
        Some(c) => validate_color(c)?,
        None => "ededed".to_string(),
    };
    validate_description(body.description.as_deref())?;
    let mut tx = Tx::begin(&state).await?;
    let label: db::Label = sqlx::query_as(&format!(
        "INSERT INTO labels (repo_id, name, color, description) VALUES ($1, $2, $3, $4) RETURNING {}",
        db::Label::COLUMNS
    ))
    .bind(access.repo.id)
    .bind(name.trim())
    .bind(&color)
    .bind(body.description.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_unique)?;
    tx.sync(
        &access.scope(),
        "label",
        label.id,
        SyncAction::Insert,
        &json::label_sync_json(&label),
    )
    .await?;
    tx.emit(Event::LabelCreated {
        repo_id: access.repo.id,
        label_id: label.id,
        actor_id: auth.user.id,
    });
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(render(&access, &state, &label))))
}

#[derive(Debug, Deserialize)]
pub struct UpdateLabel {
    pub new_name: Option<String>,
    pub color: Option<String>,
    #[serde(default, deserialize_with = "double")]
    pub description: Option<Option<String>>,
}

/// `PATCH /repos/{owner}/{repo}/labels/{name}`
pub async fn update(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, name)): Path<(String, String, String)>,
    Json(body): Json<UpdateLabel>,
) -> ApiResult<Json<Label>> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    access.require(Permission::Write)?;
    access.require_not_archived()?;
    let old = find_label(&state.db, access.repo.id, &name).await?;
    if let Some(n) = &body.new_name {
        validate_name(n)?;
    }
    let color = body.color.as_deref().map(validate_color).transpose()?;
    if let Some(d) = &body.description {
        validate_description(d.as_deref())?;
    }
    let new_name = body.new_name.as_deref().map(str::trim).unwrap_or(&old.name);
    let description = match &body.description {
        Some(d) => d.clone(),
        None => old.description.clone(),
    };
    let color = color.unwrap_or_else(|| old.color.clone());
    let mut tx = Tx::begin(&state).await?;
    let label: db::Label = sqlx::query_as(&format!(
        "UPDATE labels SET name = $2, color = $3, description = $4, updated_at = now()
          WHERE id = $1 RETURNING {}",
        db::Label::COLUMNS
    ))
    .bind(old.id)
    .bind(new_name)
    .bind(&color)
    .bind(description.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_unique)?;
    tx.sync(
        &access.scope(),
        "label",
        label.id,
        SyncAction::Update,
        &json::label_sync_json(&label),
    )
    .await?;
    let mut changes = serde_json::Map::new();
    if label.name != old.name {
        changes.insert("name".into(), json!({ "from": old.name }));
    }
    if label.color != old.color {
        changes.insert("color".into(), json!({ "from": old.color }));
    }
    if label.description != old.description {
        changes.insert("description".into(), json!({ "from": old.description }));
    }
    tx.emit(Event::LabelEdited {
        repo_id: access.repo.id,
        label_id: label.id,
        actor_id: auth.user.id,
        changes: Value::Object(changes),
    });
    tx.commit().await?;
    Ok(Json(render(&access, &state, &label)))
}

/// `DELETE /repos/{owner}/{repo}/labels/{name}`
pub async fn delete(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, name)): Path<(String, String, String)>,
) -> ApiResult<StatusCode> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    access.require(Permission::Write)?;
    access.require_not_archived()?;
    let label = find_label(&state.db, access.repo.id, &name).await?;
    let mut tx = Tx::begin(&state).await?;
    let issue_ids: Vec<i64> =
        sqlx::query_scalar("DELETE FROM issue_labels WHERE label_id = $1 RETURNING issue_id")
            .bind(label.id)
            .fetch_all(&mut *tx)
            .await?;
    sqlx::query("DELETE FROM labels WHERE id = $1")
        .bind(label.id)
        .execute(&mut *tx)
        .await?;
    tx.sync(
        &access.scope(),
        "label",
        label.id,
        SyncAction::Delete,
        &json!({ "id": label.id }),
    )
    .await?;
    // Issues that carried the label change their label set.
    for id in issue_ids {
        let issue = service::issue_by_id(&mut *tx, id).await?;
        service::sync_issue_row(&mut tx, &issue, SyncAction::Update).await?;
    }
    tx.emit(Event::LabelDeleted {
        repo_id: access.repo.id,
        label_id: label.id,
        name: label.name.clone(),
        actor_id: auth.user.id,
        label: serde_json::to_value(render(&access, &state, &label))?,
    });
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Issue labels
// ---------------------------------------------------------------------------

/// Body of add/set labels: `{"labels": [...]}`, `["a", "b"]` or
/// `[{"name": "a"}]`.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum LabelsBody {
    Object { labels: Option<Vec<Value>> },
    List(Vec<Value>),
}

impl LabelsBody {
    fn names(&self) -> ApiResult<Vec<String>> {
        match self {
            Self::Object { labels } => issues::label_names(labels.as_deref().unwrap_or(&[])),
            Self::List(l) => issues::label_names(l),
        }
    }
}

async fn issue_labels_response(
    state: &AppState,
    access: &RepoAccess,
    issue_id: i64,
) -> ApiResult<Vec<Label>> {
    let labels = json::labels_for_issues(state, &[issue_id]).await?;
    Ok(labels
        .get(&issue_id)
        .map(|ls| ls.iter().map(|l| render(access, state, l)).collect())
        .unwrap_or_default())
}

/// `GET /repos/{owner}/{repo}/issues/{issue_number}/labels`
pub async fn list_for_issue(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo, number)): Path<(String, String, i64)>,
) -> ApiResult<Page<Label>> {
    let (access, issue) = issues::load(&state, auth.as_ref(), &owner, &repo, number).await?;
    let rows: Vec<db::Label> = sqlx::query_as(&format!(
        "SELECT {} FROM labels l JOIN issue_labels il ON il.label_id = l.id
          WHERE il.issue_id = $1 ORDER BY lower(l.name), l.id LIMIT $2 OFFSET $3",
        db::prefixed("l", db::Label::COLUMNS)
    ))
    .bind(issue.id)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    Ok(p.page(rows).map(|l| render(&access, &state, &l)))
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Add,
    Set,
}

async fn change_labels(
    state: &AppState,
    auth: &AuthContext,
    owner: &str,
    repo: &str,
    number: i64,
    names: Option<Vec<String>>,
    mode: Mode,
) -> ApiResult<Vec<Label>> {
    let (access, issue) = issues::load(state, Some(auth), owner, repo, number).await?;
    access.require(Permission::Triage)?;
    access.require_not_archived()?;
    let mut tx = Tx::begin(state).await?;
    let issue = service::lock_issue(&mut tx, issue.id).await?;
    let names = names.unwrap_or_default();
    let labels = service::resolve_labels(&mut tx, access.repo.id, &names, true).await?;
    let changed = match mode {
        Mode::Add => !service::add_labels(&mut tx, &issue, auth.user.id, &labels)
            .await?
            .is_empty(),
        Mode::Set => service::replace_labels(&mut tx, &issue, auth.user.id, &labels).await?,
    };
    if changed {
        service::touch_and_sync(&mut tx, issue.id, SyncAction::Update).await?;
    }
    tx.commit().await?;
    issue_labels_response(state, &access, issue.id).await
}

/// `POST /repos/{owner}/{repo}/issues/{issue_number}/labels`
pub async fn add_to_issue(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    Json(body): Json<LabelsBody>,
) -> ApiResult<Json<Vec<Label>>> {
    let names = body.names()?;
    if names.is_empty() {
        return Err(ApiError::invalid_field(FieldError::missing_field(
            "Label", "labels",
        )));
    }
    Ok(Json(
        change_labels(&state, &auth, &owner, &repo, number, Some(names), Mode::Add).await?,
    ))
}

/// `PUT /repos/{owner}/{repo}/issues/{issue_number}/labels`
pub async fn set_for_issue(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    Json(body): Json<LabelsBody>,
) -> ApiResult<Json<Vec<Label>>> {
    let names = body.names()?;
    Ok(Json(
        change_labels(&state, &auth, &owner, &repo, number, Some(names), Mode::Set).await?,
    ))
}

/// `DELETE /repos/{owner}/{repo}/issues/{issue_number}/labels`
pub async fn remove_all_from_issue(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, number)): Path<(String, String, i64)>,
) -> ApiResult<StatusCode> {
    change_labels(&state, &auth, &owner, &repo, number, None, Mode::Set).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /repos/{owner}/{repo}/issues/{issue_number}/labels/{name}`:
/// 200 with the remaining labels; 404 if the label isn't applied.
pub async fn remove_from_issue(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, number, name)): Path<(String, String, i64, String)>,
) -> ApiResult<Response> {
    let (access, issue) = issues::load(&state, Some(&auth), &owner, &repo, number).await?;
    access.require(Permission::Triage)?;
    access.require_not_archived()?;
    let label = find_label(&state.db, access.repo.id, &name)
        .await
        .map_err(|_| label_not_found())?;
    let mut tx = Tx::begin(&state).await?;
    let issue = service::lock_issue(&mut tx, issue.id).await?;
    let removed = service::remove_labels(&mut tx, &issue, auth.user.id, &[label]).await?;
    if removed.is_empty() {
        return Err(label_not_found());
    }
    service::touch_and_sync(&mut tx, issue.id, SyncAction::Update).await?;
    tx.commit().await?;
    Ok(Json(issue_labels_response(&state, &access, issue.id).await?).into_response())
}

fn label_not_found() -> ApiError {
    ApiError::Status(StatusCode::NOT_FOUND, "Label does not exist".into())
}

/// `GET /repos/{owner}/{repo}/milestones/{milestone_number}/labels`:
/// labels of the issues in a milestone.
pub async fn list_for_milestone(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo, number)): Path<(String, String, i64)>,
) -> ApiResult<Page<Label>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let milestone_id: i64 =
        sqlx::query_scalar("SELECT id FROM milestones WHERE repo_id = $1 AND number = $2")
            .bind(access.repo.id)
            .bind(number)
            .fetch_optional(&state.db)
            .await?
            .ok_or(ApiError::NotFound)?;
    let rows: Vec<db::Label> = sqlx::query_as(&format!(
        "SELECT {} FROM labels l WHERE l.id IN (
             SELECT il.label_id FROM issue_labels il JOIN issues i ON i.id = il.issue_id
              WHERE i.milestone_id = $1)
          ORDER BY lower(l.name), l.id LIMIT $2 OFFSET $3",
        db::prefixed("l", db::Label::COLUMNS)
    ))
    .bind(milestone_id)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    Ok(p.page(rows).map(|l| render(&access, &state, &l)))
}
