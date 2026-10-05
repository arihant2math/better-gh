//! REST API rate limiting (fixed hourly window in Redis), configured by the
//! `rate_limits` site setting. Disabled by default, like GHES; when disabled
//! the middleware does nothing and `GET /rate_limit` answers 404.
//!
//! Authenticated callers are limited per user, anonymous callers per client
//! IP (`X-Forwarded-For`, else a shared bucket). Responses carry GitHub's
//! `X-RateLimit-{Limit,Remaining,Used,Reset,Resource}` headers; exceeding
//! the limit yields 403 "API rate limit exceeded".

use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use chrono::Utc;
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

fn bucket(ctx: Option<&AuthContext>, headers: &HeaderMap) -> String {
    match ctx {
        Some(c) => format!("u:{}", c.user.id),
        None => format!(
            "ip:{}",
            auth::client_ip(headers).unwrap_or_else(|| "-".into())
        ),
    }
}

fn window() -> (i64, i64) {
    let now = Utc::now().timestamp();
    let w = now / WINDOW_SECS;
    (w, (w + 1) * WINDOW_SECS)
}

/// Count one request (when `consume`) and return the caller's quota, or
/// `None` when rate limiting is disabled.
pub async fn quota(
    state: &AppState,
    ctx: Option<&AuthContext>,
    headers: &HeaderMap,
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
    let key = state.redis_key(&format!("ratelimit:{}:{w}", bucket(ctx, headers)));
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
    let q = match quota(&state, ctx.as_ref(), req.headers(), true).await {
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
                auth::client_ip(req.headers()).unwrap_or_else(|| "your IP".into())
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
