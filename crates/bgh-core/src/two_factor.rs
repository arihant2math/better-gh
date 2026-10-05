//! Second-factor state (`user_two_factor`, one row per user that started
//! enrollment; `enabled_at IS NULL` = setup pending, recovery codes in
//! `user_recovery_codes`; schema in `0100_accounts.sql`).
//!
//! Enrollment and verification flows live in bgh-accounts; site admins can
//! see the status and force-disable it (bgh-admin).

use chrono::{DateTime, Utc};
use sqlx::PgExecutor;

/// When 2FA was enabled for `user_id`, or `None` if it isn't.
pub async fn enabled_at(
    db: impl PgExecutor<'_>,
    user_id: i64,
) -> Result<Option<DateTime<Utc>>, sqlx::Error> {
    Ok(sqlx::query_scalar::<_, Option<DateTime<Utc>>>(
        "SELECT enabled_at FROM user_two_factor WHERE user_id = $1",
    )
    .bind(user_id)
    .fetch_optional(db)
    .await?
    .flatten())
}

/// Remove the second factor (and recovery codes). Returns whether an
/// enabled one existed.
pub async fn disable(db: impl PgExecutor<'_>, user_id: i64) -> Result<bool, sqlx::Error> {
    let enabled: Option<bool> = sqlx::query_scalar(
        "WITH codes AS (DELETE FROM user_recovery_codes WHERE user_id = $1)
         DELETE FROM user_two_factor WHERE user_id = $1 RETURNING enabled_at IS NOT NULL",
    )
    .bind(user_id)
    .fetch_optional(db)
    .await?;
    Ok(enabled.unwrap_or(false))
}
