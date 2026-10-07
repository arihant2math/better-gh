//! Camo-style image proxy URLs (P35).
//!
//! External `<img src>` in rendered Markdown is rewritten to
//! `{base}/_bgh/camo/{hmac}/{hex(url)}` so viewers never contact third-party
//! image hosts directly (no IP / read-receipt tracking, no mixed content).
//! `hmac` is HMAC-SHA256 of the URL with a per-instance key kept in
//! `{data_dir}/camo.key` (generated on first start). The proxy itself
//! (`bgh-uploads::camo`) verifies the signature, applies the SSRF guard and
//! size/type limits.
//!
//! The key and the on/off switch (site setting `markdown.image_proxy`,
//! default on) live outside `AppState` so the pure
//! [`crate::markdown::render`] can sign without a state handle. The switch is
//! keyed by the instance's base URL (which `render` already gets): several
//! instances can share a process (every test app does), and one instance
//! loading its settings must not flip another's switch.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{LazyLock, OnceLock, RwLock};

use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::Sha256;

/// URL path prefix of the proxy.
pub const PATH: &str = "/_bgh/camo/";

static KEY: OnceLock<[u8; 32]> = OnceLock::new();
/// Base URL (no trailing slash) → switch; absent means on (the default).
static ENABLED: LazyLock<RwLock<HashMap<String, bool>>> = LazyLock::new(Default::default);

/// Load (or create) `{data_dir}/camo.key`. The first call wins; later calls
/// (other test apps in the same process) keep the existing key. Falls back
/// to an ephemeral key when the file can't be read or written.
pub fn init(data_dir: &Path) {
    if KEY.get().is_some() {
        return;
    }
    let path = data_dir.join("camo.key");
    let key = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| hex::decode(s.trim()).ok())
        .and_then(|b| <[u8; 32]>::try_from(b).ok())
        .unwrap_or_else(|| {
            let mut k = [0u8; 32];
            rand::rng().fill_bytes(&mut k);
            let _ = std::fs::create_dir_all(data_dir);
            if let Err(err) = std::fs::write(&path, hex::encode(k)) {
                tracing::warn!(%err, path = %path.display(), "camo key not persisted");
            }
            k
        });
    let _ = KEY.set(key);
}

fn key() -> &'static [u8; 32] {
    KEY.get_or_init(|| {
        let mut k = [0u8; 32];
        rand::rng().fill_bytes(&mut k);
        k
    })
}

/// Whether the instance at `base` proxies external images (site setting
/// `markdown.image_proxy`).
pub fn enabled(base: &str) -> bool {
    let base = base.trim_end_matches('/');
    ENABLED
        .read()
        .expect("camo switch")
        .get(base)
        .copied()
        .unwrap_or(true)
}

/// Updated whenever the site settings of the instance at `base` are
/// (re)loaded.
pub fn set_enabled(base: &str, on: bool) {
    let base = base.trim_end_matches('/');
    if enabled(base) != on {
        ENABLED
            .write()
            .expect("camo switch")
            .insert(base.to_string(), on);
    }
}

fn mac(url: &str) -> Hmac<Sha256> {
    let mut m = Hmac::<Sha256>::new_from_slice(key()).expect("any key length");
    m.update(url.as_bytes());
    m
}

/// Hex HMAC of `url`.
pub fn sign(url: &str) -> String {
    hex::encode(mac(url).finalize().into_bytes())
}

/// Proxy URL for `url` under `base` (no trailing slash).
pub fn url(base: &str, url: &str) -> String {
    format!(
        "{}{PATH}{}/{}",
        base.trim_end_matches('/'),
        sign(url),
        hex::encode(url)
    )
}

/// Decode and verify `/{hmac}/{hex}`; the original URL when valid.
pub fn verify(digest: &str, hex_url: &str) -> Option<String> {
    let url = String::from_utf8(hex::decode(hex_url).ok()?).ok()?;
    let digest = hex::decode(digest).ok()?;
    mac(&url).verify_slice(&digest).ok()?;
    Some(url)
}

/// Whether `src` is an external `http(s)` image that should be proxied:
/// absolute and not on this instance (`base`).
pub fn is_external(base: &str, src: &str) -> bool {
    let lower = src.get(..8).unwrap_or(src).to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return false;
    }
    let base = base.trim_end_matches('/');
    !(src.len() > base.len()
        && src[..base.len()].eq_ignore_ascii_case(base)
        && src.as_bytes()[base.len()] == b'/')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signs_and_verifies() {
        let u = url("http://h", "https://img.example/a.png");
        let rest = u.strip_prefix("http://h/_bgh/camo/").unwrap();
        let (d, h) = rest.split_once('/').unwrap();
        assert_eq!(verify(d, h).as_deref(), Some("https://img.example/a.png"));
        assert_eq!(verify(d, &hex::encode("https://evil.example/")), None);
        assert_eq!(verify("zz", h), None);
    }

    #[test]
    fn external_detection() {
        assert!(is_external("http://h", "https://x.example/a.png"));
        assert!(is_external("http://h", "HTTP://h.evil/a.png"));
        assert!(!is_external(
            "http://h",
            "http://h/user-attachments/assets/x"
        ));
        assert!(!is_external("http://h", "/relative.png"));
        assert!(!is_external("http://h", "data:image/png;base64,AA"));
    }
}
