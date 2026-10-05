//! Self-service account lifecycle (P50): renaming the authenticated user
//! (`PATCH /user {login}`) and organizations (`PATCH /orgs/{org} {login}`,
//! owners), deleting your account (`DELETE /user`) and organizations
//! (`DELETE /orgs/{org}`).
//!
//! The mechanics (redirects for the old login and every owned repository,
//! login reservation, soft-deleting owned repositories, ghost attribution)
//! are in `bgh_core::lifecycle`, shared with the site-admin endpoints.

use axum::extract::State;
use axum::http::StatusCode;
use bgh_core::audit;
use bgh_core::lifecycle;
use bgh_core::prelude::*;
use bgh_core::ratelimit;
use serde::Deserialize;
use serde_json::json;

use crate::orgs::OrgAccess;
use crate::{util, validate};

fn account_event(
    account: &db::User,
    action: &str,
    actor_id: i64,
    data: serde_json::Value,
) -> Event {
    if account.is_org() {
        Event::OrganizationChanged {
            org_id: account.id,
            login: account.login.clone(),
            action: action.into(),
            actor_id,
            data,
        }
    } else {
        Event::UserAccountChanged {
            user_id: account.id,
            login: account.login.clone(),
            action: action.into(),
            actor_id,
            data,
        }
    }
}

/// Rename `account` to `new_login` on behalf of `actor` (the user
/// themselves, or an owner of the organization). Rate-limited to
/// [`lifecycle::RENAMES_PER_DAY`] per account.
pub async fn rename(
    state: &AppState,
    actor: &db::User,
    account: &db::User,
    new_login: &str,
) -> ApiResult<db::User> {
    let resource = if account.is_org() {
        "Organization"
    } else {
        "User"
    };
    let new_login = new_login.trim();
    if new_login.is_empty() {
        return Err(ApiError::invalid_field(FieldError::missing_field(
            resource, "login",
        )));
    }
    if new_login == account.login {
        return Ok(account.clone());
    }
    if !validate::is_valid_login(new_login) || validate::is_reserved_login(new_login) {
        return Err(ApiError::invalid_field(FieldError::invalid(
            resource, "login",
        )));
    }
    let key = format!("account_rename:{}", account.id);
    if ratelimit::count(state, &key).await >= lifecycle::RENAMES_PER_DAY {
        return Err(ApiError::Status(
            StatusCode::TOO_MANY_REQUESTS,
            format!(
                "This account was renamed too often. You can change its name {} times per day.",
                lifecycle::RENAMES_PER_DAY
            ),
        ));
    }
    let mut tx = Tx::begin(state).await?;
    let renamed = lifecycle::rename_account_in(&mut tx, account, new_login).await?;
    let (action, target) = if renamed.is_org() {
        ("org.rename", audit::Target::Org(renamed.id))
    } else {
        ("user.rename", audit::Target::User(renamed.id))
    };
    audit::log(
        &mut *tx,
        Some(actor),
        action,
        target,
        json!({ "login": renamed.login, "old_login": account.login }),
    )
    .await?;
    tx.emit(account_event(
        &renamed,
        "renamed",
        actor.id,
        json!({ "login": { "from": account.login } }),
    ));
    tx.commit().await?;
    ratelimit::hit(state, &key, 86_400).await.ok();
    Ok(renamed)
}

#[derive(Debug, Default, Deserialize)]
pub struct DeleteAccountBody {
    /// Current password (or a 2FA code for accounts without one).
    #[serde(default)]
    pub password: String,
}

/// `DELETE /user {password}` (browser session) → 204. Owned repositories
/// are soft-deleted, authored content is attributed to `ghost`. Refused
/// while the user is the only owner of an organization or the last site
/// administrator.
pub async fn delete_self(
    State(state): State<AppState>,
    auth: RequireUser,
    body: axum::body::Bytes,
) -> ApiResult<StatusCode> {
    util::require_session(&auth)?;
    let body: DeleteAccountBody = util::optional_json(&body)?;
    crate::twofa::confirm_password(&state, &auth.user, &body.password).await?;
    let orgs = lifecycle::sole_owned_orgs(&state.db, auth.user.id).await?;
    if !orgs.is_empty() {
        return Err(ApiError::unprocessable(format!(
            "You are the only owner of {}. Add another owner or delete these organizations first.",
            orgs.join(", ")
        )));
    }
    if auth.user.site_admin {
        let admins: i64 =
            sqlx::query_scalar("SELECT count(*) FROM users WHERE site_admin AND type = 'User'")
                .fetch_one(&state.db)
                .await?;
        if admins <= 1 {
            return Err(ApiError::unprocessable(
                "You are the last site administrator. Promote another administrator first.",
            ));
        }
    }
    let user = auth.user.clone();
    let mut tx = Tx::begin(&state).await?;
    // Logged first: the audit row keeps the actor's login.
    audit::log(
        &mut *tx,
        Some(&user),
        "user.destroy",
        audit::Target::User(user.id),
        json!({ "login": user.login, "self": true }),
    )
    .await?;
    let repos = lifecycle::delete_account_in(&mut tx, user.id, &user).await?;
    tx.emit(account_event(
        &user,
        "deleted",
        user.id,
        json!({ "repositories": repos }),
    ));
    tx.commit().await?;
    bgh_core::auth::destroy_user_sessions(&state, user.id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /orgs/{org}` (owners) → 202 `{}` like GitHub. The organization's
/// repositories are soft-deleted (restorable by a site admin).
pub async fn delete_org(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path(org): Path<String>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let access = OrgAccess::load(&state, auth.as_ref(), &org).await?;
    let actor = access.require_admin()?.clone();
    let org = access.org.clone();
    let mut tx = Tx::begin(&state).await?;
    audit::log(
        &mut *tx,
        Some(&actor.user),
        "org.delete",
        audit::Target::Org(org.id),
        json!({ "login": org.login }),
    )
    .await?;
    let repos = lifecycle::delete_account_in(&mut tx, actor.user.id, &org).await?;
    tx.emit(account_event(
        &org,
        "deleted",
        actor.user.id,
        json!({ "repositories": repos }),
    ));
    tx.commit().await?;
    Ok((StatusCode::ACCEPTED, Json(json!({}))))
}
