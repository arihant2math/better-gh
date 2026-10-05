//! Helpers shared by the admin handlers.

use axum::http::HeaderMap;
use bgh_core::audit::{self, Target};
use bgh_core::prelude::*;
use serde_json::Value;

/// Look up a user or organization by login; 404 if missing.
pub async fn account(state: &AppState, login: &str) -> ApiResult<db::User> {
    db::User::find_by_login(&state.db, login)
        .await?
        .ok_or(ApiError::NotFound)
}

/// Look up a user account (not an organization); 404 otherwise.
pub async fn user(state: &AppState, login: &str) -> ApiResult<db::User> {
    let u = account(state, login).await?;
    if u.is_org() {
        return Err(ApiError::NotFound);
    }
    Ok(u)
}

/// Look up an organization; 404 otherwise.
pub async fn org(state: &AppState, login: &str) -> ApiResult<db::User> {
    let u = account(state, login).await?;
    if !u.is_org() {
        return Err(ApiError::NotFound);
    }
    Ok(u)
}

/// Look up a repository by owner/name.
pub async fn repo(
    state: &AppState,
    owner: &str,
    name: &str,
) -> ApiResult<(db::User, db::Repository)> {
    let owner = account(state, owner).await?;
    let repo = db::Repository::find_by_name(&state.db, owner.id, name)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok((owner, repo))
}

/// Audit target of a repository (with its owning org, if any).
pub fn repo_target(owner: &db::User, repo_id: i64) -> Target {
    Target::Repo {
        id: repo_id,
        org_id: owner.is_org().then_some(owner.id),
    }
}

/// Audit target of an account.
pub fn account_target(u: &db::User) -> Target {
    if u.is_org() {
        Target::Org(u.id)
    } else {
        Target::User(u.id)
    }
}

/// Write an admin audit entry with the caller's IP.
pub async fn log(
    conn: &mut sqlx::PgConnection,
    actor: &AuthContext,
    headers: &HeaderMap,
    action: &str,
    target: Target,
    data: Value,
) -> ApiResult<()> {
    audit::log_with_ip(
        conn,
        Some(&actor.user),
        action,
        target,
        data,
        bgh_core::auth::client_ip(headers).as_deref(),
    )
    .await?;
    Ok(())
}

/// Validate a new login for a user or organization (422 on failure).
pub fn validate_login(resource: &str, login: &str) -> ApiResult<()> {
    if login.is_empty() {
        return Err(ApiError::invalid_field(FieldError::missing_field(
            resource, "login",
        )));
    }
    if !bgh_accounts::validate::is_valid_login(login)
        || bgh_accounts::validate::is_reserved_login(login)
    {
        return Err(ApiError::invalid_field(FieldError::invalid(
            resource, "login",
        )));
    }
    Ok(())
}

/// Map a unique-violation on `users_login_key` to 422 `already_exists`.
pub fn login_taken(resource: &'static str) -> impl Fn(sqlx::Error) -> ApiError {
    move |e| match bgh_core::db::unique_violation(&e).as_deref() {
        Some("users_login_key") => {
            ApiError::invalid_field(FieldError::already_exists(resource, "login"))
        }
        Some("user_emails_email_key") => {
            ApiError::invalid_field(FieldError::already_exists(resource, "email"))
        }
        _ => e.into(),
    }
}

/// Escape `%`, `_` and `\` for a `LIKE` pattern.
pub fn like_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// `ASC` / `DESC` from a `direction` parameter.
pub fn direction(d: Option<&str>, default_desc: bool) -> &'static str {
    match d {
        Some("asc") => "ASC",
        Some("desc") => "DESC",
        _ if default_desc => "DESC",
        _ => "ASC",
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn escapes_like() {
        assert_eq!(super::like_escape("a_b%c\\"), "a\\_b\\%c\\\\");
    }
}
