//! Account and repository operations shared by the GHES-compatible admin
//! API (`/admin/...`) and the admin UI endpoints (`/_bgh/admin/...`).
//! Every operation runs in one transaction, is audit-logged and emits the
//! events global webhooks are built from.

use axum::http::HeaderMap;
use bgh_core::audit::Target;
use bgh_core::prelude::*;
use bgh_core::sync;
use serde_json::json;

use crate::common::{self, account_target, log, login_taken, repo_target};

fn user_event(
    user: &db::User,
    action: &str,
    actor: &AuthContext,
    data: serde_json::Value,
) -> Event {
    if user.is_org() {
        Event::OrganizationChanged {
            org_id: user.id,
            login: user.login.clone(),
            action: action.into(),
            actor_id: actor.user.id,
            data,
        }
    } else {
        Event::UserAccountChanged {
            user_id: user.id,
            login: user.login.clone(),
            action: action.into(),
            actor_id: actor.user.id,
            data,
        }
    }
}

/// Create a user account without a password (GHES `POST /admin/users`:
/// the user signs in through an external provider or after a password
/// reset).
pub async fn create_user(
    state: &AppState,
    actor: &AuthContext,
    headers: &HeaderMap,
    login: &str,
    email: Option<&str>,
    suspended: bool,
) -> ApiResult<db::User> {
    common::validate_login("User", login)?;
    if let Some(email) = email
        && !bgh_accounts::validate::is_valid_email(email)
    {
        return Err(ApiError::invalid_field(FieldError::invalid(
            "User", "email",
        )));
    }
    let mut tx = Tx::begin(state).await?;
    let mut user = db::NewUser {
        login,
        email,
        name: None,
        password_hash: None,
        site_admin: false,
        // Site admins vouch for the address they type in.
        email_verified: true,
    }
    .insert(&mut tx)
    .await
    .map_err(login_taken("User"))?;
    if suspended {
        user = sqlx::query_as(&format!(
            "UPDATE users SET suspended_at = now(), updated_at = now() WHERE id = $1 RETURNING {}",
            db::User::COLUMNS
        ))
        .bind(user.id)
        .fetch_one(&mut *tx)
        .await?;
    }
    log(
        &mut tx,
        actor,
        headers,
        "user.create",
        Target::User(user.id),
        json!({ "login": user.login, "suspended": suspended }),
    )
    .await?;
    tx.emit(user_event(&user, "created", actor, json!({})));
    tx.commit().await?;
    Ok(user)
}

/// Rename a user or organization. Repository URLs follow (repos are stored
/// by id); every owned repository gets a sync update with its new name.
pub async fn rename_account(
    state: &AppState,
    actor: &AuthContext,
    headers: &HeaderMap,
    account: &db::User,
    new_login: &str,
) -> ApiResult<db::User> {
    let resource = if account.is_org() {
        "Organization"
    } else {
        "User"
    };
    common::validate_login(resource, new_login)?;
    if new_login == account.login {
        return Ok(account.clone());
    }
    let mut tx = Tx::begin(state).await?;
    // Redirects for the old login and every owned repository, login
    // reservation and sync (bgh_core::lifecycle).
    let renamed = bgh_core::lifecycle::rename_account_in(&mut tx, account, new_login).await?;
    let action = if renamed.is_org() {
        "org.rename"
    } else {
        "user.rename"
    };
    log(
        &mut tx,
        actor,
        headers,
        action,
        account_target(&renamed),
        json!({ "login": renamed.login, "old_login": account.login }),
    )
    .await?;
    tx.emit(user_event(
        &renamed,
        "renamed",
        actor,
        json!({ "login": { "from": account.login } }),
    ));
    tx.commit().await?;
    Ok(renamed)
}

/// Move a repository to `new_owner` inside `tx` (name conflicts → 422).
/// Team grants of the old organization are dropped; `internal` becomes
/// `private` for user owners; the new owner stops being a collaborator.
pub async fn transfer_repo_in(
    tx: &mut Tx,
    repo: &db::Repository,
    new_owner: &db::User,
    new_name: Option<&str>,
) -> ApiResult<db::Repository> {
    let name = new_name.unwrap_or(&repo.name);
    let visibility = if repo.visibility == "internal" && !new_owner.is_org() {
        "private"
    } else {
        repo.visibility.as_str()
    };
    let moved: db::Repository = sqlx::query_as(&format!(
        "UPDATE repositories SET owner_id = $2, name = $3, visibility = $4, updated_at = now()
          WHERE id = $1 RETURNING {}",
        db::Repository::COLUMNS
    ))
    .bind(repo.id)
    .bind(new_owner.id)
    .bind(name)
    .bind(visibility)
    .fetch_one(&mut **tx)
    .await
    .map_err(|e| match bgh_core::db::unique_violation(&e).as_deref() {
        Some("repositories_owner_name_key") => ApiError::invalid_field(FieldError::custom(
            "Repository",
            "name",
            format!("{}/{name} already exists", new_owner.login),
        )),
        _ => e.into(),
    })?;
    if moved.owner_id != repo.owner_id {
        sqlx::query("DELETE FROM team_repos WHERE repo_id = $1")
            .bind(repo.id)
            .execute(&mut **tx)
            .await?;
        sqlx::query("DELETE FROM collaborators WHERE repo_id = $1 AND user_id = $2")
            .bind(repo.id)
            .bind(new_owner.id)
            .execute(&mut **tx)
            .await?;
    }
    tx.sync_model(SyncModel::Repo, moved.id, SyncAction::Update)
        .await?;
    Ok(moved)
}

