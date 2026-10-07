//! LDAP directory authentication and sync.
//!
//! Configured by the `auth_providers.ldap` site setting
//! (`bgh_core::settings::LdapSettings`). When enabled:
//!
//! * **Sign-in.** Web login and git/LFS basic auth ask the directory first
//!   ([`LdapDirectory`], installed as `bgh_core::auth`'s password
//!   directory): find the user by `uid_field` under the search bases (with
//!   the service account), bind as the entry with the given password, check
//!   the restricted group, then provision the local account (JIT) and apply
//!   the directory profile ([`apply_profile`]): name, emails, SSH / GPG keys
//!   (`ldap_synced` rows), site admin from the admin group, and LDAP team
//!   mappings. Accounts are linked in `user_identities` (provider `ldap`,
//!   subject = normalized entry DN); an existing account with the same login
//!   is adopted. Linked accounts never use a built-in password, except site
//!   administrators (break-glass) — see `bgh_core::auth::check_password`.
//! * **Sync** ([`sync`]): the `accounts.ldap_sync` service runs a full sync
//!   every `sync_interval_hours`: suspends linked users whose entry is gone,
//!   disabled or outside the restricted group (and lifts suspensions it made
//!   once they are back), refreshes profiles and admin status, and sets the
//!   members of teams mapped to LDAP groups. GHES endpoints in bgh-admin
//!   (`/admin/ldap/*`) map users and teams and queue per-user / per-team
//!   syncs.
//!
//! Group membership reads `member` / `uniqueMember` (DNs) and `memberUid`
//! (uids) of the group entry; nested groups are not expanded.

pub mod client;
#[cfg(feature = "testing")]
pub mod fake;
pub mod sync;

use std::collections::HashSet;
use std::sync::Arc;

use bgh_core::audit;
use bgh_core::auth::{DirectoryAuth, PasswordDirectory};
use bgh_core::prelude::*;
use bgh_core::settings::{self, LdapSettings};
use futures::FutureExt;
use futures::future::BoxFuture;
use serde_json::json;

pub use client::{DirUser, Directory, LdapError, normalize_dn};

use crate::group_sync;
use crate::users;

/// `user_identities.provider` and `external_group_mappings.provider` of LDAP.
pub const PROVIDER: &str = "ldap";

/// The effective LDAP settings when LDAP is enabled.
pub async fn config(state: &AppState) -> ApiResult<Option<LdapSettings>> {
    let s = settings::load(state).await?;
    Ok(Some(s.auth_providers.ldap.clone()).filter(|l| l.enabled))
}

impl From<LdapError> for ApiError {
    fn from(e: LdapError) -> Self {
        ApiError::unprocessable(e.to_string())
    }
}

/// The LDAP password directory (registered by [`crate::register`]).
pub struct LdapDirectory;

impl PasswordDirectory for LdapDirectory {
    fn authenticate<'a>(
        &'a self,
        state: &'a AppState,
        login: &'a str,
        password: &'a str,
    ) -> BoxFuture<'a, ApiResult<DirectoryAuth>> {
        authenticate(state, login, password).boxed()
    }

    fn manages<'a>(&'a self, state: &'a AppState, user_id: i64) -> BoxFuture<'a, ApiResult<bool>> {
        async move { Ok(linked_dn(&state.db, user_id).await?.is_some()) }.boxed()
    }
}

/// Install [`LdapDirectory`] as the password directory.
pub fn install() {
    bgh_core::auth::set_password_directory(Arc::new(LdapDirectory));
}

/// The (normalized) DN a local account is linked to.
pub async fn linked_dn(db: impl sqlx::PgExecutor<'_>, user_id: i64) -> ApiResult<Option<String>> {
    Ok(sqlx::query_scalar(
        "SELECT subject FROM user_identities WHERE provider = $1 AND user_id = $2
          ORDER BY id LIMIT 1",
    )
    .bind(PROVIDER)
    .bind(user_id)
    .fetch_optional(db)
    .await?)
}

