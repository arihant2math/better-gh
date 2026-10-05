//! Admin UI: repository management (`/_bgh/admin/repos`): search, details,
//! rename / visibility / archive / disable, transfer, delete.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use bgh_core::prelude::*;
use bgh_core::time::ts;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::FromRow;

use crate::common::{self, direction, like_escape, log, repo_target};
use crate::service;

/// `(id, text, text, created, optional time)` rows.
type IdNameTimes = (i64, String, String, DateTime<Utc>, Option<DateTime<Utc>>);

#[derive(Debug, FromRow)]
struct RepoRow {
    #[sqlx(flatten)]
    repo: db::Repository,
    owner_login: String,
    owner_type: String,
}

#[derive(Debug, Serialize)]
pub struct AdminOwner {
    pub id: i64,
    pub login: String,
    #[serde(rename = "type")]
    pub kind: String,
}

/// Repository row of the admin UI.
#[derive(Debug, Serialize)]
pub struct AdminRepo {
    pub id: i64,
    pub name: String,
    pub full_name: String,
    pub owner: AdminOwner,
    pub description: Option<String>,
    pub visibility: String,
    pub private: bool,
    pub fork: bool,
    pub archived: bool,
    pub disabled: bool,
    pub default_branch: String,
    pub language: Option<String>,
    /// KB, like GitHub.
    pub size: i64,
    pub stargazers_count: i64,
    pub forks_count: i64,
    pub open_issues_count: i64,
    pub pushed_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub url: String,
    pub html_url: String,
}

fn render(state: &AppState, r: &db::Repository, owner_login: &str, owner_type: &str) -> AdminRepo {
    AdminRepo {
        id: r.id,
        name: r.name.clone(),
        full_name: format!("{owner_login}/{}", r.name),
        owner: AdminOwner {
            id: r.owner_id,
            login: owner_login.to_string(),
            kind: owner_type.to_string(),
        },
        description: r.description.clone(),
        visibility: r.visibility.clone(),
        private: r.is_private(),
        fork: r.fork,
        archived: r.archived,
        disabled: r.disabled,
        default_branch: r.default_branch.clone(),
        language: r.language.clone(),
        size: r.size,
        stargazers_count: r.stargazers_count,
        forks_count: r.forks_count,
        open_issues_count: r.open_issues_count,
        pushed_at: ts(r.pushed_at),
        created_at: r.created_at.into(),
        updated_at: r.updated_at.into(),
        url: state.urls.repo(owner_login, &r.name),
        html_url: state.urls.repo_html(owner_login, &r.name),
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct ListParams {
    /// Substring of `owner/name`.
    pub q: Option<String>,
    pub owner: Option<String>,
    /// `public` | `private` | `internal`
    pub visibility: Option<String>,
    pub archived: Option<bool>,
    pub disabled: Option<bool>,
    pub fork: Option<bool>,
    /// `name` (default) | `created` | `updated` | `pushed` | `size` | `stars`
    pub sort: Option<String>,
    pub direction: Option<String>,
}

/// `GET /_bgh/admin/repos`
pub async fn list(
    State(state): State<AppState>,
    _auth: RequireSiteAdmin,
    p: Pagination,
    Query(q): Query<ListParams>,
) -> ApiResult<Page<AdminRepo>> {
    let (order, default_desc) = match q.sort.as_deref() {
        None | Some("name") => ("lower(o.login), lower(r.name)", false),
        Some("created") => ("r.created_at", true),
        Some("updated") => ("r.updated_at", true),
        Some("pushed") => ("r.pushed_at", true),
        Some("size") => ("r.size", true),
        Some("stars") => ("r.stargazers_count", true),
        Some(_) => {
            return Err(ApiError::invalid_field(FieldError::invalid(
                "Repository",
                "sort",
            )));
        }
    };
    if let Some(v) = &q.visibility
        && !matches!(v.as_str(), "public" | "private" | "internal")
    {
        return Err(ApiError::invalid_field(FieldError::invalid(
            "Repository",
            "visibility",
        )));
    }
    let dir = direction(q.direction.as_deref(), default_desc);
    let order = order
        .split(", ")
        .map(|c| format!("{c} {dir} NULLS LAST"))
        .collect::<Vec<_>>()
        .join(", ");
    let pattern =
        q.q.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| format!("%{}%", like_escape(&s.to_lowercase())));
    let filter = "WHERE ($1::text IS NULL OR lower(o.login || '/' || r.name) LIKE $1)
                    AND ($2::text IS NULL OR lower(o.login) = lower($2))
                    AND ($3::text IS NULL OR r.visibility = $3)
                    AND ($4::bool IS NULL OR r.archived = $4)
                    AND ($5::bool IS NULL OR r.disabled = $5)
                    AND ($6::bool IS NULL OR r.fork = $6)";
    let total: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM repositories r JOIN users o ON o.id = r.owner_id {filter}"
    ))
    .bind(&pattern)
    .bind(&q.owner)
    .bind(&q.visibility)
    .bind(q.archived)
    .bind(q.disabled)
    .bind(q.fork)
    .fetch_one(&state.db)
    .await?;
    let rows: Vec<RepoRow> = sqlx::query_as(&format!(
        "SELECT {}, o.login AS owner_login, o.type AS owner_type
           FROM repositories r JOIN users o ON o.id = r.owner_id {filter}
          ORDER BY {order}, r.id {dir} LIMIT $7 OFFSET $8",
        db::prefixed("r", db::Repository::COLUMNS)
    ))
    .bind(&pattern)
    .bind(&q.owner)
    .bind(&q.visibility)
    .bind(q.archived)
    .bind(q.disabled)
    .bind(q.fork)
    .bind(p.limit())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    Ok(p.page_with_total(rows, total)
        .map(|r| render(&state, &r.repo, &r.owner_login, &r.owner_type)))
}

