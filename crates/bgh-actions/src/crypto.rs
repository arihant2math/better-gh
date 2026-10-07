//! Secret encryption.
//!
//! * At rest: XChaCha20-Poly1305 with the server key (`BGH_ACTIONS_SECRET_KEY`,
//!   or a key generated once in `{data_dir}/actions/server.key`). Stored as
//!   `nonce (24 bytes) || ciphertext`.
//! * In transit (GitHub's `public-key` flow): each repository/organization
//!   has a Curve25519 key pair; clients encrypt values with a libsodium
//!   sealed box (`crypto_box_seal`) for the public key, the server opens it
//!   with the private key (itself stored encrypted with the server key) and
//!   re-encrypts the plaintext with the server key.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use anyhow::Context;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use bgh_core::AppState;
use bgh_core::prelude::*;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use crypto_box::{PublicKey, SecretKey};
use sha2::{Digest, Sha256};

/// The server's at-rest encryption key.
#[derive(Clone)]
pub struct ServerKey(Arc<[u8; 32]>);

impl ServerKey {
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(Arc::new(bytes))
    }

    /// Key from config, or generated and persisted under the data dir.
    pub fn load(config: &bgh_core::Config) -> anyhow::Result<Self> {
        if let Some(b64) = &config.actions.secret_key {
            let bytes = STANDARD
                .decode(b64.trim())
                .context("BGH_ACTIONS_SECRET_KEY is not valid base64")?;
            let arr: [u8; 32] = bytes
                .try_into()
                .map_err(|_| anyhow::anyhow!("BGH_ACTIONS_SECRET_KEY must decode to 32 bytes"))?;
            return Ok(Self::from_bytes(arr));
        }
        let path: PathBuf = config.data_dir.join("actions").join("server.key");
        if let Ok(existing) = std::fs::read_to_string(&path) {
            let bytes = STANDARD
                .decode(existing.trim())
                .with_context(|| format!("invalid key in {}", path.display()))?;
            let arr: [u8; 32] = bytes
                .try_into()
                .map_err(|_| anyhow::anyhow!("{} must hold 32 bytes", path.display()))?;
            return Ok(Self::from_bytes(arr));
        }
        let key: [u8; 32] = rand::random();
        std::fs::create_dir_all(path.parent().expect("has parent"))?;
        write_private(&path, STANDARD.encode(key).as_bytes())?;
        Ok(Self::from_bytes(key))
    }

    /// A 32-byte subkey for `label` (HMAC-SHA256 of the label), e.g. for
    /// signing runtime tokens and blob URLs without exposing the key itself.
    pub fn derive(&self, label: &str) -> [u8; 32] {
        use hmac::{Hmac, Mac};
        let mut mac =
            <Hmac<Sha256> as Mac>::new_from_slice(self.0.as_ref()).expect("any key length");
        mac.update(label.as_bytes());
        mac.finalize().into_bytes().into()
    }

    pub fn encrypt(&self, plaintext: &[u8]) -> Vec<u8> {
        let cipher = XChaCha20Poly1305::new(self.0.as_ref().into());
        let nonce: [u8; 24] = rand::random();
        let mut out = nonce.to_vec();
        out.extend(
            cipher
                .encrypt(XNonce::from_slice(&nonce), plaintext)
                .expect("encryption cannot fail"),
        );
        out
    }

    pub fn decrypt(&self, data: &[u8]) -> anyhow::Result<Vec<u8>> {
        anyhow::ensure!(data.len() >= 24, "ciphertext too short");
        let cipher = XChaCha20Poly1305::new(self.0.as_ref().into());
        cipher
            .decrypt(XNonce::from_slice(&data[..24]), &data[24..])
            .map_err(|_| anyhow::anyhow!("secret decryption failed (wrong server key?)"))
    }

    pub fn decrypt_string(&self, data: &[u8]) -> anyhow::Result<String> {
        Ok(String::from_utf8(self.decrypt(data)?)?)
    }
}

pub(crate) fn write_private(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    match opts.open(path) {
        Ok(mut f) => f.write_all(data),
        // Another process created it first; its key wins.
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e),
    }
}

