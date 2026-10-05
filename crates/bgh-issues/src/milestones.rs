//! Milestones CRUD.

use axum::extract::State;
use axum::http::StatusCode;
use bgh_core::error::unique_violation;
use bgh_core::models::api::Milestone;
use bgh_core::perms::RepoAccess;
use bgh_core::prelude::*;
use bgh_core::views;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::issues::{double, parse_since};

async fn find(
    db: impl sqlx::PgExecutor<'_>,
    repo_id: i64,
    number: i64,
) -> ApiResult<db::Milestone> {
    sqlx::query_as::<_, db::Milestone>(&format!(
        "SELECT {} FROM milestones WHERE repo_id = $1 AND number = $2",
        db::Milestone::COLUMNS
    ))
    .bind(repo_id)
    .bind(number)
    .fetch_optional(db)
    .await?
    .ok_or(ApiError::NotFound)
}

async fn render_many(
    state: &AppState,
    access: &RepoAccess,
    rows: Vec<db::Milestone>,
) -> ApiResult<Vec<Milestone>> {
    let users = views::users_by_id(state, rows.iter().map(|m| m.creator_id)).await?;
    Ok(rows
        .iter()
        .map(|m| {
            Milestone::new(
                &state.urls,
                &access.owner.login,
                &access.repo.name,
                m,
                m.creator_id.and_then(|c| users.get(&c)),
            )
        })
        .collect())
}

async fn render(state: &AppState, access: &RepoAccess, m: db::Milestone) -> ApiResult<Milestone> {
    render_many(state, access, vec![m])
        .await?
        .pop()
        .ok_or(ApiError::NotFound)
}

#[derive(Debug, Default, Deserialize)]
pub struct ListQuery {
    pub state: Option<String>,
    pub sort: Option<String>,
    pub direction: Option<String>,
}

/// `GET /repos/{owner}/{repo}/milestones`
pub async fn list(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo)): Path<(String, String)>,
    Query(q): Query<ListQuery>,
) -> ApiResult<Page<Milestone>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let state_filter = match q.state.as_deref().unwrap_or("open") {
        "open" => "AND state = 'open'",
        "closed" => "AND state = 'closed'",
        "all" => "",
        _ => {
            return Err(ApiError::invalid_field(FieldError::invalid(
                "Milestone",
                "state",
            )));
        }
    };
    let dir = match q.direction.as_deref().unwrap_or("asc") {
        "asc" => "ASC",
        "desc" => "DESC",
        _ => {
            return Err(ApiError::invalid_field(FieldError::invalid(
                "Milestone",
                "direction",
            )));
        }
    };
    let order = match q.sort.as_deref().unwrap_or("due_on") {
        "due_on" => format!("due_on {dir} NULLS LAST, number {dir}"),
        "completeness" => format!(
            "(CASE WHEN open_issues + closed_issues = 0 THEN 0
                   ELSE closed_issues::float8 / (open_issues + closed_issues) END) {dir}, number {dir}"
        ),
        _ => {
            return Err(ApiError::invalid_field(FieldError::invalid(
                "Milestone",
                "sort",
            )));
        }
    };
    let rows: Vec<db::Milestone> = sqlx::query_as(&format!(
        "SELECT {} FROM milestones WHERE repo_id = $1 {state_filter} ORDER BY {order}
          LIMIT $2 OFFSET $3",
        db::Milestone::COLUMNS
    ))
    .bind(access.repo.id)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let page = p.page(rows);
    Ok(Page {
        items: render_many(&state, &access, page.items).await?,
        link: page.link,
    })
}

/// `GET /repos/{owner}/{repo}/milestones/{milestone_number}`
pub async fn get(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, number)): Path<(String, String, i64)>,
) -> ApiResult<Json<Milestone>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let m = find(&state.db, access.repo.id, number).await?;
    Ok(Json(render(&state, &access, m).await?))
}

#[derive(Debug, Default, Deserialize)]
pub struct MilestoneBody {
    pub title: Option<String>,
    pub state: Option<String>,
    #[serde(default, deserialize_with = "double")]
    pub description: Option<Option<String>>,
    #[serde(default, deserialize_with = "double")]
    pub due_on: Option<Option<String>>,
}

fn validate_state(s: Option<&str>) -> ApiResult<()> {
    match s {
        None | Some("open") | Some("closed") => Ok(()),
        _ => Err(ApiError::invalid_field(FieldError::invalid(
            "Milestone",
            "state",
        ))),
    }
}

fn parse_due(v: &Option<Option<String>>) -> ApiResult<Option<Option<DateTime<Utc>>>> {
    match v {
        None => Ok(None),
        Some(None) => Ok(Some(None)),
        Some(Some(s)) => parse_since(Some(s))
            .map_err(|_| ApiError::invalid_field(FieldError::invalid("Milestone", "due_on")))
            .map(Some),
    }
}

fn map_unique(e: sqlx::Error) -> ApiError {
    match unique_violation(&e).as_deref() {
        Some("milestones_repo_title_key") => {
            ApiError::invalid_field(FieldError::already_exists("Milestone", "title"))
        }
        _ => e.into(),
    }
}

