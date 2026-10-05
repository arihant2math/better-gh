//! Avatars: uploaded images and deterministic identicons.
//!
//! * `GET /avatars/u/{id}?s=` → the uploaded avatar, or a GitHub-style
//!   5×5 mirrored identicon PNG (`s` = size in px, 16-460, default 420).
//!   Responses carry `ETag` and `Cache-Control: public, max-age=86400`
//!   (`immutable` for versioned `?v=` URLs of uploads); `If-None-Match` →
//!   304.
//! * `PUT /_bgh/user/avatar` (raw PNG/JPEG/GIF/WebP body, ≤ 1 MiB),
//!   `DELETE /_bgh/user/avatar`; `PUT|DELETE /_bgh/orgs/{org}/avatar` for
//!   organization owners.
//!
//! An upload sets `users.avatar_url` to `/avatars/u/{id}?v={hash}` so every
//! rendered `avatar_url` changes and caches refresh.

use std::io::Write;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bgh_core::audit;
use bgh_core::prelude::*;
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::orgs::OrgAccess;
use crate::util;

pub const MAX_AVATAR_BYTES: usize = 1024 * 1024;
const DEFAULT_SIZE: u32 = 420;

// ---------------------------------------------------------------------------
// Identicon
// ---------------------------------------------------------------------------

/// 5×5 cell pattern (mirrored around the middle column) and RGB color
/// derived from `seed`.
pub fn identicon_pattern(seed: &str) -> ([[bool; 5]; 5], [u8; 3]) {
    let h = Sha256::digest(seed.as_bytes());
    let mut grid = [[false; 5]; 5];
    for (row, cells) in grid.iter_mut().enumerate() {
        for col in 0..3 {
            let on = h[row * 3 + col] % 2 == 0;
            cells[col] = on;
            cells[4 - col] = on;
        }
    }
    // Hue from the hash, fixed saturation/lightness (like GitHub's).
    let hue = f64::from(u16::from_be_bytes([h[28], h[29]])) / 65535.0 * 360.0;
    let sat = 0.45 + f64::from(h[30]) / 255.0 * 0.2;
    let light = 0.55 + f64::from(h[31]) / 255.0 * 0.1;
    (grid, hsl_to_rgb(hue, sat, light))
}

fn hsl_to_rgb(h: f64, s: f64, l: f64) -> [u8; 3] {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match (h / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let f = |v: f64| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    [f(r), f(g), f(b)]
}

/// Encode an RGB8 image as PNG.
pub fn encode_png(width: u32, height: u32, rgb: &[u8]) -> Vec<u8> {
    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut crc = crc32fast::Hasher::new();
        crc.update(kind);
        crc.update(data);
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        out.extend_from_slice(&crc.finalize().to_be_bytes());
    }
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // 8-bit, RGB, deflate, no filter, no interlace
    chunk(&mut out, b"IHDR", &ihdr);
    let stride = width as usize * 3;
    let mut raw = Vec::with_capacity((stride + 1) * height as usize);
    for row in rgb.chunks(stride) {
        raw.push(0); // filter: none
        raw.extend_from_slice(row);
    }
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
    z.write_all(&raw).expect("writing to a Vec");
    chunk(&mut out, b"IDAT", &z.finish().expect("writing to a Vec"));
    chunk(&mut out, b"IEND", &[]);
    out
}

/// Render the identicon for `seed` as a `size`×`size` PNG.
pub fn identicon_png(seed: &str, size: u32) -> Vec<u8> {
    let (grid, color) = identicon_pattern(seed);
    let bg = [240u8, 240, 240];
    let size = size.max(6);
    // 5 cells plus half a cell of margin on each side.
    let cell = size as f64 / 6.0;
    let margin = cell / 2.0;
    let mut px = Vec::with_capacity((size * size * 3) as usize);
    for y in 0..size {
        for x in 0..size {
            let cx = ((f64::from(x) - margin) / cell).floor();
            let cy = ((f64::from(y) - margin) / cell).floor();
            let on = (0.0..5.0).contains(&cx)
                && (0.0..5.0).contains(&cy)
                && grid[cy as usize][cx as usize];
            px.extend_from_slice(if on { &color } else { &bg });
        }
    }
    encode_png(size, size, &px)
}

