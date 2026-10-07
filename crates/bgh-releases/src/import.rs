//! Internal insert API for importers (P18 metadata import): releases and
//! assets with the source's authors, timestamps and download counts, and
//! no domain events (no webhooks, notifications or activity). Sync
//! records are written like the REST handlers do.

use bgh_core::prelude::*;
use chrono::{DateTime, Utc};
use futures::Stream;

use crate::model::{ReleaseRow, release_sync_json};
use crate::storage::{self, SpoolError};

pub struct NewRelease<'a> {
    pub tag_name: &'a str,
    pub target_commitish: &'a str,
    pub name: Option<&'a str>,
    pub body: Option<&'a str>,
    pub draft: bool,
    pub prerelease: bool,
    pub author_id: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub published_at: Option<DateTime<Utc>>,
}

/// Make sure a published release's tag exists (the git step normally
/// brought it along; otherwise it is created at `target_commitish`, like
/// GitHub does on publish). Does not emit a `Push`.
pub async fn ensure_tag(
    state: &AppState,
    repo: &db::Repository,
    r: &NewRelease<'_>,
) -> ApiResult<()> {
    if !r.draft {
        crate::git::ensure_tag(state, repo, r.tag_name, r.target_commitish).await?;
    }
    Ok(())
}

pub async fn insert_release(tx: &mut Tx, repo_id: i64, r: &NewRelease<'_>) -> ApiResult<i64> {
    let row: ReleaseRow = sqlx::query_as(&format!(
        "INSERT INTO releases (repo_id, tag_name, target_commitish, name, body, draft, prerelease,
                               author_id, created_at, updated_at, published_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $9, $10) RETURNING {}",
        ReleaseRow::COLUMNS
    ))
    .bind(repo_id)
    .bind(r.tag_name)
    .bind(r.target_commitish)
    .bind(r.name)
    .bind(r.body)
    .bind(r.draft)
    .bind(r.prerelease)
    .bind(r.author_id)
    .bind(r.created_at)
    .bind(if r.draft {
        None
    } else {
        r.published_at.or(Some(r.created_at))
    })
    .fetch_one(&mut **tx)
    .await?;
    tx.sync(
        &bgh_core::sync::repo_scope(repo_id),
        "release",
        row.id,
        SyncAction::Insert,
        &release_sync_json(&row),
    )
    .await?;
    Ok(row.id)
}

/// A downloaded asset blob, already in the asset store.
pub struct StoredBlob {
    pub sha256: String,
    pub size: i64,
}

/// Stream an asset body into the content-addressed store.
pub async fn store_blob<S, E>(state: &AppState, body: S) -> anyhow::Result<StoredBlob>
where
    S: Stream<Item = Result<bytes::Bytes, E>> + Unpin,
    E: std::fmt::Display,
{
    let spooled = storage::spool(
        &storage::tmp_dir(state),
        body,
        crate::assets::MAX_ASSET_SIZE,
    )
    .await
    .map_err(|e| match e {
        SpoolError::TooLarge => anyhow::anyhow!("asset exceeds the maximum size"),
        SpoolError::Io(e) => anyhow::anyhow!("storing asset: {e}"),
    })?;
    storage::storage(state)
        .put_file(&spooled.sha256, &spooled.path)
        .await?;
    Ok(StoredBlob {
        sha256: spooled.sha256,
        size: spooled.size as i64,
    })
}

pub struct NewAsset<'a> {
    pub name: &'a str,
    pub label: Option<&'a str>,
    pub content_type: &'a str,
    pub download_count: i64,
    pub uploader_id: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub async fn insert_asset(
    tx: &mut Tx,
    release_id: i64,
    repo_id: i64,
    a: &NewAsset<'_>,
    blob: &StoredBlob,
) -> ApiResult<i64> {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO release_assets (release_id, repo_id, name, label, content_type, size, sha256,
                                     state, download_count, uploader_id, created_at, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, 'uploaded', $8, $9, $10, $11) RETURNING id",
    )
    .bind(release_id)
    .bind(repo_id)
    .bind(a.name)
    .bind(a.label)
    .bind(a.content_type)
    .bind(blob.size)
    .bind(&blob.sha256)
    .bind(a.download_count)
    .bind(a.uploader_id)
    .bind(a.created_at)
    .bind(a.updated_at)
    .fetch_one(&mut **tx)
    .await?;
    tx.sync(
        &bgh_core::sync::repo_scope(repo_id),
        "release_asset",
        id,
        SyncAction::Insert,
        &serde_json::json!({"id": id, "release_id": release_id, "name": a.name, "size": blob.size}),
    )
    .await?;
    Ok(id)
}
