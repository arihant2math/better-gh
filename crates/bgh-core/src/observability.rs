//! Metrics instrumentation (P61) on the [`metrics`] facade.
//!
//! Domain crates call the helpers here; `bgh-server` installs the
//! Prometheus recorder and serves `/metrics` (without a recorder every call
//! is a cheap no-op). Metric names are listed in `docs/SELF_HOSTING.md`
//! ("Monitoring"); keep that table in sync.
//!
//! Label values are always bounded: `&'static str` enums, route templates
//! (not raw paths), registered job kinds. Never label with ids, logins or
//! raw URLs.

use std::sync::{OnceLock, RwLock};
use std::time::{Duration, Instant};

use metrics::{counter, gauge, histogram};

use crate::auth::{AuthContext, AuthMethod};

pub const HTTP_REQUESTS: &str = "bgh_http_requests_total";
pub const HTTP_DURATION: &str = "bgh_http_request_duration_seconds";
pub const RATELIMIT_REJECTED: &str = "bgh_ratelimit_rejected_total";
pub const JOBS_RUN: &str = "bgh_jobs_total";
pub const JOB_DURATION: &str = "bgh_job_duration_seconds";
pub const JOBS_QUEUED: &str = "bgh_jobs_queued";
pub const JOBS_FAILED: &str = "bgh_jobs_failed";
pub const EVENT_CONSUMER_LAG: &str = "bgh_event_consumer_lag";
pub const EVENT_CONSUMER_OLDEST: &str = "bgh_event_consumer_oldest_pending_seconds";
pub const DB_POOL_CONNECTIONS: &str = "bgh_db_pool_connections";
pub const DB_POOL_MAX: &str = "bgh_db_pool_max_connections";
pub const REDIS_ERRORS: &str = "bgh_redis_errors_total";
pub const GIT_OPS: &str = "bgh_git_operations_total";
pub const GIT_DURATION: &str = "bgh_git_operation_duration_seconds";
pub const SYNC_CONNECTIONS: &str = "bgh_sync_websocket_connections";
pub const WEBHOOK_DELIVERIES: &str = "bgh_webhook_deliveries_total";
pub const ACTIONS_QUEUED: &str = "bgh_actions_jobs_queued";
pub const ACTIONS_RUNNING: &str = "bgh_actions_jobs_in_progress";

/// Register help texts with the installed recorder (call once after
/// installing it).
pub fn describe() {
    use metrics::{Unit, describe_counter, describe_gauge, describe_histogram};
    describe_counter!(
        HTTP_REQUESTS,
        "HTTP requests by method, route template and status"
    );
    describe_histogram!(
        HTTP_DURATION,
        Unit::Seconds,
        "Time to response headers by method, route template and status"
    );
    describe_counter!(
        RATELIMIT_REJECTED,
        "Requests rejected by the API rate limiter, by resource"
    );
    describe_counter!(
        JOBS_RUN,
        "Background jobs run, by kind and outcome (ok, retry, failed)"
    );
    describe_histogram!(
        JOB_DURATION,
        Unit::Seconds,
        "Background job run time by kind"
    );
    describe_gauge!(JOBS_QUEUED, "Jobs waiting or running, by kind");
    describe_gauge!(
        JOBS_FAILED,
        "Permanently failed jobs kept in the queue, by kind"
    );
    describe_gauge!(
        EVENT_CONSUMER_LAG,
        "Committed events not yet processed, by listener"
    );
    describe_gauge!(
        EVENT_CONSUMER_OLDEST,
        Unit::Seconds,
        "Age of the oldest unprocessed event, by listener"
    );
    describe_gauge!(
        DB_POOL_CONNECTIONS,
        "Database pool connections by state (in_use, idle)"
    );
    describe_gauge!(DB_POOL_MAX, "Configured database pool size");
    describe_counter!(REDIS_ERRORS, "Failed Redis commands by operation");
    describe_counter!(GIT_OPS, "Git operations by kind and outcome");
    describe_histogram!(
        GIT_DURATION,
        Unit::Seconds,
        "Git operation duration by kind"
    );
    describe_gauge!(SYNC_CONNECTIONS, "Open sync WebSocket connections");
    describe_counter!(
        WEBHOOK_DELIVERIES,
        "Webhook delivery attempts by outcome (success, failure, error)"
    );
    describe_gauge!(ACTIONS_QUEUED, "Actions jobs waiting for a runner");
    describe_gauge!(ACTIONS_RUNNING, "Actions jobs in progress");
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

/// Most distinct route templates kept as labels; later ones are reported as
/// `other` (route templates are a fixed set, this is only a safety net).
const MAX_ROUTES: usize = 4096;

/// Label for requests no route matched (SPA pages, unknown paths).
pub const UNMATCHED_ROUTE: &str = "unmatched";

static ROUTES: OnceLock<RwLock<std::collections::HashSet<&'static str>>> = OnceLock::new();

/// Intern a route template as a `'static` label (one leaked string per
/// distinct template, bounded by [`MAX_ROUTES`]), so recording a request
/// does not allocate.
pub fn intern_label(template: &str) -> &'static str {
    let set = ROUTES.get_or_init(Default::default);
    if let Ok(r) = set.read()
        && let Some(s) = r.get(template)
    {
        return s;
    }
    let Ok(mut w) = set.write() else {
        return "other";
    };
    if let Some(s) = w.get(template) {
        return s;
    }
    if w.len() >= MAX_ROUTES {
        return "other";
    }
    let s: &'static str = Box::leak(template.to_owned().into_boxed_str());
    w.insert(s);
    s
}

