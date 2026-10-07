//! Results service (`ACTIONS_RESULTS_URL`): the twirp JSON services of
//! `github.actions.results.api.v1` used by the toolkit — `ArtifactService`
//! (`@actions/artifact` v2, here) and `CacheService` (`@actions/cache` v2,
//! [`crate::cache::twirp`]) — plus the Azure Blob subset behind their signed
//! URLs ([`blob`]).
//!
//! Twirp: `POST /twirp/<package>.<Service>/<Method>` with a JSON body,
//! `Authorization: Bearer <ACTIONS_RUNTIME_TOKEN>`. Requests use protobuf
//! JSON (both `snake_case` and `lowerCamelCase` field names are accepted;
//! 64-bit integers may be strings); responses use the proto field names.
//! Errors are `{"code": "...", "msg": "..."}` with twirp's status mapping.

pub mod blob;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use bgh_core::AppState;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::models::ArtifactRow;
use crate::runtime::{BlobPerm, RuntimeJob, signed_blob_url};

pub const PACKAGE: &str = "github.actions.results.api.v1";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/twirp/{service}/{method}", post(dispatch))
        .merge(blob::routes())
}

/// A twirp error.
#[derive(Debug)]
pub struct TwirpError {
    pub code: &'static str,
    pub msg: String,
}

impl TwirpError {
    pub fn new(code: &'static str, msg: impl Into<String>) -> Self {
        Self {
            code,
            msg: msg.into(),
        }
    }
    pub fn invalid(msg: impl Into<String>) -> Self {
        Self::new("invalid_argument", msg)
    }
    pub fn not_found(msg: impl Into<String>) -> Self {
        Self::new("not_found", msg)
    }
    pub fn internal(err: impl std::fmt::Display) -> Self {
        tracing::error!(%err, "results service error");
        Self::new("internal", "internal error")
    }

    fn status(&self) -> StatusCode {
        match self.code {
            "invalid_argument" | "malformed" | "out_of_range" => StatusCode::BAD_REQUEST,
            "unauthenticated" => StatusCode::UNAUTHORIZED,
            "permission_denied" | "resource_exhausted" => StatusCode::FORBIDDEN,
            "not_found" | "bad_route" => StatusCode::NOT_FOUND,
            "already_exists" | "aborted" => StatusCode::CONFLICT,
            "failed_precondition" => StatusCode::PRECONDITION_FAILED,
            "unimplemented" => StatusCode::NOT_IMPLEMENTED,
            "unavailable" => StatusCode::SERVICE_UNAVAILABLE,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for TwirpError {
    fn into_response(self) -> Response {
        (
            self.status(),
            axum::Json(json!({"code": self.code, "msg": self.msg})),
        )
            .into_response()
    }
}

impl From<sqlx::Error> for TwirpError {
    fn from(e: sqlx::Error) -> Self {
        Self::internal(e)
    }
}

impl From<anyhow::Error> for TwirpError {
    fn from(e: anyhow::Error) -> Self {
        Self::internal(format!("{e:#}"))
    }
}

impl From<bgh_core::error::ApiError> for TwirpError {
    fn from(e: bgh_core::error::ApiError) -> Self {
        Self::internal(e)
    }
}

pub type TwirpResult<T> = Result<T, TwirpError>;

/// Decode a request body.
pub fn decode<T: DeserializeOwned>(body: &[u8]) -> TwirpResult<T> {
    let body = if body.iter().all(u8::is_ascii_whitespace) {
        b"{}".as_slice()
    } else {
        body
    };
    serde_json::from_slice(body).map_err(|e| {
        TwirpError::new(
            "malformed",
            format!("the json request could not be decoded: {e}"),
        )
    })
}

/// Encode a response.
pub fn reply<T: Serialize>(v: &T) -> Response {
    (StatusCode::OK, axum::Json(v)).into_response()
}

/// Protobuf JSON int64: a number or a decimal string.
pub fn int64<'de, D: Deserializer<'de>>(d: D) -> Result<i64, D::Error> {
    use serde::de::Error;
    match Value::deserialize(d)? {
        Value::Null => Ok(0),
        Value::Number(n) => n.as_i64().ok_or_else(|| D::Error::custom("invalid int64")),
        Value::String(s) if s.is_empty() => Ok(0),
        Value::String(s) => s.parse().map_err(|_| D::Error::custom("invalid int64")),
        _ => Err(D::Error::custom("invalid int64")),
    }
}

/// Optional protobuf JSON int64 (also unwraps `google.protobuf.Int64Value`).
pub fn opt_int64<'de, D: Deserializer<'de>>(d: D) -> Result<Option<i64>, D::Error> {
    use serde::de::Error;
    match Value::deserialize(d)? {
        Value::Null => Ok(None),
        Value::Number(n) => n
            .as_i64()
            .map(Some)
            .ok_or_else(|| D::Error::custom("invalid int64")),
        Value::String(s) => s
            .parse()
            .map(Some)
            .map_err(|_| D::Error::custom("invalid int64")),
        Value::Object(o) => match o.get("value") {
            Some(Value::Number(n)) => Ok(n.as_i64()),
            Some(Value::String(s)) => s.parse().map(Some).map_err(D::Error::custom),
            _ => Ok(None),
        },
        _ => Err(D::Error::custom("invalid int64")),
    }
}

/// Optional `google.protobuf.StringValue` (plain string in JSON).
pub fn opt_string<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::String(s) => Some(s),
        Value::Object(o) => o.get("value").and_then(|v| v.as_str()).map(String::from),
        _ => None,
    })
}

