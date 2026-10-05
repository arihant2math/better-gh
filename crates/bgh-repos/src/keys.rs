//! Deploy keys. Admin only.
//!
//! * `GET|POST /repos/{o}/{r}/keys`
//! * `GET|DELETE /repos/{o}/{r}/keys/{id}`
//!
//! Keys are stored normalized (`type base64`, comment stripped) with a
//! `SHA256:<base64>` fingerprint (same format as `ssh_keys.fingerprint`).

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use bgh_core::audit;
use bgh_core::prelude::*;
use bgh_core::time::ts;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/repos/{owner}/{repo}/keys", get(list).post(create))
        .route(
            "/repos/{owner}/{repo}/keys/{id}",
            get(get_one).delete(delete),
        )
}

/// Accepted public key algorithms.
const KEY_TYPES: &[&str] = &[
    "ssh-ed25519",
    "ssh-rsa",
    "ecdsa-sha2-nistp256",
    "ecdsa-sha2-nistp384",
    "ecdsa-sha2-nistp521",
    "sk-ssh-ed25519@openssh.com",
    "sk-ecdsa-sha2-nistp256@openssh.com",
];

/// A validated OpenSSH public key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedKey {
    /// `type base64` (no comment).
    pub normalized: String,
    /// `SHA256:<base64 without padding>` of the key blob.
    pub fingerprint: String,
}

/// Parse an OpenSSH `authorized_keys`-style public key line
/// (`type base64 [comment]`). The blob's embedded type must match.
pub fn parse_public_key(input: &str) -> Option<ParsedKey> {
    let mut parts = input.split_whitespace();
    let ty = parts.next()?;
    let b64 = parts.next()?;
    if !KEY_TYPES.contains(&ty) {
        return None;
    }
    let blob = STANDARD.decode(b64).ok()?;
    let len = u32::from_be_bytes(blob.get(..4)?.try_into().ok()?) as usize;
    let embedded = blob.get(4..4 + len)?;
    if embedded != ty.as_bytes() || blob.len() <= 4 + len {
        return None;
    }
    Some(ParsedKey {
        normalized: format!("{ty} {}", STANDARD.encode(&blob)),
        fingerprint: format!("SHA256:{}", STANDARD_NO_PAD.encode(Sha256::digest(&blob))),
    })
}

#[derive(sqlx::FromRow)]
struct KeyRow {
    id: i64,
    title: String,
    key: String,
    read_only: bool,
    verified: bool,
    created_at: DateTime<Utc>,
    last_used_at: Option<DateTime<Utc>>,
    added_by: Option<String>,
}

const SELECT: &str = "SELECT k.id, k.title, k.key, k.read_only, k.verified, k.created_at,
        k.last_used_at, u.login AS added_by
   FROM deploy_keys k LEFT JOIN users u ON u.id = k.added_by_id";

/// `deploy-key`.
#[derive(Debug, Serialize)]
pub struct DeployKey {
    id: i64,
    key: String,
    url: String,
    title: String,
    verified: bool,
    created_at: Timestamp,
    read_only: bool,
    added_by: Option<String>,
    last_used: Option<Timestamp>,
    enabled: bool,
}

fn render(state: &AppState, access: &RepoAccess, k: KeyRow) -> DeployKey {
    DeployKey {
        url: format!(
            "{}/keys/{}",
            state.urls.repo(&access.owner.login, &access.repo.name),
            k.id
        ),
        id: k.id,
        key: k.key,
        title: k.title,
        verified: k.verified,
        created_at: k.created_at.into(),
        read_only: k.read_only,
        added_by: k.added_by,
        last_used: ts(k.last_used_at),
        enabled: true,
    }
}

async fn admin_access(
    state: &AppState,
    auth: &AuthContext,
    owner: &str,
    repo: &str,
) -> ApiResult<RepoAccess> {
    let access = RepoAccess::load(state, Some(auth), owner, repo).await?;
    access.require(Permission::Admin)?;
    Ok(access)
}

fn target(access: &RepoAccess) -> audit::Target {
    audit::Target::Repo {
        id: access.repo.id,
        org_id: access.owner.is_org().then_some(access.owner.id),
    }
}

