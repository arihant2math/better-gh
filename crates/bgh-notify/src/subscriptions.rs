//! Thread and repository subscriptions.
//!
//! * `GET/PUT/DELETE /notifications/threads/{id}/subscription`
//! * `GET/PUT/DELETE /repos/{owner}/{repo}/subscription` (watching):
//!   a `watches` row with `subscribed` = watching (all activity),
//!   `ignored` = never notified; no row = participating/@mentions only.
//! * `GET/PUT/DELETE /_bgh/repos/{owner}/{repo}/issues/{number}/subscription`
//!   (web client's subscribe button; reason `manual`).

use axum::extract::State;
use axum::http::StatusCode;
use bgh_core::prelude::*;
use bgh_core::time::ts;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::threads::{self, require_scope};

/// `thread_subscriptions` row.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ThreadSubRow {
    pub user_id: i64,
    pub subject_type: String,
    pub subject_id: i64,
    pub repo_id: i64,
    pub subscribed: bool,
    pub ignored: bool,
    pub reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl ThreadSubRow {
    pub const COLUMNS: &'static str =
        "user_id, subject_type, subject_id, repo_id, subscribed, ignored, reason, created_at";
}

/// Upsert a user's subscription to a thread (explicit user action: the
/// values given replace the current ones; `reason` is kept if set).
#[allow(clippy::too_many_arguments)]
pub async fn set_thread_subscription(
    conn: &mut sqlx::PgConnection,
    user_id: i64,
    subject_type: &str,
    subject_id: i64,
    repo_id: i64,
    subscribed: bool,
    ignored: bool,
    reason: Option<&str>,
) -> ApiResult<ThreadSubRow> {
    Ok(sqlx::query_as(&format!(
        "INSERT INTO thread_subscriptions
                (user_id, subject_type, subject_id, repo_id, subscribed, ignored, reason)
         VALUES ($1, $2, $3, $4, $5, $6, $7)
         ON CONFLICT (user_id, subject_type, subject_id) DO UPDATE
            SET subscribed = EXCLUDED.subscribed, ignored = EXCLUDED.ignored,
                reason = coalesce(EXCLUDED.reason, thread_subscriptions.reason)
         RETURNING {}",
        ThreadSubRow::COLUMNS
    ))
    .bind(user_id)
    .bind(subject_type)
    .bind(subject_id)
    .bind(repo_id)
    .bind(subscribed)
    .bind(ignored)
    .bind(reason)
    .fetch_one(conn)
    .await?)
}

async fn thread_sub(
    state: &AppState,
    user_id: i64,
    subject_type: &str,
    subject_id: i64,
) -> ApiResult<Option<ThreadSubRow>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {} FROM thread_subscriptions
          WHERE user_id = $1 AND subject_type = $2 AND subject_id = $3",
        ThreadSubRow::COLUMNS
    ))
    .bind(user_id)
    .bind(subject_type)
    .bind(subject_id)
    .fetch_optional(&state.db)
    .await?)
}

/// `watches` row of a user for a repository.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct WatchRow {
    pub subscribed: bool,
    pub ignored: bool,
    pub created_at: DateTime<Utc>,
}

pub async fn watch_row(
    state: &AppState,
    user_id: i64,
    repo_id: i64,
) -> ApiResult<Option<WatchRow>> {
    Ok(sqlx::query_as(
        "SELECT subscribed, ignored, created_at FROM watches WHERE user_id = $1 AND repo_id = $2",
    )
    .bind(user_id)
    .bind(repo_id)
    .fetch_optional(&state.db)
    .await?)
}

// ---------------------------------------------------------------------------
// Thread subscriptions (GitHub REST)
// ---------------------------------------------------------------------------

/// GitHub `thread-subscription`.
#[derive(Debug, Clone, Serialize)]
pub struct ThreadSubscription {
    pub subscribed: bool,
    pub ignored: bool,
    pub reason: Option<String>,
    pub created_at: Option<Timestamp>,
    pub url: String,
    pub thread_url: String,
}

fn thread_subscription_json(
    state: &AppState,
    thread_id: i64,
    sub: Option<&ThreadSubRow>,
    watching: Option<&WatchRow>,
) -> ThreadSubscription {
    let thread_url = threads::thread_url(state, thread_id);
    let (subscribed, ignored, reason, created_at) = match (sub, watching) {
        (Some(s), _) => (
            s.subscribed,
            s.ignored,
            s.reason.clone(),
            Some(s.created_at),
        ),
        (None, Some(w)) => (w.subscribed, w.ignored, None, Some(w.created_at)),
        (None, None) => (false, false, None, None),
    };
    ThreadSubscription {
        subscribed,
        ignored,
        reason,
        created_at: ts(created_at),
        url: format!("{thread_url}/subscription"),
        thread_url,
    }
}

