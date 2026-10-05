//! Repository lifecycle (P50): restoring soft-deleted repositories, the
//! retention purge, and transfers to users that wait for acceptance.
//!
//! * `GET /_bgh/repos/deleted[?owner=]`: repositories deleted in the last
//!   90 days that the caller can restore (owned by them or by orgs they
//!   own). `GET /_bgh/admin/repos/deleted`: all of them (site admin).
//! * `POST /_bgh/repos/{id}/restore` → 200 Repository (owner, org owner or
//!   site admin); 422 when the name is taken again.
//! * `GET` / `DELETE /_bgh/repos/{owner}/{repo}/transfer`: the pending
//!   transfer of a repository (repo admins), cancel it.
//! * `GET /_bgh/user/repo_transfers`, `POST
//!   /_bgh/user/repo_transfers/{id}/accept|decline`: incoming requests.
//!
//! Soft delete itself is `bgh_core::lifecycle::soft_delete_repo_in`
//! (`DELETE /repos/{o}/{r}`, admin and account deletion).

use std::collections::HashMap;
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use bgh_core::audit;
use bgh_core::lifecycle::{self, DeletedRepo};
use bgh_core::mail;
use bgh_core::models::api::{Repository, SimpleUser};
use bgh_core::prelude::*;
use bgh_core::time::Timestamp;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::json::full_repo;

pub fn web_routes() -> Router<AppState> {
    Router::new()
        .route("/_bgh/repos/deleted", get(list_deleted))
        .route("/_bgh/admin/repos/deleted", get(admin_list_deleted))
        // `{owner}` is the repository id (the name matches the sibling
        // `/_bgh/repos/{owner}/{repo}` routes, as the router requires).
        .route("/_bgh/repos/{owner}/restore", post(restore))
        .route(
            "/_bgh/repos/{owner}/{repo}/transfer",
            get(pending_transfer).delete(cancel_transfer),
        )
        .route("/_bgh/user/repo_transfers", get(incoming_transfers))
        .route(
            "/_bgh/user/repo_transfers/{id}/accept",
            post(accept_transfer),
        )
        .route(
            "/_bgh/user/repo_transfers/{id}/decline",
            post(decline_transfer),
        )
}

// ----- deleted repositories --------------------------------------------------

#[derive(Debug, Serialize)]
pub struct DeletedOwner {
    pub id: i64,
    pub login: String,
    #[serde(rename = "type")]
    pub kind: String,
}

#[derive(Debug, Serialize)]
pub struct DeletedRepository {
    pub id: i64,
    pub name: String,
    pub full_name: String,
    pub owner: DeletedOwner,
    pub visibility: String,
    pub private: bool,
    pub fork: bool,
    pub deleted_at: Timestamp,
    pub purge_at: Timestamp,
    pub deleted_by: Option<SimpleUser>,
    /// False when the owner is gone or the name is taken again.
    pub restorable: bool,
}

#[derive(Debug, Deserialize)]
pub struct DeletedQuery {
    pub owner: Option<String>,
}

#[derive(sqlx::FromRow)]
struct DeletedRow {
    #[sqlx(flatten)]
    repo: DeletedRepo,
    owner_login_now: Option<String>,
    owner_type: Option<String>,
    name_taken: bool,
}

async fn render_deleted(
    state: &AppState,
    rows: Vec<DeletedRow>,
) -> ApiResult<Vec<DeletedRepository>> {
    let ids: Vec<i64> = rows.iter().filter_map(|r| r.repo.deleted_by_id).collect();
    let users: HashMap<i64, db::User> = db::User::find_many(&state.db, &ids)
        .await?
        .into_iter()
        .map(|u| (u.id, u))
        .collect();
    Ok(rows
        .into_iter()
        .map(|r| {
            let login = r
                .owner_login_now
                .clone()
                .unwrap_or_else(|| r.repo.owner_login.clone());
            DeletedRepository {
                id: r.repo.id,
                full_name: format!("{login}/{}", r.repo.name),
                name: r.repo.name,
                owner: DeletedOwner {
                    id: r.repo.owner_id,
                    login,
                    kind: r.owner_type.clone().unwrap_or_else(|| "User".into()),
                },
                private: r.repo.visibility != "public",
                visibility: r.repo.visibility,
                fork: r.repo.fork,
                deleted_at: r.repo.deleted_at.into(),
                purge_at: r.repo.purge_after.into(),
                deleted_by: r
                    .repo
                    .deleted_by_id
                    .and_then(|id| users.get(&id))
                    .map(|u| SimpleUser::new(&state.urls, u)),
                restorable: r.owner_login_now.is_some() && !r.name_taken,
            }
        })
        .collect())
}

