//! Watching (repository subscriptions).
//!
//! * `GET /repos/{o}/{r}/subscribers`
//! * (`/repos/{o}/{r}/subscription` is owned by bgh-notify)
//! * `GET /user/subscriptions`, `GET /users/{username}/subscriptions`
//! * legacy `GET|PUT|DELETE /user/subscriptions/{owner}/{repo}` (204/404)
//!
//! `repositories.watchers_count` (rendered as `subscribers_count`) is the
//! number of `watches` rows with `subscribed = true`, maintained here in the
//! same transaction.

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use bgh_core::models::api::{MinimalRepository, SimpleUser};
use bgh_core::prelude::*;
use bgh_core::views;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/repos/{owner}/{repo}/subscribers", get(subscribers))
        .route("/user/subscriptions", get(list_for_authenticated_user))
        .route("/users/{username}/subscriptions", get(list_for_user))
        .route(
            "/user/subscriptions/{owner}/{repo}",
            get(legacy_check).put(legacy_watch).delete(legacy_unwatch),
        )
}

/// `GET /repos/{owner}/{repo}/subscribers`
async fn subscribers(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<Page<SimpleUser>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let rows: Vec<db::User> = sqlx::query_as(&format!(
        "SELECT {} FROM watches w JOIN users u ON u.id = w.user_id
          WHERE w.repo_id = $1 AND w.subscribed
          ORDER BY w.created_at, w.user_id LIMIT $2 OFFSET $3",
        db::prefixed("u", db::User::COLUMNS)
    ))
    .bind(access.repo.id)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    Ok(p.page(rows).map(|u| SimpleUser::new(&state.urls, &u)))
}

#[derive(sqlx::FromRow)]
struct WatchRow {
    subscribed: bool,
}

async fn find_watch(state: &AppState, user_id: i64, repo_id: i64) -> ApiResult<Option<WatchRow>> {
    Ok(
        sqlx::query_as("SELECT subscribed FROM watches WHERE user_id = $1 AND repo_id = $2")
            .bind(user_id)
            .bind(repo_id)
            .fetch_optional(&state.db)
            .await?,
    )
}

/// Set (`Some((subscribed, ignored))`) or delete (`None`) the caller's
/// subscription, adjusting `watchers_count` by the change in subscribed rows.
async fn set_watch(
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
    let row = match new {
        Some((subscribed, ignored)) => Some(
            sqlx::query_as::<_, WatchRow>(
                "INSERT INTO watches (user_id, repo_id, subscribed, ignored) VALUES ($1, $2, $3, $4)
                 ON CONFLICT (user_id, repo_id) DO UPDATE
                    SET subscribed = EXCLUDED.subscribed, ignored = EXCLUDED.ignored
                 RETURNING subscribed",
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
    let before = i64::from(old.unwrap_or(false));
    let after = i64::from(row.as_ref().is_some_and(|r| r.subscribed));
    if before != after {
        sqlx::query(
            "UPDATE repositories SET watchers_count = greatest(watchers_count + $2, 0) WHERE id = $1",
        )
        .bind(repo_id)
        .bind(after - before)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(row)
}

/// Repositories watched by `user_id`, most recent first.
async fn watched_page(
    state: &AppState,
    auth: Option<&AuthContext>,
    p: &Pagination,
    user_id: i64,
) -> ApiResult<Page<MinimalRepository>> {
    let rows: Vec<db::Repository> = sqlx::query_as(&format!(
        "SELECT {} FROM watches w JOIN repositories r ON r.id = w.repo_id
          WHERE w.user_id = $1 AND w.subscribed
          ORDER BY w.created_at DESC, w.repo_id DESC LIMIT $2 OFFSET $3",
        db::prefixed("r", db::Repository::COLUMNS)
    ))
    .bind(user_id)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let page = p.page(rows);
    Ok(Page {
        items: views::minimal_repos(state, auth, page.items).await?,
        link: page.link,
    })
}

/// `GET /user/subscriptions`
async fn list_for_authenticated_user(
    State(state): State<AppState>,
    auth: RequireUser,
    p: Pagination,
) -> ApiResult<Page<MinimalRepository>> {
    watched_page(&state, Some(&auth), &p, auth.user.id).await
}

/// `GET /users/{username}/subscriptions`
async fn list_for_user(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path(username): Path<String>,
) -> ApiResult<Page<MinimalRepository>> {
    let user = db::User::find_by_login(&state.db, &username)
        .await?
        .ok_or(ApiError::NotFound)?;
    watched_page(&state, auth.as_ref(), &p, user.id).await
}

/// Legacy `GET /user/subscriptions/{owner}/{repo}`: 204 if watching.
async fn legacy_check(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    match find_watch(&state, auth.user.id, access.repo.id).await? {
        Some(w) if w.subscribed => Ok(StatusCode::NO_CONTENT),
        _ => Err(ApiError::NotFound),
    }
}

/// Legacy `PUT /user/subscriptions/{owner}/{repo}`
async fn legacy_watch(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    set_watch(&state, auth.user.id, access.repo.id, Some((true, false))).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Legacy `DELETE /user/subscriptions/{owner}/{repo}`
async fn legacy_unwatch(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    set_watch(&state, auth.user.id, access.repo.id, None).await?;
    Ok(StatusCode::NO_CONTENT)
}
