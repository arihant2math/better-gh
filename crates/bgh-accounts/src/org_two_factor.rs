//! Organization two-factor requirement (`two_factor_requirement_enabled`).
//!
//! Turning it on (`PATCH /orgs/{org}`, by an owner who has 2FA) removes
//! every member and outside collaborator without 2FA, records what they had
//! in `org_two_factor_removals` and mails them. Invitations to accounts
//! without 2FA are refused (422), accepting one requires 2FA, and a removed
//! account that rejoins gets its teams and repository grants back
//! ([`take_removal`] + [`restore_grants`], called by
//! `orgs::accept_membership`).

use bgh_core::audit;
use bgh_core::mail;
use bgh_core::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::orgs;
use crate::util;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Grant {
    pub repo_id: i64,
    pub permission: String,
}

/// What a removed account had.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Removal {
    pub team_ids: Vec<i64>,
    pub repositories: sqlx::types::Json<Vec<Grant>>,
}

/// Direct repository grants of `user_id` in the org's repositories.
async fn grants(tx: &mut Tx, org_id: i64, user_id: i64) -> ApiResult<Vec<Grant>> {
    Ok(sqlx::query_as(
        "SELECT c.repo_id, c.permission FROM collaborators c
           JOIN repositories r ON r.id = c.repo_id
          WHERE r.owner_id = $1 AND c.user_id = $2 ORDER BY c.repo_id",
    )
    .bind(org_id)
    .bind(user_id)
    .fetch_all(&mut **tx)
    .await?)
}

/// Remove `user_id`'s direct grants (and pending repository invitations) in
/// the org's repositories, recording the access change.
async fn revoke_grants(tx: &mut Tx, org_id: i64, user_id: i64) -> ApiResult<()> {
    let repos: Vec<i64> = sqlx::query_scalar(
        "DELETE FROM collaborators c USING repositories r
          WHERE c.repo_id = r.id AND r.owner_id = $1 AND c.user_id = $2 RETURNING c.repo_id",
    )
    .bind(org_id)
    .bind(user_id)
    .fetch_all(&mut **tx)
    .await?;
    sqlx::query(
        "DELETE FROM repo_invitations i USING repositories r
          WHERE i.repo_id = r.id AND r.owner_id = $1 AND i.invitee_id = $2",
    )
    .bind(org_id)
    .bind(user_id)
    .execute(&mut **tx)
    .await?;
    for repo_id in repos {
        tx.sync_viewer_repo(user_id, repo_id).await?;
        tx.emit(Event::AccessChanged {
            repo_id: Some(repo_id),
            org_id: Some(org_id),
            user_id: Some(user_id),
        });
    }
    Ok(())
}

async fn record(
    tx: &mut Tx,
    org_id: i64,
    user_id: i64,
    kind: &str,
    role: Option<&str>,
    team_ids: &[i64],
    repos: &[Grant],
) -> ApiResult<()> {
    sqlx::query(
        "INSERT INTO org_two_factor_removals (org_id, user_id, kind, role, team_ids, repositories)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (org_id, user_id) DO UPDATE SET kind = EXCLUDED.kind, role = EXCLUDED.role,
             team_ids = EXCLUDED.team_ids, repositories = EXCLUDED.repositories,
             removed_at = now(), reinstated_at = NULL",
    )
    .bind(org_id)
    .bind(user_id)
    .bind(kind)
    .bind(role)
    .bind(team_ids)
    .bind(sqlx::types::Json(repos))
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Accounts of `org` without 2FA: `(user, Some(role))` for members,
/// `(user, None)` for outside collaborators. `except` is the acting owner.
async fn non_compliant(
    state: &AppState,
    org_id: i64,
    except: i64,
) -> ApiResult<Vec<(db::User, Option<String>)>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        role: Option<String>,
        #[sqlx(flatten)]
        user: db::User,
    }
    let rows: Vec<Row> = sqlx::query_as(&format!(
        "SELECT m.role, {} FROM users u
           LEFT JOIN org_members m ON m.org_id = $1 AND m.user_id = u.id
          WHERE u.id <> $2
            AND (m.id IS NOT NULL
                 OR u.id IN (SELECT c.user_id FROM collaborators c
                               JOIN repositories r ON r.id = c.repo_id WHERE r.owner_id = $1))
            AND NOT EXISTS (SELECT 1 FROM user_two_factor t
                             WHERE t.user_id = u.id AND t.enabled_at IS NOT NULL)
          ORDER BY u.id",
        db::prefixed("u", db::User::COLUMNS)
    ))
    .bind(org_id)
    .bind(except)
    .fetch_all(&state.db)
    .await?;
    Ok(rows.into_iter().map(|r| (r.user, r.role)).collect())
}

