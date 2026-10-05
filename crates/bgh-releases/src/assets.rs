//! Release assets: list, upload (API and uploads host), get/download,
//! update, delete, and the browser download route.

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bgh_core::prelude::*;
use serde::Deserialize;
use serde_json::json;
use tokio_util::io::ReaderStream;

use crate::model::{Asset, AssetRow, ReleaseRow, ReleaseUrls};
use crate::releases;
use crate::storage::{self, SpoolError};

/// Largest accepted asset (GitHub: 2 GiB).
pub const MAX_ASSET_SIZE: u64 = 2 * 1024 * 1024 * 1024;

fn urls<'a>(state: &'a AppState, access: &'a RepoAccess) -> ReleaseUrls<'a> {
    ReleaseUrls {
        urls: &state.urls,
        owner: &access.owner.login,
        repo: &access.repo.name,
    }
}

async fn render(
    state: &AppState,
    access: &RepoAccess,
    tag: &str,
    rows: Vec<AssetRow>,
) -> ApiResult<Vec<Asset>> {
    let users = bgh_core::views::users_by_id(state, rows.iter().map(|a| a.uploader_id)).await?;
    let ru = urls(state, access);
    Ok(rows
        .iter()
        .map(|a| Asset::new(&ru, tag, a, a.uploader_id.and_then(|id| users.get(&id))))
        .collect())
}

/// Delete stored blobs no asset references anymore (best effort).
pub(crate) async fn gc_blobs(state: &AppState, digests: Vec<String>) {
    if digests.is_empty() {
        return;
    }
    let orphaned: Result<Vec<String>, _> = sqlx::query_scalar(
        "SELECT d FROM unnest($1::text[]) d
          WHERE NOT EXISTS (SELECT 1 FROM release_assets a WHERE a.sha256 = d)",
    )
    .bind(&digests)
    .fetch_all(&state.db)
    .await;
    let store = storage::storage(state);
    match orphaned {
        Ok(list) => {
            for d in list {
                if let Err(err) = store.delete(&d).await {
                    tracing::warn!(?err, digest = d, "deleting release asset blob");
                }
            }
        }
        Err(err) => tracing::warn!(?err, "release asset gc"),
    }
}

/// Load an asset of the repository together with its release (drafts only
/// for writers).
async fn load(
    state: &AppState,
    access: &RepoAccess,
    asset_id: i64,
) -> ApiResult<(AssetRow, ReleaseRow)> {
    let asset: AssetRow = sqlx::query_as(&format!(
        "SELECT {} FROM release_assets WHERE id = $1 AND repo_id = $2",
        AssetRow::COLUMNS
    ))
    .bind(asset_id)
    .bind(access.repo.id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)?;
    let release = releases::load(state, access, asset.release_id).await?;
    Ok((asset, release))
}

/// `GET /repos/{owner}/{repo}/releases/{release_id}/assets`
pub async fn list(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo, id)): Path<(String, String, i64)>,
) -> ApiResult<Page<Asset>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let release = releases::load(&state, &access, id).await?;
    let rows: Vec<AssetRow> = sqlx::query_as(&format!(
        "SELECT {} FROM release_assets WHERE release_id = $1 ORDER BY id LIMIT $2 OFFSET $3",
        AssetRow::COLUMNS
    ))
    .bind(release.id)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let page = p.page(rows);
    let items = render(&state, &access, &release.tag_name, page.items).await?;
    Ok(Page {
        items,
        link: page.link,
    })
}

#[derive(Debug, Default, Deserialize)]
pub struct UploadParams {
    pub name: Option<String>,
    pub label: Option<String>,
}

/// GitHub renames special characters in asset file names to `.`.
pub fn sanitize_name(name: &str) -> String {
    let mapped: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+' | '@' | '~') {
                c
            } else {
                '.'
            }
        })
        .collect();
    mapped.trim_matches('.').to_string()
}

