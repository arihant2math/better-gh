//! Personal access token management for the web client
//! (`/_bgh/tokens`). Requires a browser session: tokens can't mint tokens.

use axum::extract::State;
use axum::http::StatusCode;
use bgh_core::audit;
use bgh_core::auth;
use bgh_core::prelude::*;
use bgh_core::time::ts;
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Scopes a PAT may carry (GitHub classic scopes).
pub const KNOWN_SCOPES: &[&str] = &[
    "repo",
    "repo:status",
    "repo_deployment",
    "public_repo",
    "repo:invite",
    "security_events",
    "admin:repo_hook",
    "write:repo_hook",
    "read:repo_hook",
    "admin:org",
    "write:org",
    "read:org",
    "admin:public_key",
    "write:public_key",
    "read:public_key",
    "admin:org_hook",
    "gist",
    "notifications",
    "user",
    "read:user",
    "user:email",
    "user:follow",
    "project",
    "read:project",
    "delete_repo",
    "write:packages",
    "read:packages",
    "delete:packages",
    "admin:gpg_key",
    "write:gpg_key",
    "read:gpg_key",
    "workflow",
    "site_admin",
];

#[derive(Debug, Deserialize)]
pub struct CreateTokenBody {
    /// Display name (GitHub calls it `note`).
    #[serde(alias = "note")]
    pub name: Option<String>,
    #[serde(default)]
    pub scopes: Vec<String>,
    /// Lifetime in days; omitted = never expires.
    pub expires_in_days: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct TokenJson {
    pub id: i64,
    pub name: String,
    pub scopes: Vec<String>,
    pub token_last_eight: String,
    pub expires_at: Option<Timestamp>,
    pub last_used_at: Option<Timestamp>,
    pub created_at: Timestamp,
    /// Only present in the creation response.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

impl TokenJson {
    fn new(t: &db::AccessToken, token: Option<String>) -> Self {
        Self {
            id: t.id,
            name: t.name.clone(),
            scopes: t.scopes.clone(),
            token_last_eight: t.token_last_eight.clone(),
            expires_at: ts(t.expires_at),
            last_used_at: ts(t.last_used_at),
            created_at: t.created_at.into(),
            token,
        }
    }
}

fn require_session(auth: &AuthContext) -> ApiResult<()> {
    if auth.is_session() {
        Ok(())
    } else {
        Err(ApiError::forbidden(
            "Token management requires a browser session.",
        ))
    }
}

/// `POST /_bgh/tokens` → 201 with the plaintext token (shown once).
pub async fn create_token(
    State(state): State<AppState>,
    auth: RequireUser,
    Json(body): Json<CreateTokenBody>,
) -> ApiResult<(StatusCode, Json<TokenJson>)> {
    require_session(&auth)?;
    let mut scopes: Vec<String> = Vec::new();
    for s in body.scopes {
        if !KNOWN_SCOPES.contains(&s.as_str()) {
            return Err(ApiError::invalid_field(FieldError::custom(
                "AccessToken",
                "scopes",
                format!("unknown scope {s:?}"),
            )));
        }
        if s == "site_admin" && !auth.user.site_admin {
            return Err(ApiError::invalid_field(FieldError::custom(
                "AccessToken",
                "scopes",
                "site_admin scope requires a site administrator",
            )));
        }
        if !scopes.contains(&s) {
            scopes.push(s);
        }
    }
    let expires_at = match body.expires_in_days {
        None => None,
        Some(d) if (1..=3650).contains(&d) => Some(Utc::now() + Duration::days(d)),
        Some(_) => {
            return Err(ApiError::invalid_field(FieldError::invalid(
                "AccessToken",
                "expires_in_days",
            )));
        }
    };
    let name = body.name.unwrap_or_default();
    let mut tx = Tx::begin(&state).await?;
    let (row, token) =
        auth::create_access_token(&mut *tx, auth.user.id, name.trim(), &scopes, expires_at).await?;
    audit::log(
        &mut *tx,
        Some(&auth.user),
        "personal_access_token.create",
        audit::Target::Token(row.id),
        json!({ "scopes": scopes }),
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(TokenJson::new(&row, Some(token)))))
}

/// `GET /_bgh/tokens` → the session user's tokens (without secrets).
pub async fn list_tokens(
    State(state): State<AppState>,
    auth: RequireUser,
) -> ApiResult<Json<Vec<TokenJson>>> {
    require_session(&auth)?;
    let rows: Vec<db::AccessToken> = sqlx::query_as(&format!(
        "SELECT {} FROM access_tokens WHERE user_id = $1 ORDER BY id DESC",
        db::AccessToken::COLUMNS
    ))
    .bind(auth.user.id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(rows.iter().map(|t| TokenJson::new(t, None)).collect()))
}

/// `DELETE /_bgh/tokens/{id}` → 204.
pub async fn delete_token(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    require_session(&auth)?;
    let mut tx = Tx::begin(&state).await?;
    let deleted = sqlx::query("DELETE FROM access_tokens WHERE id = $1 AND user_id = $2")
        .bind(id)
        .bind(auth.user.id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    if deleted == 0 {
        return Err(ApiError::NotFound);
    }
    audit::log(
        &mut *tx,
        Some(&auth.user),
        "personal_access_token.destroy",
        audit::Target::Token(id),
        json!({}),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
