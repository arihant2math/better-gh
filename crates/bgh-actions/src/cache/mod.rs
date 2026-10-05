//! Actions cache: storage, GitHub's scoping and matching rules, size limits
//! with LRU eviction, and expiry. Served to jobs by the legacy protocol
//! ([`v1`], `ACTIONS_CACHE_URL`) and the twirp `CacheService` ([`twirp`],
//! `ACTIONS_RESULTS_URL`); archives are uploaded and downloaded through
//! signed blob URLs ([`crate::results::blob`]). REST management lives in
//! [`crate::api::caches`].
//!
//! Matching follows GitHub: entries are scoped to the ref of the run that
//! saved them; a restore searches the run's ref, then the pull request's base
//! branch, then the default branch. In each scope it looks for an exact
//! match of the primary key, then the newest entry whose key starts with the
//! primary key, then with each restore key in order. The version (a hash
//! of the paths and compression method) must always match.

pub mod twirp;
pub mod v1;

use std::path::PathBuf;

use bgh_core::AppState;
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::FromRow;

use crate::runtime::RuntimeJob;

/// Longest accepted key (GitHub's limit).
pub const MAX_KEY_LENGTH: usize = 512;
/// Primary key plus restore keys.
pub const MAX_KEYS: usize = 10;
/// Reservations whose upload never completed are dropped after this long.
pub const STALE_RESERVATION_HOURS: i64 = 24;

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct CacheRow {
    pub id: i64,
    pub repo_id: i64,
    pub key: String,
    pub version: String,
    #[sqlx(rename = "ref")]
    pub git_ref: String,
    pub size_in_bytes: i64,
    pub committed: bool,
    pub run_id: Option<i64>,
    pub job_id: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub last_accessed_at: DateTime<Utc>,
}

impl CacheRow {
    pub const COLUMNS: &'static str = "id, repo_id, key, version, ref, size_in_bytes, committed, \
        run_id, job_id, created_at, last_accessed_at";
}

pub fn dir(state: &AppState) -> PathBuf {
    state.config.data_dir.join("actions").join("caches")
}

/// The committed archive of an entry.
pub fn archive_path(state: &AppState, id: i64) -> PathBuf {
    dir(state).join(id.to_string())
}

/// Where an upload is assembled before commit.
pub fn staging_path(state: &AppState, id: i64) -> PathBuf {
    dir(state).join(format!("{id}.part"))
}

/// Staged Azure blocks of an upload.
pub fn blocks_dir(state: &AppState, id: i64) -> PathBuf {
    dir(state).join(format!("{id}.blocks"))
}

async fn remove_files(state: &AppState, id: i64) {
    let _ = tokio::fs::remove_file(archive_path(state, id)).await;
    let _ = tokio::fs::remove_file(staging_path(state, id)).await;
    let _ = tokio::fs::remove_dir_all(blocks_dir(state, id)).await;
}

/// Validate a primary key or restore key.
pub fn validate_key(key: &str) -> Result<(), String> {
    if key.is_empty() {
        return Err("Key Validation Error: key cannot be empty.".into());
    }
    if key.len() > MAX_KEY_LENGTH {
        return Err(format!(
            "Key Validation Error: {key} cannot be larger than {MAX_KEY_LENGTH} characters."
        ));
    }
    if key.contains(',') {
        return Err(format!(
            "Key Validation Error: {key} cannot contain commas."
        ));
    }
    Ok(())
}

/// Effective size limit of a repository's cache, in bytes.
pub async fn size_limit(state: &AppState, repo_id: i64) -> anyhow::Result<u64> {
    let gb: Option<i32> = sqlx::query_scalar(
        "SELECT repo_cache_size_limit_in_gb FROM actions_cache_policies WHERE repo_id = $1",
    )
    .bind(repo_id)
    .fetch_optional(&state.db)
    .await?;
    let site = state.config.actions.cache_size_limit;
    Ok(match gb {
        Some(gb) => ((gb as u64) << 30).min(site),
        None => site,
    })
}

