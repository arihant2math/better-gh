//! Sub-issues API.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use bgh_core::perms::{self, RepoAccess};
use bgh_core::prelude::*;
use serde::Deserialize;
use serde_json::json;

use crate::json::{self, BodyFormat, IssueOpts, RepoInfo};
use crate::{issues, service};

/// GitHub's limits.
pub const MAX_SUB_ISSUES: i64 = 100;
pub const MAX_DEPTH: i64 = 8;

/// Drop issues whose repository the caller can't read.
async fn readable(
    state: &AppState,
    auth: Option<&AuthContext>,
    rows: Vec<db::Issue>,
) -> ApiResult<(Vec<db::Issue>, json::RepoMap)> {
    let repos = json::load_repos(state, rows.iter().map(|i| i.repo_id)).await?;
    let list: Vec<db::Repository> = repos.values().map(|r| r.repo.clone()).collect();
    let raw = perms::repo_permissions(&state.db, auth.map(|a| a.user.id), &list).await?;
    let rows = rows
        .into_iter()
        .filter(|i| {
            repos.get(&i.repo_id).is_some_and(|r| {
                perms::effective(
                    auth,
                    &r.repo,
                    raw.get(&i.repo_id).copied().unwrap_or(Permission::None),
                ) >= Permission::Read
            })
        })
        .collect();
    Ok((rows, repos))
}

/// `GET /repos/{owner}/{repo}/issues/{issue_number}/sub_issues`
pub async fn list(
    State(state): State<AppState>,
    auth: MaybeUser,
    fmt: BodyFormat,
    p: Pagination,
    Path((owner, repo, number)): Path<(String, String, i64)>,
) -> ApiResult<Page<json::Issue>> {
    let (_, parent) = issues::load(&state, auth.as_ref(), &owner, &repo, number).await?;
    let rows: Vec<db::Issue> = sqlx::query_as(&format!(
        "SELECT {} FROM issues i JOIN sub_issues s ON s.child_id = i.id
          WHERE s.parent_id = $1 ORDER BY s.position, i.id LIMIT $2 OFFSET $3",
        db::prefixed("i", db::Issue::COLUMNS)
    ))
    .bind(parent.id)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let page = p.page(rows);
    let (rows, repos) = readable(&state, auth.as_ref(), page.items).await?;
    Ok(Page {
        items: json::issues(&state, fmt, &rows, &repos, IssueOpts::default()).await?,
        link: page.link,
    })
}

/// `GET /repos/{owner}/{repo}/issues/{issue_number}/parent`
pub async fn parent(
    State(state): State<AppState>,
    auth: MaybeUser,
    fmt: BodyFormat,
    Path((owner, repo, number)): Path<(String, String, i64)>,
) -> ApiResult<Json<json::Issue>> {
    let (_, child) = issues::load(&state, auth.as_ref(), &owner, &repo, number).await?;
    let parent_id: i64 = sqlx::query_scalar("SELECT parent_id FROM sub_issues WHERE child_id = $1")
        .bind(child.id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::NotFound)?;
    let parent = service::issue_by_id(&state.db, parent_id).await?;
    let (rows, repos) = readable(&state, auth.as_ref(), vec![parent]).await?;
    let row = rows.first().ok_or(ApiError::NotFound)?;
    Ok(Json(json::issue(&state, fmt, row, &repos).await?))
}

#[derive(Debug, Default, Deserialize)]
pub struct AddBody {
    pub sub_issue_id: Option<i64>,
    #[serde(default)]
    pub replace_parent: bool,
}

fn ref_json(info: &RepoInfo, issue: &db::Issue) -> serde_json::Value {
    json!({
        "id": issue.id,
        "number": issue.number,
        "repository": format!("{}/{}", info.owner.login, info.repo.name),
    })
}

