//! Audit log (`audit_log` table).
//!
//! ```ignore
//! audit::log(&mut *tx, Some(&auth.user), "repo.create",
//!            Target::Repo { id: repo.id, org_id }, json!({"name": name})).await?;
//! ```
//! Action names follow GitHub's dotted style (`repo.create`, `org.add_member`,
//! `team.create`, `user.suspend`, `oauth_access.create`, ...).

use serde_json::Value;
use sqlx::PgExecutor;

use crate::models::db;

/// What an audit entry is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    None,
    Site,
    User(i64),
    Org(i64),
    Repo { id: i64, org_id: Option<i64> },
    Team { id: i64, org_id: i64 },
    Token(i64),
}

impl Target {
    fn parts(self) -> (Option<&'static str>, Option<i64>, Option<i64>, Option<i64>) {
        // (target_type, target_id, org_id, repo_id)
        match self {
            Self::None => (None, None, None, None),
            Self::Site => (Some("site"), None, None, None),
            Self::User(id) => (Some("user"), Some(id), None, None),
            Self::Org(id) => (Some("org"), Some(id), Some(id), None),
            Self::Repo { id, org_id } => (Some("repo"), Some(id), org_id, Some(id)),
            Self::Team { id, org_id } => (Some("team"), Some(id), Some(org_id), None),
            Self::Token(id) => (Some("token"), Some(id), None, None),
        }
    }
}

/// Append an audit log entry. Use inside the transaction of the action.
pub async fn log(
    db: impl PgExecutor<'_>,
    actor: Option<&db::User>,
    action: &str,
    target: Target,
    data: Value,
) -> Result<(), sqlx::Error> {
    log_with_ip(db, actor, action, target, data, None).await
}

/// [`log`] recording the client IP (see [`crate::auth::client_ip`]).
pub async fn log_with_ip(
    db: impl PgExecutor<'_>,
    actor: Option<&db::User>,
    action: &str,
    target: Target,
    data: Value,
    ip: Option<&str>,
) -> Result<(), sqlx::Error> {
    let (target_type, target_id, org_id, repo_id) = target.parts();
    sqlx::query(
        "INSERT INTO audit_log (actor_id, actor_login, action, target_type, target_id, org_id, repo_id, data, ip)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(actor.map(|a| a.id))
    .bind(actor.map(|a| a.login.as_str()))
    .bind(action)
    .bind(target_type)
    .bind(target_id)
    .bind(org_id)
    .bind(repo_id)
    .bind(data)
    .bind(ip)
    .execute(db)
    .await?;
    Ok(())
}
