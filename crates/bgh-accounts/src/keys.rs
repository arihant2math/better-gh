//! SSH and GPG keys.
//!
//! `GET|POST /user/keys`, `GET|DELETE /user/keys/{id}`,
//! `GET /users/{username}/keys`; `GET|POST /user/gpg_keys`,
//! `GET|DELETE /user/gpg_keys/{id}`, `GET /users/{username}/gpg_keys`.

use axum::extract::State;
use axum::http::StatusCode;
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use bgh_core::audit;
use bgh_core::error::unique_violation;
use bgh_core::prelude::*;
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::gpg;
use crate::json::{GpgKey, GpgKeyRow, SshKey, SshKeyRow, SshKeySimple};
use crate::util;

// ---------------------------------------------------------------------------
// SSH public key parsing
// ---------------------------------------------------------------------------

/// A validated OpenSSH public key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshPublicKey {
    pub algorithm: String,
    /// `"{algorithm} {base64}"` without the comment.
    pub normalized: String,
    pub comment: Option<String>,
    /// `SHA256:{base64 without padding}` (same as `ssh-keygen -l`).
    pub fingerprint: String,
}

const ALGORITHMS: &[&str] = &[
    "ssh-ed25519",
    "ssh-rsa",
    "ecdsa-sha2-nistp256",
    "ecdsa-sha2-nistp384",
    "ecdsa-sha2-nistp521",
    "sk-ssh-ed25519@openssh.com",
    "sk-ecdsa-sha2-nistp256@openssh.com",
];

/// Read one SSH wire-format `string` (u32 length + bytes).
fn read_string<'a>(buf: &mut &'a [u8]) -> Option<&'a [u8]> {
    if buf.len() < 4 {
        return None;
    }
    let len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    let rest = &buf[4..];
    if rest.len() < len {
        return None;
    }
    let (s, tail) = rest.split_at(len);
    *buf = tail;
    Some(s)
}

/// Parse and validate an OpenSSH `authorized_keys`-style public key line.
pub fn parse_ssh_key(input: &str) -> Option<SshPublicKey> {
    let mut parts = input.split_whitespace();
    let algorithm = parts.next()?;
    let data = parts.next()?;
    let comment = parts.collect::<Vec<_>>().join(" ");
    if !ALGORITHMS.contains(&algorithm) {
        return None;
    }
    let blob = STANDARD.decode(data).ok()?;
    let mut buf = blob.as_slice();
    if read_string(&mut buf)? != algorithm.as_bytes() {
        return None;
    }
    match algorithm {
        "ssh-ed25519" => {
            if read_string(&mut buf)?.len() != 32 {
                return None;
            }
        }
        "sk-ssh-ed25519@openssh.com" => {
            if read_string(&mut buf)?.len() != 32 {
                return None;
            }
            read_string(&mut buf)?; // application
        }
        "ssh-rsa" => {
            let e = read_string(&mut buf)?;
            let n = read_string(&mut buf)?;
            if e.is_empty() {
                return None;
            }
            // Modulus bits (mpint may carry a leading zero byte).
            let n = &n[n.iter().position(|b| *b != 0)?..];
            let bits = n.len() * 8 - n[0].leading_zeros() as usize;
            if bits < 1024 {
                return None;
            }
        }
        _ => {
            // ecdsa (and sk-ecdsa): curve name + point (+ application).
            let curve = read_string(&mut buf)?;
            let expected = algorithm
                .trim_start_matches("sk-")
                .trim_end_matches("@openssh.com")
                .trim_start_matches("ecdsa-sha2-");
            if curve != expected.as_bytes() {
                return None;
            }
            let point = read_string(&mut buf)?;
            if point.first() != Some(&4) {
                return None;
            }
            if algorithm.starts_with("sk-") {
                read_string(&mut buf)?;
            }
        }
    }
    if !buf.is_empty() {
        return None;
    }
    Some(SshPublicKey {
        algorithm: algorithm.to_string(),
        normalized: format!("{algorithm} {data}"),
        comment: (!comment.is_empty()).then_some(comment),
        fingerprint: format!("SHA256:{}", STANDARD_NO_PAD.encode(Sha256::digest(&blob))),
    })
}

// ---------------------------------------------------------------------------
// SSH key endpoints
// ---------------------------------------------------------------------------

