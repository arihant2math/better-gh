//! Application assembly: composes every domain crate's routers, middleware
//! and background registrations. The `bgh` binary (main.rs) and the test
//! harness both build the app through [`app`] / [`factory`].
//!
//! Routing layout:
//! * `/healthz`
//! * `/api/v3/...`: each crate's `router()` nested here (JSON 404 fallback
//!   for unknown paths and wrong methods, ETag/Last-Modified/304 inside the
//!   rate limiter, `X-GitHub-Media-Type`, CORS)
//! * every `/api/...` path: `X-GitHub-Enterprise-Version`,
//!   `X-GitHub-Request-Id`, `X-GitHub-Api-Version` validation ([`api_compat`])
//! * each crate's `web_router()` merged at the root (`/_bgh/...`, git HTTP)
//! * `/{o}/{r}/pull/{n}.diff|.patch`, `/{o}/{r}/commit/{sha}.diff|.patch`,
//!   `/{o}/{r}/compare/{a}...{b}.diff|.patch`: the matching API handler
//!   with the diff/patch media type (read access checked there)
//! * everything else: the web client from `BGH_WEB_DIR` with SPA fallback

pub mod embedded;
mod serve;
mod web;

use std::time::Duration;

use axum::Router;
use axum::body::{Body, HttpBody};
use axum::extract::{Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use bgh_core::error::ApiError;
use bgh_core::registry::{AppFactory, Registry};
use bgh_core::state::AppState;
use chrono::{DateTime, Utc};
use http_body_util::BodyExt;
use sha2::{Digest, Sha256};
use tower_http::compression::predicate::{NotForContentType, Predicate, SizeAbove};
use tower_http::compression::{CompressionLayer, DefaultPredicate};
use tower_http::cors::{Any, CorsLayer};
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::trace::{DefaultOnResponse, TraceLayer};

pub use serve::serve;
pub use web::WebFiles;

/// Register job handlers and event listeners of every domain crate.
pub fn register(reg: &mut Registry) {
    // Shared infrastructure jobs (bgh-core).
    reg.job(bgh_core::mail::send_job);
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
    bgh_projects::register(reg);
    bgh_wiki::register(reg);
    bgh_import::register(reg);
    bgh_packages::register(reg);
    bgh_uploads::register(reg);
    bgh_security::register(reg);
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
        .merge(bgh_projects::router())
        .merge(bgh_wiki::router())
        .merge(bgh_import::router())
        .merge(bgh_packages::router())
        .merge(bgh_uploads::router())
        .merge(bgh_security::router())
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
        .merge(bgh_projects::web_router())
        .merge(bgh_wiki::web_router())
        .merge(bgh_import::web_router())
        .merge(bgh_packages::web_router())
        .merge(bgh_uploads::web_router())
        .merge(bgh_security::web_router())
}

/// Build the complete application router.
pub fn app(state: AppState) -> Router {
    let api = api_routes()
        .fallback(api_not_found)
        // A known path with the wrong method is a JSON 404, like GitHub
        // (not axum's empty 405).
        .method_not_allowed_fallback(api_not_found)
        // ETag/304 inside the rate limiter, so 304s are not counted.
        .layer(middleware::from_fn(etag))
        .layer(middleware::from_fn(api_headers))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            bgh_core::ratelimit::middleware,
        ))
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any)
                .expose_headers(Any)
                .max_age(Duration::from_secs(86_400)),
        );

    // `/{o}/{r}/pull/{n}.diff` & co. are answered by the API handlers.
    let diff_api = api.clone().with_state(state.clone());
    let web_files = WebFiles::new(state.config.web_dir.clone());
    let shell_state = state.clone();
    let spa_files = web_files.clone();

    // Don't spend CPU compressing git packs (already compressed) or tiny bodies.
    let compress_when = DefaultPredicate::new()
        .and(SizeAbove::new(256))
        .and(NotForContentType::const_new("application/x-git"))
        // Already compressed or binary downloads (archives, LFS objects).
        .and(NotForContentType::const_new("application/zip"))
        .and(NotForContentType::const_new("application/x-gzip"))
        .and(NotForContentType::const_new("application/octet-stream"))
        // Registry manifests: clients verify the digest of the exact bytes.
        .and(NotForContentType::const_new("application/vnd.oci."))
        .and(NotForContentType::const_new("application/vnd.docker."));

    Router::new()
        .route("/healthz", get(healthz))
        .nest("/api/v3", api)
        .merge(web_routes())
        .route("/_bgh/{*rest}", any(api_not_found))
        .fallback(move |mut req: Request| {
            let web_files = web_files.clone();
            let state = shell_state.clone();
            let diff_api = diff_api.clone();
            async move {
                if web::rewrite_diff_request(&mut req) {
                    web::serve_diff(diff_api, req).await
                } else {
                    web_files.serve(&state, req).await
                }
            }
        })
        // API endpoints outside the nested `/api/v3` router (`/api/graphql`,
        // `/api/v3/`) get the same rate limiting.
        .layer(middleware::from_fn_with_state(
            state.clone(),
            bgh_core::ratelimit::root_middleware,
        ))
        // GHES headers and `X-GitHub-Api-Version` checks on every API path.
        .layer(middleware::from_fn(api_compat))
        // GitHub App JWTs may only call the app endpoints.
        .layer(middleware::from_fn_with_state(
            state.clone(),
            bgh_core::apps::middleware,
        ))
        .layer(middleware::from_fn_with_state(
            (spa_files, state.clone()),
            web::spa_pages,
        ))
        // Actions job tokens are limited to their `permissions:` (inside the
        // sync scope so audit entries see the triggering actor).
        .layer(middleware::from_fn_with_state(
            state.clone(),
            bgh_core::token_permissions::middleware,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            bgh_sync::http_middleware,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            bgh_core::privacy::private_mode_middleware,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            bgh_core::settings::maintenance_middleware,
        ))
        .layer(middleware::from_fn(bgh_core::auth::csrf_middleware))
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
    h.entry("x-github-media-type")
        .or_insert(HeaderValue::from_static("github.v3; format=json"));
    h.entry("x-github-api-version-selected")
        .or_insert(HeaderValue::from_static("2022-11-28"));
    resp
}

