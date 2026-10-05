//! Application assembly: composes every domain crate's routers, middleware
//! and background registrations. The `bgh` binary (main.rs) and the test
//! harness both build the app through [`app`] / [`factory`].
//!
//! Routing layout:
//! * `/healthz`
//! * `/api/v3/...`: each crate's `router()` nested here (JSON 404 fallback,
//!   ETag/304, `X-GitHub-Media-Type`, CORS)
//! * each crate's `web_router()` merged at the root (`/_bgh/...`, git HTTP)
//! * everything else: the web client from `BGH_WEB_DIR` with SPA fallback

mod web;

use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use bgh_core::error::ApiError;
use bgh_core::registry::{AppFactory, Registry};
use bgh_core::state::AppState;
use http_body_util::BodyExt;
use sha2::{Digest, Sha256};
use tower_http::compression::predicate::{NotForContentType, Predicate, SizeAbove};
use tower_http::compression::{CompressionLayer, DefaultPredicate};
use tower_http::cors::{Any, CorsLayer};
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::trace::{DefaultOnResponse, TraceLayer};

pub use web::WebFiles;

/// Register job handlers and event listeners of every domain crate.
pub fn register(reg: &mut Registry) {
    bgh_accounts::register(reg);
    bgh_repos::register(reg);
    bgh_issues::register(reg);
    bgh_pulls::register(reg);
    bgh_notify::register(reg);
    bgh_releases::register(reg);
    bgh_search::register(reg);
    bgh_admin::register(reg);
    bgh_sync::register(reg);
    bgh_graphql::register(reg);
    bgh_actions::register(reg);
}

/// REST API routes of every crate (relative to `/api/v3`).
fn api_routes() -> Router<AppState> {
    Router::new()
        .merge(bgh_accounts::router())
        .merge(bgh_repos::router())
        .merge(bgh_issues::router())
        .merge(bgh_pulls::router())
        .merge(bgh_notify::router())
        .merge(bgh_releases::router())
        .merge(bgh_search::router())
        .merge(bgh_admin::router())
        .merge(bgh_sync::router())
        .merge(bgh_graphql::router())
        .merge(bgh_actions::router())
}

/// Non-API routes of every crate (absolute paths).
fn web_routes() -> Router<AppState> {
    Router::new()
        .merge(bgh_accounts::web_router())
        .merge(bgh_repos::web_router())
        .merge(bgh_issues::web_router())
        .merge(bgh_pulls::web_router())
        .merge(bgh_notify::web_router())
        .merge(bgh_releases::web_router())
        .merge(bgh_search::web_router())
        .merge(bgh_admin::web_router())
        .merge(bgh_sync::web_router())
        .merge(bgh_graphql::web_router())
        .merge(bgh_actions::web_router())
}

/// Build the complete application router.
pub fn app(state: AppState) -> Router {
    let api = api_routes()
        .fallback(api_not_found)
        .layer(middleware::from_fn(api_headers))
        .layer(middleware::from_fn(etag))
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any)
                .expose_headers(Any)
                .max_age(Duration::from_secs(86_400)),
        );

    let web_files = WebFiles::new(state.config.web_dir.clone());

    // Don't spend CPU compressing git packs (already compressed) or tiny bodies.
    let compress_when = DefaultPredicate::new()
        .and(SizeAbove::new(256))
        .and(NotForContentType::const_new("application/x-git"));

    Router::new()
        .route("/healthz", get(healthz))
        .nest("/api/v3", api)
        .merge(web_routes())
        .route("/_bgh/{*rest}", any(api_not_found))
        .fallback(move |req: Request| {
            let web_files = web_files.clone();
            async move { web_files.serve(req).await }
        })
        .layer(middleware::from_fn(bgh_core::auth::auth_headers_middleware))
        .layer(CompressionLayer::new().compress_when(compress_when))
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(
            TraceLayer::new_for_http()
                .on_response(DefaultOnResponse::new().level(tracing::Level::INFO))
                .make_span_with(request_span),
        )
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
        .with_state(state)
}

/// The real application, for [`bgh_core::testing::TestApp::spawn_with`].
pub fn factory() -> AppFactory {
    AppFactory {
        router: app,
        register,
    }
}

/// Spawn the full application against a fresh test database.
/// Available with the `testing` feature (enable it in dev-dependencies).
#[cfg(feature = "testing")]
pub async fn test_app() -> bgh_core::testing::TestApp {
    bgh_core::testing::TestApp::spawn_with(factory()).await
}

/// Tracing span per request, tagged with the `x-request-id`.
fn request_span(req: &Request<Body>) -> tracing::Span {
    let request_id = req
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("-");
    tracing::info_span!(
        "http",
        method = %req.method(),
        path = %req.uri().path(),
        request_id
    )
}

async fn api_not_found() -> ApiError {
    ApiError::NotFound
}

async fn healthz(State(state): State<AppState>) -> Response {
    let db_ok = sqlx::query("SELECT 1").execute(&state.db).await.is_ok();
    let mut redis = state.redis.clone();
    let redis_ok = redis::cmd("PING")
        .query_async::<String>(&mut redis)
        .await
        .is_ok();
    let status = if db_ok && redis_ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (
        status,
        axum::Json(serde_json::json!({
            "status": if status == StatusCode::OK { "ok" } else { "degraded" },
            "database": db_ok,
            "redis": redis_ok,
        })),
    )
        .into_response()
}

/// GitHub API response headers.
async fn api_headers(req: Request, next: Next) -> Response {
    let mut resp = next.run(req).await;
    let h = resp.headers_mut();
    h.insert(
        "x-github-media-type",
        HeaderValue::from_static("github.v3; format=json"),
    );
    h.entry("x-github-api-version-selected")
        .or_insert(HeaderValue::from_static("2022-11-28"));
    resp
}

/// Weak ETags for successful GET JSON responses; `If-None-Match` → 304.
async fn etag(req: Request, next: Next) -> Response {
    let cacheable_method = matches!(*req.method(), Method::GET | Method::HEAD);
    let if_none_match = req.headers().get(header::IF_NONE_MATCH).cloned();
    let resp = next.run(req).await;
    let is_json = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.starts_with("application/json"));
    if !cacheable_method
        || resp.status() != StatusCode::OK
        || !is_json
        || resp.headers().contains_key(header::ETAG)
    {
        return resp;
    }
    let (mut parts, body) = resp.into_parts();
    let bytes = match body.collect().await {
        Ok(b) => b.to_bytes(),
        Err(err) => {
            tracing::error!(?err, "buffering response for etag");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    let digest = Sha256::digest(&bytes);
    let tag = format!("W/\"{}\"", hex_prefix(&digest));
    let matches = if_none_match
        .as_ref()
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|t| t.trim() == tag || t.trim() == "*"));
    if let Ok(v) = HeaderValue::from_str(&tag) {
        parts.headers.insert(header::ETAG, v);
    }
    parts
        .headers
        .entry(header::CACHE_CONTROL)
        .or_insert(HeaderValue::from_static("private, max-age=60, s-maxage=60"));
    parts.headers.insert(
        header::VARY,
        HeaderValue::from_static("Accept, Authorization, Cookie"),
    );
    if matches {
        parts.status = StatusCode::NOT_MODIFIED;
        parts.headers.remove(header::CONTENT_LENGTH);
        return Response::from_parts(parts, Body::empty());
    }
    Response::from_parts(parts, Body::from(bytes))
}

fn hex_prefix(bytes: &[u8]) -> String {
    bytes.iter().take(16).map(|b| format!("{b:02x}")).collect()
}
