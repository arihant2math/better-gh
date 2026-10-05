//! GitHub-compatible API rate limits (Redis fixed windows) and throttling
//! counters.
//!
//! * Budgets per resource: `core` (REST, per hour), `search` (`/search/*`,
//!   per minute) and `graphql` (`/api/graphql`, per hour); authenticated
//!   callers are counted per user, anonymous callers per client IP
//!   ([`auth::client_ip`]). Limits come from the `rate_limits` site setting
//!   ([`settings::RateLimitSettings`]), whose defaults are the
//!   `BGH_RATE_LIMIT*` environment variables.
//! * Every API response carries GitHub's
//!   `X-RateLimit-{Limit,Remaining,Reset,Used,Resource}` headers.
//!   `GET /rate_limit` always answers in GitHub's shape and is not counted.
//! * `304 Not Modified` responses are not counted ([`refund`]), like GitHub.
//! * Enforcement is a switch (`rate_limits.enabled`, off by default like
//!   GHES): when on, a caller over budget gets 403 "API rate limit exceeded
//!   for …" with `Retry-After`. Redis failures fail open (no headers).
//! * [`hit`] / [`count`] / [`clear`]: counters for throttling sensitive
//!   actions (failed logins, 2FA attempts, password reset mails).
//!
//! Mounted by bgh-server: [`middleware`] on the nested `/api/v3` router and
//! [`root_middleware`] at the root for the API paths outside it
//! (`/api/graphql`, `/api/v3/`).

use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use redis::AsyncCommands;
use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::auth::{self, AuthContext};
use crate::error::ApiError;
use crate::settings::{self, RateLimitSettings};
use crate::state::AppState;

/// A rate-limited resource (separate budgets).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resource {
    Core,
    Search,
    Graphql,
}

impl Resource {
    pub const ALL: [Resource; 3] = [Resource::Core, Resource::Search, Resource::Graphql];

    /// The resource a request path counts against.
    pub fn for_path(path: &str) -> Self {
        if path == "/api/graphql" {
            Self::Graphql
        } else if path.contains("/search/") {
            Self::Search
        } else {
            Self::Core
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Core => "core",
            Self::Search => "search",
            Self::Graphql => "graphql",
        }
    }

    fn window_secs(self) -> i64 {
        match self {
            Self::Search => 60,
            Self::Core | Self::Graphql => 3600,
        }
    }

    /// The limit for an (un)authenticated caller under `s`.
    pub fn limit(self, s: &RateLimitSettings, authenticated: bool) -> i64 {
        match (self, authenticated) {
            (Self::Core, true) => s.authenticated_per_hour,
            (Self::Core | Self::Graphql, false) => s.unauthenticated_per_hour,
            (Self::Search, true) => s.search_authenticated_per_minute,
            (Self::Search, false) => s.search_unauthenticated_per_minute,
            (Self::Graphql, true) => s.graphql_per_hour,
        }
    }
}

/// One budget's state (a `resources.*` entry of `GET /rate_limit`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Quota {
    pub limit: i64,
    /// Requests counted in the window, capped at `limit` (like GitHub).
    pub used: i64,
    pub remaining: i64,
    /// Unix seconds when the window resets.
    pub reset: i64,
    #[serde(skip)]
    pub resource: Resource,
    /// Uncapped count (including rejected requests).
    #[serde(skip)]
    pub count: i64,
}

impl Quota {
    fn new(resource: Resource, limit: i64, count: i64, reset: i64) -> Self {
        Self {
            limit,
            used: count.min(limit),
            remaining: (limit - count).max(0),
            reset,
            resource,
            count,
        }
    }

    pub fn exceeded(&self) -> bool {
        self.count > self.limit
    }

    /// Insert the `X-RateLimit-*` headers.
    pub fn apply(&self, h: &mut HeaderMap) {
        for (k, v) in [
            ("x-ratelimit-limit", self.limit),
            ("x-ratelimit-remaining", self.remaining),
            ("x-ratelimit-reset", self.reset),
            ("x-ratelimit-used", self.used),
        ] {
            h.insert(k, HeaderValue::from(v));
        }
        h.insert(
            "x-ratelimit-resource",
            HeaderValue::from_static(self.resource.name()),
        );
    }
}

