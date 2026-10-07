//! Observability (P61): logging setup (`BGH_LOG_FORMAT`, optional OTLP
//! traces), the Prometheus recorder, HTTP request metrics and `/metrics`.
//!
//! `/metrics` is never public by default: on the main listener it needs
//! `BGH_METRICS_TOKEN` (bearer), or it is served on its own listener
//! (`BGH_METRICS_LISTEN`). Metric names are documented in
//! `docs/SELF_HOSTING.md` ("Monitoring").

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use axum::Router;
use axum::extract::{MatchedPath, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use bgh_core::error::ApiError;
use bgh_core::observability as obs;
use bgh_core::state::AppState;
use metrics::gauge;
use metrics_exporter_prometheus::{Matcher, PrometheusBuilder, PrometheusHandle};
use tracing_subscriber::Layer;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;

// ---------------------------------------------------------------------------
// Logging
// ---------------------------------------------------------------------------

const DEFAULT_FILTER: &str = "info,sqlx=warn,tower_http=info";

/// Keeps the OTLP exporter alive; dropping it flushes pending spans.
pub struct LogGuard {
    #[cfg(feature = "otlp")]
    provider: Option<opentelemetry_sdk::trace::SdkTracerProvider>,
}

impl Drop for LogGuard {
    fn drop(&mut self) {
        #[cfg(feature = "otlp")]
        if let Some(p) = self.provider.take()
            && let Err(err) = p.shutdown()
        {
            eprintln!("OTLP exporter shutdown: {err}");
        }
    }
}

/// Install the global subscriber: `RUST_LOG` filter, `BGH_LOG_FORMAT`
/// (`pretty`, the default, or `json`: one JSON object per line on stderr,
/// with the request span's fields under `span`), and OTLP trace export
/// when `BGH_OTLP_ENDPOINT` is set (binaries built with `--features otlp`).
pub fn init_logging() -> LogGuard {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| DEFAULT_FILTER.into());
    let format = std::env::var("BGH_LOG_FORMAT").unwrap_or_default();
    let json = match format.trim().to_ascii_lowercase().as_str() {
        "json" => true,
        "" | "pretty" | "text" => false,
        other => {
            eprintln!("BGH_LOG_FORMAT={other:?} is not `json` or `pretty`; using pretty");
            false
        }
    };
    let endpoint = std::env::var("BGH_OTLP_ENDPOINT")
        .ok()
        .filter(|v| !v.trim().is_empty());

    let registry = tracing_subscriber::registry()
        .with(filter)
        .with(json.then(|| json_layer(std::io::stderr)))
        .with((!json).then(|| tracing_subscriber::fmt::layer().with_writer(std::io::stderr)));

    #[cfg(feature = "otlp")]
    {
        let (layer, provider) = match endpoint.as_deref().map(otlp::layer) {
            Some(Ok((layer, provider))) => (Some(layer), Some(provider)),
            Some(Err(err)) => {
                eprintln!("BGH_OTLP_ENDPOINT: {err:#}; traces are not exported");
                (None, None)
            }
            None => (None, None),
        };
        registry.with(layer).init();
        LogGuard { provider }
    }
    #[cfg(not(feature = "otlp"))]
    {
        registry.init();
        if endpoint.is_some() {
            tracing::warn!(
                "BGH_OTLP_ENDPOINT is set but this binary was built without the `otlp` feature"
            );
        }
        LogGuard {}
    }
}

/// The JSON log layer: flattened event fields, the current span's fields
/// (`span.request_id`, `span.user_id`, ...), no span list.
pub fn json_layer<S, W>(writer: W) -> impl Layer<S> + Send + Sync + 'static
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
    W: for<'w> MakeWriter<'w> + Send + Sync + 'static,
{
    tracing_subscriber::fmt::layer()
        .json()
        .flatten_event(true)
        .with_current_span(true)
        .with_span_list(false)
        .with_writer(writer)
}

#[cfg(feature = "otlp")]
mod otlp {
    use opentelemetry::trace::TracerProvider as _;
    use opentelemetry_otlp::{SpanExporter, WithExportConfig};
    use opentelemetry_sdk::Resource;
    use opentelemetry_sdk::trace::SdkTracerProvider;

