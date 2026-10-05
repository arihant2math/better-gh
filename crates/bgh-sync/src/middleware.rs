//! Request middleware (mounted for every route by bgh-server):
//!
//! * installs the request sync context ([`RequestSync`]) so `tx.sync(...)`
//!   records the `X-Client-Tx` header automatically;
//! * answers with `X-Bgh-Sync-Id: <max sync id written>` when the request
//!   committed synced data;
//! * makes mutations idempotent per `(user, X-Client-Tx)` for 24 h: a
//!   repeated request returns the stored response with
//!   `Idempotent-Replayed: true` instead of re-executing
//!   (docs/SYNC_PROTOCOL.md §7).

use axum::body::Body;
use axum::extract::{FromRequestParts, Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use bgh_core::auth::MaybeUser;
use bgh_core::state::AppState;
use bgh_core::sync::RequestSync;
use bgh_core::sync::context::{CLIENT_TX_HEADER, SYNC_ID_HEADER, parse_client_tx};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// How long a response is remembered for its `X-Client-Tx`.
pub const IDEMPOTENCY_TTL_SECS: u64 = 24 * 3600;
/// How long a request may hold the in-progress marker.
const PENDING_TTL_SECS: u64 = 120;
/// Larger responses are not stored (the request still succeeds).
const MAX_STORED_BODY: usize = 1024 * 1024;
const PENDING: &str = "pending";
/// Response headers kept in the idempotency record.
const KEPT_HEADERS: &[&str] = &["content-type", "location", "etag", "link", SYNC_ID_HEADER];

#[derive(Serialize, Deserialize)]
struct Stored {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

pub async fn http_middleware(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let client_tx = req
        .headers()
        .get(CLIENT_TX_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_client_tx);
    let ctx = RequestSync::new(client_tx);
    let mutation = !matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS);
    let (Some(client_tx), true) = (client_tx, mutation) else {
        return run(ctx, req, next).await;
    };

    // Resolve the caller (cached in the request extensions for the handler).
    let (mut parts, body) = req.into_parts();
    let user = MaybeUser::from_request_parts(&mut parts, &state).await;
    let req = Request::from_parts(parts, body);
    let Ok(MaybeUser(Some(auth))) = user else {
        return run(ctx, req, next).await;
    };
    let key = idempotency_key(&state, auth.user.id, client_tx);
    let mut redis = state.redis.clone();

    let claimed: Result<Option<String>, _> = redis::cmd("SET")
        .arg(&key)
        .arg(PENDING)
        .arg("NX")
        .arg("EX")
        .arg(PENDING_TTL_SECS)
        .query_async(&mut redis)
        .await;
    match claimed {
        Ok(Some(_)) => {}
        Ok(None) => {
            let existing: Result<Option<String>, _> =
                redis::cmd("GET").arg(&key).query_async(&mut redis).await;
            match existing {
                Ok(Some(v)) if v != PENDING => {
                    if let Ok(stored) = serde_json::from_str::<Stored>(&v) {
                        return replay(stored);
                    }
                }
                Ok(Some(_)) => return in_progress(),
                // Expired between SET and GET: just execute.
                Ok(None) => {}
                Err(err) => tracing::warn!(?err, "idempotency lookup"),
            }
        }
        Err(err) => {
            tracing::warn!(?err, "idempotency claim; executing without it");
            return run(ctx, req, next).await;
        }
    }

    let resp = run(ctx, req, next).await;
    let status = resp.status();
    let retryable = status.is_server_error()
        || matches!(
            status,
            StatusCode::REQUEST_TIMEOUT | StatusCode::TOO_MANY_REQUESTS | StatusCode::UNAUTHORIZED
        );
    if retryable {
        forget(&mut redis, &key).await;
        return resp;
    }
    let (parts, body) = resp.into_parts();
    let bytes = match axum::body::to_bytes(body, usize::MAX).await {
        Ok(b) => b,
        Err(err) => {
            tracing::error!(?err, "buffering response for idempotency");
            forget(&mut redis, &key).await;
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    if bytes.len() <= MAX_STORED_BODY {
        let stored = Stored {
            status: parts.status.as_u16(),
            headers: kept_headers(&parts.headers),
            body: STANDARD.encode(&bytes),
        };
        let json = serde_json::to_string(&stored).expect("serializable");
        let res: Result<(), _> = redis::cmd("SET")
            .arg(&key)
            .arg(json)
            .arg("EX")
            .arg(IDEMPOTENCY_TTL_SECS)
            .query_async(&mut redis)
            .await;
        if let Err(err) = res {
            tracing::warn!(?err, "storing idempotent response");
        }
    } else {
        forget(&mut redis, &key).await;
    }
    Response::from_parts(parts, Body::from(bytes))
}

/// Run the rest of the stack inside the sync context and add
/// `X-Bgh-Sync-Id`.
async fn run(ctx: std::sync::Arc<RequestSync>, req: Request, next: Next) -> Response {
    let mut resp = ctx.clone().scope(next.run(req)).await;
    let max = ctx.max_sync_id();
    if max > 0 {
        resp.headers_mut()
            .insert(SYNC_ID_HEADER, HeaderValue::from(max));
    }
    resp
}

fn kept_headers(h: &HeaderMap) -> Vec<(String, String)> {
    h.iter()
        .filter(|(k, _)| KEPT_HEADERS.contains(&k.as_str()))
        .filter_map(|(k, v)| Some((k.as_str().to_string(), v.to_str().ok()?.to_string())))
        .collect()
}

fn replay(stored: Stored) -> Response {
    let body = STANDARD.decode(stored.body).unwrap_or_default();
    let mut resp = Body::from(body).into_response();
    *resp.status_mut() = StatusCode::from_u16(stored.status).unwrap_or(StatusCode::OK);
    for (k, v) in stored.headers {
        if let (Ok(k), Ok(v)) = (HeaderName::try_from(k), HeaderValue::try_from(v)) {
            resp.headers_mut().insert(k, v);
        }
    }
    resp.headers_mut()
        .insert("idempotent-replayed", HeaderValue::from_static("true"));
    resp
}

/// The same tx is still executing: ask the client to retry shortly (the
/// client keeps its overlay on 429).
fn in_progress() -> Response {
    let mut resp = bgh_core::ApiError::Status(
        StatusCode::TOO_MANY_REQUESTS,
        "A request with this X-Client-Tx is still being processed.".into(),
    )
    .into_response();
    resp.headers_mut()
        .insert("retry-after", HeaderValue::from_static("1"));
    resp
}

async fn forget(redis: &mut redis::aio::ConnectionManager, key: &str) {
    let res: Result<(), _> = redis::cmd("DEL").arg(key).query_async(redis).await;
    if let Err(err) = res {
        tracing::warn!(?err, "clearing idempotency key");
    }
}

/// Redis key of the stored response for `(user, X-Client-Tx)`.
pub fn idempotency_key(state: &AppState, user_id: i64, tx: Uuid) -> String {
    state.redis_key(&format!("sync-idem:{user_id}:{tx}"))
}
