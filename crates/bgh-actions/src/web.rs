//! Non-API routes under `/_bgh/actions/...`:
//!
//! * the runner protocol for external `bgh-runner` processes (see
//!   [`crate::protocol`]);
//! * short-lived download URLs that the REST log/artifact endpoints
//!   redirect to (`/_bgh/actions/download/{token}`);
//! * live job logs as Server-Sent Events
//!   (`/_bgh/actions/jobs/{job_id}/logs/stream`): `log` events carry
//!   `{step, offset, text}` with timestamped lines (existing logs replay in
//!   chunks of at most 64 KiB), a final `done` event closes it. Viewers
//!   share one Redis connection ([`crate::live_logs`]).

use std::collections::HashMap;
use std::convert::Infallible;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Router, body::Bytes};
use bgh_core::crypto as core_crypto;
use bgh_core::prelude::*;
use futures::{StreamExt, TryStreamExt};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::logs;
use crate::models::{ArtifactRow, JobRow, RunnerRow};
use crate::protocol::{ArtifactInfo, JobCompletion, RegisterRequest, RegisterResponse, StepState};
use crate::server;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/_bgh/actions/runner/register", post(register))
        .route("/_bgh/actions/runner/self", delete(unregister))
        .route("/_bgh/actions/runner/acquire", post(acquire))
        .route("/_bgh/actions/runner/jobs/{id}/logs", post(append_log))
        .route("/_bgh/actions/runner/jobs/{id}/steps", post(update_steps))
        .route("/_bgh/actions/runner/jobs/{id}/complete", post(complete))
        .route(
            "/_bgh/actions/runner/jobs/{id}/artifacts",
            get(list_artifacts),
        )
        .route(
            "/_bgh/actions/runner/jobs/{id}/artifacts/{name}",
            put(upload_artifact),
        )
        .route(
            "/_bgh/actions/runner/jobs/{id}/artifacts/{artifact_id}/zip",
            get(download_artifact),
        )
        .route("/_bgh/actions/download/{token}", get(download))
        .route("/_bgh/actions/jobs/{id}/logs/stream", get(stream_job_logs))
}

// ---------------------------------------------------------------------------
// Runner protocol
// ---------------------------------------------------------------------------

/// The runner calling, from `Authorization: RunnerToken <token>`.
async fn runner(state: &AppState, headers: &HeaderMap) -> ApiResult<RunnerRow> {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("RunnerToken "))
        .ok_or_else(ApiError::requires_auth)?;
    server::runner_by_token(state, token.trim())
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(ApiError::bad_credentials)
}

async fn runner_job(state: &AppState, r: &RunnerRow, job_id: i64) -> ApiResult<JobRow> {
    server::runner_job(state, r, job_id)
        .await
        .map_err(ApiError::internal)?
        .ok_or(ApiError::NotFound)
}

async fn register(
    State(state): State<AppState>,
    Json(req): Json<RegisterRequest>,
) -> ApiResult<(StatusCode, Json<RegisterResponse>)> {
    let row: Option<(Option<i64>, Option<i64>)> = sqlx::query_as(
        "SELECT repo_id, org_id FROM actions_runner_tokens
          WHERE token_hash = $1 AND kind = 'registration' AND expires_at > now()",
    )
    .bind(core_crypto::sha256_hex(req.token.trim()))
    .fetch_optional(&state.db)
    .await?;
    let (repo_id, org_id) = row.ok_or_else(ApiError::bad_credentials)?;
    let created = crate::api::runners::create_runner(
        &state,
        crate::api::runners::NewRunner {
            repo_id,
            org_id,
            name: &req.name,
            labels: &req.labels,
            os: req.os.as_deref(),
            arch: req.arch.as_deref(),
            ephemeral: req.ephemeral,
            group_id: None,
            group_name: req.runner_group.as_deref(),
        },
    )
    .await?;
    let (id, name, token) = (created.runner.id, created.runner.name, created.token);
    Ok((
        StatusCode::CREATED,
        Json(RegisterResponse { id, name, token }),
    ))
}

