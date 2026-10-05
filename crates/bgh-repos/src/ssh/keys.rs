//! SSH host key persistence and public key → principal lookup.

use std::path::Path;

use bgh_core::auth::AuthMethod;
use bgh_core::prelude::*;
use russh::keys::ssh_key::LineEnding;
use russh::keys::ssh_key::private::{Ed25519Keypair, KeypairData};
use russh::keys::{HashAlg, PrivateKey, PublicKey};

/// Load the Ed25519 host key from `path`, generating (mode 0600) it on
/// first start so clients see a stable host identity.
pub fn load_or_generate_host_key(path: &Path) -> anyhow::Result<PrivateKey> {
    if let Ok(pem) = std::fs::read_to_string(path) {
        return Ok(PrivateKey::from_openssh(pem)?);
    }
    let seed: [u8; 32] = rand::random();
    let key = PrivateKey::new(
        KeypairData::Ed25519(Ed25519Keypair::from_seed(&seed)),
        "bgh host key",
    )?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let pem = key.to_openssh(LineEnding::LF)?;
    let tmp = path.with_extension("tmp");
    {
        use std::io::Write;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        let mut f = opts.open(&tmp)?;
        f.write_all(pem.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(key)
}

/// OpenSSH-style fingerprint (`SHA256:<base64, unpadded>`), the format
/// stored in `ssh_keys.fingerprint` / `deploy_keys.fingerprint`.
pub fn fingerprint(key: &PublicKey) -> String {
    key.fingerprint(HashAlg::Sha256).to_string()
}

/// A deploy key authorized for one repository.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct DeployKey {
    pub id: i64,
    pub repo_id: i64,
    pub read_only: bool,
}

/// Who an SSH connection authenticated as.
#[derive(Debug, Clone)]
pub enum Principal {
    /// A user's key: acts with the user's full permissions (like a session).
    User { ctx: Box<AuthContext>, key_id: i64 },
    /// A deploy key (possibly registered on several repositories).
    Deploy(Vec<DeployKey>),
}

impl Principal {
    pub fn name(&self) -> String {
        match self {
            Self::User { ctx, .. } => ctx.user.login.clone(),
            Self::Deploy(_) => "deploy key".into(),
        }
    }
}

#[derive(sqlx::FromRow)]
struct UserKeyRow {
    key_id: i64,
    #[sqlx(flatten)]
    user: db::User,
}

/// Resolve a public key to a principal (`None` if unknown or suspended).
pub async fn lookup(state: &AppState, key: &PublicKey) -> ApiResult<Option<Principal>> {
    let fp = fingerprint(key);
    let user: Option<UserKeyRow> = sqlx::query_as(&format!(
        "SELECT k.id AS key_id, {} FROM ssh_keys k JOIN users u ON u.id = k.user_id
          WHERE k.fingerprint = $1",
        db::prefixed("u", db::User::COLUMNS)
    ))
    .bind(&fp)
    .fetch_optional(&state.db)
    .await?;
    if let Some(row) = user {
        if row.user.is_suspended() {
            return Ok(None);
        }
        return Ok(Some(Principal::User {
            key_id: row.key_id,
            ctx: Box::new(AuthContext {
                user: row.user,
                method: AuthMethod::Password,
                scopes: None,
            }),
        }));
    }
    let deploy: Vec<DeployKey> = sqlx::query_as(
        "SELECT id, repo_id, read_only FROM deploy_keys WHERE fingerprint = $1 AND verified",
    )
    .bind(&fp)
    .fetch_all(&state.db)
    .await?;
    Ok((!deploy.is_empty()).then_some(Principal::Deploy(deploy)))
}

/// Record key usage (at most once a minute per key).
pub async fn touch(state: &AppState, principal: &Principal, repo_id: i64) {
    let r = match principal {
        Principal::User { key_id, .. } => {
            sqlx::query(
                "UPDATE ssh_keys SET last_used_at = now()
                  WHERE id = $1 AND (last_used_at IS NULL OR last_used_at < now() - interval '1 minute')",
            )
            .bind(key_id)
            .execute(&state.db)
            .await
        }
        Principal::Deploy(keys) => {
            let Some(k) = keys.iter().find(|k| k.repo_id == repo_id) else {
                return;
            };
            sqlx::query(
                "UPDATE deploy_keys SET last_used_at = now()
                  WHERE id = $1 AND (last_used_at IS NULL OR last_used_at < now() - interval '1 minute')",
            )
            .bind(k.id)
            .execute(&state.db)
            .await
        }
    };
    if let Err(err) = r {
        tracing::warn!(?err, "failed to record ssh key usage");
    }
}