/// `WHERE` clause over `d` (deleted_repositories) is appended by callers.
fn deleted_select() -> String {
    format!(
        "SELECT {}, o.login AS owner_login_now, o.type AS owner_type,
                EXISTS (SELECT 1 FROM repositories r
                         WHERE r.owner_id = d.owner_id AND lower(r.name) = lower(d.name)) AS name_taken
           FROM deleted_repositories d LEFT JOIN users o ON o.id = d.owner_id",
        db::prefixed("d", DeletedRepo::COLUMNS)
    )
}

/// Whether `user` may restore repositories owned by `owner_id`.
async fn can_restore(state: &AppState, user: &db::User, owner_id: i64) -> ApiResult<bool> {
    if user.site_admin || user.id == owner_id {
        return Ok(true);
    }
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM org_members
                         WHERE org_id = $1 AND user_id = $2 AND role = 'admin')",
    )
    .bind(owner_id)
    .bind(user.id)
    .fetch_one(&state.db)
    .await?)
}

/// `GET /_bgh/repos/deleted[?owner=login]`
async fn list_deleted(
    State(state): State<AppState>,
    auth: RequireUser,
    Query(q): Query<DeletedQuery>,
) -> ApiResult<Json<Vec<DeletedRepository>>> {
    let owner_id = match q.owner.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(login) => {
            let owner = lifecycle::resolve_owner(&state.db, login)
                .await?
                .ok_or(ApiError::NotFound)?;
            if !can_restore(&state, &auth.user, owner.id).await? {
                return Err(ApiError::NotFound);
            }
            Some(owner.id)
        }
        None => None,
    };
    let rows: Vec<DeletedRow> = sqlx::query_as(&format!(
        "{} WHERE ($2::bigint IS NULL OR d.owner_id = $2)
            AND (d.owner_id = $1 OR d.owner_id IN (
                   SELECT org_id FROM org_members WHERE user_id = $1 AND role = 'admin'))
          ORDER BY d.deleted_at DESC, d.id DESC LIMIT 500",
        deleted_select()
    ))
    .bind(auth.user.id)
    .bind(owner_id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(render_deleted(&state, rows).await?))
}

/// `GET /_bgh/admin/repos/deleted`
async fn admin_list_deleted(
    State(state): State<AppState>,
    _admin: RequireSiteAdmin,
) -> ApiResult<Json<Vec<DeletedRepository>>> {
    let rows: Vec<DeletedRow> = sqlx::query_as(&format!(
        "{} ORDER BY d.deleted_at DESC, d.id DESC LIMIT 1000",
        deleted_select()
    ))
    .fetch_all(&state.db)
    .await?;
    Ok(Json(render_deleted(&state, rows).await?))
}

/// `POST /_bgh/repos/{id}/restore`
async fn restore(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(id): Path<String>,
) -> ApiResult<Json<Repository>> {
    let id: i64 = id.parse().map_err(|_| ApiError::NotFound)?;
    let deleted = DeletedRepo::find(&state.db, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if !can_restore(&state, &auth.user, deleted.owner_id).await? {
        return Err(ApiError::NotFound);
    }
    let mut tx = Tx::begin(&state).await?;
    let (repo, owner) = lifecycle::restore_repo_in(&mut tx, id).await?;
    audit::log(
        &mut *tx,
        Some(&auth.user),
        "repo.restore",
        audit::Target::Repo {
            id,
            org_id: owner.is_org().then_some(owner.id),
        },
        json!({ "name": format!("{}/{}", owner.login, repo.name) }),
    )
    .await?;
    // The code search index is not part of the snapshot.
    bgh_core::jobs::enqueue(&mut *tx, "search.index_repo", &json!({ "repo_id": id })).await?;
    tx.commit().await?;
    let access = RepoAccess::for_repo(&state, Some(&auth), repo, owner).await?;
    Ok(Json(full_repo(&state, Some(&auth), &access).await?))
}

/// Remove repositories past their retention and expired transfer
/// requests (`bgh_core::lifecycle::purge_expired`).
pub async fn purge_expired(state: &AppState) -> anyhow::Result<usize> {
    lifecycle::purge_expired(state).await
}

/// Service `repos.purge_deleted`: [`purge_expired`] hourly.
pub async fn purge_service(state: AppState, shutdown: CancellationToken) -> anyhow::Result<()> {
    loop {
        match purge_expired(&state).await {
            Ok(n) if n > 0 => tracing::info!(purged = n, "purged deleted repositories"),
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "deleted repository purge failed"),
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(3600)) => {}
            _ = shutdown.cancelled() => return Ok(()),
        }
    }
}