/// Load the sub-issue by id with its repository; 422 if it doesn't exist,
/// is a PR, or isn't readable by the caller.
async fn load_child(
    state: &AppState,
    auth: &AuthContext,
    id: Option<i64>,
) -> ApiResult<(db::Issue, RepoInfo)> {
    let invalid = || ApiError::invalid_field(FieldError::invalid("Issue", "sub_issue_id"));
    let id = id.ok_or_else(|| {
        ApiError::invalid_field(FieldError::missing_field("Issue", "sub_issue_id"))
    })?;
    let child = service::issue_by_id(&state.db, id)
        .await
        .map_err(|_| invalid())?;
    if child.is_pull_request {
        return Err(ApiError::unprocessable(
            "Pull requests cannot be added as sub-issues",
        ));
    }
    let mut repos = json::load_repos(state, [child.repo_id]).await?;
    let info = repos.remove(&child.repo_id).ok_or_else(invalid)?;
    let access = RepoAccess::for_repo(state, Some(auth), info.repo.clone(), info.owner.clone())
        .await
        .map_err(|_| invalid())?;
    let _ = access;
    Ok((child, info))
}

async fn depth_above(tx: &mut Tx, issue_id: i64) -> ApiResult<(i64, Vec<i64>)> {
    // Ancestors of `issue_id` (nearest first).
    let ancestors: Vec<i64> = sqlx::query_scalar(
        "WITH RECURSIVE up AS (
             SELECT parent_id, 1 AS depth FROM sub_issues WHERE child_id = $1
             UNION ALL
             SELECT s.parent_id, up.depth + 1 FROM sub_issues s JOIN up ON s.child_id = up.parent_id
              WHERE up.depth < 64
         ) SELECT parent_id FROM up ORDER BY depth",
    )
    .bind(issue_id)
    .fetch_all(&mut **tx)
    .await?;
    Ok((ancestors.len() as i64, ancestors))
}

async fn depth_below(tx: &mut Tx, issue_id: i64) -> ApiResult<i64> {
    Ok(sqlx::query_scalar(
        "WITH RECURSIVE down AS (
             SELECT child_id, 1 AS depth FROM sub_issues WHERE parent_id = $1
             UNION ALL
             SELECT s.child_id, down.depth + 1 FROM sub_issues s JOIN down ON s.parent_id = down.child_id
              WHERE down.depth < 64
         ) SELECT coalesce(max(depth), 0)::bigint FROM down",
    )
    .bind(issue_id)
    .fetch_one(&mut **tx)
    .await?)
}

/// Detach `child` from its current parent (events + sync), if any.
async fn detach(
    tx: &mut Tx,
    state: &AppState,
    child: &db::Issue,
    child_info: &RepoInfo,
    actor_id: i64,
) -> ApiResult<()> {
    let old_parent: Option<i64> =
        sqlx::query_scalar("DELETE FROM sub_issues WHERE child_id = $1 RETURNING parent_id")
            .bind(child.id)
            .fetch_optional(&mut **tx)
            .await?;
    let Some(pid) = old_parent else {
        return Ok(());
    };
    let parent = service::issue_by_id(&mut **tx, pid).await?;
    let mut repos = json::load_repos(state, [parent.repo_id]).await?;
    let parent_info = repos.remove(&parent.repo_id).ok_or(ApiError::NotFound)?;
    service::add_event(
        tx,
        &parent,
        Some(actor_id),
        "sub_issue_removed",
        None,
        json!({ "sub_issue": ref_json(child_info, child) }),
    )
    .await?;
    service::add_event(
        tx,
        child,
        Some(actor_id),
        "parent_issue_removed",
        None,
        json!({ "parent_issue": ref_json(&parent_info, &parent) }),
    )
    .await?;
    service::touch_and_sync(tx, parent.id, SyncAction::Update).await?;
    tx.emit(Event::SubIssueRemoved {
        repo_id: parent.repo_id,
        parent_id: parent.id,
        sub_issue_id: child.id,
        actor_id,
    });
    Ok(())
}

