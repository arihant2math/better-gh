//! Registry bearer tokens: HS256 JWTs issued by `/v2/token` (Docker token
//! authentication spec) and verified on every `/v2/` request.
//!
//! The signing key is 32 random bytes in `{data_dir}/packages/token.key`,
//! created on first use, so every process sharing the data directory accepts
//! the same tokens.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use bgh_core::AppState;
use hmac::{Hmac, Mac};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::Sha256;

/// Token lifetime. Clients request a new one when it expires.
pub const TOKEN_TTL_SECS: i64 = 300;

/// One granted resource (`repository:<name>:<actions>`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Access {
    #[serde(rename = "type")]
    pub kind: String,
    pub name: String,
    pub actions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    pub iss: String,
    /// Login of the caller ("" for anonymous tokens).
    pub sub: String,
    pub aud: String,
    pub exp: i64,
    pub nbf: i64,
    pub iat: i64,
    pub jti: String,
    pub access: Vec<Access>,
    /// User id of the caller.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uid: Option<i64>,
    /// Repository of an Actions job token (`GITHUB_TOKEN`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jr: Option<i64>,
}

impl Claims {
    /// Whether the token grants `action` on repository `name`.
    pub fn allows(&self, name: &str, action: &str) -> bool {
        self.access.iter().any(|a| {
            a.kind == "repository"
                && a.name == name
                && a.actions.iter().any(|x| x == action || x == "*")
        })
    }
}

fn key_cache() -> &'static Mutex<HashMap<PathBuf, Arc<Vec<u8>>>> {
    static KEYS: OnceLock<Mutex<HashMap<PathBuf, Arc<Vec<u8>>>>> = OnceLock::new();
    KEYS.get_or_init(Default::default)
}

async fn signing_key(state: &AppState) -> anyhow::Result<Arc<Vec<u8>>> {
    let path = crate::storage::root(state).join("token.key");
    if let Some(k) = key_cache().lock().expect("key cache").get(&path) {
        return Ok(k.clone());
    }
    let key = match tokio::fs::read(&path).await {
        Ok(k) if k.len() >= 32 => k,
        _ => {
            tokio::fs::create_dir_all(path.parent().expect("parent")).await?;
            let mut k = vec![0u8; 32];
            rand::rng().fill_bytes(&mut k);
            // Write-then-link: the first writer wins, everyone reads its key.
            let tmp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
            tokio::fs::write(&tmp, &k).await?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ =
                    tokio::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600)).await;
            }
            let _ = tokio::fs::hard_link(&tmp, &path).await;
            let _ = tokio::fs::remove_file(&tmp).await;
            tokio::fs::read(&path).await?
        }
    };
    let key = Arc::new(key);
    key_cache()
        .lock()
        .expect("key cache")
        .insert(path, key.clone());
    Ok(key)
}

pub async fn sign(state: &AppState, claims: &Claims) -> anyhow::Result<String> {
    let key = signing_key(state).await?;
    let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"HS256","typ":"JWT"}"#);
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims)?);
    let input = format!("{header}.{payload}");
    let mut mac = Hmac::<Sha256>::new_from_slice(&key)?;
    mac.update(input.as_bytes());
    let sig = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
    Ok(format!("{input}.{sig}"))
}

/// Verify signature and lifetime; `None` for any invalid token.
pub async fn verify(state: &AppState, token: &str) -> Option<Claims> {
    let mut parts = token.split('.');
    let (h, p, s) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    let header: serde_json::Value =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(h).ok()?).ok()?;
    if header.get("alg")?.as_str()? != "HS256" {
        return None;
    }
    let key = signing_key(state).await.ok()?;
    let mut mac = Hmac::<Sha256>::new_from_slice(&key).ok()?;
    mac.update(format!("{h}.{p}").as_bytes());
    mac.verify_slice(&URL_SAFE_NO_PAD.decode(s).ok()?).ok()?;
    let claims: Claims = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(p).ok()?).ok()?;
    let now = chrono::Utc::now().timestamp();
    (claims.exp > now && claims.nbf <= now + 60).then_some(claims)
}

/// Whether a bearer credential is one of our JWTs (vs. a PAT).
pub fn is_jwt(credential: &str) -> bool {
    credential.matches('.').count() == 2
}