/// `GET /user/keys` (scope `read:public_key`).
pub async fn list_ssh(
    State(state): State<AppState>,
    auth: RequireUser,
    p: Pagination,
) -> ApiResult<Page<SshKey>> {
    auth.require_scope("read:public_key")?;
    let rows: Vec<SshKeyRow> = sqlx::query_as(&format!(
        "SELECT {} FROM ssh_keys WHERE user_id = $1 ORDER BY id LIMIT $2 OFFSET $3",
        SshKeyRow::COLUMNS
    ))
    .bind(auth.user.id)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    Ok(p.page(rows).map(|k| SshKey::new(&state.urls, &k)))
}

async fn find_ssh(state: &AppState, user_id: i64, id: i64) -> ApiResult<SshKeyRow> {
    sqlx::query_as(&format!(
        "SELECT {} FROM ssh_keys WHERE id = $1 AND user_id = $2",
        SshKeyRow::COLUMNS
    ))
    .bind(id)
    .bind(user_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)
}

/// `GET /user/keys/{key_id}` (scope `read:public_key`).
pub async fn get_ssh(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(id): Path<i64>,
) -> ApiResult<Json<SshKey>> {
    auth.require_scope("read:public_key")?;
    let row = find_ssh(&state, auth.user.id, id).await?;
    Ok(Json(SshKey::new(&state.urls, &row)))
}

#[derive(Debug, Deserialize)]
pub struct CreateSshKeyBody {
    pub title: Option<String>,
    #[serde(default)]
    pub key: String,
}

fn key_error(message: &str) -> ApiError {
    ApiError::invalid_field(FieldError::custom("PublicKey", "key", message))
}

/// `POST /user/keys` (scope `write:public_key`) → 201 key.
pub async fn create_ssh(
    State(state): State<AppState>,
    auth: RequireUser,
    Json(body): Json<CreateSshKeyBody>,
) -> ApiResult<(StatusCode, Json<SshKey>)> {
    auth.require_scope("write:public_key")?;
    bgh_core::sudo::require(&state, &auth).await?;
    if body.key.trim().is_empty() {
        return Err(ApiError::invalid_field(FieldError::missing_field(
            "PublicKey",
            "key",
        )));
    }
    let key = parse_ssh_key(&body.key).ok_or_else(|| {
        key_error("key is invalid. You must supply a key in OpenSSH public key format")
    })?;
    let title = util::non_empty(body.title)
        .or_else(|| key.comment.clone())
        .unwrap_or_default();
    let mut tx = Tx::begin(&state).await?;
    let used_by_deploy_key: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM deploy_keys WHERE fingerprint = $1)")
            .bind(&key.fingerprint)
            .fetch_one(&mut *tx)
            .await?;
    if used_by_deploy_key {
        return Err(key_error("key is already in use"));
    }
    let row: SshKeyRow = sqlx::query_as(&format!(
        "INSERT INTO ssh_keys (user_id, title, key, fingerprint) VALUES ($1, $2, $3, $4)
         RETURNING {}",
        SshKeyRow::COLUMNS
    ))
    .bind(auth.user.id)
    .bind(&title)
    .bind(&key.normalized)
    .bind(&key.fingerprint)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| match unique_violation(&e).as_deref() {
        Some("ssh_keys_fingerprint_key") => key_error("key is already in use"),
        _ => e.into(),
    })?;
    audit::log(
        &mut *tx,
        Some(&auth.user),
        "public_key.create",
        audit::Target::User(auth.user.id),
        json!({ "title": title, "fingerprint": key.fingerprint }),
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(SshKey::new(&state.urls, &row))))
}