// ----- transfers to users ----------------------------------------------------

#[derive(Debug, Serialize)]
pub struct TransferRepo {
    pub id: i64,
    pub name: String,
    pub full_name: String,
    pub private: bool,
}

#[derive(Debug, Serialize)]
pub struct RepoTransfer {
    pub id: i64,
    pub repository: TransferRepo,
    pub from: SimpleUser,
    pub to: SimpleUser,
    pub new_name: String,
    pub requested_by: Option<SimpleUser>,
    pub created_at: Timestamp,
    pub expires_at: Timestamp,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct TransferRow {
    id: i64,
    repo_id: i64,
    from_owner_id: i64,
    to_user_id: i64,
    new_name: String,
    requested_by_id: Option<i64>,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
}

const TRANSFER_COLUMNS: &str =
    "id, repo_id, from_owner_id, to_user_id, new_name, requested_by_id, created_at, expires_at";

async fn render_transfers(
    state: &AppState,
    rows: Vec<TransferRow>,
) -> ApiResult<Vec<RepoTransfer>> {
    let repo_ids: Vec<i64> = rows.iter().map(|r| r.repo_id).collect();
    let repos: HashMap<i64, db::Repository> = sqlx::query_as::<_, db::Repository>(&format!(
        "SELECT {} FROM repositories WHERE id = ANY($1)",
        db::Repository::COLUMNS
    ))
    .bind(&repo_ids)
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .map(|r| (r.id, r))
    .collect();
    let user_ids: Vec<i64> = rows
        .iter()
        .flat_map(|r| [Some(r.from_owner_id), Some(r.to_user_id), r.requested_by_id])
        .flatten()
        .collect();
    let users: HashMap<i64, db::User> = db::User::find_many(&state.db, &user_ids)
        .await?
        .into_iter()
        .map(|u| (u.id, u))
        .collect();
    let user = |id: i64| SimpleUser::or_ghost(&state.urls, users.get(&id));
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            let repo = repos.get(&r.repo_id)?;
            let from = users.get(&r.from_owner_id)?;
            Some(RepoTransfer {
                id: r.id,
                repository: TransferRepo {
                    id: repo.id,
                    name: repo.name.clone(),
                    full_name: format!("{}/{}", from.login, repo.name),
                    private: repo.visibility != "public",
                },
                from: SimpleUser::new(&state.urls, from),
                to: user(r.to_user_id),
                new_name: r.new_name,
                requested_by: r.requested_by_id.map(user),
                created_at: r.created_at.into(),
                expires_at: r.expires_at.into(),
            })
        })
        .collect())
}

