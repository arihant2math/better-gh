//! Serving the built web client (`web/dist`).
//!
//! * `/assets/*`: hashed build output, served with
//!   `Cache-Control: public, max-age=31536000, immutable`; missing → 404.
//! * other existing files (favicon, service worker, ...): `no-cache`.
//! * any other GET path: `index.html` (`no-cache`) for client-side routing.
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
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bgh_core::error::ApiError;
use tower::ServiceExt;
use tower_http::services::{ServeDir, ServeFile};

use crate::embedded::EmbeddedFiles;

pub(crate) const IMMUTABLE: &str = "public, max-age=31536000, immutable";
pub(crate) const NO_CACHE: &str = "no-cache";

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

    pub async fn serve(&self, req: Request) -> Response {
        let path = req.uri().path().to_string();
        if !matches!(*req.method(), Method::GET | Method::HEAD)
            || path.starts_with("/api/")
            || path.starts_with("/_bgh/")
        {
            return ApiError::NotFound.into_response();
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
