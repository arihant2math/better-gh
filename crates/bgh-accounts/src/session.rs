//! Web-client session endpoints: sign-up, login, logout.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bgh_core::audit;
use bgh_core::auth;
use bgh_core::prelude::*;
use serde::Deserialize;
use serde_json::json;

use crate::users::{self, NewAccount};

#[derive(Debug, Deserialize)]
pub struct SignupBody {
    #[serde(default)]
    pub login: String,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub password: String,
    pub name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct LoginBody {
    /// Login or verified email.
    #[serde(default)]
    pub login: String,
    #[serde(default)]
    pub password: String,
}

fn user_agent(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
}

fn client_ip(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(str::trim)
}

async fn start_session(
    state: &AppState,
    headers: &HeaderMap,
    user: &db::User,
    status: StatusCode,
) -> ApiResult<Response> {
    let token =
        auth::create_session(state, user.id, user_agent(headers), client_ip(headers)).await?;
    let body = users::private_user_json(state, user).await?;
    Ok((
        status,
        [(
            header::SET_COOKIE,
            auth::session_cookie(&state.config, &token),
        )],
        Json(body),
    )
        .into_response())
}

/// `POST /_bgh/signup` → 201 + session cookie. The first account becomes
/// site admin. 403 when sign-up is disabled.
pub async fn signup(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<SignupBody>,
) -> ApiResult<Response> {
    if !state.config.signup_enabled {
        return Err(ApiError::forbidden("Sign up is disabled on this instance."));
    }
    bgh_core::settings::check_signup(&state, body.email.trim()).await?;
    let user = users::create_user(
        &state,
        NewAccount {
            login: body.login.trim(),
            email: body.email.trim(),
            password: &body.password,
            name: body.name.as_deref(),
            site_admin: None,
        },
        None,
    )
    .await?;
    start_session(&state, &headers, &user, StatusCode::CREATED).await
}

/// `POST /_bgh/session` → 200 + session cookie, or 401.
pub async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<LoginBody>,
) -> ApiResult<Response> {
    if body.login.is_empty() || body.password.is_empty() {
        return Err(ApiError::bad_credentials());
    }
    let ip = auth::client_ip(&headers);
    let Some(user) = auth::verify_login(&state, body.login.trim(), &body.password).await? else {
        audit::log_with_ip(
            &state.db,
            None,
            "user.failed_login",
            audit::Target::None,
            json!({ "login": body.login.trim() }),
            ip.as_deref(),
        )
        .await?;
        return Err(ApiError::bad_credentials());
    };
    if user.is_suspended() {
        audit::log_with_ip(
            &state.db,
            Some(&user),
            "user.failed_login",
            audit::Target::User(user.id),
            json!({ "reason": "suspended" }),
            ip.as_deref(),
        )
        .await?;
        return Err(ApiError::forbidden("Sorry. Your account was suspended."));
    }
    audit::log_with_ip(
        &state.db,
        Some(&user),
        "user.login",
        audit::Target::User(user.id),
        json!({}),
        ip.as_deref(),
    )
    .await?;
    start_session(&state, &headers, &user, StatusCode::OK).await
}

/// `DELETE /_bgh/session` → 204, clears the cookie.
pub async fn logout(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<Response> {
    let caller = auth::authenticate(&state, &headers, Default::default())
        .await
        .ok()
        .flatten();
    if let Some(token) = auth::cookie(&headers, auth::SESSION_COOKIE) {
        auth::destroy_session(&state, &token).await?;
    }
    if let Some(ctx) = &caller {
        audit::log_with_ip(
            &state.db,
            Some(&ctx.user),
            "user.logout",
            audit::Target::User(ctx.user.id),
            json!({}),
            auth::client_ip(&headers).as_deref(),
        )
        .await?;
    }
    Ok((
        StatusCode::NO_CONTENT,
        [(
            header::SET_COOKIE,
            auth::clear_session_cookie(&state.config),
        )],
    )
        .into_response())
}
