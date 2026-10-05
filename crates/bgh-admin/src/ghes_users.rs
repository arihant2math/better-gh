//! GHES enterprise-admin user & organization endpoints:
//! `/admin/users`, `/admin/users/{u}/authorizations`,
//! `/users/{u}/site_admin`, `/users/{u}/suspended`,
//! `/admin/organizations/{org}`.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use bgh_core::audit::Target;
use bgh_core::crypto;
use bgh_core::models::api::SimpleUser;
use bgh_core::prelude::*;
use bgh_core::time::ts;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::common::{self, log};
use crate::service;

#[derive(Debug, Deserialize)]
pub struct CreateUserBody {
    #[serde(default)]
    pub login: String,
    pub email: Option<String>,
    #[serde(default)]
    pub suspended: bool,
}

/// `POST /admin/users` → 201 simple-user.
pub async fn create_user(
    State(state): State<AppState>,
    auth: RequireSiteAdmin,
    headers: HeaderMap,
    Json(body): Json<CreateUserBody>,
) -> ApiResult<(StatusCode, Json<SimpleUser>)> {
    let email = body
        .email
        .as_deref()
        .map(str::trim)
        .filter(|e| !e.is_empty());
    let user = service::create_user(
        &state,
        &auth,
        &headers,
        body.login.trim(),
        email,
        body.suspended,
    )
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(SimpleUser::new(&state.urls, &user)),
    ))
}

#[derive(Debug, Deserialize)]
pub struct RenameBody {
    #[serde(default)]
    pub login: String,
}

#[derive(Debug, Serialize)]
pub struct Accepted {
    pub message: &'static str,
    pub url: String,
}

/// `PATCH /admin/users/{username}` → 202 (the rename is applied
/// immediately; the response shape is GHES's "job queued").
pub async fn rename_user(
    State(state): State<AppState>,
    auth: RequireSiteAdmin,
    headers: HeaderMap,
    Path(username): Path<String>,
    Json(body): Json<RenameBody>,
) -> ApiResult<(StatusCode, Json<Accepted>)> {
    let user = common::user(&state, &username).await?;
    let renamed =
        service::rename_account(&state, &auth, &headers, &user, body.login.trim()).await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(Accepted {
            message: "Job queued to rename user. It may take a few minutes to complete.",
            url: state.urls.api(&format!("/users/{}", renamed.login)),
        }),
    ))
}

/// `DELETE /admin/users/{username}` → 204. Owned repositories are deleted;
/// authored content is attributed to `ghost`.
pub async fn delete_user(
    State(state): State<AppState>,
    auth: RequireSiteAdmin,
    headers: HeaderMap,
    Path(username): Path<String>,
) -> ApiResult<StatusCode> {
    let user = common::user(&state, &username).await?;
    service::delete_account(&state, &auth, &headers, &user, None).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `authorization` shape (impersonation OAuth tokens).
#[derive(Debug, Serialize)]
pub struct Authorization {
    pub id: i64,
    pub url: String,
    pub scopes: Vec<String>,
    pub token: String,
    pub token_last_eight: String,
    pub hashed_token: String,
    pub app: AuthorizationApp,
    pub note: Option<String>,
    pub note_url: Option<String>,
    pub updated_at: Timestamp,
    pub created_at: Timestamp,
    pub fingerprint: Option<String>,
    pub user: SimpleUser,
    pub installation: Option<serde_json::Value>,
    pub expires_at: Option<Timestamp>,
}

#[derive(Debug, Serialize)]
pub struct AuthorizationApp {
    pub client_id: String,
    pub name: String,
    pub url: String,
}

fn authorization_json(
    state: &AppState,
    t: &db::AccessToken,
    user: &db::User,
    token: String,
) -> Authorization {
    Authorization {
        id: t.id,
        url: state.urls.api(&format!("/authorizations/{}", t.id)),
        scopes: t.scopes.clone(),
        token,
        token_last_eight: t.token_last_eight.clone(),
        hashed_token: t.token_hash.clone(),
        app: AuthorizationApp {
            client_id: "bgh-site-admin".into(),
            name: "Site admin impersonation".into(),
            url: state.urls.html("/_bgh/admin"),
        },
        note: Some(t.name.clone()).filter(|n| !n.is_empty()),
        note_url: None,
        updated_at: t.created_at.into(),
        created_at: t.created_at.into(),
        fingerprint: None,
        user: SimpleUser::new(&state.urls, user),
        installation: None,
        expires_at: ts(t.expires_at),
    }
}

#[derive(Debug, Deserialize)]
pub struct ImpersonationBody {
    #[serde(default)]
    pub scopes: Vec<String>,
}

/// `POST /admin/users/{username}/authorizations` → 201 with a new
/// impersonation token, or 200 when one with the same scopes exists (its
/// secret can't be shown again, so `token` is empty).
pub async fn create_impersonation_token(
    State(state): State<AppState>,
    auth: RequireSiteAdmin,
    headers: HeaderMap,
    Path(username): Path<String>,
    Json(body): Json<ImpersonationBody>,
) -> ApiResult<(StatusCode, Json<Authorization>)> {
    let user = common::user(&state, &username).await?;
    let mut scopes: Vec<String> = Vec::new();
    for s in body.scopes {
        if !bgh_accounts::tokens::KNOWN_SCOPES.contains(&s.as_str()) || s == "site_admin" {
            return Err(ApiError::invalid_field(FieldError::custom(
                "Authorization",
                "scopes",
                format!("invalid scope {s:?}"),
            )));
        }
        if !scopes.contains(&s) {
            scopes.push(s);
        }
    }
    scopes.sort();

    let existing: Vec<db::AccessToken> = sqlx::query_as(&format!(
        "SELECT {} FROM access_tokens WHERE user_id = $1 AND kind = 'impersonation'
          AND (expires_at IS NULL OR expires_at > now()) ORDER BY id",
        db::AccessToken::COLUMNS
    ))
    .bind(user.id)
    .fetch_all(&state.db)
    .await?;
    if let Some(t) = existing.iter().find(|t| {
        let mut s = t.scopes.clone();
        s.sort();
        s == scopes
    }) {
        return Ok((
            StatusCode::OK,
            Json(authorization_json(&state, t, &user, String::new())),
        ));
    }

    let token = crypto::new_pat();
    let mut tx = Tx::begin(&state).await?;
    let row: db::AccessToken = sqlx::query_as(&format!(
        "INSERT INTO access_tokens (user_id, kind, name, token_hash, token_last_eight, scopes, created_by_id)
         VALUES ($1, 'impersonation', $2, $3, $4, $5, $6) RETURNING {}",
        db::AccessToken::COLUMNS
    ))
    .bind(user.id)
    .bind(format!("Impersonation token created by {}", auth.user.login))
    .bind(crypto::sha256_hex(&token))
    .bind(&token[token.len() - 8..])
    .bind(&scopes)
    .bind(auth.user.id)
    .fetch_one(&mut *tx)
    .await?;
    log(
        &mut tx,
        &auth,
        &headers,
        "oauth_access.create",
        Target::Token(row.id),
        json!({ "user": user.login, "user_id": user.id, "scopes": scopes, "impersonation": true }),
    )
    .await?;
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        Json(authorization_json(&state, &row, &user, token)),
    ))
}

