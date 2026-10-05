//! REST API rate limiting (fixed hourly window in Redis), configured by the
//! `rate_limits` site setting. Disabled by default, like GHES; when disabled
//! the middleware does nothing and `GET /rate_limit` answers 404.
//!
//! Authenticated callers are limited per user, anonymous callers per client
//! IP (`X-Forwarded-For`, else a shared bucket). Responses carry GitHub's
//! `X-RateLimit-{Limit,Remaining,Used,Reset,Resource}` headers; exceeding
//! the limit yields 403 "API rate limit exceeded".
//!
//! [`hit`] / [`count`] / [`clear`]: counters for throttling sensitive
//! actions (failed logins, 2FA attempts, password reset mails).

use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use redis::AsyncCommands;
use serde::Serialize;

use crate::auth::{self, AuthContext};
use crate::error::ApiError;
use crate::settings;
use crate::state::AppState;

const WINDOW_SECS: i64 = 3600;

/// One `resources.*` entry of `GET /rate_limit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Quota {
    pub limit: i64,
    pub used: i64,
    pub remaining: i64,
    pub reset: i64,
}

fn bucket(ctx: Option<&AuthContext>, ip: &str) -> String {
    match ctx {
        Some(c) => format!("u:{}", c.user.id),
        None => format!("ip:{ip}"),
    }
}

fn window() -> (i64, i64) {
    let now = Utc::now().timestamp();
    let w = now / WINDOW_SECS;
    (w, (w + 1) * WINDOW_SECS)
}

/// Count one request (when `consume`) and return the caller's quota, or
/// `None` when rate limiting is disabled. `ip` is the client IP
/// ([`auth::client_ip`]), used to bucket anonymous callers.
pub async fn quota(
    state: &AppState,
    ctx: Option<&AuthContext>,
    ip: &str,
    consume: bool,
) -> Result<Option<Quota>, ApiError> {
    let s = settings::load(state).await?;
    if !s.rate_limits.enabled {
        return Ok(None);
    }
    let limit = if ctx.is_some() {
        s.rate_limits.authenticated_per_hour
    } else {
        s.rate_limits.unauthenticated_per_hour
    };
    let (w, reset) = window();
    let key = state.redis_key(&format!("ratelimit:{}:{w}", bucket(ctx, ip)));
    let mut redis = state.redis.clone();
    let used: i64 = if consume {
        let (n,): (i64,) = redis::pipe()
            .atomic()
            .incr(&key, 1)
            .expire(&key, WINDOW_SECS)
            .ignore()
            .query_async(&mut redis)
            .await?;
        n
    } else {
        redis::cmd("GET")
            .arg(&key)
            .query_async::<Option<i64>>(&mut redis)
            .await?
            .unwrap_or(0)
    };
    Ok(Some(Quota {
        limit,
        used,
        remaining: (limit - used).max(0),
        reset,
    }))
}

fn set_headers(h: &mut HeaderMap, q: &Quota) {
    for (k, v) in [
        ("x-ratelimit-limit", q.limit),
        ("x-ratelimit-remaining", q.remaining),
        ("x-ratelimit-used", q.used),
        ("x-ratelimit-reset", q.reset),
    ] {
        h.insert(k, HeaderValue::from(v));
    }
    h.insert("x-ratelimit-resource", HeaderValue::from_static("core"));
}

/// Middleware for `/api/v3`. `GET /rate_limit` is not counted (like GitHub).
pub async fn rate_limit_middleware(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Response {
    match settings::load(&state).await {
        Ok(s) if s.rate_limits.enabled => {}
        _ => return next.run(req).await,
    }
    if req.uri().path().ends_with("/rate_limit") {
        return next.run(req).await;
    }
    // Bad credentials: let the handler render the 401.
    let Ok(ctx) = auth::resolve_request(&state, &mut req).await else {
        return next.run(req).await;
    };
    let ip = auth::client_ip(&state.config, req.headers(), req.extensions());
    let q = match quota(&state, ctx.as_ref(), &ip, true).await {
        Ok(Some(q)) => q,
        Ok(None) => return next.run(req).await,
        Err(err) => {
            // Never fail requests because Redis hiccuped.
            tracing::warn!(?err, "rate limit check failed");
            return next.run(req).await;
        }
    };
    if q.used > q.limit {
        let message = match &ctx {
            Some(c) => format!("API rate limit exceeded for user ID {}.", c.user.id),
            None => format!(
                "API rate limit exceeded for {}. (But here's the good news: Authenticated requests get a higher rate limit.)",
                ip
            ),
        };
        let mut resp = ApiError::Status(StatusCode::FORBIDDEN, message).into_response();
        set_headers(resp.headers_mut(), &q);
        return resp;
    }
    let mut resp = next.run(req).await;
    set_headers(resp.headers_mut(), &q);
    resp
}

// ---------------------------------------------------------------------------
// Throttling counters
// ---------------------------------------------------------------------------

/// Increment the counter `key` (expires `window_secs` after its first hit)
/// and return the new count.
pub async fn hit(state: &AppState, key: &str, window_secs: u64) -> redis::RedisResult<u64> {
    let key = state.redis_key(&format!("throttle:{key}"));
    let mut redis = state.redis.clone();
    let (n,): (u64,) = redis::pipe()
        .atomic()
        .incr(&key, 1)
        .cmd("EXPIRE")
        .arg(&key)
        .arg(window_secs)
        .arg("NX")
        .ignore()
        .query_async(&mut redis)
        .await?;
    Ok(n)
}

/// Current value of the counter `key` (0 if unset or on Redis errors).
pub async fn count(state: &AppState, key: &str) -> u64 {
    let mut redis = state.redis.clone();
    redis
        .get::<_, Option<u64>>(state.redis_key(&format!("throttle:{key}")))
        .await
        .ok()
        .flatten()
        .unwrap_or(0)
}

/// Reset the counter `key`.
pub async fn clear(state: &AppState, key: &str) {
    let mut redis = state.redis.clone();
    let _: Result<(), _> = redis.del(state.redis_key(&format!("throttle:{key}"))).await;
}
