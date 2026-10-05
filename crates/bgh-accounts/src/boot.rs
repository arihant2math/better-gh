//! Web-client boot data and auth endpoints (docs/SYNC_PROTOCOL.md §9-10).
//!
//! * `GET /_bgh/boot` → boot JSON `{user, csrf, config, ts}`
//! * `POST /_bgh/auth/login {login, password}` → 200 boot + session cookie;
//!   422 `{message}` on bad credentials; 401
//!   `{message, twoFactorRequired: true, twoFactorToken}` when the account
//!   has 2FA, then `POST /_bgh/auth/2fa {twoFactorToken, code}` → 200 boot
//!   + cookie
//! * `POST /_bgh/auth/signup {login, email, password}` → 201 boot + cookie
//! * `POST /_bgh/auth/logout` → 204 (emits `Event::SessionEnded` so sync
//!   closes the session's sockets with 4001)
//!
//! [`boot_json`] is also used by bgh-server to inline boot data into
//! `index.html`.

use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bgh_core::auth;
use bgh_core::crypto;
use bgh_core::prelude::*;
use serde::Serialize;
use serde_json::json;

use crate::session::{self, LoginBody, PasswordLogin, SignupBody, TwoFactorBody};
use crate::users::{self, NewAccount};
use crate::util::ClientInfo;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BootUser {
    pub id: i64,
    pub login: String,
    pub name: Option<String>,
    pub avatar_url: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BootConfig {
    pub site_name: String,
    pub signup_enabled: bool,
    pub version: String,
}

/// Boot data (`window.__BGH_BOOT__`).
#[derive(Debug, Clone, Serialize)]
pub struct Boot {
    pub user: Option<BootUser>,
    pub csrf: String,
    pub config: BootConfig,
    pub ts: Timestamp,
}

/// Boot data for `user` with the session cookie value `session_token`.
/// Signed-out boots carry a random (unchecked) CSRF value.
pub fn boot_for(state: &AppState, user: Option<&db::User>, session_token: Option<&str>) -> Boot {
    Boot {
        user: user.map(|u| BootUser {
            id: u.id,
            login: u.login.clone(),
            name: u.name.clone(),
            avatar_url: state.urls.avatar(u.id, u.avatar_url.as_deref()),
        }),
        csrf: match (user, session_token) {
            (Some(_), Some(t)) => auth::csrf_token(t),
            _ => crypto::random_token(40),
        },
        config: BootConfig {
            site_name: state.config.site_name.clone(),
            signup_enabled: state.config.signup_enabled,
            version: env!("CARGO_PKG_VERSION").to_string(),
        },
        ts: chrono::Utc::now().into(),
    }
}

/// Boot data for a request (from its session cookie only).
pub async fn boot_json(state: &AppState, headers: &HeaderMap) -> Boot {
    let token = auth::cookie(headers, auth::SESSION_COOKIE);
    let user = match &token {
        Some(_) => {
            // Only the cookie counts: strip any Authorization header.
            let mut h = HeaderMap::new();
            if let Some(c) = headers.get(header::COOKIE) {
                h.insert(header::COOKIE, c.clone());
            }
            auth::authenticate(state, &h, auth::AuthOptions::default())
                .await
                .ok()
                .flatten()
                .map(|ctx| ctx.user)
        }
        None => None,
    };
    boot_for(state, user.as_ref(), token.as_deref())
}

/// `<script>` tag for `index.html`; the JSON is escaped so it can't close
/// the script element or break out of it.
pub fn boot_script(boot: &Boot) -> String {
    let json = serde_json::to_string(boot).unwrap_or_else(|_| "null".into());
    let safe = json
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029");
    format!("<script>window.__BGH_BOOT__={safe}</script>")
}

fn no_store(mut resp: Response) -> Response {
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

/// `GET /_bgh/boot`
pub async fn get_boot(State(state): State<AppState>, headers: HeaderMap) -> Response {
    no_store(Json(boot_json(&state, &headers).await).into_response())
}

/// Create a session and answer with boot JSON + cookie.
async fn signed_in(
    state: &AppState,
    client: &ClientInfo,
    user: &db::User,
    status: StatusCode,
) -> ApiResult<Response> {
    let token = auth::create_session(
        state,
        user.id,
        client.user_agent.as_deref(),
        Some(&client.ip),
    )
    .await?;
    let boot = boot_for(state, Some(user), Some(&token));
    let mut resp = no_store((status, Json(boot)).into_response());
    resp.headers_mut().insert(
        header::SET_COOKIE,
        auth::session_cookie(&state.config, &token),
    );
    Ok(resp)
}

/// `POST /_bgh/auth/login`
pub async fn login(
    State(state): State<AppState>,
    client: ClientInfo,
    Json(body): Json<LoginBody>,
) -> ApiResult<Response> {
    match session::password_login(&state, &client, &body.login, &body.password).await {
        Ok(PasswordLogin::Done(user)) => signed_in(&state, &client, &user, StatusCode::OK).await,
        Ok(PasswordLogin::TwoFactor(token)) => {
            match body.otp.as_deref().filter(|c| !c.is_empty()) {
                Some(code) => {
                    let user = session::verify_pending_two_factor(&state, &token, code).await?;
                    signed_in(&state, &client, &user, StatusCode::OK).await
                }
                None => {
                    let mut resp = (
                        StatusCode::UNAUTHORIZED,
                        Json(json!({
                            "message": "Two-factor authentication required.",
                            "twoFactorRequired": true,
                            "twoFactorToken": token,
                        })),
                    )
                        .into_response();
                    resp.headers_mut()
                        .insert("x-github-otp", HeaderValue::from_static("required; app"));
                    Ok(resp)
                }
            }
        }
        Err(ApiError::Unauthorized { .. }) => {
            Err(ApiError::unprocessable("Incorrect username or password."))
        }
        Err(e) => Err(e),
    }
}

/// `POST /_bgh/auth/2fa {twoFactorToken, code}` → 200 boot + cookie; 422 on
/// a wrong code; 401 when the pending login expired.
pub async fn two_factor(
    State(state): State<AppState>,
    client: ClientInfo,
    Json(b): Json<TwoFactorBody>,
) -> ApiResult<Response> {
    match session::verify_pending_two_factor(&state, &b.two_factor_token, &b.code).await {
        Ok(user) => signed_in(&state, &client, &user, StatusCode::OK).await,
        Err(ApiError::Unauthorized { message, .. }) if message.contains("failed") => {
            Err(ApiError::unprocessable("Incorrect two-factor code."))
        }
        Err(e) => Err(e),
    }
}

/// `POST /_bgh/auth/signup` → 201 boot + cookie.
pub async fn signup(
    State(state): State<AppState>,
    client: ClientInfo,
    Json(body): Json<SignupBody>,
) -> ApiResult<Response> {
    if !state.config.signup_enabled {
        return Err(ApiError::forbidden("Sign up is disabled on this instance."));
    }
    if bgh_core::ratelimit::hit(&state, &format!("signup_ip:{}", client.ip), 3600).await? > 50 {
        return Err(ApiError::Status(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many sign ups. Please try again later.".into(),
        ));
    }
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
    signed_in(&state, &client, &user, StatusCode::CREATED).await
}

/// `POST /_bgh/auth/logout` → 204, clears the cookie.
pub async fn logout(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<Response> {
    session::end_cookie_session(&state, &headers).await?;
    let mut resp = StatusCode::NO_CONTENT.into_response();
    resp.headers_mut().insert(
        header::SET_COOKIE,
        auth::clear_session_cookie(&state.config),
    );
    Ok(resp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_escaping() {
        let boot = Boot {
            user: Some(BootUser {
                id: 1,
                login: "a".into(),
                name: Some("</script><b>&".into()),
                avatar_url: String::new(),
            }),
            csrf: "x".into(),
            config: BootConfig {
                site_name: "S\u{2028}".into(),
                signup_enabled: true,
                version: "0".into(),
            },
            ts: chrono::Utc::now().into(),
        };
        let s = boot_script(&boot);
        assert!(!s[8..s.len() - 9].contains('<'));
        assert!(s.contains("\\u003c/script\\u003e"));
        assert!(s.contains("\\u2028"));
        assert!(s.contains("\"avatarUrl\""));
    }
}
