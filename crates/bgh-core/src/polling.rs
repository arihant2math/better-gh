//! Polling support for list endpoints clients poll (notifications, the
//! Events API): `Last-Modified`, `If-Modified-Since` → 304 and
//! `X-Poll-Interval`, as on GitHub.
//!
//! ```ignore
//! let last = /* newest change of what the list shows */;
//! if polling::not_modified(&headers, last) {
//!     return Ok(polling::not_modified_response(last));
//! }
//! Ok(polling::with_headers(page.into_response(), last))
//! ```
//!
//! Handlers that only know `Last-Modified` after building the body return
//! [`Polled`] and mount [`conditional`] as a route layer, which turns a
//! covered 200 into a 304.

use axum::extract::Request;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};

/// Seconds clients should wait between polls (`X-Poll-Interval`).
pub const POLL_INTERVAL_SECS: u32 = 60;

/// HTTP date (`Sun, 06 Nov 1994 08:49:37 GMT`).
pub fn http_date(t: DateTime<Utc>) -> String {
    t.format("%a, %d %b %Y %H:%M:%S GMT").to_string()
}

/// Parse an HTTP date (IMF-fixdate / RFC 2822).
pub fn parse_http_date(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc2822(s.trim())
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

/// Whether the request's `If-Modified-Since` covers `last_modified`
/// (compared at whole seconds, the resolution of HTTP dates). A list with
/// nothing in it (`None`) is "not modified" against any valid date.
pub fn not_modified(headers: &HeaderMap, last_modified: Option<DateTime<Utc>>) -> bool {
    let Some(since) = headers
        .get(header::IF_MODIFIED_SINCE)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_http_date)
    else {
        return false;
    };
    last_modified.is_none_or(|t| t.timestamp() <= since.timestamp())
}

/// Add `X-Poll-Interval` and (when known) `Last-Modified` to `resp`.
pub fn with_headers(mut resp: Response, last_modified: Option<DateTime<Utc>>) -> Response {
    let h = resp.headers_mut();
    h.insert("x-poll-interval", HeaderValue::from(POLL_INTERVAL_SECS));
    if let Some(t) = last_modified
        && let Ok(v) = HeaderValue::from_str(&http_date(t))
    {
        h.insert(header::LAST_MODIFIED, v);
    }
    resp
}

/// Empty 304 with the polling headers.
pub fn not_modified_response(last_modified: Option<DateTime<Utc>>) -> Response {
    with_headers(StatusCode::NOT_MODIFIED.into_response(), last_modified)
}

/// A response body with polling headers (`Last-Modified` when known).
#[derive(Debug)]
pub struct Polled<T> {
    pub body: T,
    pub last_modified: Option<DateTime<Utc>>,
}

impl<T: IntoResponse> IntoResponse for Polled<T> {
    fn into_response(self) -> Response {
        with_headers(self.body.into_response(), self.last_modified)
    }
}

/// Route layer (`axum::middleware::from_fn(polling::conditional)`): a GET
/// answered 200 with a `Last-Modified` the request's `If-Modified-Since`
/// covers becomes an empty 304 with the same polling headers.
pub async fn conditional(req: Request, next: Next) -> Response {
    let headers = (matches!(*req.method(), Method::GET | Method::HEAD)
        && req.headers().contains_key(header::IF_MODIFIED_SINCE))
    .then(|| req.headers().clone());
    let resp = next.run(req).await;
    let Some(headers) = headers else {
        return resp;
    };
    if resp.status() != StatusCode::OK {
        return resp;
    }
    let Some(last) = resp
        .headers()
        .get(header::LAST_MODIFIED)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_http_date)
    else {
        return resp;
    };
    if !not_modified(&headers, Some(last)) {
        return resp;
    }
    let (mut parts, _) = resp.into_parts();
    parts.status = StatusCode::NOT_MODIFIED;
    for h in [header::CONTENT_LENGTH, header::CONTENT_TYPE, header::LINK] {
        parts.headers.remove(h);
    }
    Response::from_parts(parts, axum::body::Body::empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn http_dates_round_trip() {
        let t = Utc.with_ymd_and_hms(1994, 11, 6, 8, 49, 37).unwrap();
        assert_eq!(http_date(t), "Sun, 06 Nov 1994 08:49:37 GMT");
        assert_eq!(parse_http_date("Sun, 06 Nov 1994 08:49:37 GMT"), Some(t));
        assert_eq!(parse_http_date("yesterday"), None);
    }

    #[test]
    fn conditional() {
        let t = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
        let mut h = HeaderMap::new();
        assert!(!not_modified(&h, Some(t)));
        h.insert(
            header::IF_MODIFIED_SINCE,
            HeaderValue::from_str(&http_date(t)).unwrap(),
        );
        assert!(not_modified(&h, Some(t)));
        assert!(not_modified(&h, None));
        assert!(!not_modified(&h, Some(t + chrono::Duration::seconds(1))));
    }
}