/// Server key for `state`, loaded once per data dir.
pub fn server_key(state: &AppState) -> ApiResult<ServerKey> {
    type Cache = std::sync::Mutex<Vec<(PathBuf, Option<String>, ServerKey)>>;
    static KEYS: OnceLock<Cache> = OnceLock::new();
    let cache = KEYS.get_or_init(Default::default);
    let mut cache = cache.lock().expect("key cache");
    let dir = state.config.data_dir.clone();
    let configured = state.config.actions.secret_key.clone();
    if let Some((_, _, k)) = cache.iter().find(|(d, c, _)| *d == dir && *c == configured) {
        return Ok(k.clone());
    }
    let key = ServerKey::load(&state.config).map_err(ApiError::internal)?;
    cache.push((dir, configured, key.clone()));
    Ok(key)
}

/// A sealed-box key pair as stored in `actions_keys`.
pub struct BoxKeyPair {
    pub key_id: String,
    /// Base64 public key.
    pub public_key: String,
    pub secret: SecretKey,
}

/// Generate a key pair: returns the row values `(key_id, public_b64, secret_enc)`.
pub fn generate_keypair(server: &ServerKey) -> (String, String, Vec<u8>) {
    let bytes: [u8; 32] = rand::random();
    let secret = SecretKey::from_bytes(bytes);
    let public = secret.public_key();
    let public_b64 = STANDARD.encode(public.as_bytes());
    // GitHub key ids are numeric strings; derive one from the key.
    let digest = Sha256::digest(public.as_bytes());
    let n = u64::from_be_bytes(digest[..8].try_into().expect("8 bytes")) % 10u64.pow(18);
    let key_id = format!("{n:018}");
    (key_id, public_b64, server.encrypt(&bytes))
}

impl BoxKeyPair {
    pub fn from_row(
        server: &ServerKey,
        key_id: String,
        public_key: String,
        secret_enc: &[u8],
    ) -> anyhow::Result<Self> {
        let raw = server.decrypt(secret_enc)?;
        let arr: [u8; 32] = raw
            .try_into()
            .map_err(|_| anyhow::anyhow!("bad secret key length"))?;
        Ok(Self {
            key_id,
            public_key,
            secret: SecretKey::from_bytes(arr),
        })
    }

    /// Open a base64 sealed box produced by `crypto_box_seal`.
    pub fn unseal_b64(&self, encrypted_value: &str) -> Option<Vec<u8>> {
        let data = STANDARD.decode(encrypted_value.trim()).ok()?;
        self.secret.unseal(&data).ok()
    }
}

/// Seal `plaintext` for a base64 public key (what GitHub clients do with
/// libsodium). Used by tests and the runner CLI.
pub fn seal_for(public_key_b64: &str, plaintext: &[u8]) -> anyhow::Result<String> {
    let bytes = STANDARD.decode(public_key_b64)?;
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("bad public key length"))?;
    let sealed = PublicKey::from_bytes(arr)
        .seal(&mut crypto_box::aead::OsRng, plaintext)
        .map_err(|_| anyhow::anyhow!("seal failed"))?;
    Ok(STANDARD.encode(sealed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn at_rest_roundtrip() {
        let k = ServerKey::from_bytes([7; 32]);
        let c = k.encrypt(b"hunter2");
        assert_ne!(&c[24..], b"hunter2");
        assert_eq!(k.decrypt(&c).unwrap(), b"hunter2");
        assert!(ServerKey::from_bytes([8; 32]).decrypt(&c).is_err());
        assert!(k.decrypt(b"short").is_err());
    }

    #[test]
    fn sealed_box_roundtrip() {
        let server = ServerKey::from_bytes([1; 32]);
        let (key_id, public, secret_enc) = generate_keypair(&server);
        assert_eq!(key_id.len(), 18);
        let pair = BoxKeyPair::from_row(&server, key_id, public.clone(), &secret_enc).unwrap();
        let sealed = seal_for(&public, b"s3cret").unwrap();
        assert_eq!(pair.unseal_b64(&sealed).unwrap(), b"s3cret");
        assert!(pair.unseal_b64("bm9wZQ==").is_none());
    }
}
