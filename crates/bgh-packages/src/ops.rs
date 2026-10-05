//! Shared write helpers (registry and REST).

use bgh_core::models::db;
use bgh_core::{ApiResult, AppState};
use sqlx::PgConnection;

use crate::model::{self, PackageRow};

/// Live container package `owner/name`, created on first write. A package
/// created with an Actions job token is linked to (and inherits the
/// visibility of) the job's repository.
pub async fn ensure_package(
    state: &AppState,
    owner: &db::User,
    name: &str,
    creator: Option<i64>,
    job_repo: Option<i64>,
) -> ApiResult<PackageRow> {
    if let Some(p) = model::find_package(&state.db, owner.id, "container", name).await? {
        return Ok(p);
    }
    let link = match job_repo {
        Some(id) => db::Repository::find(&state.db, id)
            .await?
            .filter(|r| r.owner_id == owner.id),
        None => None,
    };
    let visibility = link
        .as_ref()
        .map(|r| r.visibility.clone())
        .unwrap_or_else(|| "private".into());
    sqlx::query(
        "INSERT INTO packages (owner_id, name, package_type, visibility, repo_id, created_by)
         VALUES ($1, $2, 'container', $3, $4, $5)
         ON CONFLICT (owner_id, package_type, lower(name)) WHERE deleted_at IS NULL DO NOTHING",
    )
    .bind(owner.id)
    .bind(name)
    .bind(&visibility)
    .bind(link.as_ref().map(|r| r.id))
    .bind(creator)
    .execute(&state.db)
    .await?;
    model::find_package(&state.db, owner.id, "container", name)
        .await?
        .ok_or(bgh_core::ApiError::NotFound)
}

/// Recompute `packages.size`: distinct linked blobs plus manifests.
pub async fn recompute_size(conn: &mut PgConnection, package_id: i64) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE packages SET size =
            coalesce((SELECT sum(b.size) FROM package_blob_links l
                        JOIN package_blobs b ON b.digest = l.digest
                       WHERE l.package_id = $1), 0)
          + coalesce((SELECT sum(octet_length(manifest)) FROM package_versions
                       WHERE package_id = $1), 0)
          WHERE id = $1",
    )
    .bind(package_id)
    .execute(conn)
    .await?;
    Ok(())
}

/// Whether `owner` may store `extra` more bytes under its storage quota;
/// `Err(message)` when not.
pub async fn check_quota(
    state: &AppState,
    owner_id: i64,
    extra: u64,
) -> ApiResult<Result<(), String>> {
    match bgh_core::settings::owner_quota_headroom(state, owner_id).await? {
        Some(h) if h.remaining_kb * 1024 < extra as i64 => Ok(Err(h.message())),
        _ => Ok(Ok(())),
    }
}