/// Largest response body that gets an ETag (and `Last-Modified`) without
/// a conditional request header; bigger or streamed bodies aren't buffered
/// just to hash them.
const ETAG_MAX_BYTES: u64 = 1 << 20;

/// Validators for successful GET JSON responses: a weak `ETag`, plus
/// `Last-Modified` from a top-level `updated_at`. `If-None-Match` (or,
/// without it, `If-Modified-Since`) → 304.
async fn etag(req: Request, next: Next) -> Response {
    let cacheable_method = matches!(*req.method(), Method::GET | Method::HEAD);
    let if_none_match = req.headers().get(header::IF_NONE_MATCH).cloned();
    let if_modified_since = req
        .headers()
        .get(header::IF_MODIFIED_SINCE)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_http_date);
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
    let conditional = if_none_match.is_some() || if_modified_since.is_some();
    let small = resp
        .body()
        .size_hint()
        .exact()
        .is_some_and(|n| n <= ETAG_MAX_BYTES);
    if !conditional && !small {
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
    if let Ok(v) = HeaderValue::from_str(&tag) {
        parts.headers.insert(header::ETAG, v);
    }
    let last_modified = match parts.headers.get(header::LAST_MODIFIED) {
        Some(v) => v.to_str().ok().and_then(parse_http_date),
        None => {
            let t = updated_at(&bytes);
            if let Some(t) = t
                && let Ok(v) = HeaderValue::from_str(&http_date(t))
            {
                parts.headers.insert(header::LAST_MODIFIED, v);
            }
            t
        }
    };
    // `If-None-Match` takes precedence over `If-Modified-Since` (RFC 9110).
    let matches = match (&if_none_match, if_modified_since, last_modified) {
        (Some(inm), _, _) => inm
            .to_str()
            .ok()
            .is_some_and(|v| v.split(',').any(|t| t.trim() == tag || t.trim() == "*")),
        (None, Some(since), Some(modified)) => modified.timestamp() <= since.timestamp(),
        _ => false,
    };
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

/// The top-level `updated_at` of a JSON object body.
fn updated_at(body: &[u8]) -> Option<DateTime<Utc>> {
    #[derive(serde::Deserialize)]
    struct UpdatedAt {
        updated_at: Option<serde_json::Value>,
    }
    if body.first() != Some(&b'{') {
        return None;
    }
    let v: UpdatedAt = serde_json::from_slice(body).ok()?;
    let t = v.updated_at?;
    DateTime::parse_from_rfc3339(t.as_str()?)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// IMF-fixdate (`Tue, 15 Nov 1994 08:12:31 GMT`).
fn http_date(t: DateTime<Utc>) -> String {
    t.format("%a, %d %b %Y %H:%M:%S GMT").to_string()
}

fn parse_http_date(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc2822(s.trim())
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// GitHub Enterprise compatibility on every API path (`/api/v3...`,
/// `/api/graphql`, `/api/uploads...`):
/// * `X-GitHub-Enterprise-Version` (Renovate's `HEAD /api/v3/` reads it)
///   and `X-GitHub-Request-Id` (the `x-request-id`);
/// * REST: an unsupported `X-GitHub-Api-Version` → 400; a supported one is
///   echoed in `X-GitHub-Api-Version-Selected`;
/// * a known path with the wrong method → JSON 404 (routes mounted outside
///   the nested API router, e.g. `/api/v3/`).
async fn api_compat(req: Request, next: Next) -> Response {
    let path = req.uri().path();
    if !path.starts_with("/api/") && path != "/api" {
        return next.run(req).await;
    }
    let rest = path == "/api/v3" || path.starts_with("/api/v3/");
    let request_id = req.headers().get("x-request-id").cloned();
    let mut selected = None;
    if rest && let Some(v) = req.headers().get("x-github-api-version") {
        let v = v.to_str().unwrap_or("").trim();
        if bgh_core::API_VERSIONS.contains(&v) {
            selected = HeaderValue::from_str(v).ok();
        } else {
            let mut resp =
                ApiError::bad_request(format!("API version {v} is not supported.")).into_response();
            api_compat_headers(&mut resp, request_id);
            return resp;
        }
    }
    let mut resp = next.run(req).await;
    // Only axum's empty routing 405 (handlers send GitHub-style JSON 405s,
    // e.g. "Pull Request is not mergeable").
    if resp.status() == StatusCode::METHOD_NOT_ALLOWED
        && !resp.headers().contains_key(header::CONTENT_TYPE)
    {
        resp = ApiError::NotFound.into_response();
    }
    if let Some(v) = selected {
        resp.headers_mut()
            .insert("x-github-api-version-selected", v);
    }
    api_compat_headers(&mut resp, request_id);
    resp
}

fn api_compat_headers(resp: &mut Response, request_id: Option<HeaderValue>) {
    let h = resp.headers_mut();
    h.insert(
        "x-github-enterprise-version",
        HeaderValue::from_static(bgh_graphql::COMPAT_GHES_VERSION),
    );
    if let Some(id) = request_id {
        h.insert("x-github-request-id", id);
    }
}

fn hex_prefix(bytes: &[u8]) -> String {
    bytes.iter().take(16).map(|b| format!("{b:02x}")).collect()
}
