//! Notification threads: rows, GitHub JSON, and the `/notifications` API.
//!
//! * `GET/PUT /notifications`, `GET/PUT /repos/{owner}/{repo}/notifications`
//! * `GET/PATCH/DELETE /notifications/threads/{id}`
//!
//! The lists support polling: `Last-Modified` (newest change of any of the
//! user's threads), `If-Modified-Since` → 304 and `X-Poll-Interval`.
//!
//! Every change to a thread is recorded as a `notification` sync action in
//! the owner's `user:{id}` scope (shape: `bgh_core::sync::shapes`); marking a
//! thread done deletes it from the client store.

use std::collections::HashMap;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use bgh_core::models::api::MinimalRepository;
use bgh_core::polling;
use bgh_core::prelude::*;
use bgh_core::sync;
use bgh_core::time::ts;
use bgh_core::views;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// `notifications` row.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct NotificationRow {
    pub id: i64,
    pub user_id: i64,
    pub repo_id: i64,
    pub subject_type: String,
    pub subject_id: i64,
    pub subject_title: String,
    pub subject_key: Option<String>,
    pub reason: String,
    pub unread: bool,
    pub done: bool,
    pub last_read_at: Option<DateTime<Utc>>,
    pub latest_comment_id: Option<i64>,
    pub latest_comment_type: Option<String>,
    pub last_actor_id: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl NotificationRow {
    pub const COLUMNS: &'static str = "id, user_id, repo_id, subject_type, subject_id, \
        subject_title, subject_key, reason, unread, done, last_read_at, latest_comment_id, \
        latest_comment_type, last_actor_id, created_at, updated_at";
}

