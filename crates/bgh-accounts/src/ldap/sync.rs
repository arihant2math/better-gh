//! LDAP user and team sync: the periodic `accounts.ldap_sync` service, the
//! per-user / per-team jobs queued by the GHES `/admin/ldap/*` endpoints,
//! and [`sync_all`] (also `POST /_bgh/admin/ldap/sync`).

use std::collections::BTreeSet;
use std::time::Duration;

use bgh_core::audit;
use bgh_core::db::AdvisoryLock;
use bgh_core::jobs::JobPayload;
use bgh_core::prelude::*;
use bgh_core::settings::LdapSettings;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::{Directory, PROVIDER, Roles, apply_profile, config, normalize_dn, sync_user_teams};
use crate::group_sync;

/// Outcome of a sync run.
#[derive(Debug, Default, Clone, Serialize, PartialEq, Eq)]
pub struct SyncReport {
    pub users: usize,
    pub suspended: usize,
    pub teams: usize,
    pub team_members_added: usize,
    pub team_members_removed: usize,
}

/// Linked accounts: `(user_id, normalized DN)`.
async fn linked_users(state: &AppState, user_id: Option<i64>) -> ApiResult<Vec<(i64, String)>> {
    Ok(sqlx::query_as(
        "SELECT user_id, subject FROM user_identities
          WHERE provider = $1 AND ($2::bigint IS NULL OR user_id = $2) ORDER BY user_id",
    )
    .bind(PROVIDER)
    .bind(user_id)
    .fetch_all(&state.db)
    .await?)
}

/// Sync every linked account and every LDAP-mapped team.
pub async fn sync_all(state: &AppState) -> ApiResult<SyncReport> {
    let Some(cfg) = config(state).await? else {
        return Ok(SyncReport::default());
    };
    let mut dir = Directory::connect(&cfg).await?;
    let res = run(state, &cfg, &mut dir, None, None).await;
    dir.close().await;
    res
}

/// Sync one linked account (profile, suspension, its team memberships).
pub async fn sync_user(state: &AppState, user_id: i64) -> ApiResult<SyncReport> {
    let Some(cfg) = config(state).await? else {
        return Ok(SyncReport::default());
    };
    let mut dir = Directory::connect(&cfg).await?;
    let res = run(state, &cfg, &mut dir, Some(user_id), Some(None)).await;
    dir.close().await;
    res
}

/// Sync one LDAP-mapped team.
pub async fn sync_team(state: &AppState, team_id: i64) -> ApiResult<SyncReport> {
    let Some(cfg) = config(state).await? else {
        return Ok(SyncReport::default());
    };
    let mut dir = Directory::connect(&cfg).await?;
    let mut report = SyncReport::default();
    let res = sync_team_with(state, &mut dir, team_id, &mut report).await;
    dir.close().await;
    res.map(|_| report)
}

/// `user`: only that account; `teams`: `None` = all mapped teams, `Some(None)`
/// = only the user's memberships.
async fn run(
    state: &AppState,
    cfg: &LdapSettings,
    dir: &mut Directory,
    user: Option<i64>,
    teams: Option<Option<i64>>,
) -> ApiResult<SyncReport> {
    let mut report = SyncReport::default();
    for (user_id, dn) in linked_users(state, user).await? {
        let Some(local) = db::User::find(&state.db, user_id).await? else {
            continue;
        };
        report.users += 1;
        let entry = dir.user_by_dn(&dn).await?;
        let roles = match &entry {
            Some(e) => Some(Roles::load(dir, cfg, e).await?),
            None => None,
        };
        match (entry, roles) {
            (Some(entry), Some(roles)) if !entry.disabled && roles.allowed => {
                let mut tx = Tx::begin(state).await?;
                apply_profile(&mut tx, &local, &entry, roles.admin).await?;
                tx.commit().await?;
                if teams == Some(None) {
                    let (a, r) = sync_user_teams(state, dir, &local, &entry).await?;
                    report.team_members_added += a;
                    report.team_members_removed += r;
                }
            }
            (entry, _) => {
                let why = match entry {
                    None => "LDAP entry not found",
                    Some(e) if e.disabled => "LDAP account disabled",
                    Some(_) => "not a member of the LDAP restricted group",
                };
                if suspend(state, &local, why).await? {
                    report.suspended += 1;
                }
            }
        }
    }
    if teams.is_none() {
        let team_ids: BTreeSet<i64> = group_sync::mappings(state, PROVIDER)
            .await?
            .into_iter()
            .map(|(t, _)| t)
            .collect();
        for team_id in team_ids {
            sync_team_with(state, dir, team_id, &mut report).await?;
        }
    }
    Ok(report)
}

