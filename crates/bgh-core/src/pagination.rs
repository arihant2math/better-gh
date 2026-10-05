//! GitHub-style `page` / `per_page` pagination with RFC 5988 `Link` headers.
//!
//! ```ignore
//! async fn list(p: Pagination, ...) -> ApiResult<Page<Thing>> {
//!     let rows = sqlx::query_as::<_, Row>("... LIMIT $1 OFFSET $2")
//!         .bind(p.limit_plus_one()).bind(p.offset())
//!         .fetch_all(&state.db).await?;
//!     Ok(p.page(rows).map(|r| Thing::new(&state.urls, &r)))
//! }
//! ```

use axum::Json;
use axum::extract::{FromRequestParts, OriginalUri};
use axum::http::request::Parts;
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use crate::error::ApiError;
use crate::state::AppState;

pub const DEFAULT_PER_PAGE: u32 = 30;
pub const MAX_PER_PAGE: u32 = 100;

/// Pagination parameters parsed from the query string, plus the request URL
/// needed to build `Link` headers.
#[derive(Debug, Clone)]
pub struct Pagination {
    pub page: u32,
    pub per_page: u32,
    /// External URL of the request without query (`{base}/api/v3/...`).
    base_url: String,
    /// Other query parameters to preserve in links (raw, already encoded).
    other_params: Vec<(String, String)>,
    per_page_explicit: bool,
}

impl Pagination {
    /// Parse from a path and raw query string.
    pub fn from_parts(external_base: &str, path: &str, query: Option<&str>) -> Self {
        let mut page = 1;
        let mut per_page = DEFAULT_PER_PAGE;
        let mut per_page_explicit = false;
        let mut other_params = Vec::new();
        for pair in query.unwrap_or("").split('&').filter(|s| !s.is_empty()) {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            match k {
                "page" => {
                    if let Ok(n) = v.parse::<u32>() {
                        page = n.max(1);
                    }
                }
                "per_page" => {
                    if let Ok(n) = v.parse::<u32>() {
                        per_page = n.clamp(1, MAX_PER_PAGE);
                        per_page_explicit = true;
                    }
                }
                _ => other_params.push((k.to_string(), v.to_string())),
            }
        }
        Self {
            page,
            per_page,
            base_url: format!("{external_base}{path}"),
            other_params,
            per_page_explicit,
        }
    }

    /// Use a different default (and cap) for `per_page`, for endpoints
    /// whose GitHub default differs from 30 (e.g. notifications: 50/50).
    pub fn with_default_per_page(mut self, default: u32, max: u32) -> Self {
        if !self.per_page_explicit {
            self.per_page = default;
        }
        self.per_page = self.per_page.clamp(1, max);
        self
    }

    /// SQL `LIMIT` for a plain page.
    pub fn limit(&self) -> i64 {
        i64::from(self.per_page)
    }

    /// SQL `LIMIT` when detecting a next page without a total count.
    pub fn limit_plus_one(&self) -> i64 {
        i64::from(self.per_page) + 1
    }

    /// SQL `OFFSET`.
    pub fn offset(&self) -> i64 {
        i64::from(self.page - 1) * i64::from(self.per_page)
    }

    fn url_for(&self, page: u32) -> String {
        let mut q: Vec<String> = self
            .other_params
            .iter()
            .map(|(k, v)| {
                if v.is_empty() {
                    k.clone()
                } else {
                    format!("{k}={v}")
                }
            })
            .collect();
        if self.per_page_explicit {
            q.push(format!("per_page={}", self.per_page));
        }
        q.push(format!("page={page}"));
        format!("{}?{}", self.base_url, q.join("&"))
    }

