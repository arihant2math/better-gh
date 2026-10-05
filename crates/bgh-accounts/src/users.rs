//! Users: creation service, `GET /user`, `GET /users/{username}`.

use axum::extract::State;
use bgh_core::audit;
use bgh_core::crypto;
use bgh_core::error::unique_violation;
use bgh_core::models::api::{PrivateUser, PrivateUserStats, PublicUser, UserStats};
use bgh_core::prelude::*;
use serde_json::json;

use crate::validate;

/// Input for [`create_user`].
#[derive(Debug, Clone)]
pub struct NewAccount<'a> {
    pub login: &'a str,
    pub email: &'a str,
    pub password: &'a str,
    pub name: Option<&'a str>,
    /// `None`: site admin iff this is the first user account.
    pub site_admin: Option<bool>,
}

/// Validate and create a user account. The first account ever created
/// becomes a site administrator unless `site_admin` says otherwise.
pub async fn create_user(
    state: &AppState,
    input: NewAccount<'_>,
    actor: Option<&db::User>,
) -> ApiResult<db::User> {
    let mut errors = Vec::new();
    if input.login.is_empty() {
        errors.push(FieldError::missing_field("User", "login"));
    } else if !validate::is_valid_login(input.login) || validate::is_reserved_login(input.login) {
        errors.push(FieldError::invalid("User", "login"));
    }
    if !validate::is_valid_email(input.email) {
        errors.push(FieldError::invalid("User", "email"));
    }
    if !validate::is_valid_password(input.password) {
        errors.push(FieldError::custom(
            "User",
            "password",
            format!(
                "password must be at least {} characters",
                validate::MIN_PASSWORD_LEN
            ),
        ));
    }
    if !errors.is_empty() {
        return Err(ApiError::validation(errors));
    }

    let password = input.password.to_string();
    let hash = tokio::task::spawn_blocking(move || crypto::hash_password(&password)).await??;

    let mut tx = Tx::begin(state).await?;
    // Serialize sign-ups so exactly one "first user" becomes admin.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('bgh_create_user'))")
        .execute(&mut *tx)
        .await?;
    let site_admin = match input.site_admin {
        Some(v) => v,
        None => {
            sqlx::query_scalar::<_, bool>(
                "SELECT NOT EXISTS (SELECT 1 FROM users WHERE type = 'User')",
            )
            .fetch_one(&mut *tx)
            .await?
        }
    };
    let user = db::NewUser {
        login: input.login,
        email: Some(input.email),
        name: input.name,
        password_hash: Some(&hash),
        site_admin,
    }
    .insert(&mut tx)
    .await
    .map_err(|e| match unique_violation(&e).as_deref() {
        Some("users_login_key") => {
            ApiError::invalid_field(FieldError::already_exists("User", "login"))
        }
        Some("user_emails_email_key") => {
            ApiError::invalid_field(FieldError::already_exists("User", "email"))
        }
        _ => e.into(),
    })?;
    audit::log(
        &mut *tx,
        actor.or(Some(&user)),
        "user.create",
        audit::Target::User(user.id),
        json!({ "login": user.login, "site_admin": site_admin }),
    )
    .await?;
    tx.emit(bgh_core::events::Event::UserAccountChanged {
        user_id: user.id,
        login: user.login.clone(),
        action: "created".into(),
        actor_id: actor.map_or(user.id, |a| a.id),
        data: json!({}),
    });
    tx.commit().await?;
    Ok(user)
}

/// Public profile counters.
pub async fn user_stats(state: &AppState, user_id: i64) -> ApiResult<UserStats> {
    Ok(sqlx::query_as(
        "SELECT (SELECT count(*) FROM repositories WHERE owner_id = $1 AND visibility = 'public') AS public_repos,
                0::bigint AS public_gists,
                (SELECT count(*) FROM follows WHERE following_id = $1) AS followers,
                (SELECT count(*) FROM follows WHERE follower_id = $1) AS following",
    )
    .bind(user_id)
    .fetch_one(&state.db)
    .await?)
}

/// Private counters for the authenticated user.
pub async fn private_stats(state: &AppState, user_id: i64) -> ApiResult<PrivateUserStats> {
    Ok(sqlx::query_as(
        "SELECT count(*) FILTER (WHERE visibility <> 'public') AS total_private_repos,
                count(*) FILTER (WHERE visibility <> 'public') AS owned_private_repos,
                coalesce(sum(size), 0)::bigint AS disk_usage,
                (SELECT count(DISTINCT c.user_id) FROM collaborators c
                   JOIN repositories r ON r.id = c.repo_id WHERE r.owner_id = $1) AS collaborators
           FROM repositories WHERE owner_id = $1",
    )
    .bind(user_id)
    .fetch_one(&state.db)
    .await?)
}

/// Render the private profile of `user`.
pub async fn private_user_json(state: &AppState, user: &db::User) -> ApiResult<PrivateUser> {
    let stats = user_stats(state, user.id).await?;
    let private = private_stats(state, user.id).await?;
    Ok(PrivateUser::new(&state.urls, user, stats, private, false))
}

/// `GET /user`
pub async fn get_authenticated_user(
    State(state): State<AppState>,
    auth: RequireUser,
) -> ApiResult<Json<PrivateUser>> {
    Ok(Json(private_user_json(&state, &auth.user).await?))
}

/// `GET /users/{username}` (also resolves organizations, like GitHub).
pub async fn get_user(
    State(state): State<AppState>,
    _auth: MaybeUser,
    Path(username): Path<String>,
) -> ApiResult<Json<PublicUser>> {
    let user = db::User::find_by_login(&state.db, &username)
        .await?
        .ok_or(ApiError::NotFound)?;
    let stats = user_stats(&state, user.id).await?;
    Ok(Json(PublicUser::new(&state.urls, &user, stats)))
}
