//! GitHub App user-to-server tokens (P46): the app's OAuth flow
//! (`/login/oauth/authorize` + `/login/oauth/access_token` with the app's
//! client id and a client secret, see `crate::oauth`) issues `bghu_…`
//! tokens (8 hours) with single-use `bghr_…` refresh tokens (6 months).
//!
//! A user-to-server token acts as the user, limited to what the app can
//! do: the repositories of the app's installations the user can reach
//! (on the user's account or an organization the user belongs to) and the
//! app's permissions (`bgh_core::apps::user_to_server_cap`,
//! `token_permissions`). Elsewhere the user is seen like an anonymous
//! caller. Coverage is computed when the token is minted; installation
//! changes strip repositories from live tokens (`super::revoke_repo`,
//! `super::strip_user_tokens`).

use bgh_core::apps::{AppRow, REFRESH_TOKEN_TTL_SECS, USER_SCOPE_PREFIX, USER_TOKEN_TTL_SECS};
use bgh_core::audit;
use bgh_core::crypto;
use bgh_core::prelude::*;
use chrono::{Duration, Utc};
use serde_json::json;

/// A freshly issued token pair.
pub struct Issued {
    pub token: String,
    pub refresh_token: String,
}

/// The GitHub App whose client id is `client_id`.
pub async fn app_by_client_id(state: &AppState, client_id: &str) -> ApiResult<Option<AppRow>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {} FROM github_apps WHERE client_id = $1",
        AppRow::COLUMNS
    ))
    .bind(client_id)
    .fetch_optional(&state.db)
    .await?)
}

/// Whether `secret` is one of the app's client secrets (records its use).
pub async fn check_client_secret(
    state: &AppState,
    app_id: i64,
    secret: Option<&str>,
) -> ApiResult<bool> {
    let Some(secret) = secret.filter(|s| !s.is_empty()) else {
        return Ok(false);
    };
    let id: Option<i64> = sqlx::query_scalar(
        "UPDATE github_app_client_secrets SET last_used_at = now()
          WHERE app_id = $1 AND secret_hash = $2 RETURNING id",
    )
    .bind(app_id)
    .bind(crypto::sha256_hex(secret))
    .fetch_optional(&state.db)
    .await?;
    Ok(id.is_some())
}

/// Whether `user_id` already authorized the app.
pub async fn authorized(state: &AppState, app_id: i64, user_id: i64) -> ApiResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM github_app_authorizations WHERE app_id = $1 AND user_id = $2)",
    )
    .bind(app_id)
    .bind(user_id)
    .fetch_one(&state.db)
    .await?)
}

/// The scopes of a user-to-server token for `user_id`: the app marker,
/// the repositories of every installation of the app the user can reach,
/// and the app's permission map.
async fn scopes(state: &AppState, app: &AppRow, user_id: i64) -> ApiResult<Vec<String>> {
    let installs: Vec<(i64, i64, String)> = sqlx::query_as(
        "SELECT id, account_id, repository_selection FROM app_installations
          WHERE app_id = $1 AND suspended_at IS NULL
            AND (account_id = $2
                 OR account_id IN (SELECT org_id FROM org_members WHERE user_id = $2))
          ORDER BY id",
    )
    .bind(app.id)
    .bind(user_id)
    .fetch_all(&state.db)
    .await?;
    let selected_ids: Vec<i64> = installs
        .iter()
        .filter(|(_, _, sel)| sel == "selected")
        .map(|(id, _, _)| *id)
        .collect();
    let selected_repos: Vec<i64> = sqlx::query_scalar(
        "SELECT repo_id FROM app_installation_repos WHERE installation_id = ANY($1) ORDER BY repo_id",
    )
    .bind(&selected_ids)
    .fetch_all(&state.db)
    .await?;
    let mut out = vec![format!("{USER_SCOPE_PREFIX}{}", app.id)];
    for (_, account, sel) in &installs {
        if sel == "all" {
            out.extend(bgh_core::apps::repo_scopes(*account, true, &[]));
        }
    }
    out.extend(bgh_core::apps::repo_scopes(0, false, &selected_repos));
    let perms = bgh_core::apps::with_metadata(&app.permissions);
    out.extend(perms.iter().map(|(k, v)| {
        let v = if v == "admin" { "write" } else { v.as_str() };
        format!("{}{k}:{v}", bgh_core::apps::PERMISSION_SCOPE_PREFIX)
    }));
    Ok(out)
}

/// Issue a token pair for `user_id` and remember the authorization.
pub async fn issue(state: &AppState, app: &AppRow, user_id: i64) -> ApiResult<Issued> {
    let scopes = scopes(state, app, user_id).await?;
    let token = crypto::new_user_to_server_token();
    let refresh_token = crypto::new_refresh_token();
    let now = Utc::now();
    let mut tx = Tx::begin(state).await?;
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO access_tokens (user_id, kind, name, token_hash, token_last_eight, scopes,
                expires_at, github_app_id, permissions)
         VALUES ($1, 'app', $2, $3, $4, $5, $6, $7, $8) RETURNING id",
    )
    .bind(user_id)
    .bind(format!("{} (user-to-server)", app.name))
    .bind(crypto::sha256_hex(&token))
    .bind(&token[token.len() - 8..])
    .bind(&scopes)
    .bind(now + Duration::seconds(USER_TOKEN_TTL_SECS))
    .bind(app.id)
    .bind(sqlx::types::Json(bgh_core::apps::with_metadata(
        &app.permissions,
    )))
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO github_app_refresh_tokens (app_id, user_id, token_hash, expires_at)
         VALUES ($1, $2, $3, $4)",
    )
    .bind(app.id)
    .bind(user_id)
    .bind(crypto::sha256_hex(&refresh_token))
    .bind(now + Duration::seconds(REFRESH_TOKEN_TTL_SECS))
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO github_app_authorizations (app_id, user_id) VALUES ($1, $2)
         ON CONFLICT (app_id, user_id) DO UPDATE SET updated_at = now()",
    )
    .bind(app.id)
    .bind(user_id)
    .execute(&mut *tx)
    .await?;
    audit::log(
        &mut *tx,
        None,
        "oauth_access.create",
        audit::Target::Token(id),
        json!({ "integration": app.slug, "user_id": user_id, "user_to_server": true }),
    )
    .await?;
    tx.commit().await?;
    Ok(Issued {
        token,
        refresh_token,
    })
}

/// Redeem a refresh token (single use): the user it was issued to.
pub async fn redeem_refresh(
    state: &AppState,
    app_id: i64,
    refresh: &str,
) -> ApiResult<Option<i64>> {
    Ok(sqlx::query_scalar(
        "DELETE FROM github_app_refresh_tokens
          WHERE app_id = $1 AND token_hash = $2 AND expires_at > now()
          RETURNING user_id",
    )
    .bind(app_id)
    .bind(crypto::sha256_hex(refresh))
    .fetch_optional(&state.db)
    .await?)
}