/// `GET /_bgh/admin/repos/{owner}/{repo}`: repository, live disk usage,
/// collaborator/team counts, recent maintenance runs.
pub async fn get(
    State(state): State<AppState>,
    _auth: RequireSiteAdmin,
    Path((owner, name)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let (owner, repo) = common::repo(&state, &owner, &name).await?;
    let counts: (i64, i64, i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM collaborators WHERE repo_id = $1),
                (SELECT count(*) FROM team_repos WHERE repo_id = $1),
                (SELECT count(*) FROM issues WHERE repo_id = $1 AND NOT is_pull_request),
                (SELECT count(*) FROM issues WHERE repo_id = $1 AND is_pull_request),
                (SELECT count(*) FROM webhooks WHERE repo_id = $1)",
    )
    .bind(repo.id)
    .fetch_one(&state.db)
    .await?;
    let runs: Vec<IdNameTimes> = sqlx::query_as(
        "SELECT id, operation, status, created_at, finished_at FROM repo_maintenance_runs
          WHERE repo_id = $1 ORDER BY id DESC LIMIT 10",
    )
    .bind(repo.id)
    .fetch_all(&state.db)
    .await?;
    let store = bgh_repos::store(&state);
    let disk_kb = if store.exists(repo.id) {
        Some(store.disk_size_kb(repo.id).await?)
    } else {
        None
    };
    let parent: Option<String> = match repo.parent_id {
        Some(id) => sqlx::query_scalar(
            "SELECT o.login || '/' || r.name FROM repositories r JOIN users o ON o.id = r.owner_id WHERE r.id = $1",
        )
        .bind(id)
        .fetch_optional(&state.db)
        .await?,
        None => None,
    };
    Ok(Json(json!({
        "repository": render(&state, &repo, &owner.login, &owner.kind),
        "parent": parent,
        "storage": {
            "path": store.path(repo.id).display().to_string(),
            "exists": disk_kb.is_some(),
            "disk_usage_kb": disk_kb,
        },
        "collaborators_count": counts.0,
        "teams_count": counts.1,
        "issues_count": counts.2,
        "pulls_count": counts.3,
        "hooks_count": counts.4,
        "maintenance": runs.into_iter().map(|(id, op, status, created, finished)| json!({
            "id": id, "operation": op, "status": status,
            "created_at": Timestamp::from(created), "finished_at": ts(finished),
        })).collect::<Vec<_>>(),
    })))
}

#[derive(Debug, Default, Deserialize)]
pub struct UpdateBody {
    pub name: Option<String>,
    pub visibility: Option<String>,
    pub archived: Option<bool>,
    /// Disabled repositories are hidden from everyone but site admins
    /// (GitHub's "disabled" state, e.g. for abuse or legal holds).
    pub disabled: Option<bool>,
}