/// `POST /repos/{owner}/{repo}/issues/{issue_number}/sub_issues` → 201
/// with the parent issue.
pub async fn add(
    State(state): State<AppState>,
    auth: RequireUser,
    fmt: BodyFormat,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    Json(body): Json<AddBody>,
) -> ApiResult<Response> {
    let (access, parent) = issues::load(&state, Some(&auth), &owner, &repo, number).await?;
    access.require(Permission::Triage)?;
    access.require_not_archived()?;
    if parent.is_pull_request {
        return Err(ApiError::NotFound);
    }
    let (child, child_info) = load_child(&state, &auth, body.sub_issue_id).await?;
    if child.id == parent.id {
        return Err(ApiError::unprocessable(
            "An issue cannot be its own sub-issue",
        ));
    }
    let parent_info = RepoInfo::from_access(&access);
    let mut tx = Tx::begin(&state).await?;
    let parent = service::lock_issue(&mut tx, parent.id).await?;
    let (above, ancestors) = depth_above(&mut tx, parent.id).await?;
    if ancestors.contains(&child.id) {
        return Err(ApiError::unprocessable(
            "Sub-issue cannot be an ancestor of the parent issue",
        ));
    }
    let below = depth_below(&mut tx, child.id).await?;
    if above + 1 + below > MAX_DEPTH {
        return Err(ApiError::unprocessable(format!(
            "Sub-issues can only be nested {MAX_DEPTH} levels deep"
        )));
    }
    let current_parent: Option<i64> =
        sqlx::query_scalar("SELECT parent_id FROM sub_issues WHERE child_id = $1")
            .bind(child.id)
            .fetch_optional(&mut *tx)
            .await?;
    match current_parent {
        Some(p) if p == parent.id => {
            return Err(ApiError::unprocessable(
                "Issue is already a sub-issue of this issue",
            ));
        }
        Some(_) if !body.replace_parent => {
            return Err(ApiError::unprocessable(
                "Sub-issue may only have one parent",
            ));
        }
        Some(_) => detach(&mut tx, &state, &child, &child_info, auth.user.id).await?,
        None => {}
    }
    let (count, max_pos): (i64, Option<f64>) =
        sqlx::query_as("SELECT count(*), max(position) FROM sub_issues WHERE parent_id = $1")
            .bind(parent.id)
            .fetch_one(&mut *tx)
            .await?;
    if count >= MAX_SUB_ISSUES {
        return Err(ApiError::unprocessable(format!(
            "Issues can have at most {MAX_SUB_ISSUES} sub-issues"
        )));
    }
    sqlx::query("INSERT INTO sub_issues (child_id, parent_id, position) VALUES ($1, $2, $3)")
        .bind(child.id)
        .bind(parent.id)
        .bind(max_pos.unwrap_or(0.0) + 1024.0)
        .execute(&mut *tx)
        .await?;
    service::add_event(
        &mut tx,
        &parent,
        Some(auth.user.id),
        "sub_issue_added",
        None,
        json!({ "sub_issue": ref_json(&child_info, &child) }),
    )
    .await?;
    service::add_event(
        &mut tx,
        &child,
        Some(auth.user.id),
        "parent_issue_added",
        None,
        json!({ "parent_issue": ref_json(&parent_info, &parent) }),
    )
    .await?;
    service::touch_and_sync(&mut tx, child.id, SyncAction::Update).await?;
    let parent = service::touch_and_sync(&mut tx, parent.id, SyncAction::Update).await?;
    tx.emit(Event::SubIssueAdded {
        repo_id: parent.repo_id,
        parent_id: parent.id,
        sub_issue_id: child.id,
        actor_id: auth.user.id,
    });
    tx.commit().await?;
    let rendered = json::issue(&state, fmt, &parent, &json::repo_map(&access)).await?;
    Ok((StatusCode::CREATED, Json(rendered)).into_response())
}

#[derive(Debug, Default, Deserialize)]
pub struct RemoveBody {
    pub sub_issue_id: Option<i64>,
}

