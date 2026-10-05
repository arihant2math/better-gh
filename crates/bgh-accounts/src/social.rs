//! Followers / following and user blocking.
//!
//! `GET /user/followers`, `GET /user/following`,
//! `GET|PUT|DELETE /user/following/{username}`,
//! `GET /users/{username}/followers|following`,
//! `GET /users/{username}/following/{target_user}`,
//! `GET /user/blocks`, `GET|PUT|DELETE /user/blocks/{username}`.
//! Organization blocks (`/orgs/{org}/blocks`) live in `orgs`.

use axum::extract::State;
use axum::http::StatusCode;
use bgh_core::audit;
use bgh_core::models::api::SimpleUser;
use bgh_core::prelude::*;
use serde_json::json;

use crate::util;

async fn page_of_users(
    state: &AppState,
    p: &Pagination,
    sql: &str,
    id: i64,
) -> ApiResult<Page<SimpleUser>> {
    let rows: Vec<db::User> = sqlx::query_as(sql)
        .bind(id)
        .bind(p.limit_plus_one())
        .bind(p.offset())
        .fetch_all(&state.db)
        .await?;
    Ok(p.page(rows).map(|u| SimpleUser::new(&state.urls, &u)))
}

fn followers_sql() -> String {
    format!(
        "SELECT {} FROM follows f JOIN users u ON u.id = f.follower_id
          WHERE f.following_id = $1 ORDER BY f.created_at, f.follower_id LIMIT $2 OFFSET $3",
        db::prefixed("u", db::User::COLUMNS)
    )
}

fn following_sql() -> String {
    format!(
        "SELECT {} FROM follows f JOIN users u ON u.id = f.following_id
          WHERE f.follower_id = $1 ORDER BY f.created_at, f.following_id LIMIT $2 OFFSET $3",
        db::prefixed("u", db::User::COLUMNS)
    )
}

/// `GET /user/followers`
pub async fn my_followers(
    State(state): State<AppState>,
    auth: RequireUser,
    p: Pagination,
) -> ApiResult<Page<SimpleUser>> {
    page_of_users(&state, &p, &followers_sql(), auth.user.id).await
}

/// `GET /user/following`
pub async fn my_following(
    State(state): State<AppState>,
    auth: RequireUser,
    p: Pagination,
) -> ApiResult<Page<SimpleUser>> {
    page_of_users(&state, &p, &following_sql(), auth.user.id).await
}

/// `GET /users/{username}/followers`
pub async fn followers(
    State(state): State<AppState>,
    _auth: MaybeUser,
    Path(username): Path<String>,
    p: Pagination,
) -> ApiResult<Page<SimpleUser>> {
    let user = util::find_account(&state, &username).await?;
    page_of_users(&state, &p, &followers_sql(), user.id).await
}

/// `GET /users/{username}/following`
pub async fn following(
    State(state): State<AppState>,
    _auth: MaybeUser,
    Path(username): Path<String>,
    p: Pagination,
) -> ApiResult<Page<SimpleUser>> {
    let user = util::find_account(&state, &username).await?;
    page_of_users(&state, &p, &following_sql(), user.id).await
}

async fn follows(state: &AppState, follower: i64, target: i64) -> ApiResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM follows WHERE follower_id = $1 AND following_id = $2)",
    )
    .bind(follower)
    .bind(target)
    .fetch_one(&state.db)
    .await?)
}

fn yes_no(b: bool) -> ApiResult<StatusCode> {
    if b {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound)
    }
}

/// `GET /user/following/{username}` → 204 / 404.
pub async fn check_my_following(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(username): Path<String>,
) -> ApiResult<StatusCode> {
    let target = util::find_account(&state, &username).await?;
    yes_no(follows(&state, auth.user.id, target.id).await?)
}

/// `GET /users/{username}/following/{target_user}` → 204 / 404.
pub async fn check_following(
    State(state): State<AppState>,
    _auth: MaybeUser,
    Path((username, target)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    let user = util::find_account(&state, &username).await?;
    let target = util::find_account(&state, &target).await?;
    yes_no(follows(&state, user.id, target.id).await?)
}

/// Whether `blocker` blocks `blocked`.
pub async fn is_blocked(
    db: impl sqlx::PgExecutor<'_>,
    blocker: i64,
    blocked: i64,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM user_blocks WHERE blocker_id = $1 AND blocked_id = $2)",
    )
    .bind(blocker)
    .bind(blocked)
    .fetch_one(db)
    .await
}