/// `'static` label of an HTTP method (`OTHER` for extension methods).
pub fn method_label(m: &http::Method) -> &'static str {
    match *m {
        http::Method::GET => "GET",
        http::Method::POST => "POST",
        http::Method::PUT => "PUT",
        http::Method::PATCH => "PATCH",
        http::Method::DELETE => "DELETE",
        http::Method::HEAD => "HEAD",
        http::Method::OPTIONS => "OPTIONS",
        _ => "OTHER",
    }
}

/// `'static` label of a status code (`"200"`, …).
pub fn status_label(code: u16) -> &'static str {
    static CODES: OnceLock<Vec<&'static str>> = OnceLock::new();
    let codes = CODES.get_or_init(|| {
        (100..600u16)
            .map(|c| &*Box::leak(c.to_string().into_boxed_str()))
            .collect()
    });
    code.checked_sub(100)
        .and_then(|i| codes.get(usize::from(i)))
        .copied()
        .unwrap_or("other")
}

/// Record one finished HTTP request.
pub fn http_request(method: &'static str, route: &'static str, status: u16, elapsed: Duration) {
    let status = status_label(status);
    let labels = [("method", method), ("route", route), ("status", status)];
    counter!(HTTP_REQUESTS, &labels).increment(1);
    histogram!(HTTP_DURATION, &labels).record(elapsed.as_secs_f64());
}

/// A request rejected by the rate limiter.
pub fn ratelimit_rejected(resource: &'static str) {
    counter!(RATELIMIT_REJECTED, "resource" => resource).increment(1);
}

// ---------------------------------------------------------------------------
// Jobs, Redis, webhooks
// ---------------------------------------------------------------------------

/// A background job finished: `outcome` is `ok`, `retry` or `failed`.
pub fn job_finished(kind: &str, outcome: &'static str, elapsed: Duration) {
    let kind = intern_label(kind);
    counter!(JOBS_RUN, "kind" => kind, "outcome" => outcome).increment(1);
    histogram!(JOB_DURATION, "kind" => kind).record(elapsed.as_secs_f64());
}

/// A Redis command failed (`op` names the caller, e.g. `ratelimit`).
pub fn redis_error(op: &'static str) {
    counter!(REDIS_ERRORS, "op" => op).increment(1);
}

/// Count a failed Redis result and pass it through.
pub fn redis_result<T>(op: &'static str, r: redis::RedisResult<T>) -> redis::RedisResult<T> {
    if r.is_err() {
        redis_error(op);
    }
    r
}

/// A webhook delivery attempt: `success` (2xx), `failure` (other status)
/// or `error` (no HTTP response: DNS, TLS, timeout, blocked target).
pub fn webhook_delivery(outcome: &'static str) {
    counter!(WEBHOOK_DELIVERIES, "outcome" => outcome).increment(1);
}

// ---------------------------------------------------------------------------
// Git
// ---------------------------------------------------------------------------

/// Times one git operation; record it with [`GitTimer::finish`]. Dropped
/// unfinished, it records outcome `error`.
#[must_use]
pub struct GitTimer {
    kind: &'static str,
    started: Instant,
    done: bool,
}

/// Start timing a git operation (`upload-pack`, `receive-pack`, `archive`,
/// `merge-tree`, ...).
pub fn git_op(kind: &'static str) -> GitTimer {
    GitTimer {
        kind,
        started: Instant::now(),
        done: false,
    }
}

impl GitTimer {
    pub fn finish(mut self, ok: bool) {
        self.done = true;
        self.record(if ok { "ok" } else { "error" });
    }

    fn record(&self, outcome: &'static str) {
        counter!(GIT_OPS, "kind" => self.kind, "outcome" => outcome).increment(1);
        histogram!(GIT_DURATION, "kind" => self.kind).record(self.started.elapsed().as_secs_f64());
    }
}

impl Drop for GitTimer {
    fn drop(&mut self) {
        if !self.done {
            self.record("error");
        }
    }
}

// ---------------------------------------------------------------------------
// Sync WebSockets
// ---------------------------------------------------------------------------

/// Counts one open sync WebSocket while alive.
#[must_use]
pub struct WsConnection(());

pub fn ws_connected() -> WsConnection {
    gauge!(SYNC_CONNECTIONS).increment(1.0);
    WsConnection(())
}

impl Drop for WsConnection {
    fn drop(&mut self) {
        gauge!(SYNC_CONNECTIONS).decrement(1.0);
    }
}

// ---------------------------------------------------------------------------
// Request context for logs
// ---------------------------------------------------------------------------

/// `auth_method` value of the request span.
pub fn auth_method_label(m: &AuthMethod) -> &'static str {
    match m {
        AuthMethod::Session { .. } => "session",
        AuthMethod::Token { .. } => "token",
        AuthMethod::Password => "password",
        AuthMethod::App { .. } => "app",
    }
}

/// Record the caller on the current span (the `http` request span declares
/// `user_id`, `token_id` and `auth_method`; other spans ignore them).
pub fn record_caller(ctx: &AuthContext) {
    let span = tracing::Span::current();
    span.record("user_id", ctx.user.id);
    span.record("auth_method", auth_method_label(&ctx.method));
    if let AuthMethod::Token { token_id } = ctx.method {
        span.record("token_id", token_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_interned_and_bounded() {
        let a = intern_label("/api/v3/repos/{owner}/{repo}");
        let b = intern_label(&String::from("/api/v3/repos/{owner}/{repo}"));
        assert!(std::ptr::eq(a, b));
        assert_eq!(status_label(200), "200");
        assert_eq!(status_label(599), "599");
        assert_eq!(status_label(42), "other");
        assert_eq!(status_label(600), "other");
        assert_eq!(method_label(&http::Method::PATCH), "PATCH");
    }
}