/// Remove every member and outside collaborator of `org` without 2FA
/// (after the requirement was turned on). Returns the removed logins.
pub async fn enforce(state: &AppState, actor: &db::User, org: &db::User) -> ApiResult<Vec<String>> {
    let mut removed = Vec::new();
    for (user, role) in non_compliant(state, org.id, actor.id).await? {
        let mut tx = Tx::begin(state).await?;
        let repos = grants(&mut tx, org.id, user.id).await?;
        let team_ids: Vec<i64> = sqlx::query_scalar(
            "SELECT tm.team_id FROM team_members tm JOIN teams t ON t.id = tm.team_id
              WHERE t.org_id = $1 AND tm.user_id = $2 ORDER BY tm.team_id",
        )
        .bind(org.id)
        .bind(user.id)
        .fetch_all(&mut *tx)
        .await?;
        let kind = if role.is_some() {
            "member"
        } else {
            "outside_collaborator"
        };
        record(
            &mut tx,
            org.id,
            user.id,
            kind,
            role.as_deref(),
            &team_ids,
            &repos,
        )
        .await?;
        revoke_grants(&mut tx, org.id, user.id).await?;
        audit::log(
            &mut *tx,
            Some(actor),
            "org.remove_two_factor_non_compliant",
            audit::Target::Org(org.id),
            json!({ "user": user.login, "kind": kind }),
        )
        .await?;
        if let Some(to) = util::primary_email(&mut *tx, user.id).await? {
            util::queue_mail(
                &mut tx,
                mail::templates::org_two_factor_removed(
                    &state.config.site_name,
                    &to,
                    &user.login,
                    &org.login,
                    &state.urls.html("/settings/security"),
                ),
            )
            .await?;
        }
        tx.commit().await?;
        if role.is_some() {
            orgs::remove_member(state, actor, org, &user).await?;
        }
        removed.push(user.login);
    }
    Ok(removed)
}

/// The recorded removal of `user_id` from `org_id`, marked reinstated.
pub async fn take_removal(tx: &mut Tx, org_id: i64, user_id: i64) -> ApiResult<Option<Removal>> {
    Ok(sqlx::query_as(
        "UPDATE org_two_factor_removals SET reinstated_at = now()
          WHERE org_id = $1 AND user_id = $2 AND reinstated_at IS NULL
            AND removed_at > now() - interval '3 months'
          RETURNING team_ids, repositories",
    )
    .bind(org_id)
    .bind(user_id)
    .fetch_optional(&mut **tx)
    .await?)
}

/// Restore the direct repository grants of a reinstated account.
pub async fn restore_grants(
    tx: &mut Tx,
    org_id: i64,
    user_id: i64,
    removal: &Removal,
) -> ApiResult<()> {
    for g in removal.repositories.iter() {
        let restored = sqlx::query(
            "INSERT INTO collaborators (repo_id, user_id, permission)
             SELECT r.id, $2, $3 FROM repositories r WHERE r.id = $1 AND r.owner_id = $4
             ON CONFLICT (repo_id, user_id) DO NOTHING",
        )
        .bind(g.repo_id)
        .bind(user_id)
        .bind(&g.permission)
        .bind(org_id)
        .execute(&mut **tx)
        .await?
        .rows_affected();
        if restored > 0 {
            tx.sync_viewer_repo(user_id, g.repo_id).await?;
            tx.emit(Event::AccessChanged {
                repo_id: Some(g.repo_id),
                org_id: Some(org_id),
                user_id: Some(user_id),
            });
        }
    }
    Ok(())
}