/// Rate-limit identity: the installation (GitHub App installation tokens
/// get a bucket each), the app (JWT), the user, or the client IP.
fn caller_key(ctx: Option<&AuthContext>, ip: &str) -> String {
    let Some(c) = ctx else {
        return format!("ip:{ip}");
    };
    if let Some(i) = crate::apps::installation_id(c) {
        format!("i:{i}")
    } else if let Some(a) = crate::apps::jwt_app_id(c) {
        format!("a:{a}")
    } else {
        format!("u:{}", c.user.id)
    }
}

/// `(window start, reset)` of the current window.
fn window(resource: Resource) -> (i64, i64) {
    let now = Utc::now().timestamp();
    let w = resource.window_secs();
    let start = now - now.rem_euclid(w);
    (start, start + w)
}

fn bucket_key(state: &AppState, resource: Resource, caller: &str, start: i64) -> String {
    state.redis_key(&format!("ratelimit:{}:{caller}:{start}", resource.name()))
}

/// The caller's quota for `resource`, counting one request when `consume`.
/// `ip` is the client IP ([`auth::client_ip`]), used for anonymous callers.
pub async fn quota(
    state: &AppState,
    limits: &RateLimitSettings,
    resource: Resource,
    ctx: Option<&AuthContext>,
    ip: &str,
    consume: bool,
) -> redis::RedisResult<Quota> {
    let limit = resource.limit(limits, ctx.is_some());
    let (start, reset) = window(resource);
    let key = bucket_key(state, resource, &caller_key(ctx, ip), start);
    let mut redis = state.redis.clone();
    let used: i64 = if consume {
        let (n,): (i64,) = redis::pipe()
            .atomic()
            .incr(&key, 1)
            .expire(&key, resource.window_secs() + 60)
            .ignore()
            .query_async(&mut redis)
            .await?;
        n
    } else {
        redis.get::<_, Option<i64>>(&key).await?.unwrap_or(0)
    };
    Ok(Quota::new(resource, limit, used, reset))
}

/// Give back one request counted by [`quota`] (`consume`) in the window of
/// `q`, e.g. for a `304 Not Modified` (GitHub doesn't count those).
pub async fn refund(
    state: &AppState,
    q: &Quota,
    ctx: Option<&AuthContext>,
    ip: &str,
) -> redis::RedisResult<Quota> {
    let start = q.reset - q.resource.window_secs();
    let key = bucket_key(state, q.resource, &caller_key(ctx, ip), start);
    let mut redis = state.redis.clone();
    let n: i64 = redis.decr(&key, 1).await?;
    Ok(Quota::new(q.resource, q.limit, n.max(0), q.reset))
}

/// Body of `GET /rate_limit` (GitHub's shape: `resources.{core, search,
/// graphql, …}` and the deprecated `rate` = core). Nothing is counted.
pub async fn status(
    state: &AppState,
    ctx: Option<&AuthContext>,
    ip: &str,
) -> Result<Value, ApiError> {
    let s = settings::load(state).await?;
    let mut resources = Map::new();
    for r in Resource::ALL {
        let q = quota(state, &s.rate_limits, r, ctx, ip, false).await?;
        resources.insert(r.name().into(), serde_json::to_value(q)?);
    }
    // `/search/code` shares the search budget here.
    resources.insert("code_search".into(), resources["search"].clone());
    // Resources GitHub reports that have no separate budget here: their
    // budget is never used.
    let reset = Utc::now().timestamp() + 3600;
    for (name, limit) in [
        ("integration_manifest", 5000),
        ("source_import", 100),
        ("code_scanning_upload", 1000),
        ("actions_runner_registration", 10000),
        ("scim", 15000),
        ("dependency_snapshots", 100),
        ("audit_log", 1750),
    ] {
        resources.insert(
            name.into(),
            json!({"limit": limit, "used": 0, "remaining": limit, "reset": reset}),
        );
    }
    let rate = resources["core"].clone();
    Ok(json!({ "resources": resources, "rate": rate }))
}