    /// Build the `Link` header value. `total` (item count) enables `last`.
    pub fn link_header(&self, has_next: bool, total: Option<i64>) -> Option<String> {
        let mut parts = Vec::new();
        let last_page = total.map(|t| {
            let pages = (t.max(0) as u64).div_ceil(u64::from(self.per_page));
            pages.max(1) as u32
        });
        if self.page > 1 {
            parts.push(format!("<{}>; rel=\"prev\"", self.url_for(self.page - 1)));
        }
        if has_next {
            parts.push(format!("<{}>; rel=\"next\"", self.url_for(self.page + 1)));
        }
        if let Some(last) = last_page
            && self.page < last
        {
            parts.push(format!("<{}>; rel=\"last\"", self.url_for(last)));
        }
        if self.page > 1 {
            parts.push(format!("<{}>; rel=\"first\"", self.url_for(1)));
        }
        (!parts.is_empty()).then(|| parts.join(", "))
    }

    /// Page from rows fetched with [`Self::limit_plus_one`]: the extra row
    /// (if any) is dropped and signals a next page.
    pub fn page<T>(&self, mut items: Vec<T>) -> Page<T> {
        let has_next = items.len() > self.per_page as usize;
        items.truncate(self.per_page as usize);
        Page {
            link: self.link_header(has_next, None),
            items,
        }
    }

    /// Page from rows fetched with [`Self::limit`] and a known total count.
    pub fn page_with_total<T>(&self, items: Vec<T>, total: i64) -> Page<T> {
        let has_next = self.offset() + (items.len() as i64) < total;
        Page {
            link: self.link_header(has_next, Some(total)),
            items,
        }
    }
}

impl FromRequestParts<AppState> for Pagination {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let uri = parts
            .extensions
            .get::<OriginalUri>()
            .map(|u| u.0.clone())
            .unwrap_or_else(|| parts.uri.clone());
        Ok(Self::from_parts(
            &state.config.base_url,
            uri.path(),
            uri.query(),
        ))
    }
}

/// A page of results: serializes as a JSON array with a `Link` header.
#[derive(Debug, Clone)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub link: Option<String>,
}

impl<T> Page<T> {
    /// Convert items (e.g. DB rows → API structs) keeping the links.
    pub fn map<U>(self, f: impl FnMut(T) -> U) -> Page<U> {
        Page {
            items: self.items.into_iter().map(f).collect(),
            link: self.link,
        }
    }
}

impl<T: Serialize> IntoResponse for Page<T> {
    fn into_response(self) -> Response {
        let mut resp = Json(self.items).into_response();
        if let Some(link) = self.link
            && let Ok(v) = HeaderValue::from_str(&link)
        {
            resp.headers_mut().insert(header::LINK, v);
        }
        resp
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_clamps() {
        let p = Pagination::from_parts(
            "http://h",
            "/api/v3/x",
            Some("per_page=500&page=0&state=open"),
        );
        assert_eq!(p.per_page, 100);
        assert_eq!(p.page, 1);
        assert_eq!(p.offset(), 0);
        let p = Pagination::from_parts("http://h", "/x", Some("page=3&per_page=10"));
        assert_eq!(p.offset(), 20);
        let d = Pagination::from_parts("http://h", "/x", None);
        assert_eq!(d.per_page, 30);
    }

    #[test]
    fn builds_links() {
        let p = Pagination::from_parts(
            "http://h",
            "/api/v3/x",
            Some("state=open&page=2&per_page=10"),
        );
        let link = p.link_header(true, Some(45)).unwrap();
        assert_eq!(
            link,
            "<http://h/api/v3/x?state=open&per_page=10&page=1>; rel=\"prev\", \
             <http://h/api/v3/x?state=open&per_page=10&page=3>; rel=\"next\", \
             <http://h/api/v3/x?state=open&per_page=10&page=5>; rel=\"last\", \
             <http://h/api/v3/x?state=open&per_page=10&page=1>; rel=\"first\""
        );
        let first = Pagination::from_parts("http://h", "/x", None);
        assert_eq!(first.link_header(false, Some(3)), None);
    }

    #[test]
    fn detects_next_page() {
        let p = Pagination::from_parts("http://h", "/x", Some("per_page=2"));
        let page = p.page(vec![1, 2, 3]);
        assert_eq!(page.items, vec![1, 2]);
        assert!(page.link.unwrap().contains("rel=\"next\""));
        let page = p.page(vec![1]);
        assert!(page.link.is_none());
    }
}
