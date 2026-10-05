//! Rate limiting backed by Redis.
//!
//! * [`middleware`]: GitHub-style API rate limits on `/api/v3` with
//!   `X-RateLimit-Limit|Remaining|Reset|Used|Resource` headers. Fixed
//!   one-hour windows per authenticated user (`BGH_RATE_LIMIT`, default
//!   5000) or per client IP for anonymous callers
//!   (`BGH_RATE_LIMIT_ANONYMOUS`, default 60); the `search` resource has a
//!   per-minute budget (30 / 10). Exceeding the limit → 403 "API rate limit
//!   exceeded". `GET /rate_limit` is not counted. Redis failures fail open.
//! * [`hit`] / [`count`] / [`clear`]: counters for throttling sensitive
//!   actions (failed logins, 2FA attempts, password reset mails).

use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use redis::AsyncCommands;
use serde::Serialize;

use crate::auth::{self, AuthContext};
use crate::error::ApiError;
use crate::state::AppState;

/// One rate-limit bucket's state (`GET /rate_limit` shape).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RateLimit {
    pub limit: u32,
    pub used: u32,
    pub remaining: u32,
    /// Unix seconds when the window resets.
    pub reset: i64,
    #[serde(skip)]
    pub resource: &'static str,
}

impl RateLimit {
    fn new(limit: u32, used: u32, reset: i64, resource: &'static str) -> Self {
        Self {
            limit,
            used,
            remaining: limit.saturating_sub(used),
            reset,
            resource,
        }
    }

    pub fn exceeded(&self) -> bool {
        self.used > self.limit
    }

    /// Insert the `X-RateLimit-*` headers.
    pub fn apply(&self, headers: &mut HeaderMap) {
        let used = self.used.min(self.limit);
        for (name, value) in [
            ("x-ratelimit-limit", self.limit.to_string()),
            ("x-ratelimit-remaining", self.remaining.to_string()),
            ("x-ratelimit-reset", self.reset.to_string()),
            ("x-ratelimit-used", used.to_string()),
            ("x-ratelimit-resource", self.resource.to_string()),
        ] {
            if let Ok(v) = HeaderValue::from_str(&value) {
                headers.insert(name, v);
            }
        }
    }
}

/// Which bucket a request counts against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resource {
    Core,
    Search,
}

impl Resource {
    pub fn for_path(path: &str) -> Self {
        if path.contains("/search/") {
            Self::Search
        } else {
            Self::Core
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Core => "core",
            Self::Search => "search",
        }
    }

    fn window_secs(self) -> i64 {
        match self {
            Self::Core => 3600,
            Self::Search => 60,
        }
    }

    /// Limit for this resource; `None` when rate limiting is disabled.
    pub fn limit(self, state: &AppState, authenticated: bool) -> Option<u32> {
        let c = &state.config;
        if c.rate_limit_authenticated == 0 {
            return None;
        }
        Some(match (self, authenticated) {
            (Self::Core, true) => c.rate_limit_authenticated,
            (Self::Core, false) => c.rate_limit_anonymous,
            (Self::Search, true) => 30,
            (Self::Search, false) => 10,
        })
    }
}

/// Rate-limit identity of a caller: the user, or the client IP.
pub fn caller_key(auth: Option<&AuthContext>, ip: &str) -> String {
    match auth {
        Some(a) => format!("u:{}", a.user.id),
        None => format!("ip:{ip}"),
    }
}

fn window(resource: Resource) -> (i64, i64) {
    let now = chrono::Utc::now().timestamp();
    let w = resource.window_secs();
    let start = now - now.rem_euclid(w);
    (start, start + w)
}

fn bucket_key(state: &AppState, resource: Resource, caller: &str, start: i64) -> String {
    state.redis_key(&format!("rl:{}:{caller}:{start}", resource.name()))
}

/// Count one request against `caller`'s bucket.
pub async fn consume(
    state: &AppState,
    resource: Resource,
    caller: &str,
    limit: u32,
) -> redis::RedisResult<RateLimit> {
    let (start, reset) = window(resource);
    let key = bucket_key(state, resource, caller, start);
    let mut redis = state.redis.clone();
    let (used,): (u32,) = redis::pipe()
        .atomic()
        .incr(&key, 1)
        .expire(&key, resource.window_secs() + 60)
        .ignore()
        .query_async(&mut redis)
        .await?;
    Ok(RateLimit::new(limit, used, reset, resource.name()))
}

/// Current state of `caller`'s bucket without counting a request.
pub async fn peek(
    state: &AppState,
    resource: Resource,
    caller: &str,
    limit: u32,
) -> redis::RedisResult<RateLimit> {
    let (start, reset) = window(resource);
    let mut redis = state.redis.clone();
    let used: Option<u32> = redis
        .get(bucket_key(state, resource, caller, start))
        .await?;
    Ok(RateLimit::new(
        limit,
        used.unwrap_or(0),
        reset,
        resource.name(),
    ))
}

/// Middleware for the REST API (layered on `/api/v3` by bgh-server).
pub async fn middleware(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    let resource = Resource::for_path(req.uri().path());
    let not_counted = req.uri().path().ends_with("/rate_limit");
    // Resolve the caller once (cached for the handler). Bad credentials are
    // treated as anonymous here; the handler reports the 401.
    let auth = auth::resolve_request(&state, &mut req).await.ok().flatten();
    let Some(limit) = resource.limit(&state, auth.is_some()) else {
        return next.run(req).await;
    };
    let ip = auth::client_ip(&state.config, req.headers(), req.extensions());
    let caller = caller_key(auth.as_ref(), &ip);
    let rl = if not_counted {
        peek(&state, resource, &caller, limit).await
    } else {
        consume(&state, resource, &caller, limit).await
    };
    let rl = match rl {
        Ok(rl) => rl,
        Err(err) => {
            tracing::warn!(?err, "rate limiter unavailable; allowing request");
            return next.run(req).await;
        }
    };
    if rl.exceeded() {
        let who = match &auth {
            Some(a) => format!("user ID {}", a.user.id),
            None => ip,
        };
        let mut resp = ApiError::forbidden(format!(
            "API rate limit exceeded for {who}. (But here's the good news: Authenticated \
             requests get a higher rate limit. Check out the documentation for more details.)"
        ))
        .into_response();
        rl.apply(resp.headers_mut());
        let retry = (rl.reset - chrono::Utc::now().timestamp()).max(1);
        if let Ok(v) = HeaderValue::from_str(&retry.to_string()) {
            resp.headers_mut().insert("retry-after", v);
        }
        return resp;
    }
    let mut resp = next.run(req).await;
    rl.apply(resp.headers_mut());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resources() {
        assert_eq!(
            Resource::for_path("/api/v3/search/issues"),
            Resource::Search
        );
        assert_eq!(Resource::for_path("/api/v3/user"), Resource::Core);
        let rl = RateLimit::new(60, 61, 0, "core");
        assert!(rl.exceeded());
        assert_eq!(rl.remaining, 0);
        let mut h = HeaderMap::new();
        rl.apply(&mut h);
        assert_eq!(h["x-ratelimit-used"], "60");
    }
}
