//! Serving the web client from files compiled into the binary (cargo
//! feature `embed-web`), with the same caching rules as the on-disk mode:
//!
//! * `/assets/*` → `public, max-age=31536000, immutable`; missing → 404.
//! * other files → `no-cache`; unknown paths → `index.html` (SPA fallback).
//! * precompressed `.br` / `.gz` siblings are served when accepted.
//! * strong `ETag` from the content hash; `If-None-Match` → 304.
//!
//! The file source is a lookup function so the serving logic is tested
//! without a built client (see the tests below).

use std::borrow::Cow;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::web::{IMMUTABLE, NO_CACHE};

/// One embedded file.
pub struct EmbeddedFile {
    pub data: Cow<'static, [u8]>,
    /// SHA-256 of `data`.
    pub hash: [u8; 32],
}

type Lookup = dyn Fn(&str) -> Option<EmbeddedFile> + Send + Sync;

/// A set of files addressed by relative path (`index.html`, `assets/x.js`).
#[derive(Clone)]
pub struct EmbeddedFiles {
    lookup: Arc<Lookup>,
}

impl EmbeddedFiles {
    pub fn new(lookup: impl Fn(&str) -> Option<EmbeddedFile> + Send + Sync + 'static) -> Self {
        Self {
            lookup: Arc::new(lookup),
        }
    }

    pub fn has_index(&self) -> bool {
        (self.lookup)("index.html").is_some()
    }

    /// Serve a GET/HEAD request (method and API-path checks are done by the caller).
    pub fn serve(&self, req: &Request) -> Response {
        let path = req.uri().path();
        let is_asset = path.starts_with("/assets/");
        let rel = match path.trim_start_matches('/') {
            "" => "index.html",
            p => p,
        };
        let rel = if (self.lookup)(rel).is_some() {
            rel
        } else if is_asset {
            return StatusCode::NOT_FOUND.into_response();
        } else {
            "index.html"
        };

        // Prefer a precompressed sibling the client accepts.
        let accepted = accepted_encodings(req.headers());
        let (file, encoding) = [("br", ".br"), ("gzip", ".gz")]
            .into_iter()
            .filter(|(enc, _)| accepted.contains(enc))
            .find_map(|(enc, ext)| (self.lookup)(&format!("{rel}{ext}")).map(|f| (f, Some(enc))))
            .or_else(|| (self.lookup)(rel).map(|f| (f, None)))
            .expect("file exists");

        let etag = format!("\"{}\"", hex16(&file.hash));
        let mut headers = HeaderMap::new();
        let mime = mime_guess::from_path(rel).first_or_octet_stream();
        if let Ok(v) = HeaderValue::from_str(mime.as_ref()) {
            headers.insert(header::CONTENT_TYPE, v);
        }
        headers.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static(if is_asset { IMMUTABLE } else { NO_CACHE }),
        );
        headers.insert(header::VARY, HeaderValue::from_static("accept-encoding"));
        if let Ok(v) = HeaderValue::from_str(&etag) {
            headers.insert(header::ETAG, v);
        }
        if let Some(enc) = encoding {
            headers.insert(header::CONTENT_ENCODING, HeaderValue::from_static(enc));
        }

        let not_modified = req
            .headers()
            .get(header::IF_NONE_MATCH)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.split(',').any(|t| t.trim() == etag || t.trim() == "*"));
        if not_modified {
            return (StatusCode::NOT_MODIFIED, headers).into_response();
        }
        headers.insert(header::CONTENT_LENGTH, HeaderValue::from(file.data.len()));
        let body = if req.method() == Method::HEAD {
            Body::empty()
        } else {
            match file.data {
                Cow::Borrowed(b) => Body::from(b),
                Cow::Owned(v) => Body::from(v),
            }
        };
        (StatusCode::OK, headers, body).into_response()
    }
}

/// Content codings from `Accept-Encoding` with a non-zero q-value.
fn accepted_encodings(headers: &HeaderMap) -> Vec<&'static str> {
    let Some(value) = headers
        .get(header::ACCEPT_ENCODING)
        .and_then(|v| v.to_str().ok())
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in value.split(',') {
        let mut parts = item.split(';');
        let name = parts.next().unwrap_or("").trim().to_ascii_lowercase();
        let rejected = parts.any(|p| {
            p.trim()
                .strip_prefix("q=")
                .and_then(|q| q.trim().parse::<f32>().ok())
                .is_some_and(|q| q <= 0.0)
        });
        if rejected {
            continue;
        }
        match name.as_str() {
            "br" => out.push("br"),
            "gzip" => out.push("gzip"),
            _ => {}
        }
    }
    out
}