/// Record the current state of `rows` in their owners' sync scopes
/// (`done` rows are deletions from the client's point of view). Shapes come
/// from `bgh_core::sync::shapes` (`notification`).
pub async fn sync_rows(tx: &mut Tx, rows: &[NotificationRow], inserted: &[bool]) -> ApiResult<()> {
    let (mut new, mut changed) = (Vec::new(), Vec::new());
    for (i, n) in rows.iter().enumerate() {
        if n.done {
            tx.sync_delete(&sync::user_scope(n.user_id), SyncModel::Notification, n.id)
                .await?;
        } else if inserted.get(i).copied().unwrap_or(false) {
            new.push(n.id);
        } else {
            changed.push(n.id);
        }
    }
    tx.sync_models(SyncModel::Notification, &new, SyncAction::Insert)
        .await?;
    tx.sync_models(SyncModel::Notification, &changed, SyncAction::Update)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// GitHub JSON
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct ThreadSubject {
    pub title: String,
    pub url: Option<String>,
    pub latest_comment_url: Option<String>,
    #[serde(rename = "type")]
    pub kind: String,
}

/// GitHub `thread`.
#[derive(Debug, Clone, Serialize)]
pub struct Thread {
    pub id: String,
    pub repository: MinimalRepository,
    pub subject: ThreadSubject,
    pub reason: String,
    pub unread: bool,
    pub updated_at: Timestamp,
    pub last_read_at: Option<Timestamp>,
    pub url: String,
    pub subscription_url: String,
}

pub fn thread_url(state: &AppState, id: i64) -> String {
    state.urls.api(&format!("/notifications/threads/{id}"))
}

/// Render rows for their owner. Threads in repositories the caller can no
/// longer read are dropped. Batched: one query each for repositories,
/// owners/permissions and issue numbers.
pub async fn render(
    state: &AppState,
    auth: &AuthContext,
    rows: Vec<NotificationRow>,
) -> ApiResult<Vec<Thread>> {
    if rows.is_empty() {
        return Ok(vec![]);
    }
    let mut repo_ids: Vec<i64> = rows.iter().map(|n| n.repo_id).collect();
    repo_ids.sort_unstable();
    repo_ids.dedup();
    let repo_rows: Vec<db::Repository> = sqlx::query_as(&format!(
        "SELECT {} FROM repositories WHERE id = ANY($1)",
        db::Repository::COLUMNS
    ))
    .bind(&repo_ids)
    .fetch_all(&state.db)
    .await?;
    let repos: HashMap<i64, MinimalRepository> = views::minimal_repos(state, Some(auth), repo_rows)
        .await?
        .into_iter()
        .map(|r| (r.id, r))
        .collect();

    let issue_ids: Vec<i64> = rows
        .iter()
        .filter(|n| matches!(n.subject_type.as_str(), "Issue" | "PullRequest"))
        .map(|n| n.subject_id)
        .collect();
    let numbers: HashMap<i64, i64> = if issue_ids.is_empty() {
        HashMap::new()
    } else {
        sqlx::query_as::<_, (i64, i64)>("SELECT id, number FROM issues WHERE id = ANY($1)")
            .bind(&issue_ids)
            .fetch_all(&state.db)
            .await?
            .into_iter()
            .collect()
    };

    let mut out = Vec::with_capacity(rows.len());
    for n in rows {
        let Some(repo) = repos.get(&n.repo_id) else {
            continue;
        };
        let subject = subject_json(state, repo, &n, numbers.get(&n.subject_id).copied());
        out.push(Thread {
            id: n.id.to_string(),
            repository: repo.clone(),
            subject,
            reason: n.reason.clone(),
            unread: n.unread,
            updated_at: n.updated_at.into(),
            last_read_at: ts(n.last_read_at),
            url: thread_url(state, n.id),
            subscription_url: format!("{}/subscription", thread_url(state, n.id)),
        });
    }
    Ok(out)
}

fn subject_json(
    state: &AppState,
    repo: &MinimalRepository,
    n: &NotificationRow,
    number: Option<i64>,
) -> ThreadSubject {
    let (owner, name) = (repo.owner.login.as_str(), repo.name.as_str());
    let urls = &state.urls;
    let api_repo = urls.repo(owner, name);
    let url = match (n.subject_type.as_str(), number) {
        ("Issue", Some(num)) => Some(urls.issue(owner, name, num)),
        ("PullRequest", Some(num)) => Some(urls.pull(owner, name, num)),
        ("Release", _) => Some(format!("{api_repo}/releases/{}", n.subject_id)),
        ("CheckSuite", _) => Some(format!("{api_repo}/check-suites/{}", n.subject_id)),
        ("Commit", _) => n
            .subject_key
            .as_deref()
            .map(|sha| urls.commit(owner, name, sha)),
        _ => None,
    };
    let latest_comment_url = match (n.latest_comment_type.as_deref(), n.latest_comment_id) {
        (Some("IssueComment"), Some(id)) => Some(urls.issue_comment(owner, name, id)),
        (Some("PullRequestReviewComment"), Some(id)) => {
            Some(format!("{api_repo}/pulls/comments/{id}"))
        }
        (Some("PullRequestReview"), Some(id)) => number
            .map(|num| format!("{api_repo}/pulls/{num}/reviews/{id}"))
            .or_else(|| url.clone()),
        _ if n.subject_type == "CheckSuite" => None,
        _ => url.clone(),
    };
    ThreadSubject {
        title: n.subject_title.clone(),
        url,
        latest_comment_url,
        kind: n.subject_type.clone(),
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// Notifications need the `notifications` (or `repo`) scope.
pub fn require_scope(auth: &AuthContext) -> ApiResult<()> {
    if auth.has_scope("notifications") || auth.has_scope("repo") {
        Ok(())
    } else {
        Err(ApiError::forbidden("Missing the 'notifications' scope."))
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct ListParams {
    pub all: Option<bool>,
    pub participating: Option<bool>,
    pub since: Option<String>,
    pub before: Option<String>,
}

pub fn parse_time(field: &str, v: Option<&str>) -> ApiResult<Option<DateTime<Utc>>> {
    match v.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(s) => DateTime::parse_from_rfc3339(s)
            .map(|d| Some(d.with_timezone(&Utc)))
            .map_err(|_| ApiError::invalid_field(FieldError::invalid("Notification", field))),
    }
}

/// Participating = everything except plain watching.
const NON_PARTICIPATING: &[&str] = &["subscribed", "security_alert"];

async fn list_rows(
    state: &AppState,
    user_id: i64,
    repo_id: Option<i64>,
    params: &ListParams,
    p: &Pagination,
) -> ApiResult<Vec<NotificationRow>> {
    let since = parse_time("since", params.since.as_deref())?;
    let before = parse_time("before", params.before.as_deref())?;
    let rows: Vec<NotificationRow> = sqlx::query_as(&format!(
        "SELECT {} FROM notifications
          WHERE user_id = $1 AND NOT done
            AND ($2 OR unread)
            AND (NOT $3 OR reason <> ALL($4))
            AND ($5::timestamptz IS NULL OR updated_at > $5)
            AND ($6::timestamptz IS NULL OR updated_at < $6)
            AND ($7::bigint IS NULL OR repo_id = $7)
          ORDER BY updated_at DESC, id DESC
          LIMIT $8 OFFSET $9",
        NotificationRow::COLUMNS
    ))
    .bind(user_id)
    .bind(params.all.unwrap_or(false))
    .bind(params.participating.unwrap_or(false))
    .bind(NON_PARTICIPATING)
    .bind(since)
    .bind(before)
    .bind(repo_id)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    Ok(rows)
}

async fn page_threads(
    state: &AppState,
    auth: &AuthContext,
    p: Pagination,
    mut rows: Vec<NotificationRow>,
) -> ApiResult<Page<Thread>> {
    let has_next = rows.len() as i64 > p.limit();
    rows.truncate(p.limit() as usize);
    Ok(Page {
        items: render(state, auth, rows).await?,
        link: p.link_header(has_next, None),
    })
}

/// Newest change of `user_id`'s threads (optionally in one repository),
/// any state included: the list's `Last-Modified`.
async fn last_changed(
    state: &AppState,
    user_id: i64,
    repo_id: Option<i64>,
) -> ApiResult<Option<DateTime<Utc>>> {
    Ok(sqlx::query_scalar(
        "SELECT max(changed_at) FROM notifications
          WHERE user_id = $1 AND ($2::bigint IS NULL OR repo_id = $2)",
    )
    .bind(user_id)
    .bind(repo_id)
    .fetch_one(&state.db)
    .await?)
}

/// A polled list: `If-Modified-Since` → 304, else the page; both with
/// `Last-Modified` and `X-Poll-Interval`.
async fn polled_list(
    state: &AppState,
    auth: &AuthContext,
    headers: &HeaderMap,
    p: Pagination,
    repo_id: Option<i64>,
    params: &ListParams,
) -> ApiResult<Response> {
    let last = last_changed(state, auth.user.id, repo_id).await?;
    if polling::not_modified(headers, last) {
        return Ok(polling::not_modified_response(last));
    }
    let rows = list_rows(state, auth.user.id, repo_id, params, &p).await?;
    let page = page_threads(state, auth, p, rows).await?;
    Ok(polling::with_headers(page.into_response(), last))
}

/// `GET /notifications`
pub async fn list(
    State(state): State<AppState>,
    auth: RequireUser,
    p: Pagination,
    headers: HeaderMap,
    Query(params): Query<ListParams>,
) -> ApiResult<Response> {
    require_scope(&auth)?;
    let p = p.with_default_per_page(50, 50);
    polled_list(&state, &auth, &headers, p, None, &params).await
}

/// `GET /repos/{owner}/{repo}/notifications`
pub async fn list_for_repo(
    State(state): State<AppState>,
    auth: RequireUser,
    p: Pagination,
    headers: HeaderMap,
    Path((owner, repo)): Path<(String, String)>,
    Query(params): Query<ListParams>,
) -> ApiResult<Response> {
    require_scope(&auth)?;
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    let p = p.with_default_per_page(50, 50);
    polled_list(&state, &auth, &headers, p, Some(access.repo.id), &params).await
}

#[derive(Debug, Default, Deserialize)]
pub struct MarkBody {
    pub last_read_at: Option<String>,
    pub read: Option<bool>,
}

async fn mark(
    state: &AppState,
    user_id: i64,
    repo_id: Option<i64>,
    body: MarkBody,
) -> ApiResult<StatusCode> {
    let at = parse_time("last_read_at", body.last_read_at.as_deref())?.unwrap_or_else(Utc::now);
    let read = body.read.unwrap_or(true);
    let mut tx = Tx::begin(state).await?;
    let rows: Vec<NotificationRow> = sqlx::query_as(&format!(
        "UPDATE notifications
            SET unread = NOT $2,
                last_read_at = CASE WHEN $2 THEN $3 ELSE last_read_at END
          WHERE user_id = $1 AND NOT done AND unread = $2 AND updated_at <= $3
            AND ($4::bigint IS NULL OR repo_id = $4)
        RETURNING {}",
        NotificationRow::COLUMNS
    ))
    .bind(user_id)
    .bind(read)
    .bind(at)
    .bind(repo_id)
    .fetch_all(&mut *tx)
    .await?;
    sync_rows(&mut tx, &rows, &[]).await?;
    tx.commit().await?;
    Ok(StatusCode::RESET_CONTENT)
}

/// `PUT /notifications` — mark everything updated before `last_read_at`
/// (default now) as read (`read: false` marks unread). 205.
pub async fn mark_all(
    State(state): State<AppState>,
    auth: RequireUser,
    body: bytes::Bytes,
) -> ApiResult<StatusCode> {
    require_scope(&auth)?;
    let body: MarkBody = crate::optional_json(&body)?;
    mark(&state, auth.id(), None, body).await
}

/// `PUT /repos/{owner}/{repo}/notifications`
pub async fn mark_repo(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo)): Path<(String, String)>,
    body: bytes::Bytes,
) -> ApiResult<StatusCode> {
    require_scope(&auth)?;
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    let body: MarkBody = crate::optional_json(&body)?;
    mark(&state, auth.id(), Some(access.repo.id), body).await
}

/// Load a thread owned by `user_id` (404 otherwise).
pub async fn load_thread(state: &AppState, user_id: i64, id: &str) -> ApiResult<NotificationRow> {
    let id: i64 = id.parse().map_err(|_| ApiError::NotFound)?;
    sqlx::query_as(&format!(
        "SELECT {} FROM notifications WHERE id = $1 AND user_id = $2",
        NotificationRow::COLUMNS
    ))
    .bind(id)
    .bind(user_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)
}

/// `GET /notifications/threads/{thread_id}`
pub async fn get_thread(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(id): Path<String>,
) -> ApiResult<Json<Thread>> {
    require_scope(&auth)?;
    let row = load_thread(&state, auth.id(), &id).await?;
    render(&state, &auth, vec![row])
        .await?
        .pop()
        .map(Json)
        .ok_or(ApiError::NotFound)
}

/// `PATCH /notifications/threads/{thread_id}` — mark read (205).
pub async fn mark_thread_read(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    require_scope(&auth)?;
    let row = load_thread(&state, auth.id(), &id).await?;
    let mut tx = Tx::begin(&state).await?;
    let rows: Vec<NotificationRow> = sqlx::query_as(&format!(
        "UPDATE notifications SET unread = false, last_read_at = now()
          WHERE id = $1 AND unread RETURNING {}",
        NotificationRow::COLUMNS
    ))
    .bind(row.id)
    .fetch_all(&mut *tx)
    .await?;
    sync_rows(&mut tx, &rows, &[]).await?;
    tx.commit().await?;
    Ok(StatusCode::RESET_CONTENT)
}

/// `DELETE /_bgh/notifications/threads/{thread_id}/read` — mark unread
/// (204; GitHub's REST API has no equivalent). docs/SYNC_PROTOCOL.md §10.
pub async fn mark_thread_unread(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    let row = load_thread(&state, auth.id(), &id).await?;
    let mut tx = Tx::begin(&state).await?;
    let rows: Vec<NotificationRow> = sqlx::query_as(&format!(
        "UPDATE notifications SET unread = true WHERE id = $1 AND NOT unread AND NOT done
        RETURNING {}",
        NotificationRow::COLUMNS
    ))
    .bind(row.id)
    .fetch_all(&mut *tx)
    .await?;
    sync_rows(&mut tx, &rows, &[]).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /notifications/threads/{thread_id}` — mark done (204).
pub async fn mark_thread_done(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    require_scope(&auth)?;
    let row = load_thread(&state, auth.id(), &id).await?;
    let mut tx = Tx::begin(&state).await?;
    let rows: Vec<NotificationRow> = sqlx::query_as(&format!(
        "UPDATE notifications SET done = true, unread = false,
                last_read_at = coalesce(last_read_at, now())
          WHERE id = $1 AND NOT done RETURNING {}",
        NotificationRow::COLUMNS
    ))
    .bind(row.id)
    .fetch_all(&mut *tx)
    .await?;
    sync_rows(&mut tx, &rows, &[]).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