// ---------------------------------------------------------------------------
// Serving
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct AvatarQuery {
    pub s: Option<u32>,
    pub size: Option<u32>,
    pub v: Option<String>,
}

fn not_modified(headers: &HeaderMap, etag: &str) -> bool {
    headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|t| t.trim() == etag || t.trim() == "*"))
}

fn image_response(
    headers: &HeaderMap,
    etag: String,
    content_type: &str,
    cache: &'static str,
    body: Vec<u8>,
) -> Response {
    let mut resp = if not_modified(headers, &etag) {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        let mut r = body.into_response();
        if let Ok(v) = HeaderValue::from_str(content_type) {
            r.headers_mut().insert(header::CONTENT_TYPE, v);
        }
        r
    };
    let h = resp.headers_mut();
    if let Ok(v) = HeaderValue::from_str(&etag) {
        h.insert(header::ETAG, v);
    }
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    h.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    resp
}

/// `GET /avatars/u/{id}`
pub async fn serve(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<AvatarQuery>,
) -> ApiResult<Response> {
    let id: i64 = id.parse().map_err(|_| ApiError::NotFound)?;
    let uploaded: Option<(String, String, Vec<u8>)> =
        sqlx::query_as("SELECT content_type, sha256, data FROM user_avatars WHERE user_id = $1")
            .bind(id)
            .fetch_optional(&state.db)
            .await?;
    if let Some((content_type, sha, data)) = uploaded {
        let cache = if q.v.as_deref().is_some_and(|v| sha.starts_with(v)) {
            "public, max-age=31536000, immutable"
        } else {
            "public, max-age=86400"
        };
        return Ok(image_response(
            &headers,
            format!("\"{sha}\""),
            &content_type,
            cache,
            data,
        ));
    }
    let size = q.s.or(q.size).unwrap_or(DEFAULT_SIZE).clamp(16, 460);
    let etag = format!("\"identicon-{id}-{size}-1\"");
    if not_modified(&headers, &etag) {
        return Ok(image_response(
            &headers,
            etag,
            "image/png",
            "public, max-age=86400",
            vec![],
        ));
    }
    let png =
        tokio::task::spawn_blocking(move || identicon_png(&format!("bgh:{id}"), size)).await?;
    Ok(image_response(
        &headers,
        etag,
        "image/png",
        "public, max-age=86400",
        png,
    ))
}

// ---------------------------------------------------------------------------
// Upload
// ---------------------------------------------------------------------------