/// `POST /repos/{owner}/{repo}/releases/{release_id}/assets?name=&label=`,
/// also served at `{base}/api/uploads/repos/...` (the `upload_url` host).
/// The raw request body is the asset; it is streamed to storage.
pub async fn upload(
    State(state): State<AppState>,
    auth: RequireUser,
    headers: HeaderMap,
    Path((owner, repo, id)): Path<(String, String, i64)>,
    Query(params): Query<UploadParams>,
    body: Body,
) -> ApiResult<Response> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    access.require(Permission::Write)?;
    access.require_not_archived()?;
    let release = releases::load(&state, &access, id).await?;
    let raw_name = params.name.unwrap_or_default();
    let name = sanitize_name(&raw_name);
    if name.is_empty() {
        return Err(ApiError::invalid_field(FieldError::missing_field(
            "ReleaseAsset",
            "name",
        )));
    }
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty())
        .unwrap_or("application/octet-stream")
        .to_string();
    let taken: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM release_assets WHERE release_id = $1 AND name = $2)",
    )
    .bind(release.id)
    .bind(&name)
    .fetch_one(&state.db)
    .await?;
    if taken {
        return Err(ApiError::invalid_field(FieldError::already_exists(
            "ReleaseAsset",
            "name",
        )));
    }

    let spooled = storage::spool(
        &storage::tmp_dir(&state),
        body.into_data_stream(),
        MAX_ASSET_SIZE,
    )
    .await
    .map_err(|e| match e {
        SpoolError::TooLarge => ApiError::invalid_field(FieldError::custom(
            "ReleaseAsset",
            "size",
            "Asset exceeds the maximum size",
        )),
        SpoolError::Io(e) => ApiError::internal(e),
    })?;
    let digest = spooled.sha256.clone();
    storage::storage(&state)
        .put_file(&spooled.sha256, &spooled.path)
        .await
        .map_err(ApiError::internal)?;

    let mut tx = Tx::begin(&state).await?;
    let inserted: Result<AssetRow, sqlx::Error> = sqlx::query_as(&format!(
        "INSERT INTO release_assets (release_id, repo_id, name, label, content_type, size,
                                     sha256, state, uploader_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7, 'uploaded', $8) RETURNING {}",
        AssetRow::COLUMNS
    ))
    .bind(release.id)
    .bind(access.repo.id)
    .bind(&name)
    .bind(params.label.as_deref().filter(|l| !l.is_empty()))
    .bind(&content_type)
    .bind(spooled.size as i64)
    .bind(&digest)
    .bind(auth.user.id)
    .fetch_one(&mut *tx)
    .await;
    let row = match inserted {
        Ok(row) => row,
        Err(e) => {
            drop(tx);
            gc_blobs(&state, vec![digest]).await;
            return Err(match bgh_core::db::unique_violation(&e).as_deref() {
                Some(_) => {
                    ApiError::invalid_field(FieldError::already_exists("ReleaseAsset", "name"))
                }
                None => e.into(),
            });
        }
    };
    sqlx::query("UPDATE releases SET updated_at = now() WHERE id = $1")
        .bind(release.id)
        .execute(&mut *tx)
        .await?;
    tx.sync(
        &access.scope(),
        "release_asset",
        row.id,
        SyncAction::Insert,
        &json!({ "id": row.id, "release_id": row.release_id, "name": row.name, "size": row.size }),
    )
    .await?;
    tx.emit(Event::ReleaseUpdated {
        repo_id: access.repo.id,
        release_id: release.id,
        actor_id: auth.user.id,
    });
    tx.commit().await?;
    let asset = render(&state, &access, &release.tag_name, vec![row])
        .await?
        .pop()
        .expect("one asset");
    Ok((StatusCode::CREATED, Json(asset)).into_response())
}

fn wants_binary(headers: &HeaderMap) -> bool {
    headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|a| a.contains("application/octet-stream"))
}

/// Stream an asset's content and count the download.
async fn download(state: &AppState, asset: &AssetRow) -> ApiResult<Response> {
    let digest = asset.sha256.as_deref().ok_or(ApiError::NotFound)?;
    let (reader, len) = storage::storage(state)
        .open(digest)
        .await
        .map_err(ApiError::internal)?
        .ok_or(ApiError::NotFound)?;
    sqlx::query("UPDATE release_assets SET download_count = download_count + 1 WHERE id = $1")
        .bind(asset.id)
        .execute(&state.db)
        .await?;
    let disposition = format!(
        "attachment; filename=\"{}\"",
        asset.name.replace(['"', '\\'], "_")
    );
    let mut resp =
        Body::from_stream(ReaderStream::with_capacity(reader, 64 * 1024)).into_response();
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    h.insert(header::CONTENT_LENGTH, HeaderValue::from(len));
    if let Ok(v) = HeaderValue::from_str(&disposition) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    if let Ok(v) = HeaderValue::from_str(&format!("\"{digest}\"")) {
        h.insert(header::ETAG, v);
    }
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=0"),
    );
    Ok(resp)
}

