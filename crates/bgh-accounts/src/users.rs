//! Users: creation service, `GET|PATCH /user`, `GET /users/{username}`,
//! `GET /users`, `GET /user/{account_id}`.

use axum::extract::State;
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use bgh_core::audit;
use bgh_core::crypto;
use bgh_core::error::unique_violation;
use bgh_core::models::api::{PrivateUser, PrivateUserStats, PublicUser, SimpleUser, UserStats};
use bgh_core::prelude::*;
use serde::Deserialize;
use serde_json::json;

use crate::util::{self, Patch};
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
    let user = insert_user(
        &mut tx,
        input.login,
        input.email,
        input.name,
        Some(&hash),
        input.site_admin,
    )
    .await?;
    audit::log(
        &mut *tx,
        actor.or(Some(&user)),
        "user.create",
        audit::Target::User(user.id),
        json!({ "login": user.login, "site_admin": user.site_admin }),
    )
    .await?;
    tx.commit().await?;
    Ok(user)
}

/// Insert a user (and its verified primary email) inside `tx`. `site_admin:
/// None` makes the first user account a site admin. Unique violations map to
/// 422 `already_exists`.
pub async fn insert_user(
    tx: &mut Tx,
    login: &str,
    email: &str,
    name: Option<&str>,
    password_hash: Option<&str>,
    site_admin: Option<bool>,
) -> ApiResult<db::User> {
    // Serialize sign-ups so exactly one "first user" becomes admin.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('bgh_create_user'))")
        .execute(&mut **tx)
        .await?;
    let site_admin = match site_admin {
        Some(v) => v,
        None => {
            sqlx::query_scalar::<_, bool>(
                "SELECT NOT EXISTS (SELECT 1 FROM users WHERE type = 'User')",
            )
            .fetch_one(&mut **tx)
            .await?
        }
    };
    db::NewUser {
        login,
        email: Some(email),
        name,
        password_hash,
        site_admin,
    }
    .insert(tx)
    .await
    .map_err(|e| match unique_violation(&e).as_deref() {
        Some("users_login_key") => {
            ApiError::invalid_field(FieldError::already_exists("User", "login"))
        }
        Some("user_emails_email_key") => {
            ApiError::invalid_field(FieldError::already_exists("User", "email"))
        }
        _ => e.into(),
    })
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
    let two_factor = util::two_factor_enabled(&state.db, user.id).await?;
    Ok(PrivateUser::new(
        &state.urls,
        user,
        stats,
        private,
        two_factor,
    ))
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
    let user = util::find_account(&state, &username).await?;
    let stats = user_stats(&state, user.id).await?;
    Ok(Json(PublicUser::new(&state.urls, &user, stats)))
}