/// `PATCH /_bgh/admin/repos/{owner}/{repo}`
pub async fn update(
    State(state): State<AppState>,
    auth: RequireSiteAdmin,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
    Json(body): Json<UpdateBody>,
) -> ApiResult<Json<AdminRepo>> {
    let (owner, repo) = common::repo(&state, &owner, &name).await?;
    let new_name = body
        .name
        .as_deref()
        .map(str::trim)
        .unwrap_or(&repo.name)
        .to_string();
    if !bgh_repos::create::is_valid_repo_name(&new_name) {
        return Err(ApiError::invalid_field(FieldError::invalid(
            "Repository",
            "name",
        )));
    }
    let visibility = body
        .visibility
        .clone()
        .unwrap_or_else(|| repo.visibility.clone());
    match visibility.as_str() {
        "public" | "private" => {}
        "internal" if owner.is_org() => {}
        _ => {
            return Err(ApiError::invalid_field(FieldError::invalid(
                "Repository",
                "visibility",
            )));
        }
    }
    let archived = body.archived.unwrap_or(repo.archived);
    let disabled = body.disabled.unwrap_or(repo.disabled);

    let mut tx = Tx::begin(&state).await?;
    let updated: db::Repository = sqlx::query_as(&format!(
        "UPDATE repositories SET name = $2, visibility = $3, archived = $4, disabled = $5,
                updated_at = now()
          WHERE id = $1 RETURNING {}",
        db::Repository::COLUMNS
    ))
    .bind(repo.id)
    .bind(&new_name)
    .bind(&visibility)
    .bind(archived)
    .bind(disabled)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| match bgh_core::db::unique_violation(&e).as_deref() {
        Some("repositories_owner_name_key") => {
            ApiError::invalid_field(FieldError::already_exists("Repository", "name"))
        }
        _ => e.into(),
    })?;
    let target = repo_target(&owner, repo.id);
    let full = format!("{}/{}", owner.login, updated.name);
    if updated.name != repo.name {
        log(
            &mut tx,
            &auth,
            &headers,
            "repo.rename",
            target,
            json!({ "repo": full, "old_name": repo.name }),
        )
        .await?;
    }
    if updated.visibility != repo.visibility {
        log(&mut tx, &auth, &headers, "repo.access", target,
            json!({ "repo": full, "visibility": updated.visibility, "previous_visibility": repo.visibility }))
            .await?;
    }
    if updated.archived != repo.archived {
        let action = if updated.archived {
            "repo.archived"
        } else {
            "repo.unarchived"
        };
        log(
            &mut tx,
            &auth,
            &headers,
            action,
            target,
            json!({ "repo": full }),
        )
        .await?;
    }
    if updated.disabled != repo.disabled {
        let action = if updated.disabled {
            "repo.disable"
        } else {
            "repo.enable"
        };
        log(
            &mut tx,
            &auth,
            &headers,
            action,
            target,
            json!({ "repo": full }),
        )
        .await?;
    }
    tx.sync_model(SyncModel::Repo, updated.id, SyncAction::Update)
        .await?;
    tx.emit(Event::RepositoryUpdated {
        repo_id: updated.id,
        actor_id: auth.user.id,
    });
    tx.commit().await?;
    Ok(Json(render(&state, &updated, &owner.login, &owner.kind)))
}

#[derive(Debug, Default, Deserialize)]
pub struct TransferBody {
    #[serde(default)]
    pub new_owner: String,
    pub new_name: Option<String>,
}

/// `POST /_bgh/admin/repos/{owner}/{repo}/transfer` → 200 repository.
pub async fn transfer(
    State(state): State<AppState>,
    auth: RequireSiteAdmin,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
    Json(body): Json<TransferBody>,
) -> ApiResult<Json<AdminRepo>> {
    let (owner, repo) = common::repo(&state, &owner, &name).await?;
    let new_owner = db::User::find_by_login(&state.db, body.new_owner.trim())
        .await?
        .filter(|u| u.kind != "Bot")
        .ok_or_else(|| ApiError::invalid_field(FieldError::invalid("Repository", "new_owner")))?;
    let new_name = body
        .new_name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty());
    if let Some(n) = new_name
        && !bgh_repos::create::is_valid_repo_name(n)
    {
        return Err(ApiError::invalid_field(FieldError::invalid(
            "Repository",
            "new_name",
        )));
    }
    let mut tx = Tx::begin(&state).await?;
    let moved = service::transfer_repo_in(&mut tx, &repo, &new_owner, new_name).await?;
    // Logged against both organizations so each org's audit log shows it.
    log(
        &mut tx,
        &auth,
        &headers,
        "repo.transfer",
        repo_target(&new_owner, repo.id),
        json!({ "from": format!("{}/{}", owner.login, repo.name), "to": format!("{}/{}", new_owner.login, moved.name) }),
    )
    .await?;
    if owner.is_org() && owner.id != new_owner.id {
        log(
            &mut tx,
            &auth,
            &headers,
            "repo.transfer_outgoing",
            repo_target(&owner, repo.id),
            json!({ "from": format!("{}/{}", owner.login, repo.name), "to": format!("{}/{}", new_owner.login, moved.name) }),
        )
        .await?;
    }
    tx.emit(Event::RepositoryUpdated {
        repo_id: moved.id,
        actor_id: auth.user.id,
    });
    tx.commit().await?;
    Ok(Json(render(
        &state,
        &moved,
        &new_owner.login,
        &new_owner.kind,
    )))
}

/// `DELETE /_bgh/admin/repos/{owner}/{repo}` → 204.
pub async fn delete(
    State(state): State<AppState>,
    auth: RequireSiteAdmin,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    let (owner, repo) = common::repo(&state, &owner, &name).await?;
    let mut tx = Tx::begin(&state).await?;
    service::delete_repo_in(&mut tx, &auth, &owner, &repo).await?;
    log(
        &mut tx,
        &auth,
        &headers,
        "repo.destroy",
        repo_target(&owner, repo.id),
        json!({ "name": format!("{}/{}", owner.login, repo.name), "by_site_admin": true }),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