/// Detect a supported image type from its magic bytes.
pub fn sniff_image(data: &[u8]) -> Option<&'static str> {
    if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if data.len() > 12 && &data[..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

async fn store(
    state: &AppState,
    actor: &db::User,
    account: &db::User,
    data: &[u8],
) -> ApiResult<db::User> {
    if data.is_empty() {
        return Err(ApiError::invalid_field(FieldError::missing_field(
            "Avatar", "image",
        )));
    }
    if data.len() > MAX_AVATAR_BYTES {
        return Err(ApiError::Status(
            StatusCode::PAYLOAD_TOO_LARGE,
            "Avatar images must be 1 MB or smaller.".into(),
        ));
    }
    let content_type = sniff_image(data).ok_or_else(|| {
        ApiError::invalid_field(FieldError::custom(
            "Avatar",
            "image",
            "must be a PNG, JPEG, GIF or WebP image",
        ))
    })?;
    let sha = hex::encode(Sha256::digest(data));
    let mut tx = Tx::begin(state).await?;
    sqlx::query(
        "INSERT INTO user_avatars (user_id, content_type, sha256, data) VALUES ($1, $2, $3, $4)
         ON CONFLICT (user_id) DO UPDATE SET content_type = EXCLUDED.content_type,
             sha256 = EXCLUDED.sha256, data = EXCLUDED.data, updated_at = now()",
    )
    .bind(account.id)
    .bind(content_type)
    .bind(&sha)
    .bind(data)
    .execute(&mut *tx)
    .await?;
    let user = set_avatar_url(
        &mut tx,
        account.id,
        Some(format!("/avatars/u/{}?v={}", account.id, &sha[..12])),
    )
    .await?;
    util::sync_profile(&mut tx, &state.urls, &user).await?;
    audit::log(
        &mut *tx,
        Some(actor),
        "user.avatar_update",
        audit::Target::User(account.id),
        json!({}),
    )
    .await?;
    tx.commit().await?;
    Ok(user)
}

async fn set_avatar_url(tx: &mut Tx, id: i64, url: Option<String>) -> ApiResult<db::User> {
    Ok(sqlx::query_as(&format!(
        "UPDATE users SET avatar_url = $2, updated_at = now() WHERE id = $1 RETURNING {}",
        db::User::COLUMNS
    ))
    .bind(id)
    .bind(url)
    .fetch_one(&mut **tx)
    .await?)
}

async fn clear(state: &AppState, account: &db::User) -> ApiResult<db::User> {
    let mut tx = Tx::begin(state).await?;
    sqlx::query("DELETE FROM user_avatars WHERE user_id = $1")
        .bind(account.id)
        .execute(&mut *tx)
        .await?;
    let user = set_avatar_url(&mut tx, account.id, None).await?;
    util::sync_profile(&mut tx, &state.urls, &user).await?;
    tx.commit().await?;
    Ok(user)
}

fn avatar_json(state: &AppState, u: &db::User) -> Json<serde_json::Value> {
    Json(json!({ "avatar_url": state.urls.avatar(u.id, u.avatar_url.as_deref()) }))
}

/// `PUT /_bgh/user/avatar` (raw image body) → `{avatar_url}`.
pub async fn upload_mine(
    State(state): State<AppState>,
    auth: RequireUser,
    body: Bytes,
) -> ApiResult<Json<serde_json::Value>> {
    util::require_session(&auth)?;
    let user = store(&state, &auth.user, &auth.user, &body).await?;
    Ok(avatar_json(&state, &user))
}

/// `DELETE /_bgh/user/avatar` → `{avatar_url}` (back to the identicon).
pub async fn delete_mine(
    State(state): State<AppState>,
    auth: RequireUser,
) -> ApiResult<Json<serde_json::Value>> {
    util::require_session(&auth)?;
    let user = clear(&state, &auth.user).await?;
    Ok(avatar_json(&state, &user))
}

/// `PUT /_bgh/orgs/{org}/avatar` (owners).
pub async fn upload_org(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(org): Path<String>,
    body: Bytes,
) -> ApiResult<Json<serde_json::Value>> {
    let access = OrgAccess::load(&state, Some(&auth), &org).await?;
    access.require_admin()?;
    let org = store(&state, &auth.user, &access.org, &body).await?;
    Ok(avatar_json(&state, &org))
}

/// `DELETE /_bgh/orgs/{org}/avatar` (owners).
pub async fn delete_org(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(org): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let access = OrgAccess::load(&state, Some(&auth), &org).await?;
    access.require_admin()?;
    let org = clear(&state, &access.org).await?;
    Ok(avatar_json(&state, &org))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identicons_are_deterministic_and_symmetric() {
        let (g1, c1) = identicon_pattern("bgh:1");
        let (g2, c2) = identicon_pattern("bgh:1");
        assert_eq!((g1, c1), (g2, c2));
        assert_ne!(identicon_pattern("bgh:2"), (g1, c1));
        for row in g1 {
            assert_eq!(row[0], row[4]);
            assert_eq!(row[1], row[3]);
        }
        let png = identicon_png("bgh:1", 64);
        assert_eq!(sniff_image(&png), Some("image/png"));
        assert_eq!(&png[16..24], &[0, 0, 0, 64, 0, 0, 0, 64]);
    }
}