fn exceeded_response(q: &Quota, ctx: Option<&AuthContext>, ip: &str) -> Response {
    let message = match ctx {
        Some(c) => format!("API rate limit exceeded for user ID {}.", c.user.id),
        None => format!(
            "API rate limit exceeded for {ip}. (But here's the good news: Authenticated \
             requests get a higher rate limit. Check out the documentation for more details.)"
        ),
    };
    let mut resp = ApiError::forbidden(message).into_response();
    q.apply(resp.headers_mut());
    let retry = (q.reset - Utc::now().timestamp()).max(1);
    resp.headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from(retry));
    resp
}

/// Count the request against its resource, enforce the limit when enabled
/// and add the `X-RateLimit-*` headers.
async fn limit(state: AppState, mut req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    // Resolve the caller once (cached for the handler). Bad credentials are
    // counted as anonymous here; the handler reports the 401.
    let ctx = auth::resolve_request(&state, &mut req).await.ok().flatten();
    let ip = auth::client_ip(&state.config, req.headers(), req.extensions());
    let resource = Resource::for_path(&path);
    let not_counted = path.ends_with("/rate_limit");
    let q = match settings::load(&state).await {
        Ok(s) => quota(
            &state,
            &s.rate_limits,
            resource,
            ctx.as_ref(),
            &ip,
            !not_counted,
        )
        .await
        .map(|q| (q, s.rate_limits.enabled))
        .map_err(|e| tracing::warn!(err = ?e, "rate limiter unavailable; allowing request")),
        Err(err) => {
            tracing::warn!(?err, "loading rate limit settings");
            Err(())
        }
    };
    let Ok((q, enforce)) = q else {
        return next.run(req).await;
    };
    if enforce && !not_counted && q.exceeded() {
        return exceeded_response(&q, ctx.as_ref(), &ip);
    }
    let mut resp = next.run(req).await;
    // Conditional requests answered with 304 are free (mount the ETag layer
    // inside this middleware).
    let q = if resp.status() == StatusCode::NOT_MODIFIED && !not_counted {
        refund(&state, &q, ctx.as_ref(), &ip).await.unwrap_or(q)
    } else {
        q
    };
    q.apply(resp.headers_mut());
    resp
}

/// Middleware for the nested REST API router (`/api/v3/...`).
pub async fn middleware(State(state): State<AppState>, req: Request, next: Next) -> Response {
    limit(state, req, next).await
}

/// Root-level middleware for API endpoints mounted outside the nested
/// `/api/v3` router: `/api/graphql` and the API root `/api/v3/`. Other
/// paths pass through untouched.
pub async fn root_middleware(State(state): State<AppState>, req: Request, next: Next) -> Response {
    match req.uri().path() {
        "/api/graphql" | "/api/v3/" => limit(state, req, next).await,
        _ => next.run(req).await,
    }
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
    fn resources_and_headers() {
        assert_eq!(
            Resource::for_path("/api/v3/search/issues"),
            Resource::Search
        );
        assert_eq!(Resource::for_path("/api/v3/user"), Resource::Core);
        assert_eq!(Resource::for_path("/api/graphql"), Resource::Graphql);
        assert_eq!(Resource::for_path("/repos/o/graphql"), Resource::Core);
        let s = RateLimitSettings::default();
        assert_eq!(Resource::Search.limit(&s, false), 10);
        assert_eq!(Resource::Graphql.limit(&s, true), 5000);
        let q = Quota::new(Resource::Core, 60, 61, 0);
        assert!(q.exceeded());
        assert_eq!(q.remaining, 0);
        let mut h = HeaderMap::new();
        q.apply(&mut h);
        assert_eq!(h["x-ratelimit-used"], "60");
        assert_eq!(h["x-ratelimit-resource"], "core");
    }
}
