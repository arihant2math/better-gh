//! Non-API routes under `/_bgh/actions/...`:
//!
//! * the runner protocol for external `bgh-runner` processes (see
//!   [`crate::protocol`]);
//! * short-lived download URLs that the REST log/artifact endpoints
//!   redirect to (`/_bgh/actions/download/{token}`);
//! * live job logs as Server-Sent Events
//!   (`/_bgh/actions/jobs/{job_id}/logs/stream`): `log` events carry
//!   `{step, text}` with timestamped lines, a final `done` event closes it.

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
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::logs::{self, LogEvent};
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
    // Stream the body to a temp file.
    let dir = state.config.data_dir.join("actions").join("tmp");
    tokio::fs::create_dir_all(&dir).await?;
    let tmp = dir.join(format!("{}.zip", uuid::Uuid::new_v4()));
    {
        use tokio::io::AsyncWriteExt;
        let mut f = tokio::fs::File::create(&tmp).await?;
        let mut stream = body.into_data_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| ApiError::bad_request(e.to_string()))?;
            f.write_all(&chunk).await?;
        }
        f.flush().await?;
    }
    let res = server::store_artifact(&state, &job, &name, &tmp, q.retention_days).await;
    let _ = tokio::fs::remove_file(&tmp).await;
    let row = res.map_err(ApiError::internal)?;
    Ok((
        StatusCode::CREATED,
        Json(ArtifactInfo {
            id: row.id,
            name: row.name,
            size_in_bytes: row.size_in_bytes,
        }),
    ))
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
) -> ApiResult<Response> {
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|_| ApiError::Gone("The file is no longer available".into()))?;
    let len = file.metadata().await?.len();
    let body = Body::from_stream(tokio_util::io::ReaderStream::new(file));
    let mut resp = Response::new(body);
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(ct).map_err(ApiError::internal)?,
    );
    h.insert(header::CONTENT_LENGTH, HeaderValue::from(len));
    if let Some(name) = filename
        && let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{name}\""))
    {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    Ok(resp)
}

async fn download(State(state): State<AppState>, Path(token): Path<String>) -> ApiResult<Response> {
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
            let text = logs::read_job(&state, job_id).await;
            Ok(([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], text).into_response())
        }
        Download::RunLogs(run_id, attempt) => {
            let zip = crate::api::runs::build_run_logs_zip(&state, run_id, attempt)
                .await
                .map_err(ApiError::internal)?;
            Ok((
                [
                    (header::CONTENT_TYPE, "application/zip".to_string()),
                    (
                        header::CONTENT_DISPOSITION,
                        format!("attachment; filename=\"logs_{run_id}.zip\""),
                    ),
                ],
                zip,
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

    // Subscribe before reading files so nothing is lost in between.
    let client =
        redis::Client::open(state.config.redis_url.as_str()).map_err(ApiError::internal)?;
    let mut pubsub = client.get_async_pubsub().await?;
    pubsub.subscribe(logs::channel(&state, log_job)).await?;
    let mut messages = pubsub.into_on_message();

    let (tx, rx) = tokio::sync::mpsc::channel::<SseEvent>(64);
    tokio::spawn(async move {
        let mut sent: HashMap<i64, u64> = HashMap::new();
        for step in logs::steps(&state, log_job).await {
            let text = logs::read_step(&state, log_job, step).await;
            sent.insert(step, text.len() as u64);
            if !text.is_empty() {
                let ev = SseEvent::default()
                    .event("log")
                    .json_data(json!({"step": step, "text": text}))
                    .expect("json");
                if tx.send(ev).await.is_err() {
                    return;
                }
            }
        }
        let finished = |job: Option<JobRow>| job.is_none_or(|j| j.status == "completed");
        if finished(JobRow::find(&state.db, job_id).await.ok().flatten()) {
            let _ = tx.send(SseEvent::default().event("done").data("{}")).await;
            return;
        }
        let mut check = tokio::time::interval(Duration::from_secs(5));
        loop {
            tokio::select! {
                msg = messages.next() => {
                    let Some(msg) = msg else { break };
                    let Ok(payload) = msg.get_payload::<String>() else { continue };
                    let Ok(ev) = serde_json::from_str::<LogEvent>(&payload) else { continue };
                    if ev.done {
                        break;
                    }
                    let seen = sent.entry(ev.step).or_insert(0);
                    let end = ev.offset + ev.text.len() as u64;
                    if end <= *seen {
                        continue;
                    }
                    let skip = seen.saturating_sub(ev.offset) as usize;
                    let text = ev.text.get(skip..).unwrap_or("").to_string();
                    *seen = end;
                    let sse = SseEvent::default()
                        .event("log")
                        .json_data(json!({"step": ev.step, "text": text}))
                        .expect("json");
                    if tx.send(sse).await.is_err() {
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
