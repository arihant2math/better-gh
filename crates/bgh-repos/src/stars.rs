//! Starring.
//!
//! * `GET /repos/{o}/{r}/stargazers` (`application/vnd.github.star+json`
//!   adds `starred_at`)
//! * `GET /user/starred`, `GET /users/{username}/starred`
//! * `GET|PUT|DELETE /user/starred/{owner}/{repo}`
//!
//! `repositories.stargazers_count` is maintained here, in the same
//! transaction as the `stars` row change.

use std::collections::HashMap;

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use bgh_core::models::api::{MinimalRepository, SimpleUser};
use bgh_core::prelude::*;
use bgh_core::views;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::json::repo_sync_json;
use crate::media::{Media, media};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/repos/{owner}/{repo}/stargazers", get(stargazers))
        .route("/user/starred", get(list_for_authenticated_user))
        .route("/users/{username}/starred", get(list_for_user))
        .route(
            "/user/starred/{owner}/{repo}",
            get(check).put(star).delete(unstar),
        )
}

#[derive(sqlx::FromRow)]
struct StargazerRow {
    #[sqlx(flatten)]
    user: db::User,
    starred_at: DateTime<Utc>,
}

#[derive(Serialize)]
struct Stargazer {
    starred_at: Timestamp,
    user: SimpleUser,
}

#[derive(Serialize)]
struct StarredRepo {
    starred_at: Timestamp,
    repo: MinimalRepository,
}

/// `GET /repos/{owner}/{repo}/stargazers`: oldest star first.
async fn stargazers(
    State(state): State<AppState>,
    auth: MaybeUser,
    headers: HeaderMap,
    p: Pagination,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<Response> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let rows: Vec<StargazerRow> = sqlx::query_as(&format!(
        "SELECT {}, s.created_at AS starred_at FROM stars s JOIN users u ON u.id = s.user_id
          WHERE s.repo_id = $1 ORDER BY s.created_at, s.user_id LIMIT $2 OFFSET $3",
        db::prefixed("u", db::User::COLUMNS)
    ))
    .bind(access.repo.id)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let page = p.page(rows);
    Ok(if media(&headers) == Media::Star {
        page.map(|r| Stargazer {
            starred_at: r.starred_at.into(),
            user: SimpleUser::new(&state.urls, &r.user),
        })
        .into_response()
    } else {
        page.map(|r| SimpleUser::new(&state.urls, &r.user))
            .into_response()
    })
}

#[derive(Debug, Default, Deserialize)]
struct StarredParams {
    /// `created` (star time, default) | `updated` (last push)
    sort: Option<String>,
    /// `asc` | `desc` (default)
    direction: Option<String>,
}

#[derive(sqlx::FromRow)]
struct StarredRow {
    #[sqlx(flatten)]
    repo: db::Repository,
    starred_at: DateTime<Utc>,
}

/// Starred repositories of `user_id`, rendered for `auth` (repositories the
/// caller can't read are dropped).
async fn starred_page(
    state: &AppState,
    auth: Option<&AuthContext>,
    headers: &HeaderMap,
    p: &Pagination,
    params: &StarredParams,
    user_id: i64,
) -> ApiResult<Response> {
    let dir = match params.direction.as_deref() {
        Some("asc") => "ASC",
        _ => "DESC",
    };
    let order = match params.sort.as_deref() {
        Some("updated") => format!("r.pushed_at {dir} NULLS LAST, r.id {dir}"),
        _ => format!("s.created_at {dir}, s.repo_id {dir}"),
    };
    let rows: Vec<StarredRow> = sqlx::query_as(&format!(
        "SELECT {}, s.created_at AS starred_at FROM stars s JOIN repositories r ON r.id = s.repo_id
          WHERE s.user_id = $1 ORDER BY {order} LIMIT $2 OFFSET $3",
        db::prefixed("r", db::Repository::COLUMNS)
    ))
    .bind(user_id)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let page = p.page(rows);
    let starred_at: HashMap<i64, DateTime<Utc>> = page
        .items
        .iter()
        .map(|r| (r.repo.id, r.starred_at))
        .collect();
    let repos = views::minimal_repos(
        state,
        auth,
        page.items.into_iter().map(|r| r.repo).collect(),
    )
    .await?;
    Ok(if media(headers) == Media::Star {
        let items: Vec<StarredRepo> = repos
            .into_iter()
            .map(|repo| StarredRepo {
                starred_at: starred_at[&repo.id].into(),
                repo,
            })
            .collect();
        Page {
            items,
            link: page.link,
        }
        .into_response()
    } else {
        Page {
            items: repos,
            link: page.link,
        }
        .into_response()
    })
}