/// `GET /notifications/threads/{thread_id}/subscription`
pub async fn get_thread_subscription(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(id): Path<String>,
) -> ApiResult<Json<ThreadSubscription>> {
    require_scope(&auth)?;
    let n = threads::load_thread(&state, auth.id(), &id).await?;
    let sub = thread_sub(&state, auth.id(), &n.subject_type, n.subject_id).await?;
    let watching = watch_row(&state, auth.id(), n.repo_id).await?;
    Ok(Json(thread_subscription_json(
        &state,
        n.id,
        sub.as_ref(),
        watching.as_ref(),
    )))
}

#[derive(Debug, Default, Deserialize)]
pub struct ThreadSubBody {
    pub ignored: Option<bool>,
}

/// `PUT /notifications/threads/{thread_id}/subscription` — subscribe, or
/// mute with `{"ignored": true}`.
pub async fn set_thread_subscription_handler(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(id): Path<String>,
    body: bytes::Bytes,
) -> ApiResult<Json<ThreadSubscription>> {
    require_scope(&auth)?;
    let n = threads::load_thread(&state, auth.id(), &id).await?;
    let body: ThreadSubBody = crate::optional_json(&body)?;
    let ignored = body.ignored.unwrap_or(false);
    let mut tx = Tx::begin(&state).await?;
    let row = set_thread_subscription(
        &mut tx,
        auth.id(),
        &n.subject_type,
        n.subject_id,
        n.repo_id,
        !ignored,
        ignored,
        Some("manual"),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(thread_subscription_json(
        &state,
        n.id,
        Some(&row),
        None,
    )))
}

/// `DELETE /notifications/threads/{thread_id}/subscription` — stop
/// receiving notifications for the thread until participating again.
pub async fn delete_thread_subscription(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    require_scope(&auth)?;
    let n = threads::load_thread(&state, auth.id(), &id).await?;
    let mut tx = Tx::begin(&state).await?;
    set_thread_subscription(
        &mut tx,
        auth.id(),
        &n.subject_type,
        n.subject_id,
        n.repo_id,
        false,
        false,
        None,
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Repository subscription (watching)
// ---------------------------------------------------------------------------

/// GitHub `repository-subscription`.
#[derive(Debug, Clone, Serialize)]
pub struct RepositorySubscription {
    pub subscribed: bool,
    pub ignored: bool,
    pub reason: Option<String>,
    pub created_at: Timestamp,
    pub url: String,
    pub repository_url: String,
}

fn repo_subscription_json(
    access: &RepoAccess,
    state: &AppState,
    w: &WatchRow,
) -> RepositorySubscription {
    let repository_url = state.urls.repo(&access.owner.login, &access.repo.name);
    RepositorySubscription {
        subscribed: w.subscribed,
        ignored: w.ignored,
        reason: None,
        created_at: w.created_at.into(),
        url: format!("{repository_url}/subscription"),
        repository_url,
    }
}

/// `participating` | `subscribed` | `ignored` (client `viewerRepo.watching`).
pub fn watching_state(w: Option<(bool, bool)>) -> &'static str {
    match w {
        Some((_, true)) => "ignored",
        Some((true, false)) => "subscribed",
        _ => "participating",
    }
}

/// `GET /repos/{owner}/{repo}/subscription` — 404 when not watching.
pub async fn get_repo_subscription(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<Json<RepositorySubscription>> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    let w = watch_row(&state, auth.id(), access.repo.id)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(repo_subscription_json(&access, &state, &w)))
}

#[derive(Debug, Default, Deserialize)]
pub struct RepoSubBody {
    pub subscribed: Option<bool>,
    pub ignored: Option<bool>,
}

/// Change a user's watch state for a repository, maintaining
/// `repositories.watchers_count` and the viewer's `viewerRepo` sync row.
/// `None` removes the watch (participating only).
pub async fn set_watch(
    state: &AppState,
    user_id: i64,
    repo_id: i64,
    new: Option<(bool, bool)>,
) -> ApiResult<Option<WatchRow>> {
    let mut tx = Tx::begin(state).await?;
    let old: Option<bool> = sqlx::query_scalar(
        "SELECT subscribed FROM watches WHERE user_id = $1 AND repo_id = $2 FOR UPDATE",
    )
    .bind(user_id)
    .bind(repo_id)
    .fetch_optional(&mut *tx)
    .await?;
    let row: Option<WatchRow> = match new {
        Some((subscribed, ignored)) => Some(
            sqlx::query_as(
                "INSERT INTO watches (user_id, repo_id, subscribed, ignored)
                 VALUES ($1, $2, $3, $4)
                 ON CONFLICT (user_id, repo_id) DO UPDATE
                    SET subscribed = EXCLUDED.subscribed, ignored = EXCLUDED.ignored
                 RETURNING subscribed, ignored, created_at",
            )
            .bind(user_id)
            .bind(repo_id)
            .bind(subscribed)
            .bind(ignored)
            .fetch_one(&mut *tx)
            .await?,
        ),
        None => {
            sqlx::query("DELETE FROM watches WHERE user_id = $1 AND repo_id = $2")
                .bind(user_id)
                .bind(repo_id)
                .execute(&mut *tx)
                .await?;
            None
        }
    };
    let delta =
        i64::from(row.as_ref().is_some_and(|r| r.subscribed)) - i64::from(old.unwrap_or(false));
    if delta != 0 {
        sqlx::query(
            "UPDATE repositories SET watchers_count = greatest(watchers_count + $2, 0) WHERE id = $1",
        )
        .bind(repo_id)
        .bind(delta)
        .execute(&mut *tx)
        .await?;
        tx.sync_model(SyncModel::Repo, repo_id, SyncAction::Update)
            .await?;
    }
    tx.sync_viewer_repo(user_id, repo_id).await?;
    tx.commit().await?;
    Ok(row)
}