/// Record a pending transfer of `access` to user `to` (from
/// `POST /repos/{o}/{r}/transfer`) and email them the acceptance link.
pub(crate) async fn request_transfer(
    state: &AppState,
    auth: &AuthContext,
    access: &RepoAccess,
    to: &db::User,
    new_name: &str,
) -> ApiResult<()> {
    if db::Repository::find_by_name(&state.db, to.id, new_name)
        .await?
        .is_some()
    {
        return Err(ApiError::invalid_field(FieldError::custom(
            "Repository",
            "name",
            format!("{}/{new_name} already exists", to.login),
        )));
    }
    let mut tx = Tx::begin(state).await?;
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO repo_transfers
            (repo_id, from_owner_id, to_user_id, new_name, requested_by_id, expires_at)
         VALUES ($1, $2, $3, $4, $5, now() + make_interval(hours => $6))
         ON CONFLICT (repo_id) DO UPDATE SET
            from_owner_id = EXCLUDED.from_owner_id, to_user_id = EXCLUDED.to_user_id,
            new_name = EXCLUDED.new_name, requested_by_id = EXCLUDED.requested_by_id,
            created_at = now(), expires_at = EXCLUDED.expires_at
         RETURNING id",
    )
    .bind(access.repo.id)
    .bind(access.owner.id)
    .bind(to.id)
    .bind(new_name)
    .bind(auth.user.id)
    .bind(lifecycle::TRANSFER_TTL_HOURS as i32)
    .fetch_one(&mut *tx)
    .await?;
    audit::log(
        &mut *tx,
        Some(&auth.user),
        "repo.transfer_start",
        audit::Target::Repo {
            id: access.repo.id,
            org_id: access.owner.is_org().then_some(access.owner.id),
        },
        json!({ "from": access.full_name(), "to": format!("{}/{new_name}", to.login) }),
    )
    .await?;
    let email: Option<String> =
        sqlx::query_scalar("SELECT email FROM user_emails WHERE user_id = $1 AND is_primary")
            .bind(to.id)
            .fetch_optional(&mut *tx)
            .await?;
    if let Some(email) = email {
        tx.enqueue(&mail::SendEmail::new(mail::templates::repo_transfer(
            &state.config.site_name,
            &email,
            &to.login,
            &auth.user.login,
            &access.full_name(),
            &state
                .urls
                .html(&format!("/settings/repositories/transfers?id={id}")),
            lifecycle::TRANSFER_TTL_HOURS,
        )))
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// `GET /_bgh/repos/{owner}/{repo}/transfer` (repo admins): 404 when none.
async fn pending_transfer(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<Json<RepoTransfer>> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    access.require(Permission::Admin)?;
    let rows: Vec<TransferRow> = sqlx::query_as(&format!(
        "SELECT {TRANSFER_COLUMNS} FROM repo_transfers WHERE repo_id = $1 AND expires_at > now()"
    ))
    .bind(access.repo.id)
    .fetch_all(&state.db)
    .await?;
    render_transfers(&state, rows)
        .await?
        .pop()
        .map(Json)
        .ok_or(ApiError::NotFound)
}

/// `DELETE /_bgh/repos/{owner}/{repo}/transfer` (repo admins) → 204.
async fn cancel_transfer(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    access.require(Permission::Admin)?;
    let done = sqlx::query("DELETE FROM repo_transfers WHERE repo_id = $1")
        .bind(access.repo.id)
        .execute(&state.db)
        .await?;
    if done.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /_bgh/user/repo_transfers`
async fn incoming_transfers(
    State(state): State<AppState>,
    auth: RequireUser,
) -> ApiResult<Json<Vec<RepoTransfer>>> {
    let rows: Vec<TransferRow> = sqlx::query_as(&format!(
        "SELECT {TRANSFER_COLUMNS} FROM repo_transfers
          WHERE to_user_id = $1 AND expires_at > now() ORDER BY created_at DESC"
    ))
    .bind(auth.user.id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(render_transfers(&state, rows).await?))
}

/// The caller's transfer request `id` (404 otherwise; 410 once expired).
async fn own_transfer(state: &AppState, auth: &AuthContext, id: &str) -> ApiResult<TransferRow> {
    let id: i64 = id.parse().map_err(|_| ApiError::NotFound)?;
    let row: TransferRow = sqlx::query_as(&format!(
        "SELECT {TRANSFER_COLUMNS} FROM repo_transfers WHERE id = $1 AND to_user_id = $2"
    ))
    .bind(id)
    .bind(auth.user.id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)?;
    if row.expires_at <= Utc::now() {
        sqlx::query("DELETE FROM repo_transfers WHERE id = $1")
            .bind(row.id)
            .execute(&state.db)
            .await?;
        return Err(ApiError::Gone("This transfer request has expired.".into()));
    }
    Ok(row)
}

/// `POST /_bgh/user/repo_transfers/{id}/accept` → 200 Repository (moved).
async fn accept_transfer(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(id): Path<String>,
) -> ApiResult<Json<Repository>> {
    let row = own_transfer(&state, &auth, &id).await?;
    let repo = db::Repository::find(&state.db, row.repo_id)
        .await?
        .filter(|r| r.owner_id == row.from_owner_id)
        .ok_or(ApiError::NotFound)?;
    let owner = db::User::find(&state.db, repo.owner_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    // The request was made by an admin of the repository; the recipient
    // acts on it without needing access of their own.
    let source = RepoAccess {
        repo,
        owner,
        permission: Permission::Admin,
        authenticated: true,
    };
    let moved = crate::settings::apply_transfer(
        &state,
        &auth,
        &source,
        auth.user.clone(),
        row.new_name.clone(),
        &[],
    )
    .await?;
    sqlx::query("DELETE FROM repo_transfers WHERE id = $1")
        .bind(row.id)
        .execute(&state.db)
        .await?;
    Ok(Json(full_repo(&state, Some(&auth), &moved).await?))
}

/// `POST /_bgh/user/repo_transfers/{id}/decline` → 204.
async fn decline_transfer(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    let row = own_transfer(&state, &auth, &id).await?;
    sqlx::query("DELETE FROM repo_transfers WHERE id = $1")
        .bind(row.id)
        .execute(&state.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
