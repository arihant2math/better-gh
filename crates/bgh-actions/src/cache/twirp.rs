//! Twirp `github.actions.results.api.v1.CacheService` (`@actions/cache`
//! v2): `CreateCacheEntry` → signed upload URL (Azure Put Blob / Put Block
//! List, [`crate::results::blob`]), `FinalizeCacheEntryUpload`,
//! `GetCacheEntryDownloadURL`. Failures the client treats as "not saved" or
//! "miss" are `{"ok": false}` responses, like GitHub's.

use axum::response::Response;
use bgh_core::AppState;
use serde::Deserialize;
use serde_json::json;

use super::{CommitError, Reserved};
use crate::results::{TwirpError, TwirpResult, decode, int64, reply};
use crate::runtime::{BlobPerm, RuntimeJob, signed_blob_url};

const UPLOAD_URL_TTL: i64 = 6 * 3600;
const DOWNLOAD_URL_TTL: i64 = 3600;

pub async fn handle(
    state: &AppState,
    rj: &RuntimeJob,
    method: &str,
    body: &[u8],
) -> TwirpResult<Response> {
    match method {
        "CreateCacheEntry" => create(state, rj, body).await,
        "FinalizeCacheEntryUpload" => finalize(state, rj, body).await,
        "GetCacheEntryDownloadURL" => download_url(state, rj, body).await,
        _ => Ok(crate::results::bad_route("CacheService", method)),
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct CreateRequest {
    key: String,
    version: String,
}

async fn create(state: &AppState, rj: &RuntimeJob, body: &[u8]) -> TwirpResult<Response> {
    let req: CreateRequest = decode(body)?;
    super::validate_key(&req.key).map_err(TwirpError::invalid)?;
    if req.version.is_empty() {
        return Err(TwirpError::invalid("version is required"));
    }
    match super::reserve(state, rj, &req.key, &req.version, None).await? {
        Reserved::Ok(row) => {
            let url = signed_blob_url(
                state,
                "cache",
                &row.id.to_string(),
                BlobPerm::Write,
                UPLOAD_URL_TTL,
            )?;
            Ok(reply(&json!({"ok": true, "signed_upload_url": url})))
        }
        Reserved::Exists => Ok(reply(&json!({
            "ok": false,
            "signed_upload_url": "",
            "message": "cache entry with the same key, version, and scope already exists",
        }))),
        Reserved::TooLarge { .. } => unreachable!("no size declared"),
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct FinalizeRequest {
    key: String,
    version: String,
    #[serde(alias = "sizeBytes", deserialize_with = "int64")]
    size_bytes: i64,
}

async fn finalize(state: &AppState, rj: &RuntimeJob, body: &[u8]) -> TwirpResult<Response> {
    let req: FinalizeRequest = decode(body)?;
    let entry: Option<super::CacheRow> = sqlx::query_as(&format!(
        "SELECT {} FROM actions_caches
          WHERE repo_id = $1 AND ref = $2 AND key = $3 AND version = $4
            AND job_id = $5 AND NOT committed",
        super::CacheRow::COLUMNS
    ))
    .bind(rj.repo.id)
    .bind(rj.write_scope())
    .bind(&req.key)
    .bind(&req.version)
    .bind(rj.job.id)
    .fetch_optional(&state.db)
    .await?;
    let Some(entry) = entry else {
        return Err(TwirpError::not_found(
            "no reserved cache entry for this key and version",
        ));
    };
    let declared = (req.size_bytes > 0).then_some(req.size_bytes);
    match super::commit(state, &entry, declared).await {
        Ok(row) => Ok(reply(&json!({"ok": true, "entry_id": row.id.to_string()}))),
        Err(CommitError::Other(e)) => Err(e.into()),
        Err(e) => Ok(reply(
            &json!({"ok": false, "entry_id": "0", "message": e.to_string()}),
        )),
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct DownloadRequest {
    key: String,
    #[serde(alias = "restoreKeys")]
    restore_keys: Vec<String>,
    version: String,
}

async fn download_url(state: &AppState, rj: &RuntimeJob, body: &[u8]) -> TwirpResult<Response> {
    let req: DownloadRequest = decode(body)?;
    let mut keys = vec![req.key];
    keys.extend(req.restore_keys);
    keys.retain(|k| !k.trim().is_empty());
    if keys.is_empty() || keys.len() > super::MAX_KEYS {
        return Err(TwirpError::invalid("1 to 10 keys are required"));
    }
    for k in &keys {
        super::validate_key(k).map_err(TwirpError::invalid)?;
    }
    let miss = || reply(&json!({"ok": false, "signed_download_url": "", "matched_key": ""}));
    let Some(entry) =
        super::lookup(state, rj.repo.id, &rj.read_scopes(), &keys, &req.version).await?
    else {
        return Ok(miss());
    };
    let url = signed_blob_url(
        state,
        "cache",
        &entry.id.to_string(),
        BlobPerm::Read,
        DOWNLOAD_URL_TTL,
    )?;
    Ok(reply(&json!({
        "ok": true,
        "signed_download_url": url,
        "matched_key": entry.key,
    })))
}
