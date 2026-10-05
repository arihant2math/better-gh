//! Password hashing, random tokens and token hashing.

use argon2::Argon2;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use rand::Rng;
use rand::distr::Alphanumeric;
use sha2::{Digest, Sha256};

/// Prefix of personal access tokens (`bghp_…`).
pub const PAT_PREFIX: &str = "bghp_";

/// Prefix of OAuth app access tokens (`bgho_…`).
pub const OAUTH_TOKEN_PREFIX: &str = "bgho_";

/// Prefix of GitHub App installation access tokens (`bghs_…`, GitHub's
/// `ghs_`).
pub const INSTALLATION_TOKEN_PREFIX: &str = "bghs_";

/// Prefix of fine-grained personal access tokens (`bgh_pat_…`, GitHub's
/// `github_pat_`).
pub const FINE_GRAINED_PAT_PREFIX: &str = "bgh_pat_";

/// Hash a password with Argon2id (PHC string format).
pub fn hash_password(password: &str) -> anyhow::Result<String> {
    let mut salt_bytes = [0u8; 16];
    rand::rng().fill(&mut salt_bytes);
    let salt = SaltString::encode_b64(&salt_bytes).map_err(|e| anyhow::anyhow!("{e}"))?;
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| anyhow::anyhow!("hashing password: {e}"))
}

/// Verify a password against a stored PHC hash. Returns false on malformed hashes.
pub fn verify_password(password: &str, phc: &str) -> bool {
    match PasswordHash::new(phc) {
        Ok(parsed) => Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

/// Random alphanumeric string of `len` characters (CSPRNG).
pub fn random_token(len: usize) -> String {
    rand::rng()
        .sample_iter(&Alphanumeric)
        .take(len)
        .map(char::from)
        .collect()
}

/// New personal access token: `bghp_` + 40 alphanumerics.
pub fn new_pat() -> String {
    format!("{PAT_PREFIX}{}", random_token(40))
}

/// New OAuth access token: `bgho_` + 40 alphanumerics.
pub fn new_oauth_token() -> String {
    format!("{OAUTH_TOKEN_PREFIX}{}", random_token(40))
}

/// New installation access token: `bghs_` + 40 alphanumerics.
pub fn new_installation_token() -> String {
    format!("{INSTALLATION_TOKEN_PREFIX}{}", random_token(40))
}

/// New fine-grained personal access token: `bgh_pat_` + 60 alphanumerics.
pub fn new_fine_grained_pat() -> String {
    format!("{FINE_GRAINED_PAT_PREFIX}{}", random_token(60))
}

/// Constant-time string comparison (for secrets compared in memory).
pub fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Hex SHA-256 of a secret. Tokens and session ids are stored only hashed.
pub fn sha256_hex(input: &str) -> String {
    hex::encode(Sha256::digest(input.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_roundtrip() {
        let h = hash_password("hunter22").unwrap();
        assert!(h.starts_with("$argon2id$"));
        assert!(verify_password("hunter22", &h));
        assert!(!verify_password("hunter23", &h));
        assert!(!verify_password("x", "not-a-hash"));
    }

    #[test]
    fn tokens() {
        let t = new_pat();
        assert!(t.starts_with("bghp_"));
        assert_eq!(t.len(), 45);
        assert_ne!(new_pat(), t);
        assert_eq!(sha256_hex("abc").len(), 64);
    }
}