/// Check a password against the directory (see the module docs).
pub async fn authenticate(
    state: &AppState,
    login: &str,
    password: &str,
) -> ApiResult<DirectoryAuth> {
    let Some(cfg) = config(state).await? else {
        return Ok(DirectoryAuth::NotConfigured);
    };
    let mut dir = match Directory::connect(&cfg).await {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(error = %e, "LDAP sign-in: directory unavailable");
            return Ok(DirectoryAuth::Unavailable);
        }
    };
    let res = authenticate_with(state, &cfg, &mut dir, login, password).await;
    dir.close().await;
    res
}

async fn authenticate_with(
    state: &AppState,
    cfg: &LdapSettings,
    dir: &mut Directory,
    login: &str,
    password: &str,
) -> ApiResult<DirectoryAuth> {
    let unavailable = |e: LdapError| {
        tracing::warn!(error = %e, "LDAP sign-in failed");
        Ok(DirectoryAuth::Unavailable)
    };
    let entry = match dir.find_user(login).await {
        Ok(Some(u)) => u,
        Ok(None) => return Ok(DirectoryAuth::UnknownUser),
        Err(e) => return unavailable(e),
    };
    if entry.disabled {
        return Ok(DirectoryAuth::Rejected);
    }
    match dir.check_password(&entry.dn, password).await {
        Ok(true) => {}
        Ok(false) => return Ok(DirectoryAuth::Rejected),
        Err(e) => return unavailable(e),
    }
    let roles = match Roles::load(dir, cfg, &entry).await {
        Ok(r) => r,
        Err(e) => return unavailable(e),
    };
    if !roles.allowed {
        return Ok(DirectoryAuth::Rejected);
    }
    let Some(user) = provision(state, cfg, &entry, roles.admin).await? else {
        return Ok(DirectoryAuth::Rejected);
    };
    if let Err(e) = sync_user_teams(state, dir, &user, &entry).await {
        tracing::warn!(user = %user.login, error = %e, "LDAP team sync at sign-in failed");
    }
    let user = db::User::find(&state.db, user.id)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(DirectoryAuth::Authenticated(Box::new(user)))
}

/// What the configured groups say about an entry.
#[derive(Debug, Clone, Copy)]
pub struct Roles {
    /// Member of the admin group (`None` when no admin group is set).
    pub admin: Option<bool>,
    /// Allowed to sign in (restricted group, when set).
    pub allowed: bool,
}

impl Roles {
    pub async fn load(
        dir: &mut Directory,
        cfg: &LdapSettings,
        entry: &DirUser,
    ) -> Result<Self, LdapError> {
        let member = async |dir: &mut Directory, group: &Option<String>| match group
            .as_deref()
            .map(str::trim)
            .filter(|g| !g.is_empty())
        {
            Some(g) => Ok::<_, LdapError>(Some(
                dir.group_members(g)
                    .await?
                    .is_some_and(|m| m.contains(entry)),
            )),
            None => Ok(None),
        };
        let admin = member(dir, &cfg.admin_group).await?;
        let allowed = member(dir, &cfg.restricted_group).await?.unwrap_or(true);
        Ok(Self { admin, allowed })
    }
}