/// Suspend an account on behalf of LDAP sync (returns whether it changed).
/// Site administrators are demoted first unless they are the last one, in
/// which case they are left alone (logged).
async fn suspend(state: &AppState, user: &db::User, reason: &str) -> ApiResult<bool> {
    if user.is_suspended() {
        return Ok(false);
    }
    let mut tx = Tx::begin(state).await?;
    if user.site_admin {
        let others: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM users WHERE site_admin AND type = 'User' AND id <> $1
               AND suspended_at IS NULL",
        )
        .bind(user.id)
        .fetch_one(&mut *tx)
        .await?;
        if others == 0 {
            tracing::warn!(user = %user.login, reason, "LDAP sync: not suspending the last site administrator");
            return Ok(false);
        }
        sqlx::query("UPDATE users SET site_admin = false, updated_at = now() WHERE id = $1")
            .bind(user.id)
            .execute(&mut *tx)
            .await?;
    }
    let reason = format!("LDAP: {reason}");
    sqlx::query(
        "UPDATE users SET suspended_at = now(), suspended_reason = $2, updated_at = now()
          WHERE id = $1",
    )
    .bind(user.id)
    .bind(&reason)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO ldap_user_sync (user_id, synced_at, suspended_by_sync) VALUES ($1, now(), true)
         ON CONFLICT (user_id) DO UPDATE SET synced_at = now(), suspended_by_sync = true",
    )
    .bind(user.id)
    .execute(&mut *tx)
    .await?;
    audit::log(
        &mut *tx,
        None,
        "user.suspend",
        audit::Target::User(user.id),
        json!({ "login": user.login, "reason": reason, "ldap": true }),
    )
    .await?;
    tx.emit(Event::UserAccountChanged {
        user_id: user.id,
        login: user.login.clone(),
        action: "suspended".into(),
        actor_id: user.id,
        data: json!({ "reason": reason, "ldap": true }),
    });
    tx.commit().await?;
    bgh_core::auth::destroy_user_sessions(state, user.id).await?;
    Ok(true)
}

async fn sync_team_with(
    state: &AppState,
    dir: &mut Directory,
    team_id: i64,
    report: &mut SyncReport,
) -> ApiResult<()> {
    let groups = group_sync::team_groups(state, PROVIDER, team_id).await?;
    if groups.is_empty() {
        return Ok(());
    }
    let mut dns = Vec::new();
    let mut uids = Vec::new();
    for g in &groups {
        if let Some(m) = dir.group_members(g).await? {
            dns.extend(m.dns);
            uids.extend(m.uids);
        }
    }
    // Members that have (active) linked accounts.
    let user_ids: BTreeSet<i64> = sqlx::query_scalar::<_, i64>(
        "SELECT DISTINCT u.id FROM users u
           LEFT JOIN user_identities i ON i.user_id = u.id AND i.provider = $1
          WHERE u.type = 'User' AND u.suspended_at IS NULL
            AND (i.subject = ANY($2)
                 OR (lower(u.login) = ANY($3)
                     AND EXISTS (SELECT 1 FROM user_identities j
                                  WHERE j.user_id = u.id AND j.provider = $1)))",
    )
    .bind(PROVIDER)
    .bind(&dns)
    .bind(&uids)
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .collect();
    let (added, removed) =
        group_sync::set_team_members(state, team_id, PROVIDER, &user_ids).await?;
    report.teams += 1;
    report.team_members_added += added;
    report.team_members_removed += removed;
    Ok(())
}

/// Map a local account to a directory entry (`None` removes the link).
pub async fn set_user_mapping(
    state: &AppState,
    actor: &db::User,
    user: &db::User,
    dn: Option<&str>,
) -> ApiResult<Option<String>> {
    let mut tx = Tx::begin(state).await?;
    sqlx::query("DELETE FROM user_identities WHERE provider = $1 AND user_id = $2")
        .bind(PROVIDER)
        .bind(user.id)
        .execute(&mut *tx)
        .await?;
    let dn = dn.map(normalize_dn).filter(|d| !d.is_empty());
    if let Some(dn) = &dn {
        sqlx::query(
            "INSERT INTO user_identities (user_id, provider, subject) VALUES ($1, $2, $3)
             ON CONFLICT (provider, subject) DO UPDATE SET user_id = EXCLUDED.user_id",
        )
        .bind(user.id)
        .bind(PROVIDER)
        .bind(dn)
        .execute(&mut *tx)
        .await?;
    }
    audit::log(
        &mut *tx,
        Some(actor),
        "user.ldap_mapping",
        audit::Target::User(user.id),
        json!({ "login": user.login, "ldap_dn": dn }),
    )
    .await?;
    tx.commit().await?;
    Ok(dn)
}