/// `PUT /repos/{owner}/{repo}/subscription` — `{"subscribed": true}` to
/// watch, `{"ignored": true}` to ignore.
pub async fn set_repo_subscription(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo)): Path<(String, String)>,
    body: bytes::Bytes,
) -> ApiResult<Json<RepositorySubscription>> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    let body: RepoSubBody = crate::optional_json(&body)?;
    let ignored = body.ignored.unwrap_or(false);
    let subscribed = !ignored && body.subscribed.unwrap_or(true);
    let row = set_watch(
        &state,
        auth.id(),
        access.repo.id,
        Some((subscribed, ignored)),
    )
    .await?
    .ok_or(ApiError::NotFound)?;
    Ok(Json(repo_subscription_json(&access, &state, &row)))
}

/// `DELETE /repos/{owner}/{repo}/subscription` — stop watching (204).
pub async fn delete_repo_subscription(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    set_watch(&state, auth.id(), access.repo.id, None).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Issue / PR subscription (web client)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct IssueSubscription {
    pub subscribed: bool,
    pub ignored: bool,
    pub reason: Option<String>,
    /// `participating` | `subscribed` | `ignored` for the repository.
    pub repository_watching: &'static str,
}

async fn issue_subject(
    state: &AppState,
    access: &RepoAccess,
    number: i64,
) -> ApiResult<(&'static str, i64)> {
    let row: Option<(i64, bool)> =
        sqlx::query_as("SELECT id, is_pull_request FROM issues WHERE repo_id = $1 AND number = $2")
            .bind(access.repo.id)
            .bind(number)
            .fetch_optional(&state.db)
            .await?;
    let (id, is_pr) = row.ok_or(ApiError::NotFound)?;
    Ok((if is_pr { "PullRequest" } else { "Issue" }, id))
}

async fn issue_subscription_json(
    state: &AppState,
    user_id: i64,
    repo_id: i64,
    sub: Option<ThreadSubRow>,
) -> ApiResult<IssueSubscription> {
    let w = watch_row(state, user_id, repo_id).await?;
    let watching = watching_state(w.as_ref().map(|w| (w.subscribed, w.ignored)));
    Ok(match sub {
        Some(s) => IssueSubscription {
            subscribed: s.subscribed,
            ignored: s.ignored,
            reason: s.reason,
            repository_watching: watching,
        },
        None => IssueSubscription {
            subscribed: watching == "subscribed",
            ignored: watching == "ignored",
            reason: (watching == "subscribed").then(|| "subscribed".to_string()),
            repository_watching: watching,
        },
    })
}

/// `GET /_bgh/repos/{owner}/{repo}/issues/{number}/subscription`
pub async fn get_issue_subscription(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, number)): Path<(String, String, i64)>,
) -> ApiResult<Json<IssueSubscription>> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    let (kind, id) = issue_subject(&state, &access, number).await?;
    let sub = thread_sub(&state, auth.id(), kind, id).await?;
    Ok(Json(
        issue_subscription_json(&state, auth.id(), access.repo.id, sub).await?,
    ))
}

#[derive(Debug, Default, Deserialize)]
pub struct IssueSubBody {
    pub subscribed: Option<bool>,
    pub ignored: Option<bool>,
}

/// `PUT /_bgh/repos/{owner}/{repo}/issues/{number}/subscription`
pub async fn set_issue_subscription(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    body: bytes::Bytes,
) -> ApiResult<Json<IssueSubscription>> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    let (kind, id) = issue_subject(&state, &access, number).await?;
    let body: IssueSubBody = crate::optional_json(&body)?;
    let ignored = body.ignored.unwrap_or(false);
    let subscribed = !ignored && body.subscribed.unwrap_or(true);
    let mut tx = Tx::begin(&state).await?;
    let row = set_thread_subscription(
        &mut tx,
        auth.id(),
        kind,
        id,
        access.repo.id,
        subscribed,
        ignored,
        Some("manual"),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(
        issue_subscription_json(&state, auth.id(), access.repo.id, Some(row)).await?,
    ))
}

/// `DELETE /_bgh/repos/{owner}/{repo}/issues/{number}/subscription` —
/// unsubscribe (204).
pub async fn delete_issue_subscription(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, number)): Path<(String, String, i64)>,
) -> ApiResult<StatusCode> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    let (kind, id) = issue_subject(&state, &access, number).await?;
    let mut tx = Tx::begin(&state).await?;
    set_thread_subscription(
        &mut tx,
        auth.id(),
        kind,
        id,
        access.repo.id,
        false,
        false,
        None,
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
