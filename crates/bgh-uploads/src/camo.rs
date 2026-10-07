//! Camo-style image proxy (P35), see [`bgh_core::camo`].
//!
//! * `GET /_bgh/camo/{hmac}/{hex-url}`: fetch the external image (SSRF
//!   guard, ≤ 3 redirects each re-checked, 10 s timeout, ≤ 5 MiB, image
//!   content types only) and serve it locked down (`nosniff`, sandboxing
//!   CSP). 404 when the signature is bad, the proxy is disabled (site
//!   setting `markdown.image_proxy`) or the upstream fails.
//! * `POST /_bgh/camo/sign` `{urls: [...]}` → `{urls: {url: proxied}}`:
//!   signs the external image URLs of client-rendered Markdown (the web
//!   renderer can't hold the key). Same power as posting a comment with
//!   those images; non-external URLs and a disabled proxy map to
//!   themselves.

use std::collections::HashMap;
use std::time::Duration;

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderValue, header};
use axum::response::Response;
use bgh_core::prelude::*;
use bgh_core::{camo, settings, ssrf};
use futures::StreamExt;
use serde::Deserialize;

/// Largest proxied image.
pub const MAX_BYTES: usize = 5 * 1024 * 1024;
const MAX_REDIRECTS: usize = 3;
const MAX_SIGN: usize = 100;

const TYPES: &[&str] = &[
    "image/png",
    "image/jpeg",
    "image/gif",
    "image/webp",
    "image/avif",
    "image/svg+xml",
    "image/bmp",
    "image/x-icon",
    "image/vnd.microsoft.icon",
];

/// SVGs may carry styles; scripts never run (sandbox, no script-src).
const CSP: &str = "default-src 'none'; img-src data:; style-src 'unsafe-inline'; sandbox";

pub async fn proxy(
    State(state): State<AppState>,
    Path((digest, hex_url)): Path<(String, String)>,
) -> ApiResult<Response> {
    if !settings::load(&state).await?.markdown.image_proxy {
        return Err(ApiError::NotFound);
    }
    let url = camo::verify(&digest, &hex_url).ok_or(ApiError::NotFound)?;
    let (content_type, body) = fetch(&state, &url).await.map_err(|err| {
        tracing::debug!(%url, %err, "camo fetch failed");
        ApiError::NotFound
    })?;
    let mut resp = Response::new(Body::from(body));
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&content_type).map_err(ApiError::internal)?,
    );
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=86400"),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    Ok(resp)
}

async fn fetch(state: &AppState, url: &str) -> Result<(String, Vec<u8>), String> {
    let policy = ssrf::Policy::load(state).await;
    let mut url = url.to_string();
    for _ in 0..=MAX_REDIRECTS {
        let target = ssrf::resolve(&policy, &url).await?;
        let mut client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .user_agent("bgh-camo");
        if target.host.parse::<std::net::IpAddr>().is_err() {
            client = client.resolve_to_addrs(&target.host, &target.addrs);
        }
        let client = client.build().map_err(|e| e.to_string())?;
        let resp = client
            .get(target.url.clone())
            .header(header::ACCEPT, "image/*")
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if resp.status().is_redirection() {
            let next = resp
                .headers()
                .get(header::LOCATION)
                .and_then(|l| l.to_str().ok())
                .ok_or("redirect without location")?;
            url = target
                .url
                .join(next)
                .map_err(|e| e.to_string())?
                .to_string();
            continue;
        }
        if !resp.status().is_success() {
            return Err(format!("upstream status {}", resp.status()));
        }
        let content_type = resp
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        if !TYPES.contains(&content_type.as_str()) {
            return Err(format!("content type {content_type:?} is not an image"));
        }
        if resp
            .content_length()
            .is_some_and(|n| n as usize > MAX_BYTES)
        {
            return Err("image too large".into());
        }
        let mut body = Vec::new();
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| e.to_string())?;
            if body.len() + chunk.len() > MAX_BYTES {
                return Err("image too large".into());
            }
            body.extend_from_slice(&chunk);
        }
        return Ok((content_type, body));
    }
    Err("too many redirects".into())
}

#[derive(Debug, Deserialize)]
pub struct SignBody {
    urls: Vec<String>,
}

pub async fn sign(
    State(state): State<AppState>,
    Json(body): Json<SignBody>,
) -> ApiResult<Json<serde_json::Value>> {
    if body.urls.len() > MAX_SIGN {
        return Err(ApiError::unprocessable(format!(
            "at most {MAX_SIGN} urls per request"
        )));
    }
    let on = settings::load(&state).await?.markdown.image_proxy;
    let base = &state.config.base_url;
    let urls: HashMap<&str, String> = body
        .urls
        .iter()
        .map(|u| {
            let signed = if on && camo::is_external(base, u) {
                camo::url("", u)
            } else {
                u.clone()
            };
            (u.as_str(), signed)
        })
        .collect();
    Ok(Json(serde_json::json!({ "enabled": on, "urls": urls })))
}
