//! Local accounts for SAML subjects (see the module docs of
//! [`crate::saml`]).

use std::collections::HashSet;

use bgh_core::audit;
use bgh_core::prelude::*;
use serde_json::json;

use super::response::Authenticated;
use super::{PROVIDER, Sp};
use crate::directory_keys::{self, Source};
use crate::{group_sync, sso, users};

/// The account linked to a NameID.
pub async fn linked_user(db: impl sqlx::PgExecutor<'_>, name_id: &str) -> ApiResult<Option<i64>> {
    Ok(sqlx::query_scalar(
        "SELECT user_id FROM user_identities WHERE provider = $1 AND subject = $2",
    )
    .bind(PROVIDER)
    .bind(name_id)
    .fetch_optional(db)
    .await?)
}

/// The NameID an account is linked to.
pub async fn linked_name_id(
    db: impl sqlx::PgExecutor<'_>,
    user_id: i64,
) -> ApiResult<Option<String>> {
    Ok(sqlx::query_scalar(
        "SELECT subject FROM user_identities WHERE provider = $1 AND user_id = $2
          ORDER BY id LIMIT 1",
    )
    .bind(PROVIDER)
    .bind(user_id)
    .fetch_optional(db)
    .await?)
}

/// The login the IdP proposes: the username attribute, else the NameID
/// (the local part of an email address).
fn wanted_login(sp: &Sp, a: &Authenticated) -> String {
    sp.settings
        .username_attribute
        .as_deref()
        .filter(|n| !n.trim().is_empty())
        .and_then(|n| a.first(n))
        .unwrap_or(&a.name_id)
        .to_string()
}

fn attr_values(a: &Authenticated, name: &str) -> Vec<String> {
    if name.trim().is_empty() {
        return Vec::new();
    }
    a.attr(name)
        .iter()
        .flat_map(|v| v.lines())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .collect()
}

/// Whether an attribute value means "yes" (`administrator`).
fn truthy(v: &str) -> bool {
    matches!(v.trim().to_ascii_lowercase().as_str(), "true" | "1" | "yes")
}

/// Find, link or create the account of a validated assertion and apply
/// its attributes. `Err` carries a message for the sign-in page.
pub async fn sign_in(
    state: &AppState,
    sp: &Sp,
    a: &Authenticated,
) -> ApiResult<Result<db::User, String>> {
    let s = &sp.settings;
    let wanted = wanted_login(sp, a);
    let full_name = a.first(&s.full_name_attribute).map(str::to_string);
    let emails: Vec<String> = attr_values(a, &s.emails_attribute)
        .into_iter()
        .filter(|e| e.contains('@'))
        .collect();
    let admin: Option<bool> = s
        .admin_attribute
        .as_deref()
        .filter(|n| !n.trim().is_empty())
        .and_then(|n| a.attributes.get(n))
        .map(|v| v.iter().any(|x| truthy(x)));

    let mut tx = Tx::begin(state).await?;
    let linked: Option<i64> = sqlx::query_scalar(
        "UPDATE user_identities SET last_login_at = now(), email = coalesce($3, email)
          WHERE provider = $1 AND subject = $2 RETURNING user_id",
    )
    .bind(PROVIDER)
    .bind(&a.name_id)
    .bind(emails.first())
    .fetch_optional(&mut *tx)
    .await?;
    let user = match linked {
        Some(id) => db::User::find(&mut *tx, id)
            .await?
            .ok_or(ApiError::NotFound)?,
        None => {
            // An account provisioned by SCIM with this userName, else one
            // with the same login (not linked to another SAML subject).
            let scim: Option<i64> = sqlx::query_scalar(
                "SELECT user_id FROM scim_users
                  WHERE org_id IS NULL AND (lower(user_name) = lower($1) OR lower(user_name) = lower($2))
                  ORDER BY lower(user_name) = lower($1) DESC LIMIT 1",
            )
            .bind(&a.name_id)
            .bind(&wanted)
            .fetch_optional(&mut *tx)
            .await?;
            let existing = match scim {
                Some(id) => db::User::find(&mut *tx, id).await?,
                None => {
                    let login = wanted.split('@').next().unwrap_or(&wanted);
                    sqlx::query_as(&format!(
                        "SELECT {} FROM users u WHERE u.type = 'User' AND lower(u.login) = lower($1)
                           AND NOT EXISTS (SELECT 1 FROM user_identities i
                                            WHERE i.user_id = u.id AND i.provider = $2)",
                        db::prefixed("u", db::User::COLUMNS)
                    ))
                    .bind(login)
                    .bind(PROVIDER)
                    .fetch_optional(&mut *tx)
                    .await?
                }
            };
            let user = match existing {
                Some(u) => u,
                None if s.jit_provisioning => {
                    let login = sso::available_login(state, &wanted).await?;
                    let mut email = None;
                    for e in &emails {
                        let taken: bool = sqlx::query_scalar(
                            "SELECT EXISTS (SELECT 1 FROM user_emails WHERE lower(email) = lower($1))",
                        )
                        .bind(e)
                        .fetch_one(&mut *tx)
                        .await?;
                        if !taken {
                            email = Some(e.clone());
                            break;
                        }
                    }
                    let host = url::Url::parse(&state.config.base_url)
                        .ok()
                        .and_then(|u| u.host_str().map(str::to_string))
                        .unwrap_or_else(|| "localhost".into());
                    let email = email.unwrap_or_else(|| format!("{login}@users.noreply.{host}"));
                    if let Err(denied) = sso::jit_signup_allowed(state, &email).await? {
                        return Ok(Err(denied));
                    }
                    let user = users::insert_user(
                        &mut tx,
                        &login,
                        &email,
                        full_name.as_deref(),
                        None,
                        Some(admin.unwrap_or(false)),
                    )
                    .await?;
                    audit::log(
                        &mut *tx,
                        Some(&user),
                        "user.create",
                        audit::Target::User(user.id),
                        json!({ "login": login, "saml": true }),
                    )
                    .await?;
                    user
                }
                None => {
                    return Ok(Err(
                        "No account is linked to this identity. Ask a site administrator to provision it."
                            .into(),
                    ));
                }
            };
            sqlx::query(
                "INSERT INTO user_identities (user_id, provider, subject, email)
                 VALUES ($1, $2, $3, $4)",
            )
            .bind(user.id)
            .bind(PROVIDER)
            .bind(&a.name_id)
            .bind(emails.first())
            .execute(&mut *tx)
            .await?;
            audit::log(
                &mut *tx,
                Some(&user),
                "user.saml_link",
                audit::Target::User(user.id),
                json!({ "name_id": a.name_id }),
            )
            .await?;
            user
        }
    };
    if user.is_suspended() {
        tx.commit().await?;
        return Ok(Ok(user));
    }
    apply_profile(&mut tx, &user, sp, a, full_name.as_deref(), &emails, admin).await?;
    tx.commit().await?;
    if let Some(name) = s
        .groups_attribute
        .as_deref()
        .filter(|n| !n.trim().is_empty())
        && a.attributes.contains_key(name)
    {
        let groups: HashSet<String> = attr_values(a, name).into_iter().collect();
        group_sync::apply_user_groups(state, &user, PROVIDER, &groups).await?;
    }
    Ok(Ok(db::User::find(&state.db, user.id)
        .await?
        .ok_or(ApiError::NotFound)?))
}