/// `DELETE /user/keys/{key_id}` (scope `admin:public_key`) → 204.
pub async fn delete_ssh(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    auth.require_scope("admin:public_key")?;
    let mut tx = Tx::begin(&state).await?;
    let fingerprint: String = sqlx::query_scalar(
        "DELETE FROM ssh_keys WHERE id = $1 AND user_id = $2 RETURNING fingerprint",
    )
    .bind(id)
    .bind(auth.user.id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(ApiError::NotFound)?;
    audit::log(
        &mut *tx,
        Some(&auth.user),
        "public_key.delete",
        audit::Target::User(auth.user.id),
        json!({ "fingerprint": fingerprint }),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /users/{username}/keys` → key-simple list (public).
pub async fn list_user_ssh(
    State(state): State<AppState>,
    _auth: MaybeUser,
    Path(username): Path<String>,
    p: Pagination,
) -> ApiResult<Page<SshKeySimple>> {
    let user = util::find_account(&state, &username).await?;
    let rows: Vec<SshKeyRow> = sqlx::query_as(&format!(
        "SELECT {} FROM ssh_keys WHERE user_id = $1 ORDER BY id LIMIT $2 OFFSET $3",
        SshKeyRow::COLUMNS
    ))
    .bind(user.id)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    Ok(p.page(rows).map(|k| SshKeySimple {
        id: k.id,
        key: k.key,
        created_at: k.created_at.into(),
        last_used: bgh_core::time::ts(k.last_used_at),
    }))
}

// ---------------------------------------------------------------------------
// GPG keys
// ---------------------------------------------------------------------------

/// Primary keys of `user_id` (paginated) with their subkeys.
async fn gpg_page(state: &AppState, user_id: i64, p: &Pagination) -> ApiResult<Page<GpgKey>> {
    let primaries: Vec<GpgKeyRow> = sqlx::query_as(&format!(
        "SELECT {} FROM gpg_keys WHERE user_id = $1 AND primary_key_id IS NULL
          ORDER BY id LIMIT $2 OFFSET $3",
        GpgKeyRow::COLUMNS
    ))
    .bind(user_id)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let ids: Vec<i64> = primaries.iter().map(|k| k.id).collect();
    let subkeys: Vec<GpgKeyRow> = sqlx::query_as(&format!(
        "SELECT {} FROM gpg_keys WHERE primary_key_id = ANY($1) ORDER BY id",
        GpgKeyRow::COLUMNS
    ))
    .bind(&ids)
    .fetch_all(&state.db)
    .await?;
    let page = p.page(primaries);
    Ok(page.map(|k| {
        let subs: Vec<&GpgKeyRow> = subkeys
            .iter()
            .filter(|s| s.primary_key_id == Some(k.id))
            .collect();
        GpgKey::new(&k, &subs)
    }))
}

/// `GET /user/gpg_keys` (scope `read:gpg_key`).
pub async fn list_gpg(
    State(state): State<AppState>,
    auth: RequireUser,
    p: Pagination,
) -> ApiResult<Page<GpgKey>> {
    auth.require_scope("read:gpg_key")?;
    gpg_page(&state, auth.user.id, &p).await
}

/// `GET /users/{username}/gpg_keys` (public).
pub async fn list_user_gpg(
    State(state): State<AppState>,
    _auth: MaybeUser,
    Path(username): Path<String>,
    p: Pagination,
) -> ApiResult<Page<GpgKey>> {
    let user = util::find_account(&state, &username).await?;
    gpg_page(&state, user.id, &p).await
}

async fn load_gpg(state: &AppState, user_id: i64, id: i64) -> ApiResult<GpgKey> {
    let rows: Vec<GpgKeyRow> = sqlx::query_as(&format!(
        "SELECT {} FROM gpg_keys WHERE user_id = $1 AND (id = $2 OR primary_key_id = $2)
          ORDER BY id",
        GpgKeyRow::COLUMNS
    ))
    .bind(user_id)
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    GpgKey::group(&rows)
        .into_iter()
        .find(|k| k.id == id)
        .ok_or(ApiError::NotFound)
}

/// `GET /user/gpg_keys/{gpg_key_id}` (scope `read:gpg_key`).
pub async fn get_gpg(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(id): Path<i64>,
) -> ApiResult<Json<GpgKey>> {
    auth.require_scope("read:gpg_key")?;
    Ok(Json(load_gpg(&state, auth.user.id, id).await?))
}

#[derive(Debug, Deserialize)]
pub struct CreateGpgKeyBody {
    pub name: Option<String>,
    #[serde(default)]
    pub armored_public_key: String,
}

/// `POST /user/gpg_keys` (scope `write:gpg_key`) → 201 gpg-key.
pub async fn create_gpg(
    State(state): State<AppState>,
    auth: RequireUser,
    Json(body): Json<CreateGpgKeyBody>,
) -> ApiResult<(StatusCode, Json<GpgKey>)> {
    auth.require_scope("write:gpg_key")?;
    bgh_core::sudo::require(&state, &auth).await?;
    if body.armored_public_key.trim().is_empty() {
        return Err(ApiError::invalid_field(FieldError::missing_field(
            "GpgKey",
            "armored_public_key",
        )));
    }
    let parsed = gpg::parse_armored(&body.armored_public_key).map_err(|e| {
        ApiError::invalid_field(FieldError::custom(
            "GpgKey",
            "armored_public_key",
            format!("We got an error doing that: {e}"),
        ))
    })?;
    let verified: Vec<String> =
        sqlx::query_scalar("SELECT lower(email) FROM user_emails WHERE user_id = $1 AND verified")
            .bind(auth.user.id)
            .fetch_all(&state.db)
            .await?;
    let emails: Vec<serde_json::Value> = parsed
        .emails
        .iter()
        .map(|e| json!({ "email": e, "verified": verified.contains(&e.to_lowercase()) }))
        .collect();
    let mut tx = Tx::begin(&state).await?;
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM gpg_keys WHERE user_id = $1 AND key_id = $2)",
    )
    .bind(auth.user.id)
    .bind(&parsed.key_id)
    .fetch_one(&mut *tx)
    .await?;
    if exists {
        return Err(ApiError::invalid_field(FieldError::custom(
            "GpgKey",
            "key_id",
            "key_id already exists",
        )));
    }
    let name = util::non_empty(body.name);
    let primary_id: i64 = sqlx::query_scalar(
        "INSERT INTO gpg_keys (user_id, name, key_id, public_key, raw_key, emails, can_sign,
                               can_encrypt_comms, can_encrypt_storage, can_certify, expires_at,
                               created_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, now()) RETURNING id",
    )
    .bind(auth.user.id)
    .bind(&name)
    .bind(&parsed.key_id)
    .bind(&parsed.public_key)
    .bind(body.armored_public_key.trim())
    .bind(serde_json::Value::Array(emails))
    .bind(parsed.can_sign)
    .bind(parsed.can_encrypt_comms)
    .bind(parsed.can_encrypt_storage)
    .bind(parsed.can_certify)
    .bind(parsed.expires_at)
    .fetch_one(&mut *tx)
    .await?;
    for sub in &parsed.subkeys {
        sqlx::query(
            "INSERT INTO gpg_keys (user_id, key_id, primary_key_id, public_key, can_sign,
                                   can_encrypt_comms, can_encrypt_storage, can_certify, expires_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(auth.user.id)
        .bind(&sub.key_id)
        .bind(primary_id)
        .bind(&sub.public_key)
        .bind(sub.can_sign)
        .bind(sub.can_encrypt_comms)
        .bind(sub.can_encrypt_storage)
        .bind(sub.can_certify)
        .bind(sub.expires_at)
        .execute(&mut *tx)
        .await?;
    }
    audit::log(
        &mut *tx,
        Some(&auth.user),
        "gpg_key.create",
        audit::Target::User(auth.user.id),
        json!({ "key_id": parsed.key_id }),
    )
    .await?;
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        Json(load_gpg(&state, auth.user.id, primary_id).await?),
    ))
}

/// `DELETE /user/gpg_keys/{gpg_key_id}` (scope `admin:gpg_key`) → 204.
pub async fn delete_gpg(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    auth.require_scope("admin:gpg_key")?;
    let mut tx = Tx::begin(&state).await?;
    let key_id: String = sqlx::query_scalar(
        "DELETE FROM gpg_keys WHERE id = $1 AND user_id = $2 AND primary_key_id IS NULL
         RETURNING key_id",
    )
    .bind(id)
    .bind(auth.user.id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(ApiError::NotFound)?;
    audit::log(
        &mut *tx,
        Some(&auth.user),
        "gpg_key.delete",
        audit::Target::User(auth.user.id),
        json!({ "key_id": key_id }),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ED25519: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl ada@example";

    #[test]
    fn parses_ed25519() {
        let k = parse_ssh_key(ED25519).unwrap();
        assert_eq!(k.algorithm, "ssh-ed25519");
        assert_eq!(k.comment.as_deref(), Some("ada@example"));
        assert!(k.fingerprint.starts_with("SHA256:"));
        assert!(!k.normalized.contains("ada@"));
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_ssh_key("").is_none());
        assert!(parse_ssh_key("ssh-ed25519 notbase64!").is_none());
        assert!(parse_ssh_key("ssh-dss AAAAB3NzaC1kc3MAAACBAP").is_none());
        // Algorithm mismatch between the label and the blob.
        assert!(
            parse_ssh_key(
                "ssh-rsa AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl"
            )
            .is_none()
        );
    }
}