async fn dispatch(
    State(state): State<AppState>,
    Path((service, method)): Path<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let json = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_none_or(|ct| ct.starts_with("application/json"));
    if !json {
        return TwirpError::new(
            "bad_route",
            "only the JSON encoding (Content-Type: application/json) is supported",
        )
        .into_response();
    }
    let Some(service) = service
        .strip_prefix(PACKAGE)
        .and_then(|s| s.strip_prefix('.'))
    else {
        return bad_route(&service, &method);
    };
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split_once(' '))
        .filter(|(s, _)| s.eq_ignore_ascii_case("bearer"))
        .map(|(_, t)| t.trim().to_string());
    let Some(token) = token else {
        return TwirpError::new("unauthenticated", "missing runtime token").into_response();
    };
    let rj = match crate::runtime::load_job(&state, &token).await {
        Ok(Some(rj)) => rj,
        Ok(None) => {
            return TwirpError::new("unauthenticated", "invalid or expired runtime token")
                .into_response();
        }
        Err(e) => return TwirpError::internal(e).into_response(),
    };
    let result = match (service, method.as_str()) {
        ("CacheService", m) => crate::cache::twirp::handle(&state, &rj, m, &body).await,
        ("ArtifactService", "CreateArtifact") => create_artifact(&state, &rj, &body).await,
        ("ArtifactService", "FinalizeArtifact") => finalize_artifact(&state, &rj, &body).await,
        ("ArtifactService", "ListArtifacts") => list_artifacts(&state, &rj, &body).await,
        ("ArtifactService", "GetSignedArtifactURL") => signed_url(&state, &rj, &body).await,
        ("ArtifactService", "DeleteArtifact") => delete_artifact(&state, &rj, &body).await,
        _ => return bad_route(service, &method),
    };
    result.unwrap_or_else(IntoResponse::into_response)
}

pub fn bad_route(service: &str, method: &str) -> Response {
    TwirpError::new(
        "bad_route",
        format!("no handler for path \"/twirp/{service}/{method}\""),
    )
    .into_response()
}

// ---------------------------------------------------------------------------
// ArtifactService
// ---------------------------------------------------------------------------

/// Backend ids of the calling job; requests must name their own job.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Ids {
    #[serde(alias = "workflowRunBackendId")]
    workflow_run_backend_id: String,
    #[serde(alias = "workflowJobRunBackendId")]
    workflow_job_run_backend_id: String,
}

impl Ids {
    fn check(&self, rj: &RuntimeJob, job_too: bool) -> TwirpResult<()> {
        let run_ok = self.workflow_run_backend_id == rj.run.id.to_string();
        let job_ok = !job_too || self.workflow_job_run_backend_id == rj.job.id.to_string();
        if run_ok && job_ok {
            Ok(())
        } else {
            Err(TwirpError::new(
                "permission_denied",
                "the backend ids do not match the runtime token",
            ))
        }
    }
}