/// `DELETE /repos/{owner}/{repo}/issues/{issue_number}/sub_issue` → 200
/// with the parent issue.
pub async fn remove(
    State(state): State<AppState>,
    auth: RequireUser,
    fmt: BodyFormat,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    Json(body): Json<RemoveBody>,
) -> ApiResult<Json<json::Issue>> {
    let (access, parent) = issues::load(&state, Some(&auth), &owner, &repo, number).await?;
    access.require(Permission::Triage)?;
    access.require_not_archived()?;
    let id = body.sub_issue_id.ok_or_else(|| {
        ApiError::invalid_field(FieldError::missing_field("Issue", "sub_issue_id"))
    })?;
    let is_child: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM sub_issues WHERE child_id = $1 AND parent_id = $2)",
    )
    .bind(id)
    .bind(parent.id)
    .fetch_one(&state.db)
    .await?;
    if !is_child {
        return Err(ApiError::NotFound);
    }
    let child = service::issue_by_id(&state.db, id).await?;
    let mut repos = json::load_repos(&state, [child.repo_id]).await?;
    let child_info = repos.remove(&child.repo_id).ok_or(ApiError::NotFound)?;
    let mut tx = Tx::begin(&state).await?;
    service::lock_issue(&mut tx, parent.id).await?;
    detach(&mut tx, &state, &child, &child_info, auth.user.id).await?;
    service::touch_and_sync(&mut tx, child.id, SyncAction::Update).await?;
    tx.commit().await?;
    let parent = service::issue_by_id(&state.db, parent.id).await?;
    Ok(Json(
        json::issue(&state, fmt, &parent, &json::repo_map(&access)).await?,
    ))
}

#[derive(Debug, Default, Deserialize)]
pub struct PriorityBody {
    pub sub_issue_id: Option<i64>,
    pub after_id: Option<i64>,
    pub before_id: Option<i64>,
}

/// `PATCH /repos/{owner}/{repo}/issues/{issue_number}/sub_issues/priority`
/// → 200 with the parent issue.
pub async fn reprioritize(
    State(state): State<AppState>,
    auth: RequireUser,
    fmt: BodyFormat,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    Json(body): Json<PriorityBody>,
) -> ApiResult<Json<json::Issue>> {
    let (access, parent) = issues::load(&state, Some(&auth), &owner, &repo, number).await?;
    access.require(Permission::Triage)?;
    access.require_not_archived()?;
    let id = body.sub_issue_id.ok_or_else(|| {
        ApiError::invalid_field(FieldError::missing_field("Issue", "sub_issue_id"))
    })?;
    if body.after_id.is_some() == body.before_id.is_some() {
        return Err(ApiError::unprocessable(
            "Exactly one of after_id or before_id must be provided",
        ));
    }
    let mut tx = Tx::begin(&state).await?;
    service::lock_issue(&mut tx, parent.id).await?;
    let siblings: Vec<(i64, f64)> = sqlx::query_as(
        "SELECT child_id, position FROM sub_issues WHERE parent_id = $1 ORDER BY position, child_id",
    )
    .bind(parent.id)
    .fetch_all(&mut *tx)
    .await?;
    if !siblings.iter().any(|(c, _)| *c == id) {
        return Err(ApiError::NotFound);
    }
    let others: Vec<(i64, f64)> = siblings.into_iter().filter(|(c, _)| *c != id).collect();
    let anchor = body.after_id.or(body.before_id).unwrap_or_default();
    let Some(idx) = others.iter().position(|(c, _)| *c == anchor) else {
        return Err(ApiError::invalid_field(FieldError::invalid(
            "Issue",
            if body.after_id.is_some() {
                "after_id"
            } else {
                "before_id"
            },
        )));
    };
    let new_pos = if body.after_id.is_some() {
        match others.get(idx + 1) {
            Some((_, next)) => (others[idx].1 + next) / 2.0,
            None => others[idx].1 + 1024.0,
        }
    } else {
        match idx.checked_sub(1).and_then(|i| others.get(i)) {
            Some((_, prev)) => (prev + others[idx].1) / 2.0,
            None => others[idx].1 - 1024.0,
        }
    };
    sqlx::query("UPDATE sub_issues SET position = $2 WHERE child_id = $1")
        .bind(id)
        .bind(new_pos)
        .execute(&mut *tx)
        .await?;
    let parent = service::touch_and_sync(&mut tx, parent.id, SyncAction::Update).await?;
    tx.commit().await?;
    Ok(Json(
        json::issue(&state, fmt, &parent, &json::repo_map(&access)).await?,
    ))
}
