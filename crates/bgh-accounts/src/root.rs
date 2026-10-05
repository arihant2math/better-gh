//! Miscellaneous root endpoints: `POST /markdown`, `POST /markdown/raw`,
//! `GET /emojis`, `GET /zen`, `GET /octocat`, `GET /versions`, and the
//! locally served emoji images (`GET /_bgh/emoji/{code}.svg`).
//!
//! Emoji images are Twemoji SVGs bundled gzip-compressed in
//! `assets/emoji/` (see its NOTICE.md for attribution and
//! `scripts/build-emoji-assets.mjs` to regenerate).

use std::collections::HashMap;
use std::io::Read;
use std::sync::LazyLock;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bgh_core::markdown::{self, RenderContext};
use bgh_core::prelude::*;
use serde::Deserialize;
use serde_json::{Map, Value};

static EMOJI_NAMES: &str = include_str!("../assets/emoji/emojis.tsv");
static EMOJI_IMAGES: &[u8] = include_bytes!("../assets/emoji/twemoji.bin");

/// Shortcode → image file name (`+1` → `1f44d`), sorted by name.
static NAMES: LazyLock<Vec<(&'static str, &'static str)>> = LazyLock::new(|| {
    EMOJI_NAMES
        .lines()
        .filter_map(|l| l.split_once('\t'))
        .collect()
});

/// Image file name → gzip'd SVG (records of `u16le len, name, u32le len,
/// data`, see the build script).
static IMAGES: LazyLock<HashMap<&'static str, &'static [u8]>> = LazyLock::new(|| {
    let mut map = HashMap::new();
    let mut rest = EMOJI_IMAGES;
    while rest.len() >= 2 {
        let n = u16::from_le_bytes([rest[0], rest[1]]) as usize;
        let Some(name) = rest.get(2..2 + n).and_then(|b| std::str::from_utf8(b).ok()) else {
            break;
        };
        let Some(len) = rest.get(2 + n..6 + n) else {
            break;
        };
        let len = u32::from_le_bytes([len[0], len[1], len[2], len[3]]) as usize;
        let Some(data) = rest.get(6 + n..6 + n + len) else {
            break;
        };
        map.insert(name, data);
        rest = &rest[6 + n + len..];
    }
    map
});

#[derive(Deserialize)]
pub struct MarkdownBody {
    text: Option<String>,
    mode: Option<String>,
    context: Option<String>,
}

fn html_response(html: String) -> Response {
    (
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/html;charset=utf-8"),
        )],
        html,
    )
        .into_response()
}

/// `POST /markdown`: render `text` as plain Markdown (`mode: markdown`, the
/// default) or as user content (`mode: gfm`: `@mentions`, `#123` and SHAs
/// linked, the latter two against the `context` repository).
pub async fn render(
    State(state): State<AppState>,
    Json(body): Json<MarkdownBody>,
) -> ApiResult<Response> {
    let Some(text) = body.text else {
        return Err(ApiError::unprocessable(
            "Invalid request.\n\n\"text\" wasn't supplied.",
        ));
    };
    let gfm = match body.mode.as_deref() {
        None | Some("markdown") => false,
        Some("gfm") => true,
        Some(_) => {
            return Err(ApiError::unprocessable(
                "Invalid request.\n\nFor 'properties/mode', the value is not one of \"markdown\", \"gfm\".",
            ));
        }
    };
    let mut ctx = RenderContext::new(&state.urls.base);
    ctx.references = gfm;
    if gfm
        && let Some((owner, name)) = body.context.as_deref().and_then(|c| c.split_once('/'))
        && !owner.is_empty()
        && !name.is_empty()
        && !name.contains('/')
    {
        ctx = ctx.with_repo(owner, name);
    }
    Ok(html_response(markdown::render(&text, &ctx)))
}