/// upload-artifact's name rules.
fn validate_artifact_name(name: &str) -> TwirpResult<()> {
    const BAD: &[char] = &['"', ':', '<', '>', '|', '*', '?', '\r', '\n', '\\', '/'];
    if name.trim().is_empty() {
        return Err(TwirpError::invalid("Provided artifact name input is empty"));
    }
    if let Some(c) = name.chars().find(|c| BAD.contains(c)) {
        return Err(TwirpError::invalid(format!(
            "The artifact name is not valid: {name}. Contains the following character: {c:?}"
        )));
    }
    Ok(())
}

/// Staging id of an artifact upload: `{job}-{sha256(name)[..16]}`.
fn upload_id(job_id: i64, name: &str) -> String {
    let h = hex::encode(Sha256::digest(name.as_bytes()));
    format!("{job_id}-{}", &h[..16])
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct CreateArtifactRequest {
    #[serde(flatten)]
    ids: Ids,
    name: String,
    #[serde(alias = "expiresAt", deserialize_with = "opt_string")]
    expires_at: Option<String>,
    version: i64,
}

/// Upload URLs stay valid for six hours.
const UPLOAD_URL_TTL: i64 = 6 * 3600;
/// Download URLs stay valid for one hour.
const DOWNLOAD_URL_TTL: i64 = 3600;

async fn active_artifact(
    state: &AppState,
    run_id: i64,
    name: &str,
) -> TwirpResult<Option<ArtifactRow>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {} FROM actions_artifacts WHERE run_id = $1 AND name = $2 AND NOT expired",
        ArtifactRow::COLUMNS
    ))
    .bind(run_id)
    .bind(name)
    .fetch_optional(&state.db)
    .await?)
}