    /// OTLP/HTTP (protobuf) trace export to `endpoint` (the collector base
    /// URL, e.g. `http://otel-collector:4318`; `/v1/traces` is appended).
    pub fn layer<S>(
        endpoint: &str,
    ) -> anyhow::Result<(
        tracing_opentelemetry::OpenTelemetryLayer<S, opentelemetry_sdk::trace::Tracer>,
        SdkTracerProvider,
    )>
    where
        S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
    {
        let endpoint = endpoint.trim().trim_end_matches('/');
        let url = if endpoint.ends_with("/v1/traces") {
            endpoint.to_string()
        } else {
            format!("{endpoint}/v1/traces")
        };
        let exporter = SpanExporter::builder()
            .with_http()
            .with_endpoint(url)
            .build()?;
        let service =
            std::env::var("OTEL_SERVICE_NAME").unwrap_or_else(|_| "better-github".to_string());
        let provider = SdkTracerProvider::builder()
            .with_batch_exporter(exporter)
            .with_resource(Resource::builder().with_service_name(service).build())
            .build();
        let tracer = provider.tracer("bgh");
        Ok((tracing_opentelemetry::layer().with_tracer(tracer), provider))
    }
}

// ---------------------------------------------------------------------------
// Prometheus recorder
// ---------------------------------------------------------------------------

/// Histogram buckets (seconds) for every `*_seconds` histogram.
const BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0, 300.0,
];

static HANDLE: OnceLock<PrometheusHandle> = OnceLock::new();

/// Install the global Prometheus recorder (idempotent) and return its
/// handle.
pub fn install() -> &'static PrometheusHandle {
    HANDLE.get_or_init(|| {
        let builder = PrometheusBuilder::new()
            .set_buckets_for_metric(Matcher::Suffix("_seconds".into()), BUCKETS)
            .expect("non-empty buckets");
        let recorder = builder.build_recorder();
        let handle = recorder.handle();
        if metrics::set_global_recorder(recorder).is_err() {
            tracing::warn!("a metrics recorder was already installed; /metrics stays empty");
        }
        obs::describe();
        handle
    })
}

// ---------------------------------------------------------------------------
// HTTP metrics
// ---------------------------------------------------------------------------