/// Find (by link, else by login) or create (JIT) the local account of a
/// directory entry and apply its profile. `None` when the entry has no
/// account and JIT provisioning is off.
pub async fn provision(
    state: &AppState,
    cfg: &LdapSettings,
    entry: &DirUser,
    admin: Option<bool>,
) -> ApiResult<Option<db::User>> {
    let dn = normalize_dn(&entry.dn);
    let mut tx = Tx::begin(state).await?;
    let linked: Option<i64> = sqlx::query_scalar(
        "UPDATE user_identities SET last_login_at = now(), email = $3
          WHERE provider = $1 AND subject = $2 RETURNING user_id",
    )
    .bind(PROVIDER)
    .bind(&dn)
    .bind(entry.emails.first())
    .fetch_optional(&mut *tx)
    .await?;
    let user = match linked {
        Some(id) => db::User::find(&mut *tx, id).await?,
        None => {
            // Adopt an existing account with the same login, unless it is
            // linked to another entry.
            let existing: Option<db::User> = sqlx::query_as(&format!(
                "SELECT {} FROM users u WHERE u.type = 'User' AND lower(u.login) = lower($1)
                   AND NOT EXISTS (SELECT 1 FROM user_identities i
                                    WHERE i.user_id = u.id AND i.provider = $2)",
                db::prefixed("u", db::User::COLUMNS)
            ))
            .bind(&entry.uid)
            .bind(PROVIDER)
            .fetch_optional(&mut *tx)
            .await?;
            let user = match existing {
                Some(u) => u,
                None if cfg.jit_provisioning => create(state, &mut tx, entry, admin).await?,
                None => return Ok(None),
            };
            sqlx::query(
                "INSERT INTO user_identities (user_id, provider, subject, email)
                 VALUES ($1, $2, $3, $4)",
            )
            .bind(user.id)
            .bind(PROVIDER)
            .bind(&dn)
            .bind(entry.emails.first())
            .execute(&mut *tx)
            .await?;
            audit::log(
                &mut *tx,
                Some(&user),
                "user.ldap_link",
                audit::Target::User(user.id),
                json!({ "ldap_dn": dn }),
            )
            .await?;
            Some(user)
        }
    };
    let Some(user) = user else {
        return Ok(None);
    };
    apply_profile(&mut tx, &user, entry, admin).await?;
    tx.commit().await?;
    Ok(Some(user))
}

/// A login for a new account from the directory uid.
fn login_from_uid(uid: &str) -> String {
    let mut login: String = uid
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    while login.contains("--") {
        login = login.replace("--", "-");
    }
    login.trim_matches('-').chars().take(39).collect()
}

async fn create(
    state: &AppState,
    tx: &mut Tx,
    entry: &DirUser,
    admin: Option<bool>,
) -> ApiResult<db::User> {
    let login = login_from_uid(&entry.uid);
    if !crate::validate::is_valid_login(&login) || crate::validate::is_reserved_login(&login) {
        return Err(ApiError::unprocessable(format!(
            "LDAP user {:?} has no valid login",
            entry.uid
        )));
    }
    let mut email = None;
    for e in &entry.emails {
        let taken: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM user_emails WHERE lower(email) = lower($1) AND verified)",
        )
        .bind(e)
        .fetch_one(&mut **tx)
        .await?;
        if !taken && e.contains('@') {
            email = Some(e.clone());
            break;
        }
    }
    let host = url::Url::parse(&state.config.base_url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_else(|| "localhost".into());
    let email = email.unwrap_or_else(|| format!("{login}@users.noreply.{host}"));
    let user = users::insert_user(
        tx,
        &login,
        &email,
        entry.name.as_deref(),
        None,
        Some(admin.unwrap_or(false)),
        true,
    )
    .await?;
    audit::log(
        &mut **tx,
        Some(&user),
        "user.create",
        audit::Target::User(user.id),
        json!({ "login": login, "ldap_dn": normalize_dn(&entry.dn) }),
    )
    .await?;
    Ok(user)
}