/// Map a team to a directory group (`None` removes the mapping).
pub async fn set_team_mapping(
    state: &AppState,
    actor: &db::User,
    team: &db::Team,
    dn: Option<&str>,
) -> ApiResult<Option<String>> {
    let dn = dn.map(normalize_dn).filter(|d| !d.is_empty());
    let mut tx = Tx::begin(state).await?;
    group_sync::set_team_groups(&mut tx, PROVIDER, team.id, dn.as_slice()).await?;
    audit::log(
        &mut *tx,
        Some(actor),
        "team.ldap_mapping",
        audit::Target::Team {
            id: team.id,
            org_id: team.org_id,
        },
        json!({ "team": team.slug, "ldap_dn": dn }),
    )
    .await?;
    tx.commit().await?;
    Ok(dn)
}

// ---------------------------------------------------------------------------
// Jobs and the periodic service
// ---------------------------------------------------------------------------

/// Full sync (`POST /_bgh/admin/ldap/sync`).
#[derive(Debug, Serialize, Deserialize)]
pub struct LdapSyncAll {}

impl JobPayload for LdapSyncAll {
    const KIND: &'static str = "accounts.ldap_sync";
    const MAX_ATTEMPTS: i32 = 3;
}

/// One user (`POST /admin/ldap/users/{u}/sync`).
#[derive(Debug, Serialize, Deserialize)]
pub struct LdapSyncUser {
    pub user_id: i64,
}

impl JobPayload for LdapSyncUser {
    const KIND: &'static str = "accounts.ldap_sync_user";
    const MAX_ATTEMPTS: i32 = 3;
}

/// One team (`POST /admin/ldap/teams/{id}/sync`).
#[derive(Debug, Serialize, Deserialize)]
pub struct LdapSyncTeam {
    pub team_id: i64,
}

impl JobPayload for LdapSyncTeam {
    const KIND: &'static str = "accounts.ldap_sync_team";
    const MAX_ATTEMPTS: i32 = 3;
}

fn job_err(e: ApiError) -> anyhow::Error {
    anyhow::anyhow!("LDAP sync failed: {e:?}")
}

pub async fn sync_all_job(state: AppState, _job: LdapSyncAll) -> anyhow::Result<()> {
    let report = sync_all(&state).await.map_err(job_err)?;
    tracing::info!(?report, "LDAP sync finished");
    Ok(())
}

pub async fn sync_user_job(state: AppState, job: LdapSyncUser) -> anyhow::Result<()> {
    sync_user(&state, job.user_id).await.map_err(job_err)?;
    Ok(())
}

pub async fn sync_team_job(state: AppState, job: LdapSyncTeam) -> anyhow::Result<()> {
    sync_team(&state, job.team_id).await.map_err(job_err)?;
    Ok(())
}

/// How often the service checks whether a sync is due.
const TICK: Duration = Duration::from_secs(300);

/// Advisory-lock key of the sync run: `hashtext('bgh_ldap_sync')`, the key
/// earlier versions took in SQL.
const LOCK_KEY: i64 = -2_145_784_662;

/// The `accounts.ldap_sync` service: runs [`sync_all`] every
/// `sync_interval_hours` while LDAP and sync are enabled. One process runs
/// it per interval (Redis `SET NX` on the schedule key) and runs never
/// overlap (pg advisory lock).
pub async fn service(state: AppState, shutdown: CancellationToken) -> anyhow::Result<()> {
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return Ok(()),
            _ = tokio::time::sleep(TICK) => {}
        }
        if let Err(e) = tick(&state).await {
            tracing::warn!(error = ?e, "LDAP sync failed");
        }
    }
}

async fn tick(state: &AppState) -> anyhow::Result<()> {
    let Some(cfg) = config(state).await.map_err(job_err)? else {
        return Ok(());
    };
    if !cfg.sync_enabled {
        return Ok(());
    }
    let interval = u64::from(cfg.sync_interval_hours.max(1)) * 3600;
    let mut redis = state.redis.clone();
    let due: bool = redis::cmd("SET")
        .arg(state.redis_key("ldap_sync:scheduled"))
        .arg(1)
        .arg("NX")
        .arg("EX")
        .arg(interval)
        .query_async::<Option<String>>(&mut redis)
        .await?
        .is_some();
    if !due {
        return Ok(());
    }
    let Some(lock) = AdvisoryLock::try_acquire(&state.db, LOCK_KEY).await? else {
        return Ok(());
    };
    let res = sync_all(state).await;
    lock.release().await;
    let report = res.map_err(job_err)?;
    tracing::info!(?report, "LDAP sync finished");
    Ok(())
}
