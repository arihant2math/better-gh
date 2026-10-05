//! RFC 6238 TOTP (HMAC-SHA1, 6 digits, 30 s steps) and RFC 4648 base32.

use hmac::{Hmac, Mac};
use rand::Rng;
use sha1::Sha1;

pub const STEP_SECS: i64 = 30;
pub const DIGITS: u32 = 6;
/// Accepted clock drift in steps (either direction).
pub const WINDOW: i64 = 1;

const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// Base32 without padding.
pub fn base32_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(5) * 8);
    let (mut buf, mut bits) = (0u32, 0u32);
    for &b in data {
        buf = (buf << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((buf >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((buf << (5 - bits)) & 31) as usize] as char);
    }
    out
}

/// Base32 decode (case-insensitive, ignores padding, spaces and dashes).
pub fn base32_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 5 / 8);
    let (mut buf, mut bits) = (0u32, 0u32);
    for c in s.bytes() {
        if matches!(c, b'=' | b' ' | b'-') {
            continue;
        }
        let v = ALPHABET.iter().position(|a| *a == c.to_ascii_uppercase())? as u32;
        buf = (buf << 5) | v;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Some(out)
}

/// A new random 160-bit secret, base32-encoded.
pub fn new_secret() -> String {
    let mut bytes = [0u8; 20];
    rand::rng().fill(&mut bytes);
    base32_encode(&bytes)
}

/// RFC 4226 HOTP value for `counter`.
pub fn hotp(key: &[u8], counter: u64) -> u32 {
    let mut mac = Hmac::<Sha1>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(&counter.to_be_bytes());
    let h = mac.finalize().into_bytes();
    let offset = (h[19] & 0x0f) as usize;
    let bin = (u32::from(h[offset]) & 0x7f) << 24
        | u32::from(h[offset + 1]) << 16
        | u32::from(h[offset + 2]) << 8
        | u32::from(h[offset + 3]);
    bin % 10u32.pow(DIGITS)
}

/// Code for `secret` at unix time `now`.
pub fn code_at(secret: &str, now: i64) -> Option<String> {
    let key = base32_decode(secret)?;
    Some(format!(
        "{:0width$}",
        hotp(&key, (now / STEP_SECS) as u64),
        width = DIGITS as usize
    ))
}

/// Verify `code` at unix time `now`; returns the matched time step so the
/// caller can reject replays (steps `<= last_used_step`).
pub fn verify(secret: &str, code: &str, now: i64, last_used_step: i64) -> Option<i64> {
    let code: String = code.chars().filter(|c| !c.is_whitespace()).collect();
    if code.len() != DIGITS as usize || !code.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let want: u32 = code.parse().ok()?;
    let key = base32_decode(secret)?;
    let step = now / STEP_SECS;
    (-WINDOW..=WINDOW)
        .map(|d| step + d)
        .filter(|s| *s > last_used_step && *s >= 0)
        .find(|s| hotp(&key, *s as u64) == want)
}

/// `otpauth://` URI for authenticator apps.
pub fn otpauth_uri(issuer: &str, account: &str, secret: &str) -> String {
    let enc = |s: &str| {
        s.bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                    (b as char).to_string()
                }
                _ => format!("%{b:02X}"),
            })
            .collect::<String>()
    };
    format!(
        "otpauth://totp/{}:{}?secret={secret}&issuer={}&algorithm=SHA1&digits={DIGITS}&period={STEP_SECS}",
        enc(issuer),
        enc(account),
        enc(issuer)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base32_roundtrip() {
        assert_eq!(base32_encode(b"foobar"), "MZXW6YTBOI");
        assert_eq!(base32_decode("mzxw6ytboi").unwrap(), b"foobar");
        let s = new_secret();
        assert_eq!(base32_decode(&s).unwrap().len(), 20);
    }

    #[test]
    fn rfc6238_vectors() {
        // RFC 6238 appendix B, SHA-1 key "12345678901234567890" (8 digits there;
        // we compare the low 6 digits).
        let secret = base32_encode(b"12345678901234567890");
        for (t, code8) in [
            (59, "94287082"),
            (1_111_111_109, "07081804"),
            (1_234_567_890, "89005924"),
            (2_000_000_000, "69279037"),
        ] {
            assert_eq!(code_at(&secret, t).unwrap(), code8[2..]);
        }
    }

    #[test]
    fn verifies_with_window_and_replay_protection() {
        let secret = new_secret();
        let now = 1_700_000_000;
        let code = code_at(&secret, now - 30).unwrap();
        let step = verify(&secret, &code, now, 0).unwrap();
        assert_eq!(step, (now - 30) / STEP_SECS);
        assert!(verify(&secret, &code, now, step).is_none());
        assert!(verify(&secret, "12345", now, 0).is_none());
        assert!(
            otpauth_uri("Better GitHub", "ada", &secret)
                .starts_with("otpauth://totp/Better%20GitHub:ada?")
        );
    }
}