fn hex16(bytes: &[u8]) -> String {
    bytes.iter().take(16).map(|b| format!("{b:02x}")).collect()
}

/// `web/dist` as compiled into the binary.
#[cfg(feature = "embed-web")]
pub fn bundled() -> EmbeddedFiles {
    #[derive(rust_embed::Embed)]
    #[folder = "$CARGO_MANIFEST_DIR/../../web/dist"]
    #[exclude = ".vite/*"]
    struct Dist;

    EmbeddedFiles::new(|path| {
        Dist::get(path).map(|f| EmbeddedFile {
            hash: f.metadata.sha256_hash(),
            data: f.data,
        })
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use axum::body::to_bytes;
    use sha2::{Digest, Sha256};

    use super::*;

    fn files() -> EmbeddedFiles {
        let map: HashMap<&'static str, &'static [u8]> = HashMap::from([
            ("index.html", &b"<!doctype html><div id=app></div>"[..]),
            ("index.html.br", b"BR-INDEX"),
            ("assets/app-abc.js", b"console.log(1)"),
            ("assets/app-abc.js.br", b"BROTLI"),
            ("assets/app-abc.js.gz", b"GZIP"),
            ("sw.js", b"self.x=1"),
        ]);
        EmbeddedFiles::new(move |p| {
            map.get(p).map(|d| EmbeddedFile {
                data: Cow::Borrowed(*d),
                hash: Sha256::digest(d).into(),
            })
        })
    }

    fn get(path: &str, headers: &[(&str, &str)]) -> Request {
        let mut b = Request::builder().uri(path);
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        b.body(Body::empty()).unwrap()
    }

    async fn body(resp: Response) -> String {
        String::from_utf8(to_bytes(resp.into_body(), 1 << 20).await.unwrap().to_vec()).unwrap()
    }

    fn header<'a>(resp: &'a Response, name: &str) -> Option<&'a str> {
        resp.headers().get(name).and_then(|v| v.to_str().ok())
    }

    #[tokio::test]
    async fn assets_are_immutable_and_precompressed() {
        let f = files();
        let resp = f.serve(&get("/assets/app-abc.js", &[]));
        assert_eq!(resp.status(), 200);
        assert_eq!(header(&resp, "cache-control"), Some(IMMUTABLE));
        assert_eq!(
            header(&resp, "content-type"),
            Some("text/javascript"),
            "mime from extension"
        );
        assert_eq!(header(&resp, "content-encoding"), None);
        assert_eq!(body(resp).await, "console.log(1)");

        let resp = f.serve(&get(
            "/assets/app-abc.js",
            &[("accept-encoding", "gzip, br")],
        ));
        assert_eq!(header(&resp, "content-encoding"), Some("br"));
        assert_eq!(header(&resp, "content-type"), Some("text/javascript"));
        assert_eq!(body(resp).await, "BROTLI");

        let resp = f.serve(&get(
            "/assets/app-abc.js",
            &[("accept-encoding", "br;q=0, gzip")],
        ));
        assert_eq!(header(&resp, "content-encoding"), Some("gzip"));
        assert_eq!(body(resp).await, "GZIP");

        let resp = f.serve(&get("/assets/missing.js", &[]));
        assert_eq!(resp.status(), 404);
    }

    #[tokio::test]
    async fn spa_fallback_and_conditional_get() {
        let f = files();
        for path in ["/", "/alice/repo/issues/1", "/index.html"] {
            let resp = f.serve(&get(path, &[]));
            assert_eq!(resp.status(), 200, "{path}");
            assert_eq!(header(&resp, "cache-control"), Some(NO_CACHE));
            assert_eq!(header(&resp, "content-type"), Some("text/html"));
            assert!(body(resp).await.contains("id=app"));
        }
        let resp = f.serve(&get("/", &[("accept-encoding", "br")]));
        assert_eq!(body(resp).await, "BR-INDEX");

        let resp = f.serve(&get("/sw.js", &[]));
        assert_eq!(header(&resp, "cache-control"), Some(NO_CACHE));
        let etag = header(&resp, "etag").unwrap().to_string();
        let resp = f.serve(&get("/sw.js", &[("if-none-match", &etag)]));
        assert_eq!(resp.status(), 304);
        assert_eq!(body(resp).await, "");

        let head = Request::builder()
            .method(Method::HEAD)
            .uri("/sw.js")
            .body(Body::empty())
            .unwrap();
        let resp = f.serve(&head);
        assert_eq!(header(&resp, "content-length"), Some("8"));
        assert_eq!(body(resp).await, "");
    }
}
