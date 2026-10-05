//! Serving the built web client (`web/dist`).
//!
//! * `/assets/*`: hashed build output, served with
//!   `Cache-Control: public, max-age=31536000, immutable`; missing → 404.
//! * other existing files (favicon, service worker, ...): `no-cache`.
//! * any other GET path (and `/`, `/index.html`): the app shell
//!   `index.html` for client-side routing, with the viewer's boot data
//!   (docs/SYNC_PROTOCOL.md §9) injected at `<!--BGH_BOOT-->`
//!   (`Cache-Control: no-cache, private`).
//!
//! Precompressed `.br` / `.gz` siblings are served when the client accepts
//! them. Non-GET requests and API-looking paths get a JSON 404.
//!
//! With the `embed-web` cargo feature the files compiled into the binary
//! are served (same rules, see [`crate::embedded`]) unless `BGH_WEB_DIR`
//! points at a directory containing an `index.html`.

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::Request;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use bgh_core::error::ApiError;
use bgh_core::state::AppState;
use tower::ServiceExt;
use tower_http::services::{ServeDir, ServeFile};

use crate::embedded::EmbeddedFiles;

pub(crate) const IMMUTABLE: &str = "public, max-age=31536000, immutable";
pub(crate) const NO_CACHE: &str = "no-cache";
/// Replaced with the boot `<script>` in the app shell.
pub const BOOT_PLACEHOLDER: &str = "<!--BGH_BOOT-->";

#[derive(Clone)]
pub struct WebFiles {
    dir: Arc<PathBuf>,
    embedded: Option<EmbeddedFiles>,
}

impl WebFiles {
    /// Serve from `dir`, or from the embedded client (feature `embed-web`)
    /// when `dir` has no `index.html`.
    pub fn new(dir: PathBuf) -> Self {
        #[cfg(feature = "embed-web")]
        if !dir.join("index.html").is_file() {
            let files = crate::embedded::bundled();
            if files.has_index() {
                tracing::debug!("serving the embedded web client");
                return Self::embedded(files);
            }
        }
        Self {
            dir: Arc::new(dir),
            embedded: None,
        }
    }

    /// Serve the given in-memory files.
    pub fn embedded(files: EmbeddedFiles) -> Self {
        Self {
            dir: Arc::new(PathBuf::new()),
            embedded: Some(files),
        }
    }

    /// Whether a built web client (`index.html`) is available.
    pub fn has_shell(&self) -> bool {
        match &self.embedded {
            Some(files) => files.contains("index.html"),
            None => self.dir.join("index.html").is_file(),
        }
    }

    /// Whether `path` should get the app shell (no file of that name).
    fn is_shell(&self, path: &str) -> bool {
        if path.starts_with("/assets/") {
            return false;
        }
        let rel = path.trim_start_matches('/');
        if rel.is_empty() || rel == "index.html" {
            return true;
        }
        if rel.split('/').any(|seg| seg == ".." || seg.is_empty()) {
            return true;
        }
        match &self.embedded {
            Some(files) => !files.contains(rel),
            None => !self.dir.join(rel).is_file(),
        }
    }

    /// `index.html` with boot data injected.
    async fn shell(&self, state: &AppState, headers: &HeaderMap, head: bool) -> Option<Response> {
        let html = match &self.embedded {
            Some(files) => files.read("index.html")?,
            None => tokio::fs::read(self.dir.join("index.html")).await.ok()?,
        };
        let html = String::from_utf8_lossy(&html);
        let boot = bgh_accounts::boot::boot_json(state, headers).await;
        let html = html.replacen(BOOT_PLACEHOLDER, &bgh_accounts::boot::boot_script(&boot), 1);
        let mut resp = if head {
            StatusCode::OK.into_response()
        } else {
            Html(html).into_response()
        };
        let h = resp.headers_mut();
        h.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/html; charset=utf-8"),
        );
        h.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-cache, private"),
        );
        h.insert(header::VARY, HeaderValue::from_static("Cookie"));
        Some(resp)
    }

    pub async fn serve(&self, state: &AppState, req: Request) -> Response {
        let path = req.uri().path().to_string();
        if !matches!(*req.method(), Method::GET | Method::HEAD)
            || path.starts_with("/api/")
            || path.starts_with("/_bgh/")
        {
            return ApiError::NotFound.into_response();
        }
        let head = req.method() == Method::HEAD;
        if self.is_shell(&path)
            && let Some(resp) = self.shell(state, &req.headers().clone(), head).await
        {
            return resp;
        }
        if let Some(files) = &self.embedded {
            return files.serve(&req);
        }
        let index = self.dir.join("index.html");
        if !index.is_file() {
            return (
                StatusCode::NOT_FOUND,
                "web client not built (run `npm run build` in web/ or set BGH_WEB_DIR)\n",
            )
                .into_response();
        }

        let is_asset = path.starts_with("/assets/");
        let result = if is_asset {
            ServeDir::new(self.dir.as_ref())
                .precompressed_br()
                .precompressed_gzip()
                .oneshot(req)
                .await
        } else {
            ServeDir::new(self.dir.as_ref())
                .precompressed_br()
                .precompressed_gzip()
                .append_index_html_on_directories(false)
                .fallback(
                    ServeFile::new(&index)
                        .precompressed_br()
                        .precompressed_gzip(),
                )
                .oneshot(req)
                .await
        };
        let mut resp = match result {
            Ok(r) => r.map(axum::body::Body::new),
            Err(err) => {
                tracing::error!(?err, "serving static file");
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
        };
        let cache = if is_asset && resp.status().is_success() {
            IMMUTABLE
        } else {
            NO_CACHE
        };
        resp.headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
        resp
    }
}

/// Pages that have a server-rendered fallback in a domain crate but are
/// owned by the web client when it is built: browsers (`Accept: text/html`)
/// get the app shell, which talks to the matching `/_bgh` JSON endpoints.
/// Other clients (and form POSTs) still reach the crate's handler.
const SPA_PAGES: &[&str] = &["/login/device", "/login/oauth/authorize"];

pub(crate) async fn spa_pages(
    axum::extract::State((web, state)): axum::extract::State<(WebFiles, AppState)>,
    req: Request,
    next: axum::middleware::Next,
) -> Response {
    let wants_html = req
        .headers()
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|a| a.contains("text/html"));
    if matches!(*req.method(), Method::GET | Method::HEAD)
        && wants_html
        && SPA_PAGES.contains(&req.uri().path().trim_end_matches('/'))
        && web.has_shell()
    {
        let head = req.method() == Method::HEAD;
        if let Some(resp) = web.shell(&state, req.headers(), head).await {
            return resp;
        }
    }
    next.run(req).await
}