async fn unregister(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<StatusCode> {
    let r = runner(&state, &headers).await?;
    sqlx::query("DELETE FROM actions_runners WHERE id = $1")
        .bind(r.id)
        .execute(&state.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct WaitQuery {
    wait: Option<u64>,
}

async fn acquire(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<WaitQuery>,
) -> ApiResult<Response> {
    let r = runner(&state, &headers).await?;
    let wait = Duration::from_secs(q.wait.unwrap_or(30).min(60));
    match server::acquire(&state, &r, wait)
        .await
        .map_err(ApiError::internal)?
    {
        Some(spec) => Ok(Json(spec).into_response()),
        None => Ok(StatusCode::NO_CONTENT.into_response()),
    }
}

#[derive(Deserialize)]
struct StepQuery {
    step: Option<i64>,
}

async fn append_log(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Query(q): Query<StepQuery>,
    body: Bytes,
) -> ApiResult<StatusCode> {
    let r = runner(&state, &headers).await?;
    let job = runner_job(&state, &r, id).await?;
    if job.status == "completed" {
        return Err(ApiError::conflict("Job is completed"));
    }
    let text = String::from_utf8_lossy(&body);
    logs::append(&state, job.id, q.step.unwrap_or(1).max(1), &text)
        .await
        .map_err(ApiError::internal)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn update_steps(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(steps): Json<Vec<StepState>>,
) -> ApiResult<Response> {
    let r = runner(&state, &headers).await?;
    match server::update_steps(&state, &r, id, &steps)
        .await
        .map_err(ApiError::internal)?
    {
        Some(hb) => Ok(Json(hb).into_response()),
        None => Err(ApiError::NotFound),
    }
}

async fn complete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(result): Json<JobCompletion>,
) -> ApiResult<StatusCode> {
    let r = runner(&state, &headers).await?;
    if server::complete_job(&state, &r, id, &result)
        .await
        .map_err(ApiError::internal)?
    {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound)
    }
}

async fn list_artifacts(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<Vec<ArtifactInfo>>> {
    let r = runner(&state, &headers).await?;
    let job = runner_job(&state, &r, id).await?;
    let rows = server::run_artifacts(&state, job.run_id)
        .await
        .map_err(ApiError::internal)?;
    Ok(Json(
        rows.into_iter()
            .map(|a| ArtifactInfo {
                id: a.id,
                name: a.name,
                size_in_bytes: a.size_in_bytes,
            })
            .collect(),
    ))
}

#[derive(Deserialize)]
struct UploadQuery {
    retention_days: Option<i64>,
}

async fn upload_artifact(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, name)): Path<(i64, String)>,
    Query(q): Query<UploadQuery>,
    body: Body,
) -> ApiResult<(StatusCode, Json<ArtifactInfo>)> {
    let r = runner(&state, &headers).await?;
    let job = runner_job(&state, &r, id).await?;
    if name.is_empty() || name.contains(['/', '\\', '"', ':', '<', '>', '|', '*', '?']) {
        return Err(ApiError::invalid_field(FieldError::invalid(
            "Artifact", "name",
        )));
    }
    // Stream the body to a temp file, hashing as it goes and enforcing the
    // size limit before anything past it is written.
    let dir = state.config.data_dir.join("actions").join("tmp");
    tokio::fs::create_dir_all(&dir).await?;
    let tmp = dir.join(format!("{}.zip", uuid::Uuid::new_v4()));
    let spooled = spool_artifact(body, &tmp, server::max_artifact_size(&state)).await;
    let res = match spooled {
        Ok(digest) => {
            server::store_artifact(&state, &job, &name, &tmp, Some(digest), q.retention_days)
                .await
                .map_err(ApiError::internal)
        }
        Err(e) => Err(e),
    };
    let _ = tokio::fs::remove_file(&tmp).await;
    // Keep the spool's 413 / 400; only store failures are internal.
    let row = res?;
    Ok((
        StatusCode::CREATED,
        Json(ArtifactInfo {
            id: row.id,
            name: row.name,
            size_in_bytes: row.size_in_bytes,
        }),
    ))
}

/// Write an artifact upload body to `path`; returns its `sha256:` digest.
/// Over `max` bytes is a 413.
async fn spool_artifact(body: Body, path: &std::path::Path, max: u64) -> ApiResult<String> {
    use sha2::{Digest, Sha256};
    use tokio::io::AsyncWriteExt;
    let mut f = tokio::fs::File::create(path).await?;
    let mut hasher = Sha256::new();
    let mut size = 0u64;
    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| ApiError::bad_request(e.to_string()))?;
        size += chunk.len() as u64;
        if size > max {
            return Err(ApiError::Status(
                StatusCode::PAYLOAD_TOO_LARGE,
                format!("Artifacts may not exceed {max} bytes"),
            ));
        }
        hasher.update(&chunk);
        f.write_all(&chunk).await?;
    }
    f.flush().await?;
    Ok(format!("sha256:{}", hex::encode(hasher.finalize())))
}