/// Soft-delete a repository (restorable for 90 days; storage is purged by
/// bgh-repos `repos.purge_deleted`).
pub async fn delete_repo_in(
    tx: &mut Tx,
    actor: &AuthContext,
    owner: &db::User,
    repo: &db::Repository,
) -> ApiResult<()> {
    bgh_core::lifecycle::soft_delete_repo_in(tx, actor.user.id, owner, repo).await
}

/// Delete a user or organization. Authored content (issues, comments,
/// reviews, ...) is kept and attributed to the `ghost` user. Owned
/// repositories move to `transfer_to` when given, otherwise they are
/// deleted.
pub async fn delete_account(
    state: &AppState,
    actor: &AuthContext,
    headers: &HeaderMap,
    account: &db::User,
    transfer_to: Option<&db::User>,
) -> ApiResult<()> {
    if account.id == actor.user.id {
        return Err(ApiError::forbidden("You can't delete your own account."));
    }
    if let Some(t) = transfer_to
        && t.id == account.id
    {
        return Err(ApiError::invalid_field(FieldError::invalid(
            "User",
            "transfer_repositories_to",
        )));
    }
    let mut tx = Tx::begin(state).await?;
    let repos: Vec<db::Repository> = sqlx::query_as(&format!(
        "SELECT {} FROM repositories WHERE owner_id = $1 ORDER BY id FOR UPDATE",
        db::Repository::COLUMNS
    ))
    .bind(account.id)
    .fetch_all(&mut *tx)
    .await?;
    for repo in &repos {
        match transfer_to {
            Some(new_owner) => {
                transfer_repo_in(&mut tx, repo, new_owner, None).await?;
                log(
                    &mut tx,
                    actor,
                    headers,
                    "repo.transfer",
                    repo_target(new_owner, repo.id),
                    json!({ "from": account.login, "to": new_owner.login, "repo": repo.name }),
                )
                .await?;
            }
            None => delete_repo_in(&mut tx, actor, account, repo).await?,
        }
    }
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(account.id)
        .execute(&mut *tx)
        .await?;
    let action = if account.is_org() {
        "org.delete"
    } else {
        "user.destroy"
    };
    log(
        &mut tx,
        actor,
        headers,
        action,
        account_target(account),
        json!({
            "login": account.login,
            "repositories": repos.len(),
            "transferred_to": transfer_to.map(|t| t.login.clone()),
        }),
    )
    .await?;
    if account.is_org() {
        tx.sync_delete(&sync::org_scope(account.id), SyncModel::Org, account.id)
            .await?;
    }
    tx.emit(user_event(account, "deleted", actor, json!({})));
    tx.commit().await?;
    if !account.is_org() {
        bgh_core::auth::destroy_user_sessions(state, account.id).await?;
    }
    Ok(())
}

/// Promote / demote a site administrator. The last site admin can't be
/// demoted.
pub async fn set_site_admin(
    state: &AppState,
    actor: &AuthContext,
    headers: &HeaderMap,
    user: &db::User,
    admin: bool,
) -> ApiResult<()> {
    if user.site_admin == admin {
        return Ok(());
    }
    let mut tx = Tx::begin(state).await?;
    if !admin {
        let others: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM users WHERE site_admin AND type = 'User' AND id <> $1",
        )
        .bind(user.id)
        .fetch_one(&mut *tx)
        .await?;
        if others == 0 {
            return Err(ApiError::unprocessable(
                "You can't demote the last site administrator.",
            ));
        }
    }
    sqlx::query("UPDATE users SET site_admin = $2, updated_at = now() WHERE id = $1")
        .bind(user.id)
        .bind(admin)
        .execute(&mut *tx)
        .await?;
    let (action, event) = if admin {
        ("user.promote", "promoted")
    } else {
        ("user.demote", "demoted")
    };
    log(
        &mut tx,
        actor,
        headers,
        action,
        Target::User(user.id),
        json!({ "login": user.login }),
    )
    .await?;
    tx.emit(user_event(user, event, actor, json!({})));
    tx.commit().await?;
    Ok(())
}

/// Suspend / unsuspend a user. Suspension signs the user out everywhere;
/// tokens stop working immediately (checked on every request).
pub async fn set_suspended(
    state: &AppState,
    actor: &AuthContext,
    headers: &HeaderMap,
    user: &db::User,
    suspend: bool,
    reason: Option<&str>,
) -> ApiResult<()> {
    if suspend {
        if user.id == actor.user.id {
            return Err(ApiError::forbidden("You can't suspend yourself."));
        }
        if user.site_admin {
            return Err(ApiError::forbidden(
                "Site administrators can't be suspended. Demote the user first.",
            ));
        }
    }
    let mut tx = Tx::begin(state).await?;
    let changed = if suspend {
        sqlx::query(
            "UPDATE users SET suspended_at = now(), suspended_reason = $2, updated_at = now()
              WHERE id = $1 AND suspended_at IS NULL",
        )
    } else {
        sqlx::query(
            "UPDATE users SET suspended_at = NULL, suspended_reason = $2, updated_at = now()
              WHERE id = $1 AND suspended_at IS NOT NULL",
        )
    }
    .bind(user.id)
    .bind(reason)
    .execute(&mut *tx)
    .await?
    .rows_affected()
        > 0;
    if !changed {
        return Ok(());
    }
    let (action, event) = if suspend {
        ("user.suspend", "suspended")
    } else {
        ("user.unsuspend", "unsuspended")
    };
    log(
        &mut tx,
        actor,
        headers,
        action,
        Target::User(user.id),
        json!({ "login": user.login, "reason": reason }),
    )
    .await?;
    tx.emit(user_event(user, event, actor, json!({ "reason": reason })));
    tx.commit().await?;
    if suspend {
        bgh_core::auth::destroy_user_sessions(state, user.id).await?;
    }
    Ok(())
}