/// `POST /markdown/raw`: the request body (`text/plain` or
/// `text/x-markdown`) rendered as plain Markdown.
pub async fn render_raw(State(state): State<AppState>, body: Bytes) -> ApiResult<Response> {
    let text = std::str::from_utf8(&body)
        .map_err(|_| ApiError::bad_request("Problems parsing request body: invalid UTF-8"))?;
    let mut ctx = RenderContext::new(&state.urls.base);
    ctx.references = false;
    Ok(html_response(markdown::render(text, &ctx)))
}

/// `GET /emojis`: shortcode → image URL.
pub async fn emojis(State(state): State<AppState>) -> axum::Json<Value> {
    let map: Map<String, Value> = NAMES
        .iter()
        .map(|(name, code)| {
            let url = state.urls.html(&format!("/_bgh/emoji/{code}.svg"));
            ((*name).to_string(), Value::String(url))
        })
        .collect();
    axum::Json(Value::Object(map))
}

/// `GET /_bgh/emoji/{code}.svg`: a bundled emoji image (sent gzip-encoded
/// when the client accepts it).
pub async fn emoji_image(Path(file): Path<String>, headers: HeaderMap) -> ApiResult<Response> {
    let code = file.strip_suffix(".svg").ok_or(ApiError::NotFound)?;
    let gz = *IMAGES.get(code).ok_or(ApiError::NotFound)?;
    let accepts_gzip = headers
        .get(header::ACCEPT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|e| e.trim().starts_with("gzip")));
    let mut resp = if accepts_gzip {
        let mut r = gz.into_response();
        r.headers_mut()
            .insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
        r
    } else {
        let mut svg = Vec::new();
        flate2::read::GzDecoder::new(gz)
            .read_to_end(&mut svg)
            .map_err(ApiError::internal)?;
        svg.into_response()
    };
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("image/svg+xml"),
    );
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=31536000, immutable"),
    );
    h.insert(header::VARY, HeaderValue::from_static("Accept-Encoding"));
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; style-src 'unsafe-inline'"),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    Ok(resp)
}

const ZEN: &[&str] = &[
    "Responsive is better than fast.",
    "It's not fully shipped until it's fast.",
    "Anything added dilutes everything else.",
    "Practicality beats purity.",
    "Approachable is better than simple.",
    "Mind your words, they are important.",
    "Speak like a human.",
    "Half measures are as bad as nothing at all.",
    "Encourage flow.",
    "Non-blocking is better than blocking.",
    "Favor focus over features.",
    "Avoid administrative distraction.",
    "Design for failure.",
    "Keep it logically awesome.",
];

fn random_zen() -> &'static str {
    ZEN[rand::random_range(0..ZEN.len())]
}

fn text_response(content_type: &'static str, body: String) -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, HeaderValue::from_static(content_type))],
        body,
    )
        .into_response()
}

/// `GET /zen`: a random sentence from the Zen of GitHub (`text/plain`).
pub async fn zen() -> Response {
    text_response("text/plain;charset=utf-8", random_zen().to_string())
}

#[derive(Deserialize)]
pub struct OctocatQuery {
    s: Option<String>,
}

/// `GET /octocat`: ASCII art saying `s` (or a random zen sentence).
pub async fn octocat(Query(q): Query<OctocatQuery>) -> Response {
    let words: String =
        q.s.unwrap_or_else(|| random_zen().to_string())
            .chars()
            .filter(|c| !c.is_control())
            .take(200)
            .collect();
    let width = words.chars().count() + 2;
    let line = "-".repeat(width);
    let art = format!(
        "\n  {line}\n | {words} |\n  {line}\n   /\n  /\n   /\\_/\\\n  ( o.o )\n   > ^ <\n  /|   |\\\n (_|   |_)\n"
    );
    text_response("application/octocat-stream", art)
}

/// `GET /versions`: the supported `X-GitHub-Api-Version` values.
pub async fn versions() -> axum::Json<Vec<&'static str>> {
    axum::Json(bgh_core::API_VERSIONS.to_vec())
}
