//! Second-factor state (`user_two_factor`, one row per enrolled user).
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
    sqlx::query_scalar("SELECT enabled_at FROM user_two_factor WHERE user_id = $1")
        .bind(user_id)
        .fetch_optional(db)
        .await
}

/// Remove the second factor (and recovery codes). Returns whether one existed.
pub async fn disable(db: impl PgExecutor<'_>, user_id: i64) -> Result<bool, sqlx::Error> {
    Ok(
        sqlx::query("DELETE FROM user_two_factor WHERE user_id = $1")
            .bind(user_id)
            .execute(db)
            .await?
            .rows_affected()
            > 0,
    )
}
