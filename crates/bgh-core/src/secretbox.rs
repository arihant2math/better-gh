//! At-rest encryption of server-held secrets (import and mirror
//! credentials, ...).
//!
//! XChaCha20-Poly1305 with the server key, stored as `nonce (24 bytes) ||
//! ciphertext`. The key is the one Actions uses for its secrets
//! (`BGH_ACTIONS_SECRET_KEY`, or a key generated once in
//! `{data_dir}/actions/server.key`), so a deployment has a single key to
//! back up.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::Context;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};

use crate::{ApiError, ApiResult, AppState, Config};

/// The server's at-rest encryption key.
#[derive(Clone)]
pub struct ServerKey(Arc<[u8; 32]>);

impl ServerKey {
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(Arc::new(bytes))
    }

    /// Key from config, or generated and persisted under the data dir.
    pub fn load(config: &Config) -> anyhow::Result<Self> {
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
        for _ in 0..2 {
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
            if write_new_private(&path, STANDARD.encode(key).as_bytes())? {
                return Ok(Self::from_bytes(key));
            }
            // Someone else created it first: read theirs.
        }
        anyhow::bail!("could not load {}", path.display())
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
}

/// Create `path` with mode 0600; `false` when it already exists.
fn write_new_private(path: &std::path::Path, data: &[u8]) -> std::io::Result<bool> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    match opts.open(path) {
        Ok(mut f) => f.write_all(data).map(|_| true),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(e),
    }
}

/// Server key for `state`, loaded once per data dir.
pub fn server_key(state: &AppState) -> ApiResult<ServerKey> {
    type Cache = Mutex<Vec<(PathBuf, Option<String>, ServerKey)>>;
    static KEYS: OnceLock<Cache> = OnceLock::new();
    let mut cache = KEYS
        .get_or_init(Default::default)
        .lock()
        .expect("key cache");
    let dir = state.config.data_dir.clone();
    let configured = state.config.actions.secret_key.clone();
    if let Some((_, _, k)) = cache.iter().find(|(d, c, _)| *d == dir && *c == configured) {
        return Ok(k.clone());
    }
    let key = ServerKey::load(&state.config).map_err(ApiError::internal)?;
    cache.push((dir, configured, key.clone()));
    Ok(key)
}

/// Encrypt `plaintext` with the server key.
pub fn seal(state: &AppState, plaintext: &str) -> ApiResult<Vec<u8>> {
    Ok(server_key(state)?.encrypt(plaintext.as_bytes()))
}

/// Decrypt a value produced by [`seal`].
pub fn open(state: &AppState, data: &[u8]) -> ApiResult<String> {
    let bytes = server_key(state)?
        .decrypt(data)
        .map_err(ApiError::internal)?;
    String::from_utf8(bytes).map_err(ApiError::internal)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let k = ServerKey::from_bytes([7; 32]);
        let c = k.encrypt(b"hunter2");
        assert_ne!(&c[24..], b"hunter2");
        assert_eq!(k.decrypt(&c).unwrap(), b"hunter2");
        assert!(ServerKey::from_bytes([8; 32]).decrypt(&c).is_err());
        assert!(k.decrypt(&c[..10]).is_err());
    }
}