/// `GET /repos/{owner}/{repo}/releases/assets/{asset_id}`: JSON, or the
/// content itself with `Accept: application/octet-stream`.
pub async fn get(
    State(state): State<AppState>,
    auth: MaybeUser,
    headers: HeaderMap,
    Path((owner, repo, asset_id)): Path<(String, String, i64)>,
) -> ApiResult<Response> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let (asset, release) = load(&state, &access, asset_id).await?;
    if wants_binary(&headers) {
        return download(&state, &asset).await;
    }
    let json = render(&state, &access, &release.tag_name, vec![asset])
        .await?
        .pop()
        .expect("one asset");
    Ok(Json(json).into_response())
}

#[derive(Debug, Default, Deserialize)]
pub struct AssetPatch {
    pub name: Option<String>,
    pub label: Option<String>,
    pub state: Option<String>,
}

/// `PATCH /repos/{owner}/{repo}/releases/assets/{asset_id}`
pub async fn update(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, asset_id)): Path<(String, String, i64)>,
    Json(body): Json<AssetPatch>,
) -> ApiResult<Json<Asset>> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    access.require(Permission::Write)?;
    access.require_not_archived()?;
    let (asset, release) = load(&state, &access, asset_id).await?;
    let name = match body.name.as_deref() {
        Some(n) => {
            let n = sanitize_name(n);
            if n.is_empty() {
                return Err(ApiError::invalid_field(FieldError::invalid(
                    "ReleaseAsset",
                    "name",
                )));
            }
            n
        }
        None => asset.name.clone(),
    };
    if let Some(s) = body.state.as_deref()
        && s != "uploaded"
        && s != "open"
    {
        return Err(ApiError::invalid_field(FieldError::invalid(
            "ReleaseAsset",
            "state",
        )));
    }
    let mut tx = Tx::begin(&state).await?;
    let row: AssetRow = sqlx::query_as(&format!(
        "UPDATE release_assets SET name = $2, label = coalesce($3, label),
                state = coalesce($4, state), updated_at = now()
          WHERE id = $1 RETURNING {}",
        AssetRow::COLUMNS
    ))
    .bind(asset.id)
    .bind(&name)
    .bind(&body.label)
    .bind(&body.state)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| match bgh_core::db::unique_violation(&e).as_deref() {
        Some(_) => ApiError::invalid_field(FieldError::already_exists("ReleaseAsset", "name")),
        None => e.into(),
    })?;
    tx.sync(
        &access.scope(),
        "release_asset",
        row.id,
        SyncAction::Update,
        &json!({ "id": row.id, "release_id": row.release_id, "name": row.name, "size": row.size }),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(
        render(&state, &access, &release.tag_name, vec![row])
            .await?
            .pop()
            .expect("one asset"),
    ))
}

/// `DELETE /repos/{owner}/{repo}/releases/assets/{asset_id}`
pub async fn delete(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, asset_id)): Path<(String, String, i64)>,
) -> ApiResult<StatusCode> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    access.require(Permission::Write)?;
    access.require_not_archived()?;
    let (asset, _) = load(&state, &access, asset_id).await?;
    let mut tx = Tx::begin(&state).await?;
    sqlx::query("DELETE FROM release_assets WHERE id = $1")
        .bind(asset.id)
        .execute(&mut *tx)
        .await?;
    tx.sync(
        &access.scope(),
        "release_asset",
        asset.id,
        SyncAction::Delete,
        &json!({ "id": asset.id }),
    )
    .await?;
    tx.commit().await?;
    gc_blobs(&state, asset.sha256.into_iter().collect()).await;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /{owner}/{repo}/releases/download/{tag}/{name}` (browser download).
pub async fn browser_download(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, tag, name)): Path<(String, String, String, String)>,
) -> ApiResult<Response> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let asset: AssetRow = sqlx::query_as(&format!(
        "SELECT {} FROM release_assets a
          WHERE a.name = $3 AND a.release_id = (
              SELECT r.id FROM releases r
               WHERE r.repo_id = $1 AND r.tag_name = $2 AND (NOT r.draft OR $4)
               ORDER BY r.draft, r.id DESC LIMIT 1)",
        db::prefixed("a", AssetRow::COLUMNS)
    ))
    .bind(access.repo.id)
    .bind(&tag)
    .bind(&name)
    .bind(access.permission >= Permission::Write)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)?;
    download(&state, &asset).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_names() {
        assert_eq!(
            sanitize_name("app v1 (linux).tar.gz"),
            "app.v1..linux..tar.gz"
        );
        assert_eq!(sanitize_name(".hidden."), "hidden");
        assert_eq!(sanitize_name("ok-1.0_x+y.zip"), "ok-1.0_x+y.zip");
    }
}