async fn download_artifact(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, artifact_id)): Path<(i64, i64)>,
) -> ApiResult<Response> {
    let r = runner(&state, &headers).await?;
    let job = runner_job(&state, &r, id).await?;
    let ok: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM actions_artifacts WHERE id = $1 AND run_id = $2 AND NOT expired)",
    )
    .bind(artifact_id)
    .bind(job.run_id)
    .fetch_one(&state.db)
    .await?;
    if !ok {
        return Err(ApiError::NotFound);
    }
    file_response(
        &server::artifact_path(&state, artifact_id),
        "application/zip",
        None,
        &headers,
    )
    .await
}

// ---------------------------------------------------------------------------
// Signed downloads
// ---------------------------------------------------------------------------

/// What a download token grants.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Download {
    /// Plain-text log of a job (the log-owning job id).
    JobLog(i64),
    /// Zip of all job logs of `(run_id, attempt)`.
    RunLogs(i64, i32),
    Artifact(i64),
}

const DOWNLOAD_TTL_SECS: u64 = 60;

/// 302 to `/_bgh/actions/download/{token}` (valid one minute).
pub async fn redirect_download(state: &AppState, what: Download) -> ApiResult<Response> {
    let token = core_crypto::random_token(40);
    let mut redis = state.redis.clone();
    let _: () = redis::cmd("SET")
        .arg(state.redis_key(&format!("actions:dl:{token}")))
        .arg(serde_json::to_string(&what)?)
        .arg("EX")
        .arg(DOWNLOAD_TTL_SECS)
        .query_async(&mut redis)
        .await?;
    let url = format!("{}/_bgh/actions/download/{token}", state.config.base_url);
    let mut resp = StatusCode::FOUND.into_response();
    resp.headers_mut().insert(
        header::LOCATION,
        HeaderValue::from_str(&url).map_err(ApiError::internal)?,
    );
    Ok(resp)
}

async fn file_response(
    path: &std::path::Path,
    ct: &str,
    filename: Option<&str>,
    headers: &HeaderMap,
) -> ApiResult<Response> {
    let len = tokio::fs::metadata(path)
        .await
        .map_err(|_| ApiError::Gone("The file is no longer available".into()))?
        .len();
    files_response(vec![(path.to_path_buf(), len)], ct, filename, headers).await
}

/// Parse a single-range `Range: bytes=…` header against `total` bytes.
/// `None`: no (usable) range, serve everything; `Some(Err(()))`: not
/// satisfiable.
pub fn parse_range(value: Option<&HeaderValue>, total: u64) -> Option<Result<(u64, u64), ()>> {
    let spec = value?.to_str().ok()?.trim().strip_prefix("bytes=")?;
    if spec.contains(',') {
        return None; // multiple ranges: serve the whole body
    }
    let (a, b) = spec.split_once('-')?;
    let (a, b) = (a.trim(), b.trim());
    let range = if a.is_empty() {
        let n: u64 = b.parse().ok()?;
        if n == 0 || total == 0 {
            return Some(Err(()));
        }
        (total.saturating_sub(n), total - 1)
    } else {
        let start: u64 = a.parse().ok()?;
        let end = if b.is_empty() {
            u64::MAX
        } else {
            b.parse().ok()?
        };
        if end < start {
            return None;
        }
        if start >= total {
            return Some(Err(()));
        }
        (start, end.min(total - 1))
    };
    Some(Ok(range))
}