fn like_prefix(prefix: &str) -> String {
    let mut out = String::with_capacity(prefix.len() + 1);
    for c in prefix.chars() {
        if matches!(c, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('%');
    out
}

/// Find the entry a restore of `keys` (primary first) at `version` gets,
/// searching `scopes` in order, and mark it accessed.
pub async fn lookup(
    state: &AppState,
    repo_id: i64,
    scopes: &[String],
    keys: &[String],
    version: &str,
) -> anyhow::Result<Option<CacheRow>> {
    let Some(primary) = keys.first() else {
        return Ok(None);
    };
    for scope in scopes {
        let exact: Option<CacheRow> = sqlx::query_as(&format!(
            "SELECT {} FROM actions_caches
              WHERE repo_id = $1 AND ref = $2 AND version = $3 AND key = $4 AND committed",
            CacheRow::COLUMNS
        ))
        .bind(repo_id)
        .bind(scope)
        .bind(version)
        .bind(primary)
        .fetch_optional(&state.db)
        .await?;
        let mut found = exact;
        if found.is_none() {
            for k in keys {
                found = sqlx::query_as(&format!(
                    "SELECT {} FROM actions_caches
                      WHERE repo_id = $1 AND ref = $2 AND version = $3 AND key LIKE $4 AND committed
                      ORDER BY created_at DESC, id DESC LIMIT 1",
                    CacheRow::COLUMNS
                ))
                .bind(repo_id)
                .bind(scope)
                .bind(version)
                .bind(like_prefix(k))
                .fetch_optional(&state.db)
                .await?;
                if found.is_some() {
                    break;
                }
            }
        }
        if let Some(row) = found {
            let row: CacheRow = sqlx::query_as(&format!(
                "UPDATE actions_caches SET last_accessed_at = now() WHERE id = $1 RETURNING {}",
                CacheRow::COLUMNS
            ))
            .bind(row.id)
            .fetch_one(&state.db)
            .await?;
            return Ok(Some(row));
        }
    }
    Ok(None)
}

/// Outcome of [`reserve`].
#[derive(Debug)]
pub enum Reserved {
    Ok(CacheRow),
    /// An entry with this key and version already exists (or another job
    /// is uploading it).
    Exists,
    /// The declared size exceeds the repository's limit.
    TooLarge {
        limit: u64,
    },
}

/// Reserve a new entry in the job's scope. A reservation left behind by a
/// job that is no longer running is taken over.
pub async fn reserve(
    state: &AppState,
    rj: &RuntimeJob,
    key: &str,
    version: &str,
    size: Option<i64>,
) -> anyhow::Result<Reserved> {
    let limit = size_limit(state, rj.repo.id).await?;
    if let Some(size) = size
        && size as u64 > limit
    {
        return Ok(Reserved::TooLarge { limit });
    }
    let scope = rj.write_scope();
    let inserted: Option<CacheRow> = sqlx::query_as(&format!(
        "INSERT INTO actions_caches (repo_id, key, version, ref, run_id, job_id)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (repo_id, ref, key, version) DO NOTHING
         RETURNING {}",
        CacheRow::COLUMNS
    ))
    .bind(rj.repo.id)
    .bind(key)
    .bind(version)
    .bind(&scope)
    .bind(rj.run.id)
    .bind(rj.job.id)
    .fetch_optional(&state.db)
    .await?;
    if let Some(row) = inserted {
        remove_files(state, row.id).await;
        return Ok(Reserved::Ok(row));
    }
    // Take over an abandoned reservation (its job finished without commit).
    let taken: Option<CacheRow> = sqlx::query_as(&format!(
        "UPDATE actions_caches c SET run_id = $5, job_id = $6, created_at = now(),
                last_accessed_at = now(), size_in_bytes = 0
          WHERE repo_id = $1 AND ref = $2 AND key = $3 AND version = $4 AND NOT committed
            AND NOT EXISTS (SELECT 1 FROM actions_jobs j
                             WHERE j.id = c.job_id AND j.status = 'in_progress')
          RETURNING {}",
        CacheRow::COLUMNS
    ))
    .bind(rj.repo.id)
    .bind(&scope)
    .bind(key)
    .bind(version)
    .bind(rj.run.id)
    .bind(rj.job.id)
    .fetch_optional(&state.db)
    .await?;
    match taken {
        Some(row) => {
            remove_files(state, row.id).await;
            Ok(Reserved::Ok(row))
        }
        None => Ok(Reserved::Exists),
    }
}

/// An uncommitted entry reserved by the job `rj`.
pub async fn reserved_by(
    state: &AppState,
    rj: &RuntimeJob,
    id: i64,
) -> anyhow::Result<Option<CacheRow>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {} FROM actions_caches
          WHERE id = $1 AND repo_id = $2 AND job_id = $3 AND NOT committed",
        CacheRow::COLUMNS
    ))
    .bind(id)
    .bind(rj.repo.id)
    .bind(rj.job.id)
    .fetch_optional(&state.db)
    .await?)
}

