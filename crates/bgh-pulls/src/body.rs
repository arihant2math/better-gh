//! `Accept: application/vnd.github.{raw,text,html,full}+json` for pull
//! requests, reviews and review comments: `body` / `body_text` /
//! `body_html` per the requested media type.
//!
//! Applied per route with [`formatted`], which runs the wrapped handler
//! unchanged (GraphQL calls the handlers directly) and rewrites the JSON
//! response only when a non-raw format was requested.

use axum::body::{Body, to_bytes};
use axum::extract::{Request, State};
use axum::handler::Handler;
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Response};
use bgh_core::AppState;
use bgh_core::markdown::{self, RenderContext};
use futures::future::BoxFuture;
use serde_json::Value;

/// Which body representations to return.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BodyFormat {
    /// `body` only (default and `.raw`).
    #[default]
    Raw,
    /// `body_text` only.
    Text,
    /// `body_html` only.
    Html,
    /// `body`, `body_text` and `body_html`.
    Full,
}

impl BodyFormat {
    pub fn from_accept(accept: &str) -> Self {
        for part in accept.split(',') {
            let mt = part.split(';').next().unwrap_or("").trim();
            let Some(rest) = mt.strip_prefix("application/vnd.github") else {
                continue;
            };
            let rest = rest.strip_prefix(".v3").unwrap_or(rest);
            let param = rest.strip_suffix("+json").unwrap_or(rest);
            match param {
                ".text" => return Self::Text,
                ".html" => return Self::Html,
                ".full" => return Self::Full,
                ".raw" => return Self::Raw,
                _ => {}
            }
        }
        Self::Raw
    }

    pub fn from_headers(headers: &HeaderMap) -> Self {
        headers
            .get(header::ACCEPT)
            .and_then(|v| v.to_str().ok())
            .map(Self::from_accept)
            .unwrap_or_default()
    }

    fn param(self) -> &'static str {
        match self {
            Self::Raw => "raw",
            Self::Text => "text",
            Self::Html => "html",
            Self::Full => "full",
        }
    }

    /// Rewrite `body` of one resource object.
    pub fn apply(self, obj: &mut serde_json::Map<String, Value>, ctx: &RenderContext<'_>) {
        if self == Self::Raw {
            return;
        }
        let Some(body) = obj.get("body").cloned() else {
            return;
        };
        let html = body.as_str().map(|b| markdown::render(b, ctx));
        if matches!(self, Self::Text | Self::Full) {
            obj.insert(
                "body_text".into(),
                html.as_deref()
                    .map(|h| Value::String(html_to_text(h)))
                    .unwrap_or(Value::Null),
            );
        }
        if matches!(self, Self::Html | Self::Full) {
            obj.insert(
                "body_html".into(),
                html.map(Value::String).unwrap_or(Value::Null),
            );
        }
        if self != Self::Full {
            obj.remove("body");
        }
    }

    /// Rewrite a response body: one object or an array of objects.
    pub fn apply_value(self, v: &mut Value, ctx: &RenderContext<'_>) {
        match v {
            Value::Object(o) => self.apply(o, ctx),
            Value::Array(items) => {
                for item in items {
                    if let Value::Object(o) = item {
                        self.apply(o, ctx);
                    }
                }
            }
            _ => {}
        }
    }
}

/// `{owner}/{repo}` from a `/…/repos/{owner}/{repo}/…` request path.
fn owner_repo(path: &str) -> Option<(String, String)> {
    let mut it = path.split('/').skip_while(|s| *s != "repos").skip(1);
    Some((it.next()?.to_string(), it.next()?.to_string()))
}

/// Wrap a handler so its JSON response honors the body media types.
pub fn formatted<H, T>(
    h: H,
) -> impl FnOnce(State<AppState>, Request) -> BoxFuture<'static, Response> + Clone + Send + Sync + 'static
where
    H: Handler<T, AppState> + Sync,
    T: 'static,
{
    move |State(state): State<AppState>, req: Request| {
        Box::pin(async move {
            let fmt = BodyFormat::from_headers(req.headers());
            let repo = owner_repo(req.uri().path());
            let resp = h.call(req, state.clone()).await;
            match (fmt, repo) {
                (BodyFormat::Raw, _) | (_, None) => resp,
                (fmt, Some((owner, repo))) => reformat(&state, fmt, &owner, &repo, resp).await,
            }
        })
    }
}

async fn reformat(
    state: &AppState,
    fmt: BodyFormat,
    owner: &str,
    repo: &str,
    resp: Response,
) -> Response {
    let is_json = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.starts_with("application/json"));
    if !is_json || !resp.status().is_success() {
        return resp;
    }
    let (mut parts, body) = resp.into_parts();
    let Ok(bytes) = to_bytes(body, usize::MAX).await else {
        return axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let Ok(mut v) = serde_json::from_slice::<Value>(&bytes) else {
        return Response::from_parts(parts, Body::from(bytes));
    };
    let ctx = RenderContext::new(&state.config.base_url).with_repo(owner, repo);
    fmt.apply_value(&mut v, &ctx);
    let out = serde_json::to_vec(&v).unwrap_or_default();
    parts.headers.remove(header::CONTENT_LENGTH);
    if let Ok(mt) = HeaderValue::from_str(&format!("github.v3; param={}; format=json", fmt.param()))
    {
        parts.headers.insert("x-github-media-type", mt);
    }
    Response::from_parts(parts, Body::from(out))
}

/// Plain-text rendering of sanitized HTML (tags stripped, entities decoded,
/// block elements separated by newlines).
pub fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut chars = html.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if c == '<' {
            let end = html[i..].find('>').map(|e| i + e).unwrap_or(html.len() - 1);
            let tag = html[i + 1..end]
                .trim_start_matches('/')
                .split(|c: char| c.is_whitespace() || c == '>' || c == '/')
                .next()
                .unwrap_or("")
                .to_ascii_lowercase();
            if matches!(
                tag.as_str(),
                "p" | "br"
                    | "li"
                    | "div"
                    | "h1"
                    | "h2"
                    | "h3"
                    | "h4"
                    | "h5"
                    | "h6"
                    | "pre"
                    | "tr"
                    | "blockquote"
                    | "ul"
                    | "ol"
                    | "hr"
            ) && !out.ends_with('\n')
                && !out.is_empty()
            {
                out.push('\n');
            }
            while let Some((j, _)) = chars.peek() {
                if *j > end {
                    break;
                }
                chars.next();
            }
        } else if c == '&' {
            let rest = &html[i..];
            let (text, len) = [
                ("&amp;", "&"),
                ("&lt;", "<"),
                ("&gt;", ">"),
                ("&quot;", "\""),
                ("&#39;", "'"),
                ("&nbsp;", " "),
            ]
            .iter()
            .find(|(e, _)| rest.starts_with(e))
            .map(|(e, t)| (*t, e.len()))
            .unwrap_or(("&", 1));
            out.push_str(text);
            for _ in 1..len {
                chars.next();
            }
        } else {
            out.push(c);
        }
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_accept() {
        assert_eq!(
            BodyFormat::from_accept("application/vnd.github.full+json"),
            BodyFormat::Full
        );
        assert_eq!(
            BodyFormat::from_accept("application/vnd.github.v3.html+json"),
            BodyFormat::Html
        );
        assert_eq!(
            BodyFormat::from_accept("application/vnd.github+json"),
            BodyFormat::Raw
        );
        assert_eq!(
            owner_repo("/api/v3/repos/o/r/pulls/1"),
            Some(("o".into(), "r".into()))
        );
    }
}
