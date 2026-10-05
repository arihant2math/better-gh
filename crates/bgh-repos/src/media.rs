//! `Accept` media types (`application/vnd.github.raw`, `.diff`, ...).

use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Response};

/// The representation a client asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Media {
    Json,
    Raw,
    Html,
    Diff,
    Patch,
    Sha,
    Object,
    Star,
    Full,
    Text,
    Base64,
}

/// Parse `Accept` (`application/vnd.github[.v3][.param][+json]`); the first
/// GitHub media type wins.
pub fn media(headers: &HeaderMap) -> Media {
    let Some(accept) = headers.get(header::ACCEPT).and_then(|v| v.to_str().ok()) else {
        return Media::Json;
    };
    for part in accept.split(',') {
        let ty = part
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        let Some(rest) = ty.strip_prefix("application/vnd.github") else {
            continue;
        };
        let rest = rest.strip_prefix(".v3").unwrap_or(rest);
        let rest = rest.strip_prefix('.').unwrap_or(rest);
        let param = rest.split('+').next().unwrap_or("");
        return match param {
            "raw" => Media::Raw,
            "html" => Media::Html,
            "diff" => Media::Diff,
            "patch" => Media::Patch,
            "sha" => Media::Sha,
            "object" => Media::Object,
            "star" => Media::Star,
            "full" => Media::Full,
            "text" => Media::Text,
            "base64" => Media::Base64,
            _ => Media::Json,
        };
    }
    Media::Json
}

/// Value of `X-GitHub-Media-Type` for a media type.
fn media_header(m: Media) -> &'static str {
    match m {
        Media::Raw => "github.v3; param=raw",
        Media::Html => "github.v3; param=html",
        Media::Diff => "github.v3; param=diff",
        Media::Patch => "github.v3; param=patch",
        Media::Sha => "github.v3; param=sha",
        Media::Object => "github.v3; param=object; format=json",
        Media::Star => "github.v3; param=star; format=json",
        Media::Full => "github.v3; param=full; format=json",
        Media::Text => "github.v3; param=text; format=json",
        Media::Base64 => "github.v3; param=base64; format=json",
        Media::Json => "github.v3; format=json",
    }
}

/// A non-JSON body (`.raw`, `.diff`, `.patch`, `.sha`, `.html`).
pub fn body(m: Media, content_type: &'static str, bytes: impl Into<axum::body::Body>) -> Response {
    let mut resp = bytes.into().into_response();
    let h = resp.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    h.insert(
        "x-github-media-type",
        HeaderValue::from_static(media_header(m)),
    );
    resp
}

/// Mark a response as immutable (content addressed by a full SHA).
pub fn immutable(mut resp: Response, private: bool) -> Response {
    let v = if private {
        "private, max-age=31536000, immutable"
    } else {
        "public, max-age=31536000, immutable"
    };
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static(v));
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(v: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(header::ACCEPT, v.parse().unwrap());
        h
    }

    #[test]
    fn parses_media_types() {
        assert_eq!(media(&HeaderMap::new()), Media::Json);
        assert_eq!(media(&h("application/vnd.github.raw")), Media::Raw);
        assert_eq!(media(&h("application/vnd.github.v3.raw")), Media::Raw);
        assert_eq!(media(&h("application/vnd.github.raw+json")), Media::Raw);
        assert_eq!(media(&h("application/vnd.github.v3.diff")), Media::Diff);
        assert_eq!(media(&h("application/vnd.github.star+json")), Media::Star);
        assert_eq!(media(&h("application/vnd.github+json")), Media::Json);
        assert_eq!(
            media(&h("text/html, application/vnd.github.html")),
            Media::Html
        );
    }
}