/// `PUT /user/following/{username}` (scope `user:follow`) → 204.
pub async fn follow(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(username): Path<String>,
) -> ApiResult<StatusCode> {
    auth.require_scope("user:follow")?;
    let target = util::find_account(&state, &username).await?;
    if target.id == auth.user.id {
        return Err(ApiError::unprocessable("You can't follow yourself"));
    }
    if is_blocked(&state.db, target.id, auth.user.id).await? {
        return Err(ApiError::forbidden("You can't follow this user"));
    }
    let mut tx = Tx::begin(&state).await?;
    let inserted = sqlx::query(
        "INSERT INTO follows (follower_id, following_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
    )
    .bind(auth.user.id)
    .bind(target.id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if inserted > 0 {
        tx.emit(Event::UserFollowed {
            actor_id: auth.user.id,
            target_id: target.id,
        });
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /user/following/{username}` (scope `user:follow`) → 204.
pub async fn unfollow(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(username): Path<String>,
) -> ApiResult<StatusCode> {
    auth.require_scope("user:follow")?;
    let target = util::find_account(&state, &username).await?;
    sqlx::query("DELETE FROM follows WHERE follower_id = $1 AND following_id = $2")
        .bind(auth.user.id)
        .bind(target.id)
        .execute(&state.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Blocks (shared by users and organizations)
// ---------------------------------------------------------------------------

/// Users blocked by `blocker_id`.
pub async fn list_blocked(
    state: &AppState,
    blocker_id: i64,
    p: &Pagination,
) -> ApiResult<Page<SimpleUser>> {
    let sql = format!(
        "SELECT {} FROM user_blocks b JOIN users u ON u.id = b.blocked_id
          WHERE b.blocker_id = $1 ORDER BY b.created_at, b.blocked_id LIMIT $2 OFFSET $3",
        db::prefixed("u", db::User::COLUMNS)
    );
    page_of_users(state, p, &sql, blocker_id).await
}

/// Block `blocked` on behalf of `blocker` (user or org): also removes
/// follows in both directions.
pub async fn block(
    state: &AppState,
    actor: &db::User,
    blocker: &db::User,
    blocked: &db::User,
) -> ApiResult<()> {
    if blocked.id == blocker.id || blocked.id == actor.id {
        return Err(ApiError::unprocessable("You can't block yourself"));
    }
    if blocked.is_org() {
        return Err(ApiError::unprocessable("Organizations can't be blocked"));
    }
    let mut tx = Tx::begin(state).await?;
    sqlx::query(
        "INSERT INTO user_blocks (blocker_id, blocked_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
    )
    .bind(blocker.id)
    .bind(blocked.id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "DELETE FROM follows WHERE (follower_id = $1 AND following_id = $2)
                                 OR (follower_id = $2 AND following_id = $1)",
    )
    .bind(blocker.id)
    .bind(blocked.id)
    .execute(&mut *tx)
    .await?;
    let target = if blocker.is_org() {
        audit::Target::Org(blocker.id)
    } else {
        audit::Target::User(blocker.id)
    };
    let action = if blocker.is_org() {
        "org.block_user"
    } else {
        "user.block_user"
    };
    audit::log(
        &mut *tx,
        Some(actor),
        action,
        target,
        json!({ "blocked_user": blocked.login }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn unblock(state: &AppState, blocker_id: i64, blocked_id: i64) -> ApiResult<()> {
    sqlx::query("DELETE FROM user_blocks WHERE blocker_id = $1 AND blocked_id = $2")
        .bind(blocker_id)
        .bind(blocked_id)
        .execute(&state.db)
        .await?;
    Ok(())
}

/// `GET /user/blocks` (scope `user`).
pub async fn my_blocks(
    State(state): State<AppState>,
    auth: RequireUser,
    p: Pagination,
) -> ApiResult<Page<SimpleUser>> {
    auth.require_scope("user")?;
    list_blocked(&state, auth.user.id, &p).await
}

/// `GET /user/blocks/{username}` → 204 if blocked, else 404.
pub async fn check_my_block(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(username): Path<String>,
) -> ApiResult<StatusCode> {
    auth.require_scope("user")?;
    let target = util::find_account(&state, &username).await?;
    yes_no(is_blocked(&state.db, auth.user.id, target.id).await?)
}

/// `PUT /user/blocks/{username}` → 204.
pub async fn block_user(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(username): Path<String>,
) -> ApiResult<StatusCode> {
    auth.require_scope("user")?;
    let target = util::find_account(&state, &username).await?;
    block(&state, &auth.user, &auth.user, &target).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /user/blocks/{username}` → 204.
pub async fn unblock_user(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(username): Path<String>,
) -> ApiResult<StatusCode> {
    auth.require_scope("user")?;
    let target = util::find_account(&state, &username).await?;
    unblock(&state, auth.user.id, target.id).await?;
    Ok(StatusCode::NO_CONTENT)
}