/// Stream the concatenation of `files` (each cut at the given length), with
/// `Range` support. Nothing is buffered beyond `ReaderStream`'s chunks.
async fn files_response(
    files: Vec<(std::path::PathBuf, u64)>,
    ct: &str,
    filename: Option<&str>,
    headers: &HeaderMap,
) -> ApiResult<Response> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    let total: u64 = files.iter().map(|(_, n)| n).sum();
    let (status, start, end) = match parse_range(headers.get(header::RANGE), total) {
        None => (StatusCode::OK, 0, total),
        Some(Ok((a, b))) => (StatusCode::PARTIAL_CONTENT, a, b + 1),
        Some(Err(())) => {
            let mut resp = StatusCode::RANGE_NOT_SATISFIABLE.into_response();
            resp.headers_mut().insert(
                header::CONTENT_RANGE,
                HeaderValue::from_str(&format!("bytes */{total}")).map_err(ApiError::internal)?,
            );
            return Ok(resp);
        }
    };
    // (path, offset in file, bytes to send) for the files the range covers.
    let mut parts = Vec::new();
    let mut pos = 0u64;
    for (path, len) in files {
        let (lo, hi) = (start.max(pos), end.min(pos + len));
        if lo < hi {
            parts.push((path, lo - pos, hi - lo));
        }
        pos += len;
    }
    let body = futures::stream::iter(parts)
        .then(|(path, offset, n)| async move {
            let mut f = tokio::fs::File::open(&path).await?;
            f.seek(std::io::SeekFrom::Start(offset)).await?;
            Ok::<_, std::io::Error>(tokio_util::io::ReaderStream::new(f.take(n)))
        })
        .try_flatten();
    let mut resp = Response::new(Body::from_stream(body));
    *resp.status_mut() = status;
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(ct).map_err(ApiError::internal)?,
    );
    h.insert(header::CONTENT_LENGTH, HeaderValue::from(end - start));
    h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    if status == StatusCode::PARTIAL_CONTENT {
        h.insert(
            header::CONTENT_RANGE,
            HeaderValue::from_str(&format!("bytes {start}-{}/{total}", end - 1))
                .map_err(ApiError::internal)?,
        );
    }
    if let Some(name) = filename
        && let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{name}\""))
    {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    Ok(resp)
}

async fn download(
    State(state): State<AppState>,
    Path(token): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let mut redis = state.redis.clone();
    let raw: Option<String> = redis::cmd("GET")
        .arg(state.redis_key(&format!("actions:dl:{token}")))
        .query_async(&mut redis)
        .await?;
    let what: Download = raw
        .and_then(|r| serde_json::from_str(&r).ok())
        .ok_or(ApiError::NotFound)?;
    match what {
        Download::JobLog(job_id) => {
            files_response(
                logs::job_files(&state, job_id).await,
                "text/plain; charset=utf-8",
                None,
                &headers,
            )
            .await
        }
        Download::RunLogs(run_id, attempt) => {
            let (zip, len) = crate::api::runs::build_run_logs_zip(&state, run_id, attempt)
                .await
                .map_err(ApiError::internal)?;
            // An unlinked temp file: gone once the body is dropped.
            let body = Body::from_stream(tokio_util::io::ReaderStream::new(
                tokio::fs::File::from_std(zip),
            ));
            Ok((
                [
                    (header::CONTENT_TYPE, "application/zip".to_string()),
                    (
                        header::CONTENT_DISPOSITION,
                        format!("attachment; filename=\"logs_{run_id}.zip\""),
                    ),
                    (header::CONTENT_LENGTH, len.to_string()),
                ],
                body,
            )
                .into_response())
        }
        Download::Artifact(id) => {
            let a: ArtifactRow = sqlx::query_as(&format!(
                "SELECT {} FROM actions_artifacts WHERE id = $1",
                ArtifactRow::COLUMNS
            ))
            .bind(id)
            .fetch_optional(&state.db)
            .await?
            .ok_or(ApiError::NotFound)?;
            file_response(
                &server::artifact_path(&state, a.id),
                "application/zip",
                Some(&format!("{}.zip", a.name)),
                &headers,
            )
            .await
        }
    }
}

// ---------------------------------------------------------------------------
// Live logs (SSE)
// ---------------------------------------------------------------------------

