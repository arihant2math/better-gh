//! Release objects.

use bgh_core::AppState;
use bgh_core::node_id::{self, NodeType};
use bgh_core::time::{Timestamp, ts};
use bgh_core::urls::encode_path;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use super::common::{self, RepoCtx, user_or_ghost, user_or_null};

#[derive(Debug, Clone, sqlx::FromRow)]
struct ReleaseRow {
    id: i64,
    tag_name: String,
    target_commitish: String,
    name: Option<String>,
    body: Option<String>,
    draft: bool,
    prerelease: bool,
    author_id: Option<i64>,
    published_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct AssetRow {
    id: i64,
    name: String,
    label: Option<String>,
    content_type: String,
    size: i64,
    state: String,
    download_count: i64,
    uploader_id: Option<i64>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

/// Webhook `release` object by id (`None` if deleted).
pub async fn release(
    state: &AppState,
    ctx: &RepoCtx,
    release_id: i64,
) -> anyhow::Result<Option<Value>> {
    let row: Option<ReleaseRow> = sqlx::query_as(
        "SELECT id, tag_name, target_commitish, name, body, draft, prerelease, author_id,
                published_at, created_at
         FROM releases WHERE id = $1 AND repo_id = $2",
    )
    .bind(release_id)
    .bind(ctx.repo.id)
    .fetch_optional(&state.db)
    .await?;
    let Some(rel) = row else { return Ok(None) };
    let assets: Vec<AssetRow> = sqlx::query_as(
        "SELECT id, name, label, content_type, size, state, download_count, uploader_id,
                created_at, updated_at
         FROM release_assets WHERE release_id = $1 ORDER BY id",
    )
    .bind(rel.id)
    .fetch_all(&state.db)
    .await?;
    let users = common::users(
        state,
        std::iter::once(rel.author_id).chain(assets.iter().map(|a| a.uploader_id)),
    )
    .await?;

    let urls = &state.urls;
    let (o, r) = (ctx.owner_login(), ctx.name());
    let repo_api = ctx.api_url(urls);
    let repo_html = ctx.html_url(urls);
    let url = format!("{repo_api}/releases/{}", rel.id);
    let tag = encode_path(&rel.tag_name);
    let assets: Vec<Value> = assets
        .iter()
        .map(|a| {
            json!({
                "url": format!("{repo_api}/releases/assets/{}", a.id),
                "id": a.id,
                "node_id": node_id::encode(NodeType::ReleaseAsset, a.id),
                "name": a.name,
                "label": a.label,
                "uploader": user_or_null(urls, &users, a.uploader_id),
                "content_type": a.content_type,
                "state": a.state,
                "size": a.size,
                "download_count": a.download_count,
                "created_at": Timestamp(a.created_at),
                "updated_at": Timestamp(a.updated_at),
                "browser_download_url": format!(
                    "{repo_html}/releases/download/{tag}/{}",
                    encode_path(&a.name)
                ),
            })
        })
        .collect();
    Ok(Some(json!({
        "url": url,
        "assets_url": format!("{url}/assets"),
        "upload_url": urls.html(&format!(
            "/api/uploads/repos/{o}/{r}/releases/{}/assets{{?name,label}}",
            rel.id
        )),
        "html_url": format!("{repo_html}/releases/tag/{tag}"),
        "id": rel.id,
        "author": user_or_ghost(urls, &users, rel.author_id),
        "node_id": node_id::encode(NodeType::Release, rel.id),
        "tag_name": rel.tag_name,
        "target_commitish": rel.target_commitish,
        "name": rel.name,
        "draft": rel.draft,
        "prerelease": rel.prerelease,
        "created_at": Timestamp(rel.created_at),
        "published_at": ts(rel.published_at),
        "assets": assets,
        "tarball_url": format!("{repo_api}/tarball/{tag}"),
        "zipball_url": format!("{repo_api}/zipball/{tag}"),
        "body": rel.body,
    })))
}