/// Apply a directory entry to the local account inside `tx`: name, emails,
/// directory SSH / GPG keys, site admin (`admin`, when an admin group is
/// configured; the last site admin is never demoted) and lifting a
/// suspension made by sync.
pub async fn apply_profile(
    tx: &mut Tx,
    user: &db::User,
    entry: &DirUser,
    admin: Option<bool>,
) -> ApiResult<()> {
    let mut changed = false;
    if let Some(name) = entry.name.as_deref()
        && user.name.as_deref() != Some(name)
    {
        sqlx::query("UPDATE users SET name = $2, updated_at = now() WHERE id = $1")
            .bind(user.id)
            .bind(name)
            .execute(&mut **tx)
            .await?;
        changed = true;
    }
    for email in entry.emails.iter().filter(|e| e.contains('@')) {
        // Verified by the directory; addresses owned by others are skipped.
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
    sync_ssh_keys(tx, user, &entry.ssh_keys).await?;
    sync_gpg_keys(tx, user, &entry.gpg_keys).await?;
    if let Some(admin) = admin
        && admin != user.site_admin
    {
        let others: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM users WHERE site_admin AND type = 'User' AND id <> $1
               AND suspended_at IS NULL",
        )
        .bind(user.id)
        .fetch_one(&mut **tx)
        .await?;
        if admin || others > 0 {
            sqlx::query("UPDATE users SET site_admin = $2, updated_at = now() WHERE id = $1")
                .bind(user.id)
                .bind(admin)
                .execute(&mut **tx)
                .await?;
            let action = if admin { "user.promote" } else { "user.demote" };
            audit::log(
                &mut **tx,
                None,
                action,
                audit::Target::User(user.id),
                json!({ "login": user.login, "ldap": true }),
            )
            .await?;
            tx.emit(Event::UserAccountChanged {
                user_id: user.id,
                login: user.login.clone(),
                action: if admin { "promoted" } else { "demoted" }.into(),
                actor_id: user.id,
                data: json!({ "ldap": true }),
            });
            changed = true;
        }
    }
    let lifted = sqlx::query(
        "UPDATE users u SET suspended_at = NULL, suspended_reason = NULL, updated_at = now()
           FROM ldap_user_sync s
          WHERE u.id = $1 AND s.user_id = u.id AND s.suspended_by_sync
            AND u.suspended_at IS NOT NULL",
    )
    .bind(user.id)
    .execute(&mut **tx)
    .await?
    .rows_affected()
        > 0;
    if lifted {
        audit::log(
            &mut **tx,
            None,
            "user.unsuspend",
            audit::Target::User(user.id),
            json!({ "login": user.login, "reason": "LDAP entry active", "ldap": true }),
        )
        .await?;
        tx.emit(Event::UserAccountChanged {
            user_id: user.id,
            login: user.login.clone(),
            action: "unsuspended".into(),
            actor_id: user.id,
            data: json!({ "ldap": true }),
        });
    }
    sqlx::query(
        "INSERT INTO ldap_user_sync (user_id, synced_at, suspended_by_sync)
         VALUES ($1, now(), false)
         ON CONFLICT (user_id) DO UPDATE SET synced_at = now(), suspended_by_sync = false",
    )
    .bind(user.id)
    .execute(&mut **tx)
    .await?;
    if changed {
        tx.sync_user(user.id).await?;
    }
    Ok(())
}

/// Replace the directory SSH keys of `user` with `keys`.
async fn sync_ssh_keys(tx: &mut Tx, user: &db::User, keys: &[String]) -> ApiResult<()> {
    crate::directory_keys::sync_ssh_keys(tx, user, keys, crate::directory_keys::Source::Ldap).await
}

/// Replace the directory GPG keys of `user` with `keys` (armored).
async fn sync_gpg_keys(tx: &mut Tx, user: &db::User, keys: &[String]) -> ApiResult<()> {
    crate::directory_keys::sync_gpg_keys(tx, user, keys, crate::directory_keys::Source::Ldap).await
}

/// Apply the LDAP team mappings to one user (sign-in / user sync).
pub async fn sync_user_teams(
    state: &AppState,
    dir: &mut Directory,
    user: &db::User,
    entry: &DirUser,
) -> ApiResult<(usize, usize)> {
    let maps = group_sync::mappings(state, PROVIDER).await?;
    let mut member_of = HashSet::new();
    let groups: HashSet<&String> = maps.iter().map(|(_, g)| g).collect();
    for group in groups {
        if dir
            .group_members(group)
            .await?
            .is_some_and(|m| m.contains(entry))
        {
            member_of.insert(group.clone());
        }
    }
    group_sync::apply_user_groups(state, user, PROVIDER, &member_of).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logins_from_uids() {
        assert_eq!(login_from_uid("alice"), "alice");
        assert_eq!(login_from_uid("John.Smith"), "John-Smith");
        assert_eq!(login_from_uid("a__b"), "a-b");
    }
}