/// `GET /_bgh/actions/jobs/{job_id}/logs/stream` (session or token auth,
/// read access to the repository).
pub async fn stream_job_logs(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path(job_id): Path<i64>,
) -> ApiResult<Response> {
    let job = JobRow::find(&state.db, job_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let repo = db::Repository::find(&state.db, job.repo_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let owner = db::User::find(&state.db, repo.owner_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    RepoAccess::for_repo(&state, auth.as_ref(), repo, owner).await?;
    let log_job = job.log_owner();

    // Subscribe before reading files so nothing is lost in between. One
    // shared Redis connection serves every viewer.
    let hub = crate::live_logs::LogHub::get(&state)
        .await
        .map_err(ApiError::internal)?;
    let mut sub = hub.subscribe(log_job).await.map_err(ApiError::internal)?;

    let (tx, rx) = tokio::sync::mpsc::channel::<SseEvent>(64);
    tokio::spawn(async move {
        use crate::live_logs::LiveMsg;
        use tokio::sync::broadcast::error::RecvError;
        let mut sent: HashMap<i64, u64> = HashMap::new();
        if catch_up(&state, log_job, &mut sent, &tx).await.is_err() {
            return;
        }
        let finished = |job: Option<JobRow>| job.is_none_or(|j| j.status == "completed");
        if finished(JobRow::find(&state.db, job_id).await.ok().flatten()) {
            let _ = tx.send(SseEvent::default().event("done").data("{}")).await;
            return;
        }
        let mut check = tokio::time::interval(Duration::from_secs(5));
        loop {
            tokio::select! {
                msg = sub.rx.recv() => {
                    let ev = match msg {
                        Ok(LiveMsg::Event(ev)) => ev,
                        // Chunks may be missing: re-read from the files.
                        Ok(LiveMsg::Resync) | Err(RecvError::Lagged(_)) => {
                            if catch_up(&state, log_job, &mut sent, &tx).await.is_err() {
                                return;
                            }
                            continue;
                        }
                        Err(RecvError::Closed) => break,
                    };
                    if ev.done {
                        break;
                    }
                    let seen = sent.get(&ev.step).copied().unwrap_or(0);
                    if ev.offset > seen {
                        // A gap before this chunk: the files have it.
                        if catch_up(&state, log_job, &mut sent, &tx).await.is_err() {
                            return;
                        }
                        continue;
                    }
                    let end = ev.offset + ev.text.len() as u64;
                    if end <= seen {
                        continue;
                    }
                    let skip = (seen - ev.offset) as usize;
                    let Some(text) = ev.text.get(skip..) else { continue };
                    sent.insert(ev.step, end);
                    if tx.send(log_event(ev.step, seen, text)).await.is_err() {
                        return;
                    }
                }
                _ = check.tick() => {
                    if finished(JobRow::find(&state.db, job_id).await.ok().flatten()) {
                        break;
                    }
                }
                _ = tx.closed() => return,
            }
        }
        let _ = tx.send(SseEvent::default().event("done").data("{}")).await;
    });
    let stream = tokio_stream::wrappers::ReceiverStream::new(rx).map(Ok::<_, Infallible>);
    Ok(Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response())
}

/// Largest `text` of one replayed SSE `log` event.
const REPLAY_CHUNK: usize = 64 << 10;

fn log_event(step: i64, offset: u64, text: &str) -> SseEvent {
    SseEvent::default()
        .event("log")
        .json_data(json!({"step": step, "offset": offset, "text": text}))
        .expect("json")
}

/// Send everything in the job's step files past `sent`, in bounded chunks.
/// `Err` when the viewer is gone.
async fn catch_up(
    state: &AppState,
    job_id: i64,
    sent: &mut HashMap<i64, u64>,
    tx: &tokio::sync::mpsc::Sender<SseEvent>,
) -> Result<(), ()> {
    for step in logs::steps(state, job_id).await {
        let offset = sent.entry(step).or_insert(0);
        while let Ok(Some((text, next))) =
            logs::read_chunk(state, job_id, step, *offset, REPLAY_CHUNK).await
        {
            let ev = log_event(step, *offset, &text);
            *offset = next;
            tx.send(ev).await.map_err(|_| ())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn spool_hashes_and_caps() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.zip");
        let digest = spool_artifact(Body::from("hello"), &path, 5).await.unwrap();
        // sha256("hello")
        assert_eq!(
            digest,
            "sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"hello");
        let err = spool_artifact(Body::from("hello!"), &path, 5)
            .await
            .unwrap_err();
        assert_eq!(err.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[test]
    fn ranges() {
        let h = |s: &str| HeaderValue::from_str(s).unwrap();
        assert_eq!(parse_range(None, 10), None);
        assert_eq!(parse_range(Some(&h("bytes=2-4")), 10), Some(Ok((2, 4))));
        assert_eq!(parse_range(Some(&h("bytes=2-")), 10), Some(Ok((2, 9))));
        assert_eq!(parse_range(Some(&h("bytes=5-99")), 10), Some(Ok((5, 9))));
        assert_eq!(parse_range(Some(&h("bytes=-3")), 10), Some(Ok((7, 9))));
        assert_eq!(parse_range(Some(&h("bytes=-30")), 10), Some(Ok((0, 9))));
        assert_eq!(parse_range(Some(&h("bytes=10-")), 10), Some(Err(())));
        assert_eq!(parse_range(Some(&h("bytes=-0")), 10), Some(Err(())));
        assert_eq!(parse_range(Some(&h("bytes=4-2")), 10), None);
        assert_eq!(parse_range(Some(&h("bytes=0-1,4-5")), 10), None);
        assert_eq!(parse_range(Some(&h("items=0-1")), 10), None);
    }
}