/// Name, verified emails, SSH/GPG keys (only when the attributes are
/// present) and site admin (the last site admin is never demoted).
async fn apply_profile(
    tx: &mut Tx,
    user: &db::User,
    sp: &Sp,
    a: &Authenticated,
    full_name: Option<&str>,
    emails: &[String],
    admin: Option<bool>,
) -> ApiResult<()> {
    let s = &sp.settings;
    let mut changed = false;
    if let Some(name) = full_name
        && user.name.as_deref() != Some(name)
    {
        sqlx::query("UPDATE users SET name = $2, updated_at = now() WHERE id = $1")
            .bind(user.id)
            .bind(name)
            .execute(&mut **tx)
            .await?;
        changed = true;
    }
    for email in emails {
        sqlx::query(
            "INSERT INTO user_emails (user_id, email, verified, is_primary)
             SELECT $1, $2, true,
                    NOT EXISTS (SELECT 1 FROM user_emails WHERE user_id = $1 AND is_primary)
              WHERE NOT EXISTS (SELECT 1 FROM user_emails WHERE lower(email) = lower($2))",
        )
        .bind(user.id)
        .bind(email)
        .execute(&mut **tx)
        .await?;
    }
    if a.attributes.contains_key(&s.ssh_keys_attribute) {
        let keys = attr_values(a, &s.ssh_keys_attribute);
        directory_keys::sync_ssh_keys(tx, user, &keys, Source::Saml).await?;
    }
    if a.attributes.contains_key(&s.gpg_keys_attribute) {
        // Armored keys span lines: keep each value whole.
        let keys: Vec<String> = a.attr(&s.gpg_keys_attribute).to_vec();
        directory_keys::sync_gpg_keys(tx, user, &keys, Source::Saml).await?;
    }
    if let Some(admin) = admin
        && admin != user.site_admin
        && set_site_admin(tx, user, admin, json!({ "saml": true })).await?
    {
        changed = true;
    }
    if changed {
        tx.sync_user(user.id).await?;
    }
    Ok(())
}

/// Promote / demote on behalf of an identity provider; the last active
/// site admin is never demoted. Returns whether it changed.
pub async fn set_site_admin(
    tx: &mut Tx,
    user: &db::User,
    admin: bool,
    data: serde_json::Value,
) -> ApiResult<bool> {
    if !admin {
        let others: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM users WHERE site_admin AND type = 'User' AND id <> $1
               AND suspended_at IS NULL",
        )
        .bind(user.id)
        .fetch_one(&mut **tx)
        .await?;
        if others == 0 {
            return Ok(false);
        }
    }
    sqlx::query("UPDATE users SET site_admin = $2, updated_at = now() WHERE id = $1")
        .bind(user.id)
        .bind(admin)
        .execute(&mut **tx)
        .await?;
    let mut log = json!({ "login": user.login });
    if let (Some(l), Some(d)) = (log.as_object_mut(), data.as_object()) {
        l.extend(d.clone());
    }
    audit::log(
        &mut **tx,
        None,
        if admin { "user.promote" } else { "user.demote" },
        audit::Target::User(user.id),
        log,
    )
    .await?;
    tx.emit(Event::UserAccountChanged {
        user_id: user.id,
        login: user.login.clone(),
        action: if admin { "promoted" } else { "demoted" }.into(),
        actor_id: user.id,
        data,
    });
    Ok(true)
}