/// Why a commit failed.
#[derive(Debug, thiserror::Error)]
pub enum CommitError {
    #[error("No data was uploaded for this cache entry.")]
    NoData,
    #[error("Uploaded size {actual} does not match the declared size {declared}.")]
    SizeMismatch { declared: i64, actual: u64 },
    #[error("Cache size of {size} bytes is over the {limit} bytes limit of this repository.")]
    TooLarge { size: u64, limit: u64 },
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Commit an uploaded entry (its staging file becomes the archive), then
/// evict least recently used entries beyond the repository limit.
pub async fn commit(
    state: &AppState,
    entry: &CacheRow,
    declared: Option<i64>,
) -> Result<CacheRow, CommitError> {
    let staging = staging_path(state, entry.id);
    let actual = match tokio::fs::metadata(&staging).await {
        Ok(m) => m.len(),
        Err(_) => return Err(CommitError::NoData),
    };
    if let Some(d) = declared
        && d >= 0
        && d as u64 != actual
    {
        return Err(CommitError::SizeMismatch {
            declared: d,
            actual,
        });
    }
    let limit = size_limit(state, entry.repo_id).await?;
    if actual > limit {
        remove_files(state, entry.id).await;
        sqlx::query("DELETE FROM actions_caches WHERE id = $1")
            .bind(entry.id)
            .execute(&state.db)
            .await
            .map_err(anyhow::Error::from)?;
        return Err(CommitError::TooLarge {
            size: actual,
            limit,
        });
    }
    tokio::fs::rename(&staging, archive_path(state, entry.id))
        .await
        .map_err(anyhow::Error::from)?;
    let _ = tokio::fs::remove_dir_all(blocks_dir(state, entry.id)).await;
    let row: CacheRow = sqlx::query_as(&format!(
        "UPDATE actions_caches SET committed = true, size_in_bytes = $2, created_at = now(),
                last_accessed_at = now()
          WHERE id = $1 RETURNING {}",
        CacheRow::COLUMNS
    ))
    .bind(entry.id)
    .bind(actual as i64)
    .fetch_one(&state.db)
    .await
    .map_err(anyhow::Error::from)?;
    evict(state, entry.repo_id).await?;
    Ok(row)
}

/// Delete committed entries of `repo_id` beyond its size limit, least
/// recently accessed first. Returns the evicted ids.
pub async fn evict(state: &AppState, repo_id: i64) -> anyhow::Result<Vec<i64>> {
    let limit = size_limit(state, repo_id).await?;
    let ids: Vec<i64> = sqlx::query_scalar(
        "WITH ranked AS (
             SELECT id, sum(size_in_bytes) OVER (ORDER BY last_accessed_at DESC, id DESC) AS total
               FROM actions_caches WHERE repo_id = $1 AND committed
         )
         DELETE FROM actions_caches WHERE id IN (SELECT id FROM ranked WHERE total > $2)
         RETURNING id",
    )
    .bind(repo_id)
    .bind(limit as i64)
    .fetch_all(&state.db)
    .await?;
    for id in &ids {
        remove_files(state, *id).await;
    }
    if !ids.is_empty() {
        tracing::info!(repo_id, evicted = ids.len(), "evicted actions caches");
    }
    Ok(ids)
}

/// Delete entries by id (their files too).
pub async fn delete(state: &AppState, ids: &[i64]) -> anyhow::Result<()> {
    sqlx::query("DELETE FROM actions_caches WHERE id = ANY($1)")
        .bind(ids)
        .execute(&state.db)
        .await?;
    for id in ids {
        remove_files(state, *id).await;
    }
    Ok(())
}

/// Maintenance: drop entries unused for the retention period and
/// reservations that were never committed.
pub async fn expire(state: &AppState) -> anyhow::Result<usize> {
    let days = state.config.actions.cache_retention_days.max(1);
    let ids: Vec<i64> = sqlx::query_scalar(
        "DELETE FROM actions_caches
          WHERE (committed AND last_accessed_at < now() - make_interval(days => $1))
             OR (NOT committed AND created_at < now() - make_interval(hours => $2))
         RETURNING id",
    )
    .bind(days as i32)
    .bind(STALE_RESERVATION_HOURS as i32)
    .fetch_all(&state.db)
    .await?;
    for id in &ids {
        remove_files(state, *id).await;
    }
    Ok(ids.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn like_escaping() {
        assert_eq!(like_prefix("npm-"), "npm-%");
        assert_eq!(like_prefix("a_b%c\\"), "a\\_b\\%c\\\\%");
    }

    #[test]
    fn keys() {
        assert!(validate_key("linux-npm-abc").is_ok());
        assert!(validate_key("a,b").is_err());
        assert!(validate_key("").is_err());
        assert!(validate_key(&"x".repeat(513)).is_err());
    }
}