/// `DELETE /admin/users/{username}/authorizations` → 204 (revokes all of
/// the user's impersonation tokens).
pub async fn delete_impersonation_tokens(
    State(state): State<AppState>,
    auth: RequireSiteAdmin,
    headers: HeaderMap,
    Path(username): Path<String>,
) -> ApiResult<StatusCode> {
    let user = common::user(&state, &username).await?;
    let mut tx = Tx::begin(&state).await?;
    let ids: Vec<i64> = sqlx::query_scalar(
        "DELETE FROM access_tokens WHERE user_id = $1 AND kind = 'impersonation' RETURNING id",
    )
    .bind(user.id)
    .fetch_all(&mut *tx)
    .await?;
    if !ids.is_empty() {
        log(
            &mut tx,
            &auth,
            &headers,
            "oauth_access.destroy",
            Target::User(user.id),
            json!({ "user": user.login, "token_ids": ids, "impersonation": true }),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `PUT /users/{username}/site_admin` → 204.
pub async fn promote(
    State(state): State<AppState>,
    auth: RequireSiteAdmin,
    headers: HeaderMap,
    Path(username): Path<String>,
) -> ApiResult<StatusCode> {
    let user = common::user(&state, &username).await?;
    service::set_site_admin(&state, &auth, &headers, &user, true).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /users/{username}/site_admin` → 204.
pub async fn demote(
    State(state): State<AppState>,
    auth: RequireSiteAdmin,
    headers: HeaderMap,
    Path(username): Path<String>,
) -> ApiResult<StatusCode> {
    let user = common::user(&state, &username).await?;
    service::set_site_admin(&state, &auth, &headers, &user, false).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Default, Deserialize)]
pub struct ReasonBody {
    pub reason: Option<String>,
}

/// `PUT /users/{username}/suspended` (`{"reason": ...}`) → 204.
pub async fn suspend(
    State(state): State<AppState>,
    auth: RequireSiteAdmin,
    headers: HeaderMap,
    Path(username): Path<String>,
    Json(body): Json<ReasonBody>,
) -> ApiResult<StatusCode> {
    let user = common::user(&state, &username).await?;
    service::set_suspended(&state, &auth, &headers, &user, true, body.reason.as_deref()).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /users/{username}/suspended` (`{"reason": ...}`) → 204.
pub async fn unsuspend(
    State(state): State<AppState>,
    auth: RequireSiteAdmin,
    headers: HeaderMap,
    Path(username): Path<String>,
    Json(body): Json<ReasonBody>,
) -> ApiResult<StatusCode> {
    let user = common::user(&state, &username).await?;
    service::set_suspended(
        &state,
        &auth,
        &headers,
        &user,
        false,
        body.reason.as_deref(),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `PATCH /admin/organizations/{org}` (`{"login": new}`) → 202.
pub async fn rename_org(
    State(state): State<AppState>,
    auth: RequireSiteAdmin,
    headers: HeaderMap,
    Path(org): Path<String>,
    Json(body): Json<RenameBody>,
) -> ApiResult<(StatusCode, Json<Accepted>)> {
    let org = common::org(&state, &org).await?;
    let renamed = service::rename_account(&state, &auth, &headers, &org, body.login.trim()).await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(Accepted {
            message: "Job queued to rename organization. It may take a few minutes to complete.",
            url: state.urls.org(&renamed.login),
        }),
    ))
}