/// `GET /repos/{owner}/{repo}/keys`
async fn list(
    State(state): State<AppState>,
    auth: RequireUser,
    p: Pagination,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<Page<DeployKey>> {
    let access = admin_access(&state, &auth, &owner, &repo).await?;
    let rows: Vec<KeyRow> = sqlx::query_as(&format!(
        "{SELECT} WHERE k.repo_id = $1 ORDER BY k.id LIMIT $2 OFFSET $3"
    ))
    .bind(access.repo.id)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    Ok(p.page(rows).map(|k| render(&state, &access, k)))
}

#[derive(Debug, Deserialize)]
struct CreateBody {
    title: Option<String>,
    key: Option<String>,
    read_only: Option<bool>,
}

fn in_use() -> ApiError {
    ApiError::invalid_field(FieldError::custom(
        "PublicKey",
        "key",
        "key is already in use",
    ))
}

/// `POST /repos/{owner}/{repo}/keys` `{title, key, read_only}`
async fn create(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo)): Path<(String, String)>,
    Json(body): Json<CreateBody>,
) -> ApiResult<(StatusCode, Json<DeployKey>)> {
    let access = admin_access(&state, &auth, &owner, &repo).await?;
    let raw = body.key.unwrap_or_default();
    if raw.trim().is_empty() {
        return Err(ApiError::invalid_field(FieldError::missing_field(
            "PublicKey",
            "key",
        )));
    }
    let parsed = parse_public_key(&raw).ok_or_else(|| {
        ApiError::invalid_field(FieldError::custom(
            "PublicKey",
            "key",
            "key is invalid. You must supply a key in OpenSSH public key format",
        ))
    })?;
    let title = body.title.unwrap_or_default().trim().to_string();

    let mut tx = Tx::begin(&state).await?;
    let used: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM deploy_keys WHERE repo_id = $1 AND fingerprint = $2)
             OR EXISTS (SELECT 1 FROM ssh_keys WHERE fingerprint = $2)",
    )
    .bind(access.repo.id)
    .bind(&parsed.fingerprint)
    .fetch_one(&mut *tx)
    .await?;
    if used {
        return Err(in_use());
    }
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO deploy_keys (repo_id, title, key, fingerprint, read_only, added_by_id)
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
    )
    .bind(access.repo.id)
    .bind(&title)
    .bind(&parsed.normalized)
    .bind(&parsed.fingerprint)
    .bind(body.read_only.unwrap_or(true))
    .bind(auth.user.id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| match bgh_core::db::unique_violation(&e).as_deref() {
        Some("deploy_keys_repo_id_fingerprint_key") => in_use(),
        _ => e.into(),
    })?;
    let row: KeyRow = sqlx::query_as(&format!("{SELECT} WHERE k.id = $1"))
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    audit::log(
        &mut *tx,
        Some(&auth.user),
        "public_key.create",
        target(&access),
        json!({ "id": id, "title": title, "fingerprint": parsed.fingerprint,
                "read_only": row.read_only, "deploy_key": true }),
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(render(&state, &access, row))))
}

/// `GET /repos/{owner}/{repo}/keys/{id}`
async fn get_one(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, id)): Path<(String, String, i64)>,
) -> ApiResult<Json<DeployKey>> {
    let access = admin_access(&state, &auth, &owner, &repo).await?;
    let row: KeyRow = sqlx::query_as(&format!("{SELECT} WHERE k.repo_id = $1 AND k.id = $2"))
        .bind(access.repo.id)
        .bind(id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(render(&state, &access, row)))
}

/// `DELETE /repos/{owner}/{repo}/keys/{id}`
async fn delete(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, id)): Path<(String, String, i64)>,
) -> ApiResult<StatusCode> {
    let access = admin_access(&state, &auth, &owner, &repo).await?;
    let mut tx = Tx::begin(&state).await?;
    let fingerprint: String = sqlx::query_scalar(
        "DELETE FROM deploy_keys WHERE repo_id = $1 AND id = $2 RETURNING fingerprint",
    )
    .bind(access.repo.id)
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(ApiError::NotFound)?;
    audit::log(
        &mut *tx,
        Some(&auth.user),
        "public_key.delete",
        target(&access),
        json!({ "id": id, "fingerprint": fingerprint, "deploy_key": true }),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blob(ty: &str, rest: &[u8]) -> String {
        let mut b = (ty.len() as u32).to_be_bytes().to_vec();
        b.extend_from_slice(ty.as_bytes());
        b.extend_from_slice(rest);
        STANDARD.encode(b)
    }

    #[test]
    fn parses_and_normalizes() {
        let b = blob("ssh-ed25519", &[0, 0, 0, 32, 7, 7, 7]);
        let k = parse_public_key(&format!("  ssh-ed25519 {b} me@host\n")).unwrap();
        assert_eq!(k.normalized, format!("ssh-ed25519 {b}"));
        assert!(k.fingerprint.starts_with("SHA256:"));
        assert!(!k.fingerprint.ends_with('='));
    }

    #[test]
    fn rejects_bad_keys() {
        assert!(parse_public_key("").is_none());
        assert!(parse_public_key("ssh-ed25519").is_none());
        assert!(parse_public_key("ssh-ed25519 !!!notbase64").is_none());
        let b = blob("ssh-rsa", &[1, 2, 3]);
        assert!(parse_public_key(&format!("ssh-ed25519 {b}")).is_none());
        let b = blob("ssh-dss", &[1, 2, 3]);
        assert!(parse_public_key(&format!("ssh-dss {b}")).is_none());
        let b = blob("ssh-ed25519", &[]);
        assert!(parse_public_key(&format!("ssh-ed25519 {b}")).is_none());
    }
}
