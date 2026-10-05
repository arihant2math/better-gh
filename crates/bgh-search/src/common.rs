//! Shared search plumbing: request params, the `{total_count,
//! incomplete_results, items}` envelope, scoring wrapper, text matches.

use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Response};
use bgh_core::prelude::*;
use serde::{Deserialize, Serialize};

use crate::query::{self, Query};

/// GitHub only serves the first 1000 results of a search.
pub const MAX_RESULTS: i64 = 1000;

#[derive(Debug, Default, Deserialize)]
pub struct SearchParams {
    pub q: Option<String>,
    pub sort: Option<String>,
    pub order: Option<String>,
}

impl SearchParams {
    /// Parsed `q` (422 `missing` if absent/blank).
    pub fn query(&self) -> ApiResult<(String, Query)> {
        let q = self.q.as_deref().map(str::trim).unwrap_or("");
        if q.is_empty() {
            return Err(ApiError::invalid_field(FieldError::new(
                "Search", "q", "missing",
            )));
        }
        if q.len() > 256 {
            return Err(ApiError::invalid_field(FieldError::custom(
                "Search",
                "q",
                "The search is longer than 256 characters.",
            )));
        }
        Ok((q.to_string(), query::parse(q)))
    }

    /// `asc` → true; default desc.
    pub fn ascending(&self) -> bool {
        self.order.as_deref() == Some("asc")
    }

    pub fn sort(&self) -> Option<&str> {
        self.sort.as_deref().filter(|s| !s.is_empty())
    }
}

/// Reject pages beyond the first 1000 results.
pub fn check_window(p: &Pagination) -> ApiResult<()> {
    if p.offset() >= MAX_RESULTS {
        return Err(ApiError::unprocessable(
            "Only the first 1000 search results are available",
        ));
    }
    Ok(())
}

/// `LIMIT` for a page, clipped to the 1000-result window.
pub fn page_limit(p: &Pagination) -> i64 {
    p.limit().min(MAX_RESULTS - p.offset()).max(0)
}

/// An item with its `score` (and optional `text_matches`).
#[derive(Debug, Clone, Serialize)]
pub struct Scored<T> {
    #[serde(flatten)]
    pub item: T,
    pub score: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_matches: Option<Vec<TextMatch>>,
}

/// The search response envelope.
#[derive(Debug, Clone, Serialize)]
pub struct SearchResult<T> {
    pub total_count: i64,
    pub incomplete_results: bool,
    pub items: Vec<T>,
    #[serde(skip)]
    pub link: Option<String>,
}

impl<T> SearchResult<T> {
    pub fn new(p: &Pagination, total: i64, items: Vec<T>, incomplete: bool) -> Self {
        let reachable = total.min(MAX_RESULTS);
        let has_next = p.offset() + (items.len() as i64) < reachable;
        Self {
            link: p.link_header(has_next, Some(reachable)),
            total_count: total,
            incomplete_results: incomplete,
            items,
        }
    }
}

impl<T: Serialize> IntoResponse for SearchResult<T> {
    fn into_response(self) -> Response {
        let link = self.link.clone();
        let mut resp = Json(self).into_response();
        if let Some(link) = link
            && let Ok(v) = HeaderValue::from_str(&link)
        {
            resp.headers_mut().insert(header::LINK, v);
        }
        resp
    }
}

/// Whether the client asked for `application/vnd.github.text-match+json`.
pub fn wants_text_matches(headers: &HeaderMap) -> bool {
    headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|a| a.contains("text-match"))
}

#[derive(Debug, Clone, Serialize)]
pub struct MatchSpan {
    pub text: String,
    pub indices: [usize; 2],
}

/// `text_matches[]` entry.
#[derive(Debug, Clone, Serialize)]
pub struct TextMatch {
    pub object_url: String,
    pub object_type: String,
    pub property: String,
    pub fragment: String,
    pub matches: Vec<MatchSpan>,
}

/// Find case-insensitive occurrences of `needles` in `text` and build a
/// fragment (around the first match, at most ~`width` bytes) with match
/// indices relative to the fragment (in characters, like GitHub).
pub fn text_match(
    object_url: &str,
    object_type: &str,
    property: &str,
    text: &str,
    needles: &[&str],
    width: usize,
) -> Option<TextMatch> {
    let lower = text.to_lowercase();
    if lower.len() != text.len() {
        // Case folding changed byte offsets (rare scripts): match exactly.
        return text_match_exact(object_url, object_type, property, text, needles, width);
    }
    let mut hits: Vec<(usize, usize)> = Vec::new();
    for n in needles.iter().filter(|n| !n.is_empty()) {
        let n = n.to_lowercase();
        let mut from = 0;
        while let Some(pos) = lower[from..].find(&n) {
            let s = from + pos;
            hits.push((s, s + n.len()));
            from = s + n.len().max(1);
        }
    }
    build_fragment(object_url, object_type, property, text, hits, width)
}

fn text_match_exact(
    object_url: &str,
    object_type: &str,
    property: &str,
    text: &str,
    needles: &[&str],
    width: usize,
) -> Option<TextMatch> {
    let mut hits = Vec::new();
    for n in needles.iter().filter(|n| !n.is_empty()) {
        for (s, m) in text.match_indices(n) {
            hits.push((s, s + m.len()));
        }
    }
    build_fragment(object_url, object_type, property, text, hits, width)
}

/// Build a fragment from byte-range hits.
pub fn build_fragment(
    object_url: &str,
    object_type: &str,
    property: &str,
    text: &str,
    mut hits: Vec<(usize, usize)>,
    width: usize,
) -> Option<TextMatch> {
    if hits.is_empty() {
        return None;
    }
    hits.sort();
    let first = hits[0].0;
    let mut start = first.saturating_sub(width / 3);
    while !text.is_char_boundary(start) {
        start -= 1;
    }
    // Prefer starting at a line boundary close to the match.
    if let Some(nl) = text[start..first].rfind('\n') {
        start += nl + 1;
    }
    let mut end = (start + width).min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let fragment = &text[start..end];
    let char_idx = |byte: usize| fragment[..byte].chars().count();
    let matches = hits
        .into_iter()
        .filter(|(s, e)| *s >= start && *e <= end)
        .map(|(s, e)| MatchSpan {
            text: text[s..e].to_string(),
            indices: [char_idx(s - start), char_idx(e - start)],
        })
        .collect();
    Some(TextMatch {
        object_url: object_url.to_string(),
        object_type: object_type.to_string(),
        property: property.to_string(),
        fragment: fragment.to_string(),
        matches,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fragments() {
        let m = text_match(
            "u",
            "Issue",
            "title",
            "Fix the Crash in parser",
            &["crash"],
            200,
        )
        .unwrap();
        assert_eq!(m.fragment, "Fix the Crash in parser");
        assert_eq!(m.matches[0].text, "Crash");
        assert_eq!(m.matches[0].indices, [8, 13]);
        assert!(text_match("u", "Issue", "title", "nothing", &["crash"], 200).is_none());
        let long = format!("{}\nneedle here\n{}", "x".repeat(500), "y".repeat(500));
        let m = text_match("u", "FileContent", "content", &long, &["needle"], 100).unwrap();
        assert!(m.fragment.starts_with("needle here"));
        assert_eq!(m.matches[0].indices, [0, 6]);
    }
}