/// `POST /repos/{owner}/{repo}/milestones`
pub async fn create(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo)): Path<(String, String)>,
    Json(body): Json<MilestoneBody>,
) -> ApiResult<(StatusCode, Json<Milestone>)> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    access.require(Permission::Write)?;
    access.require_not_archived()?;
    let title = body.title.as_deref().map(str::trim).unwrap_or("");
    if title.is_empty() {
        return Err(ApiError::invalid_field(FieldError::missing_field(
            "Milestone",
            "title",
        )));
    }
    validate_state(body.state.as_deref())?;
    let due = parse_due(&body.due_on)?.flatten();
    let st = body.state.as_deref().unwrap_or("open");
    let mut tx = Tx::begin(&state).await?;
    let number: i64 = sqlx::query_scalar(
        "UPDATE repositories SET next_milestone_number = next_milestone_number + 1
          WHERE id = $1 RETURNING next_milestone_number - 1",
    )
    .bind(access.repo.id)
    .fetch_one(&mut *tx)
    .await?;
    let m: db::Milestone = sqlx::query_as(&format!(
        "INSERT INTO milestones (repo_id, number, title, description, state, creator_id, due_on, closed_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, CASE WHEN $5 = 'closed' THEN now() END) RETURNING {}",
        db::Milestone::COLUMNS
    ))
    .bind(access.repo.id)
    .bind(number)
    .bind(title)
    .bind(body.description.clone().flatten())
    .bind(st)
    .bind(auth.user.id)
    .bind(due)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_unique)?;
    tx.sync_model(SyncModel::Milestone, m.id, SyncAction::Insert)
        .await?;
    tx.emit(Event::MilestoneCreated {
        repo_id: access.repo.id,
        milestone_id: m.id,
        actor_id: auth.user.id,
    });
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(render(&state, &access, m).await?)))
}

/// `PATCH /repos/{owner}/{repo}/milestones/{milestone_number}`
pub async fn update(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    Json(body): Json<MilestoneBody>,
) -> ApiResult<Json<Milestone>> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    access.require(Permission::Write)?;
    access.require_not_archived()?;
    let old = find(&state.db, access.repo.id, number).await?;
    validate_state(body.state.as_deref())?;
    let title = match body.title.as_deref().map(str::trim) {
        Some("") => {
            return Err(ApiError::invalid_field(FieldError::missing_field(
                "Milestone",
                "title",
            )));
        }
        Some(t) => t.to_string(),
        None => old.title.clone(),
    };
    let due = match parse_due(&body.due_on)? {
        Some(d) => d,
        None => old.due_on,
    };
    let description = match &body.description {
        Some(d) => d.clone(),
        None => old.description.clone(),
    };
    let st = body.state.clone().unwrap_or_else(|| old.state.clone());
    let mut tx = Tx::begin(&state).await?;
    let m: db::Milestone = sqlx::query_as(&format!(
        "UPDATE milestones SET title = $2, description = $3, state = $4, due_on = $5,
                closed_at = CASE WHEN $4 = 'closed' THEN coalesce(closed_at, now()) END,
                updated_at = now()
          WHERE id = $1 RETURNING {}",
        db::Milestone::COLUMNS
    ))
    .bind(old.id)
    .bind(&title)
    .bind(description.as_deref())
    .bind(&st)
    .bind(due)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_unique)?;
    tx.sync_model(SyncModel::Milestone, m.id, SyncAction::Update)
        .await?;
    let mut changes = serde_json::Map::new();
    if m.title != old.title {
        changes.insert("title".into(), json!({ "from": old.title }));
    }
    if m.description != old.description {
        changes.insert("description".into(), json!({ "from": old.description }));
    }
    if m.due_on != old.due_on {
        changes.insert(
            "due_on".into(),
            json!({ "from": old.due_on.map(Timestamp::from) }),
        );
    }
    let (repo_id, milestone_id, actor_id) = (access.repo.id, m.id, auth.user.id);
    if !changes.is_empty() {
        tx.emit(Event::MilestoneEdited {
            repo_id,
            milestone_id,
            actor_id,
            changes: Value::Object(changes),
        });
    }
    if m.state != old.state {
        tx.emit(if m.state == "closed" {
            Event::MilestoneClosed {
                repo_id,
                milestone_id,
                actor_id,
            }
        } else {
            Event::MilestoneOpened {
                repo_id,
                milestone_id,
                actor_id,
            }
        });
    }
    tx.commit().await?;
    Ok(Json(render(&state, &access, m).await?))
}

/// `DELETE /repos/{owner}/{repo}/milestones/{milestone_number}`
pub async fn delete(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, number)): Path<(String, String, i64)>,
) -> ApiResult<StatusCode> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    access.require(Permission::Write)?;
    access.require_not_archived()?;
    let m = find(&state.db, access.repo.id, number).await?;
    // GitHub REST JSON as it was before deletion (webhook payloads).
    let milestone = serde_json::to_value(render(&state, &access, m.clone()).await?)?;
    let mut tx = Tx::begin(&state).await?;
    let issue_ids: Vec<i64> = sqlx::query_scalar(
        "UPDATE issues SET milestone_id = NULL WHERE milestone_id = $1 RETURNING id",
    )
    .bind(m.id)
    .fetch_all(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM milestones WHERE id = $1")
        .bind(m.id)
        .execute(&mut *tx)
        .await?;
    tx.sync_models(SyncModel::Issue, &issue_ids, SyncAction::Update)
        .await?;
    tx.sync_delete(&access.scope(), SyncModel::Milestone, m.id)
        .await?;
    tx.emit(Event::MilestoneDeleted {
        repo_id: access.repo.id,
        milestone_id: m.id,
        number: m.number,
        title: m.title.clone(),
        actor_id: auth.user.id,
        milestone,
    });
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