/// `GET /user/starred`
async fn list_for_authenticated_user(
    State(state): State<AppState>,
    auth: RequireUser,
    headers: HeaderMap,
    p: Pagination,
    Query(params): Query<StarredParams>,
) -> ApiResult<Response> {
    starred_page(&state, Some(&auth), &headers, &p, &params, auth.user.id).await
}

/// `GET /users/{username}/starred`
async fn list_for_user(
    State(state): State<AppState>,
    auth: MaybeUser,
    headers: HeaderMap,
    p: Pagination,
    Path(username): Path<String>,
    Query(params): Query<StarredParams>,
) -> ApiResult<Response> {
    let user = db::User::find_by_login(&state.db, &username)
        .await?
        .ok_or(ApiError::NotFound)?;
    starred_page(&state, auth.as_ref(), &headers, &p, &params, user.id).await
}

/// `GET /user/starred/{owner}/{repo}`: 204 if starred, else 404.
async fn check(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    let starred: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM stars WHERE user_id = $1 AND repo_id = $2)",
    )
    .bind(auth.user.id)
    .bind(access.repo.id)
    .fetch_one(&state.db)
    .await?;
    if starred {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound)
    }
}

/// `PUT /user/starred/{owner}/{repo}` (idempotent).
async fn star(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    set_star(&state, &auth, &access, true).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /user/starred/{owner}/{repo}` (idempotent).
async fn unstar(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    set_star(&state, &auth, &access, false).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Star or unstar; the counter, sync record and event only change when the
/// `stars` row actually changed.
async fn set_star(
    state: &AppState,
    auth: &AuthContext,
    access: &RepoAccess,
    starred: bool,
) -> ApiResult<()> {
    let repo_id = access.repo.id;
    let mut tx = Tx::begin(state).await?;
    let changed = if starred {
        sqlx::query(
            "INSERT INTO stars (user_id, repo_id) VALUES ($1, $2)
             ON CONFLICT (user_id, repo_id) DO NOTHING",
        )
    } else {
        sqlx::query("DELETE FROM stars WHERE user_id = $1 AND repo_id = $2")
    }
    .bind(auth.user.id)
    .bind(repo_id)
    .execute(&mut *tx)
    .await?
    .rows_affected()
        > 0;
    if !changed {
        return Ok(());
    }
    let delta: i64 = if starred { 1 } else { -1 };
    let updated: db::Repository = sqlx::query_as(&format!(
        "UPDATE repositories SET stargazers_count = greatest(stargazers_count + $2, 0)
          WHERE id = $1 RETURNING {}",
        db::Repository::COLUMNS
    ))
    .bind(repo_id)
    .bind(delta)
    .fetch_one(&mut *tx)
    .await?;
    tx.sync(
        &access.scope(),
        "repository",
        repo_id,
        SyncAction::Update,
        &repo_sync_json(&updated, &access.owner.login),
    )
    .await?;
    tx.emit(Event::RepositoryStarred {
        repo_id,
        actor_id: auth.user.id,
        starred,
    });
    tx.commit().await?;
    Ok(())
}