async fn create_artifact(state: &AppState, rj: &RuntimeJob, body: &[u8]) -> TwirpResult<Response> {
    let req: CreateArtifactRequest = decode(body)?;
    req.ids.check(rj, true)?;
    validate_artifact_name(&req.name)?;
    if req.version != 0 && req.version < 4 {
        return Err(TwirpError::invalid("unsupported artifact version"));
    }
    if active_artifact(state, rj.run.id, &req.name)
        .await?
        .is_some()
    {
        return Err(TwirpError::new(
            "already_exists",
            "an artifact with this name already exists on the workflow run",
        ));
    }
    let id = upload_id(rj.job.id, &req.name);
    let _ = tokio::fs::remove_file(blob::artifact_staging_path(state, &id)).await;
    let _ = tokio::fs::remove_dir_all(blob::artifact_blocks_dir(state, &id)).await;
    let url = signed_blob_url(
        state,
        "artifact-upload",
        &id,
        BlobPerm::Write,
        UPLOAD_URL_TTL,
    )?;
    let _ = req.expires_at;
    Ok(reply(&json!({"ok": true, "signed_upload_url": url})))
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct FinalizeArtifactRequest {
    #[serde(flatten)]
    ids: Ids,
    name: String,
    #[serde(deserialize_with = "int64")]
    size: i64,
    #[serde(deserialize_with = "opt_string")]
    hash: Option<String>,
}

async fn finalize_artifact(
    state: &AppState,
    rj: &RuntimeJob,
    body: &[u8],
) -> TwirpResult<Response> {
    let req: FinalizeArtifactRequest = decode(body)?;
    req.ids.check(rj, true)?;
    validate_artifact_name(&req.name)?;
    let id = upload_id(rj.job.id, &req.name);
    let staging = blob::artifact_staging_path(state, &id);
    let meta = tokio::fs::metadata(&staging)
        .await
        .map_err(|_| TwirpError::not_found("no upload found for this artifact"))?;
    if req.size > 0 && meta.len() != req.size as u64 {
        return Err(TwirpError::invalid(format!(
            "uploaded size {} does not match the declared size {}",
            meta.len(),
            req.size
        )));
    }
    let row = crate::server::store_artifact(state, &rj.job, &req.name, &staging, None).await?;
    let _ = tokio::fs::remove_file(&staging).await;
    if let Some(hash) = req.hash.filter(|h| !h.is_empty())
        && row.digest.as_deref() != Some(hash.as_str())
    {
        tracing::warn!(artifact = row.id, %hash, digest = ?row.digest, "artifact digest mismatch");
    }
    Ok(reply(
        &json!({"ok": true, "artifact_id": row.id.to_string()}),
    ))
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ListArtifactsRequest {
    #[serde(flatten)]
    ids: Ids,
    #[serde(alias = "nameFilter", deserialize_with = "opt_string")]
    name_filter: Option<String>,
    #[serde(alias = "idFilter", deserialize_with = "opt_int64")]
    id_filter: Option<i64>,
}

fn artifact_json(a: &ArtifactRow) -> Value {
    let mut v = json!({
        "workflow_run_backend_id": a.run_id.to_string(),
        "workflow_job_run_backend_id": a.job_id.map(|j| j.to_string()).unwrap_or_default(),
        "database_id": a.id.to_string(),
        "name": a.name,
        "size": a.size_in_bytes.to_string(),
        "created_at": bgh_core::time::Timestamp(a.created_at),
    });
    if let Some(d) = &a.digest {
        v["digest"] = json!(d);
    }
    v
}

async fn list_artifacts(state: &AppState, rj: &RuntimeJob, body: &[u8]) -> TwirpResult<Response> {
    let req: ListArtifactsRequest = decode(body)?;
    req.ids.check(rj, false)?;
    let rows: Vec<ArtifactRow> = sqlx::query_as(&format!(
        "SELECT {} FROM actions_artifacts
          WHERE run_id = $1 AND NOT expired
            AND ($2::text IS NULL OR name = $2) AND ($3::bigint IS NULL OR id = $3)
          ORDER BY id",
        ArtifactRow::COLUMNS
    ))
    .bind(rj.run.id)
    .bind(&req.name_filter)
    .bind(req.id_filter)
    .fetch_all(&state.db)
    .await?;
    let artifacts: Vec<Value> = rows.iter().map(artifact_json).collect();
    Ok(reply(&json!({ "artifacts": artifacts })))
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct NamedRequest {
    #[serde(flatten)]
    ids: Ids,
    name: String,
}

async fn signed_url(state: &AppState, rj: &RuntimeJob, body: &[u8]) -> TwirpResult<Response> {
    let req: NamedRequest = decode(body)?;
    req.ids.check(rj, false)?;
    let a = active_artifact(state, rj.run.id, &req.name)
        .await?
        .ok_or_else(|| TwirpError::not_found("artifact not found"))?;
    let url = signed_blob_url(
        state,
        "artifact",
        &a.id.to_string(),
        BlobPerm::Read,
        DOWNLOAD_URL_TTL,
    )?;
    Ok(reply(&json!({ "signed_url": url })))
}

async fn delete_artifact(state: &AppState, rj: &RuntimeJob, body: &[u8]) -> TwirpResult<Response> {
    let req: NamedRequest = decode(body)?;
    req.ids.check(rj, false)?;
    let a = active_artifact(state, rj.run.id, &req.name)
        .await?
        .ok_or_else(|| TwirpError::not_found("artifact not found"))?;
    sqlx::query("DELETE FROM actions_artifacts WHERE id = $1")
        .bind(a.id)
        .execute(&state.db)
        .await?;
    let _ = tokio::fs::remove_file(crate::server::artifact_path(state, a.id)).await;
    Ok(reply(&json!({"ok": true, "artifact_id": a.id.to_string()})))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_both_spellings_and_string_ints() {
        let r: FinalizeArtifactRequest = decode(
            br#"{"workflowRunBackendId":"1","workflow_job_run_backend_id":"2","name":"a","size":"42","hash":"sha256:x"}"#,
        )
        .unwrap();
        assert_eq!(r.ids.workflow_run_backend_id, "1");
        assert_eq!(r.ids.workflow_job_run_backend_id, "2");
        assert_eq!(r.size, 42);
        assert_eq!(r.hash.as_deref(), Some("sha256:x"));
        let l: ListArtifactsRequest =
            decode(br#"{"workflow_run_backend_id":"1","idFilter":"7"}"#).unwrap();
        assert_eq!(l.id_filter, Some(7));
        assert!(decode::<NamedRequest>(b"").is_ok());
        assert!(decode::<NamedRequest>(b"{").is_err());
    }

    #[test]
    fn artifact_names() {
        assert!(validate_artifact_name("dist").is_ok());
        assert!(validate_artifact_name("a/b").is_err());
        assert!(validate_artifact_name(" ").is_err());
    }
}
