//! Invalidation of cached commit/tag signature verifications (P25).
//!
//! `bgh_repos::signatures` caches one verification result per signed
//! object in `signature_verifications`. A result depends on the signer's
//! keys and on who owns (and verified) the committer's e-mail, so writes to
//! those call these helpers in the same transaction; the next read
//! re-verifies.

use sqlx::PgConnection;

/// Forget results for signatures naming any of `keys` (OpenPGP key ids,
/// uppercase hex, or SSH `SHA256:` fingerprints).
pub async fn forget_keys(conn: &mut PgConnection, keys: &[String]) -> sqlx::Result<()> {
    if keys.is_empty() {
        return Ok(());
    }
    sqlx::query("DELETE FROM signature_verifications WHERE signer_key = ANY($1)")
        .bind(keys)
        .execute(conn)
        .await?;
    Ok(())
}

/// Forget results for objects committed/tagged with `email`.
pub async fn forget_email(conn: &mut PgConnection, email: &str) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM signature_verifications WHERE email = lower($1)")
        .bind(email)
        .execute(conn)
        .await?;
    Ok(())
}