/// Route template of the handled request, passed outwards in the response
/// extensions by [`route_label`].
#[derive(Clone, Copy)]
struct RouteLabel(&'static str);

/// `route_layer` middleware: tags the response with the matched route
/// template (the innermost router that matched wins).
pub async fn route_label(req: Request, next: Next) -> Response {
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map(|m| m.as_str())
        // The outer router's view of a nested router (unknown API path).
        .filter(|p| !p.contains("__private__"))
        .map(obs::intern_label);
    let mut resp = next.run(req).await;
    if let Some(r) = route
        && resp.extensions().get::<RouteLabel>().is_none()
    {
        resp.extensions_mut().insert(RouteLabel(r));
    }
    resp
}

/// Outermost middleware: request count and latency (to response headers)
/// by method, route template and status.
pub async fn http_metrics(req: Request, next: Next) -> Response {
    let method = obs::method_label(req.method());
    let started = Instant::now();
    let resp = next.run(req).await;
    let route = resp
        .extensions()
        .get::<RouteLabel>()
        .map_or(obs::UNMATCHED_ROUTE, |r| r.0);
    obs::http_request(method, route, resp.status().as_u16(), started.elapsed());
    resp
}

// ---------------------------------------------------------------------------
// /metrics
// ---------------------------------------------------------------------------

/// `GET /metrics` on the main listener: 404 unless `BGH_METRICS_TOKEN` is
/// set, then 401 without the bearer token.
pub async fn metrics_main(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if state.config.metrics_token.is_none() {
        return ApiError::NotFound.into_response();
    }
    serve_metrics(&state, &headers).await
}

/// Router of the dedicated `BGH_METRICS_LISTEN` listener.
pub fn metrics_router(state: AppState) -> Router {
    Router::new()
        .route(
            "/metrics",
            get(
                |State(state): State<AppState>, headers: HeaderMap| async move {
                    serve_metrics(&state, &headers).await
                },
            ),
        )
        .with_state(state)
}

async fn serve_metrics(state: &AppState, headers: &HeaderMap) -> Response {
    if let Some(token) = &state.config.metrics_token
        && !bearer_matches(headers, token)
    {
        let mut resp = ApiError::requires_auth().into_response();
        resp.headers_mut()
            .insert(header::WWW_AUTHENTICATE, "Bearer".parse().expect("static"));
        return resp;
    }
    let handle = install();
    collect_gauges(state).await;
    handle.run_upkeep();
    (
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        handle.render(),
    )
        .into_response()
}

fn bearer_matches(headers: &HeaderMap, token: &str) -> bool {
    let Some(given) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .map(|(_, t)| t.trim())
    else {
        return false;
    };
    // Constant time in the token length.
    given.len() == token.len()
        && given
            .bytes()
            .zip(token.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
}

/// Job kinds and listeners reported before, so a series that disappeared
/// from the query result is reset to 0 instead of going stale.
static SEEN_KINDS: Mutex<Option<HashSet<&'static str>>> = Mutex::new(None);

/// Scrape-time gauges read from the database and the pool.
async fn collect_gauges(state: &AppState) {
    let pool = &state.db;
    let size = pool.size() as f64;
    let idle = pool.num_idle() as f64;
    gauge!(obs::DB_POOL_CONNECTIONS, "state" => "in_use").set((size - idle).max(0.0));
    gauge!(obs::DB_POOL_CONNECTIONS, "state" => "idle").set(idle);
    gauge!(obs::DB_POOL_MAX).set(f64::from(state.config.db_max_connections));

    let jobs: Result<Vec<(String, i64, i64)>, _> = sqlx::query_as(
        "SELECT kind, count(*) FILTER (WHERE failed_at IS NULL),
                count(*) FILTER (WHERE failed_at IS NOT NULL)
           FROM jobs GROUP BY kind",
    )
    .fetch_all(pool)
    .await;
    match jobs {
        Ok(rows) => {
            let mut now = HashSet::new();
            for (kind, queued, failed) in rows {
                let kind = obs::intern_label(&kind);
                now.insert(kind);
                gauge!(obs::JOBS_QUEUED, "kind" => kind).set(queued as f64);
                gauge!(obs::JOBS_FAILED, "kind" => kind).set(failed as f64);
            }
            let mut seen = SEEN_KINDS.lock().unwrap_or_else(|e| e.into_inner());
            let seen = seen.get_or_insert_with(HashSet::new);
            for gone in seen.difference(&now) {
                gauge!(obs::JOBS_QUEUED, "kind" => *gone).set(0.0);
                gauge!(obs::JOBS_FAILED, "kind" => *gone).set(0.0);
            }
            seen.extend(now);
        }
        Err(err) => tracing::warn!(?err, "metrics: job queue depth"),
    }

    match bgh_core::outbox::consumer_lag(pool).await {
        Ok(rows) => {
            for c in rows {
                let listener = obs::intern_label(&c.listener);
                gauge!(obs::EVENT_CONSUMER_LAG, "listener" => listener).set(c.lag as f64);
                gauge!(obs::EVENT_CONSUMER_OLDEST, "listener" => listener)
                    .set(c.oldest_pending_secs);
            }
        }
        Err(err) => tracing::warn!(?err, "metrics: event consumer lag"),
    }

    let actions: Result<(i64, i64), _> = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE status = 'queued'),
                count(*) FILTER (WHERE status = 'in_progress')
           FROM actions_jobs WHERE status IN ('queued', 'in_progress')",
    )
    .fetch_one(pool)
    .await;
    match actions {
        Ok((queued, running)) => {
            gauge!(obs::ACTIONS_QUEUED).set(queued as f64);
            gauge!(obs::ACTIONS_RUNNING).set(running as f64);
        }
        Err(err) => tracing::warn!(?err, "metrics: actions queue"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bearer_token_check() {
        let mut h = HeaderMap::new();
        assert!(!bearer_matches(&h, "s3cret"));
        h.insert(header::AUTHORIZATION, "Bearer s3cret".parse().unwrap());
        assert!(bearer_matches(&h, "s3cret"));
        assert!(!bearer_matches(&h, "s3cre"));
        h.insert(header::AUTHORIZATION, "token s3cret".parse().unwrap());
        assert!(!bearer_matches(&h, "s3cret"));
    }
}