/// `GET /user/{account_id}`
pub async fn get_user_by_id(
    State(state): State<AppState>,
    _auth: MaybeUser,
    Path(id): Path<String>,
) -> ApiResult<Json<PublicUser>> {
    let id: i64 = id.parse().map_err(|_| ApiError::NotFound)?;
    let user = db::User::find(&state.db, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let stats = user_stats(&state, user.id).await?;
    Ok(Json(PublicUser::new(&state.urls, &user, stats)))
}

#[derive(Debug, Deserialize)]
pub struct ListUsersQuery {
    pub since: Option<i64>,
    pub per_page: Option<u32>,
}

/// `GET /users?since=&per_page=`: all users and organizations by id, with
/// `since`-based `Link` pagination like GitHub.
pub async fn list_users(
    State(state): State<AppState>,
    _auth: MaybeUser,
    Query(q): Query<ListUsersQuery>,
) -> ApiResult<Response> {
    let per_page = q.per_page.unwrap_or(30).clamp(1, 100);
    let since = q.since.unwrap_or(0);
    let rows: Vec<db::User> = sqlx::query_as(&format!(
        "SELECT {} FROM users WHERE id > $1 AND type IN ('User', 'Organization')
          ORDER BY id LIMIT $2",
        db::User::COLUMNS
    ))
    .bind(since)
    .bind(i64::from(per_page) + 1)
    .fetch_all(&state.db)
    .await?;
    let has_next = rows.len() > per_page as usize;
    let items: Vec<SimpleUser> = rows
        .iter()
        .take(per_page as usize)
        .map(|u| SimpleUser::new(&state.urls, u))
        .collect();
    let base = state.urls.api("/users");
    let mut links = Vec::new();
    if has_next && let Some(last) = items.last() {
        links.push(format!(
            "<{base}?per_page={per_page}&since={}>; rel=\"next\"",
            last.id
        ));
    }
    links.push(format!("<{base}{{?since}}>; rel=\"first\""));
    let mut resp = Json(items).into_response();
    if let Ok(v) = HeaderValue::from_str(&links.join(", ")) {
        resp.headers_mut().insert(header::LINK, v);
    }
    Ok(resp)
}

#[derive(Debug, Deserialize)]
pub struct UpdateUserBody {
    #[serde(default)]
    pub name: Patch<String>,
    #[serde(default)]
    pub email: Patch<String>,
    #[serde(default)]
    pub blog: Patch<String>,
    #[serde(default)]
    pub twitter_username: Patch<String>,
    #[serde(default)]
    pub company: Patch<String>,
    #[serde(default)]
    pub location: Patch<String>,
    #[serde(default)]
    pub hireable: Patch<bool>,
    #[serde(default)]
    pub bio: Patch<String>,
}

fn text_field(
    errors: &mut Vec<FieldError>,
    field: &str,
    v: Patch<String>,
    max: usize,
) -> Option<Option<String>> {
    let v = v.into_option()?;
    let v = util::non_empty(v);
    if v.as_ref().is_some_and(|s| s.chars().count() > max) {
        errors.push(FieldError::custom(
            "User",
            field,
            format!("{field} is too long (maximum is {max} characters)"),
        ));
    }
    Some(v)
}

/// `PATCH /user` (scope `user`) → private-user.
pub async fn update_authenticated_user(
    State(state): State<AppState>,
    auth: RequireUser,
    Json(body): Json<UpdateUserBody>,
) -> ApiResult<Json<PrivateUser>> {
    auth.require_scope("user")?;
    let mut errors = Vec::new();
    let name = text_field(&mut errors, "name", body.name, 255);
    let blog = text_field(&mut errors, "blog", body.blog, 255);
    let twitter = text_field(&mut errors, "twitter_username", body.twitter_username, 15);
    let company = text_field(&mut errors, "company", body.company, 255);
    let location = text_field(&mut errors, "location", body.location, 255);
    let bio = text_field(&mut errors, "bio", body.bio, 160);
    let email = text_field(&mut errors, "email", body.email, 254);
    let twitter = twitter.map(|t| t.map(|t| t.trim_start_matches('@').to_string()));
    if let Some(Some(email)) = &email {
        let verified: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM user_emails
                             WHERE user_id = $1 AND lower(email) = lower($2) AND verified)",
        )
        .bind(auth.user.id)
        .bind(email)
        .fetch_one(&state.db)
        .await?;
        if !verified {
            errors.push(FieldError::custom(
                "User",
                "email",
                "email must be one of your verified email addresses",
            ));
        }
    }
    if !errors.is_empty() {
        return Err(ApiError::validation(errors));
    }
    let hireable = body.hireable.into_option();
    let mut tx = Tx::begin(&state).await?;
    let user: db::User = sqlx::query_as(&format!(
        "UPDATE users SET
            name = CASE WHEN $2 THEN $3 ELSE name END,
            email = CASE WHEN $4 THEN $5 ELSE email END,
            blog = CASE WHEN $6 THEN $7 ELSE blog END,
            twitter_username = CASE WHEN $8 THEN $9 ELSE twitter_username END,
            company = CASE WHEN $10 THEN $11 ELSE company END,
            location = CASE WHEN $12 THEN $13 ELSE location END,
            hireable = CASE WHEN $14 THEN $15 ELSE hireable END,
            bio = CASE WHEN $16 THEN $17 ELSE bio END,
            updated_at = now()
          WHERE id = $1 RETURNING {}",
        db::User::COLUMNS
    ))
    .bind(auth.user.id)
    .bind(name.is_some())
    .bind(name.flatten())
    .bind(email.is_some())
    .bind(email.clone().flatten())
    .bind(blog.is_some())
    .bind(blog.flatten())
    .bind(twitter.is_some())
    .bind(twitter.flatten())
    .bind(company.is_some())
    .bind(company.flatten())
    .bind(location.is_some())
    .bind(location.flatten())
    .bind(hireable.is_some())
    .bind(hireable.flatten())
    .bind(bio.is_some())
    .bind(bio.flatten())
    .fetch_one(&mut *tx)
    .await?;
    if let Some(email) = &email {
        // Keep the primary email's visibility in line with the public email.
        sqlx::query(
            "UPDATE user_emails SET visibility = CASE
                 WHEN $2::text IS NOT NULL AND lower(email) = lower($2) THEN 'public'
                 ELSE 'private' END
              WHERE user_id = $1 AND is_primary",
        )
        .bind(user.id)
        .bind(email.as_deref())
        .execute(&mut *tx)
        .await?;
    }
    util::sync_profile(&mut tx, &state.urls, &user).await?;
    audit::log(
        &mut *tx,
        Some(&auth.user),
        "user.update",
        audit::Target::User(user.id),
        json!({}),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(private_user_json(&state, &user).await?))
}
