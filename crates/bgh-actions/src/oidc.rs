//! OpenID Connect id-tokens for workflows (`permissions: id-token: write`),
//! the keyless cloud authentication of `aws-actions/configure-aws-credentials`,
//! `google-github-actions/auth`, `azure/login` and `@actions/core`'s
//! `getIDToken()`.
//!
//! * Issuer `{base_url}/_services/token` (the GHES convention), with
//!   `/.well-known/openid-configuration` and `/.well-known/jwks` below it.
//! * RS256 signing keys live in `{data_dir}/actions/oidc/` as PKCS#1 PEM
//!   files named `{created_unix}-{kid}.pem`; the newest one signs. Keys are
//!   rotated every [`ROTATE_AFTER_DAYS`] days (maintenance loop) or on
//!   demand (`POST /_bgh/admin/actions/oidc/rotate-key`); the previous key
//!   stays in the JWKS until the one after it is replaced.
//! * A job whose `GITHUB_TOKEN` has `id_token: write` gets
//!   `ACTIONS_ID_TOKEN_REQUEST_URL` (`{issuer}/idtoken?api-version=2.0`) and
//!   `ACTIONS_ID_TOKEN_REQUEST_TOKEN` (its `GITHUB_TOKEN`). `GET` (or
//!   `POST`) on the URL with `Authorization: Bearer <token>` and an
//!   optional `&audience=` answers `{"value": "<jwt>"}` while the job runs.
//! * Claims follow GitHub's documented shape; `sub` follows the template
//!   of `/repos/{o}/{r}/actions/oidc/customization/sub` (or the
//!   organization's).

use std::collections::HashMap;
use std::path::{Path as FsPath, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::Context;
use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use bgh_core::models::db;
use bgh_core::node_id::{self, NodeType};
use bgh_core::prelude::*;
use bgh_core::token_permissions::{Access, Category, TokenPermissions};
use rsa::pkcs1::{DecodeRsaPrivateKey, EncodeRsaPrivateKey};
use rsa::pkcs1v15::SigningKey;
use rsa::pkcs8::{EncodePublicKey, LineEnding};
use rsa::signature::{SignatureEncoding, Signer};
use rsa::traits::PublicKeyParts;
use rsa::{RsaPrivateKey, RsaPublicKey};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::engine::StoredJob;
use crate::models::JobRow;

/// Path of the issuer below the base URL.
pub const ISSUER_PATH: &str = "/_services/token";
/// Lifetime of an id-token.
pub const TOKEN_LIFETIME_SECS: i64 = 300;
/// Age after which the maintenance loop rotates the signing key.
pub const ROTATE_AFTER_DAYS: i64 = 90;

/// Claims of every token, in the order they are emitted (also the
/// discovery document's `claims_supported`).
pub const CLAIMS: &[&str] = &[
    "jti",
    "sub",
    "aud",
    "ref",
    "sha",
    "repository",
    "repository_owner",
    "repository_owner_id",
    "run_id",
    "run_number",
    "run_attempt",
    "repository_visibility",
    "repository_id",
    "actor_id",
    "actor",
    "workflow",
    "head_ref",
    "base_ref",
    "event_name",
    "ref_protected",
    "ref_type",
    "workflow_ref",
    "workflow_sha",
    "job_workflow_ref",
    "job_workflow_sha",
    "runner_environment",
    "environment",
    "environment_node_id",
    "check_run_id",
    "iss",
    "nbf",
    "exp",
    "iat",
];

/// Claims that can't appear in a `sub` template.
const NOT_IN_TEMPLATE: &[&str] = &["jti", "sub", "aud", "iss", "nbf", "exp", "iat"];

/// GitHub's default `sub` template.
pub const DEFAULT_TEMPLATE: &[&str] = &["repo", "context"];

pub fn issuer(state: &AppState) -> String {
    state.urls.html(ISSUER_PATH)
}

/// `ACTIONS_ID_TOKEN_REQUEST_URL` (the toolkit appends `&audience=`).
pub fn request_url(state: &AppState) -> String {
    format!("{}/idtoken?api-version=2.0", issuer(state))
}

/// Whether a job token with `permissions` may request id-tokens.
pub fn allowed(permissions: &TokenPermissions) -> bool {
    permissions.get(Category::IdToken) == Access::Write
}

// ---------------------------------------------------------------------------
// Signing keys
// ---------------------------------------------------------------------------

/// One RS256 signing key.
pub struct OidcKey {
    pub kid: String,
    /// Unix seconds.
    pub created: i64,
    key: RsaPrivateKey,
    file: PathBuf,
}

impl OidcKey {
    /// The key's JWK (public part).
    pub fn jwk(&self) -> Value {
        let public = RsaPublicKey::from(&self.key);
        json!({
            "kty": "RSA",
            "alg": "RS256",
            "use": "sig",
            "kid": self.kid,
            "n": URL_SAFE_NO_PAD.encode(public.n().to_bytes_be()),
            "e": URL_SAFE_NO_PAD.encode(public.e().to_bytes_be()),
        })
    }

    /// Sign `claims` as a compact JWS.
    pub fn sign(&self, claims: &Value) -> anyhow::Result<String> {
        let head = URL_SAFE_NO_PAD.encode(serde_json::to_vec(
            &json!({"typ": "JWT", "alg": "RS256", "kid": self.kid}),
        )?);
        let body = URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims)?);
        let input = format!("{head}.{body}");
        let sig = SigningKey::<Sha256>::new(self.key.clone()).sign(input.as_bytes());
        Ok(format!(
            "{input}.{}",
            URL_SAFE_NO_PAD.encode(sig.to_bytes())
        ))
    }
}

/// The site's signing keys, oldest first; the last one signs.
pub type KeySet = Arc<Vec<Arc<OidcKey>>>;

fn key_dir(state: &AppState) -> PathBuf {
    state.config.data_dir.join("actions").join("oidc")
}

/// `*.pem` file names in `dir`, sorted (= by creation time).
fn list(dir: &FsPath) -> std::io::Result<Vec<String>> {
    let mut names = Vec::new();
    match std::fs::read_dir(dir) {
        Ok(entries) => {
            for e in entries {
                let name = e?.file_name().to_string_lossy().into_owned();
                if name.ends_with(".pem") && !name.starts_with('.') {
                    names.push(name);
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    names.sort();
    Ok(names)
}

fn read_key(dir: &FsPath, name: &str) -> anyhow::Result<OidcKey> {
    let file = dir.join(name);
    let (created, kid) = name
        .trim_end_matches(".pem")
        .split_once('-')
        .with_context(|| format!("unexpected key file name {name}"))?;
    let pem = std::fs::read_to_string(&file)?;
    let key = RsaPrivateKey::from_pkcs1_pem(&pem)
        .with_context(|| format!("invalid key in {}", file.display()))?;
    Ok(OidcKey {
        kid: kid.to_string(),
        created: created.parse().context("key file timestamp")?,
        key,
        file,
    })
}

/// Generate a key and write it to `dir` (blocking).
fn generate(dir: &FsPath) -> anyhow::Result<String> {
    let mut rng = rsa::rand_core::OsRng;
    let key = RsaPrivateKey::new(&mut rng, 2048)?;
    let der = RsaPublicKey::from(&key).to_public_key_der()?;
    let kid = hex::encode(&Sha256::digest(der.as_bytes())[..16]);
    // Strictly after the newest existing key, so name order = age order
    // even for rotations within one second.
    let newest = list(dir)?
        .iter()
        .filter_map(|n| n.split_once('-')?.0.parse::<i64>().ok())
        .max()
        .unwrap_or(0);
    let now = chrono::Utc::now().timestamp().max(newest + 1);
    std::fs::create_dir_all(dir)?;
    let name = format!("{now:012}-{kid}.pem");
    crate::crypto::write_private(
        &dir.join(&name),
        key.to_pkcs1_pem(LineEnding::LF)?.as_bytes(),
    )?;
    Ok(kid)
}

/// Serializes key generation within the process.
fn gen_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(Default::default)
}

/// The signing keys (generating the first one when there is none).
/// Parsed keys are cached per data dir until the directory listing changes
/// (a rotation by another process).
pub async fn keys(state: &AppState) -> anyhow::Result<KeySet> {
    type Cache = Mutex<HashMap<PathBuf, (Vec<String>, KeySet)>>;
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let dir = key_dir(state);
    let mut names = list(&dir)?;
    if names.is_empty() {
        let _guard = gen_lock().lock().await;
        names = list(&dir)?;
        if names.is_empty() {
            let d = dir.clone();
            tokio::task::spawn_blocking(move || generate(&d)).await??;
            names = list(&dir)?;
        }
    }
    let cache = CACHE.get_or_init(Default::default);
    if let Some((cached, set)) = cache.lock().expect("oidc key cache").get(&dir)
        && *cached == names
    {
        return Ok(set.clone());
    }
    let d = dir.clone();
    let n = names.clone();
    let set: KeySet = tokio::task::spawn_blocking(move || {
        n.iter()
            .map(|name| read_key(&d, name).map(Arc::new))
            .collect::<anyhow::Result<Vec<_>>>()
    })
    .await??
    .into();
    cache
        .lock()
        .expect("oidc key cache")
        .insert(dir, (names, set.clone()));
    Ok(set)
}

/// The current signing key.
pub async fn current_key(state: &AppState) -> anyhow::Result<Arc<OidcKey>> {
    keys(state)
        .await?
        .last()
        .cloned()
        .context("no OIDC signing key")
}

/// Rotate the signing key when `force` or when the current one is older
/// than [`ROTATE_AFTER_DAYS`]; returns the new key id. Keys older than the
/// previous one are removed once their successor is an hour old (no token
/// they signed can still be valid).
pub async fn rotate(state: &AppState, force: bool) -> anyhow::Result<Option<String>> {
    if !force && list(&key_dir(state))?.is_empty() {
        // No key yet: the first token request creates one.
        return Ok(None);
    }
    let current = current_key(state).await?;
    let now = chrono::Utc::now().timestamp();
    if !force && now - current.created < ROTATE_AFTER_DAYS * 86_400 {
        return Ok(None);
    }
    let dir = key_dir(state);
    let kid = {
        let _guard = gen_lock().lock().await;
        // Another task may have rotated meanwhile.
        let latest = keys(state).await?;
        match latest.last() {
            Some(k) if !force && k.kid != current.kid => return Ok(None),
            _ => {}
        }
        let d = dir.clone();
        tokio::task::spawn_blocking(move || generate(&d)).await??
    };
    let set = keys(state).await?;
    let keep_from = set.len().saturating_sub(2);
    for (i, k) in set.iter().enumerate().take(keep_from) {
        if set[i + 1].created < now - 3600
            && let Err(err) = std::fs::remove_file(&k.file)
        {
            tracing::warn!(?err, kid = %k.kid, "removing old OIDC key failed");
        }
    }
    tracing::info!(%kid, "rotated Actions OIDC signing key");
    Ok(Some(kid))
}

// ---------------------------------------------------------------------------
// Claims
// ---------------------------------------------------------------------------

/// The `sub` template for `repo` (repository setting, else organization,
/// else [`DEFAULT_TEMPLATE`]).
pub async fn sub_template(
    db: impl sqlx::PgExecutor<'_> + Copy,
    repo: &db::Repository,
) -> anyhow::Result<Vec<String>> {
    let row: Option<(bool, Vec<String>)> = sqlx::query_as(
        "SELECT use_default, include_claim_keys FROM actions_oidc_sub_claims WHERE repo_id = $1",
    )
    .bind(repo.id)
    .fetch_optional(db)
    .await?;
    let default = || DEFAULT_TEMPLATE.iter().map(|s| s.to_string()).collect();
    match row {
        None | Some((true, _)) => Ok(default()),
        Some((false, keys)) if !keys.is_empty() => Ok(keys),
        Some((false, _)) => {
            let org: Option<Vec<String>> = sqlx::query_scalar(
                "SELECT include_claim_keys FROM actions_oidc_sub_claims WHERE org_id = $1",
            )
            .bind(repo.owner_id)
            .fetch_optional(db)
            .await?;
            Ok(org.filter(|k| !k.is_empty()).unwrap_or_else(default))
        }
    }
}

/// `sub` from a template: `repo:o/r`, the context (`environment:prod`,
/// `pull_request` or `ref:refs/heads/main`), then `key:value` of claims.
pub fn render_sub(template: &[String], claims: &Map<String, Value>) -> String {
    let s = |k: &str| match claims.get(k) {
        Some(Value::String(v)) => v.clone(),
        Some(Value::Null) | None => String::new(),
        Some(v) => v.to_string(),
    };
    let context = if claims.contains_key("environment") {
        format!("environment:{}", s("environment"))
    } else if s("event_name") == "pull_request" {
        "pull_request".to_string()
    } else {
        format!("ref:{}", s("ref"))
    };
    template
        .iter()
        .map(|k| match k.as_str() {
            "repo" => format!("repo:{}", s("repository")),
            "context" => context.clone(),
            other => format!("{other}:{}", s(other)),
        })
        .collect::<Vec<_>>()
        .join(":")
}

/// The claims of an id-token for running `job` (without `sub`, `aud` and
/// the time claims).
pub async fn job_claims(state: &AppState, job: &JobRow) -> anyhow::Result<Map<String, Value>> {
    let stored: StoredJob = serde_json::from_value(job.spec.clone().context("job has no spec")?)?;
    let spec = &stored.spec;
    let g = &spec.github;
    let repo = db::Repository::find(&state.db, job.repo_id)
        .await?
        .context("repository deleted")?;
    let gs = |k: &str| match g.get(k) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(v) => v.to_string(),
    };
    let workflow_ref = gs("workflow_ref");
    let job_workflow_ref = Some(gs("job_workflow_ref"))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| workflow_ref.clone());
    let job_workflow_sha = Some(gs("job_workflow_sha"))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| gs("workflow_sha"));
    let mut c = Map::new();
    let mut put = |k: &str, v: String| {
        c.insert(k.to_string(), Value::String(v));
    };
    put("ref", gs("ref"));
    put("sha", gs("sha"));
    put("repository", gs("repository"));
    put("repository_owner", gs("repository_owner"));
    put("repository_owner_id", gs("repository_owner_id"));
    put("run_id", gs("run_id"));
    put("run_number", gs("run_number"));
    put("run_attempt", job.run_attempt.to_string());
    put("repository_visibility", repo.visibility.clone());
    put("repository_id", repo.id.to_string());
    put("actor_id", gs("actor_id"));
    put("actor", gs("actor"));
    put("workflow", gs("workflow"));
    put("head_ref", gs("head_ref"));
    put("base_ref", gs("base_ref"));
    put("event_name", gs("event_name"));
    put(
        "ref_protected",
        if gs("ref_protected") == "true" {
            "true"
        } else {
            "false"
        }
        .into(),
    );
    put("ref_type", gs("ref_type"));
    put("workflow_ref", workflow_ref);
    put("workflow_sha", gs("workflow_sha"));
    put("job_workflow_ref", job_workflow_ref);
    put("job_workflow_sha", job_workflow_sha);
    put("runner_environment", "self-hosted".into());
    if let Some(env) = spec.environment.as_deref().filter(|e| !e.is_empty()) {
        put("environment", env.to_string());
        let env_id: Option<i64> = sqlx::query_scalar(
            "SELECT id FROM actions_environments WHERE repo_id = $1 AND lower(name) = lower($2)",
        )
        .bind(repo.id)
        .bind(env)
        .fetch_optional(&state.db)
        .await?;
        if let Some(id) = env_id {
            put(
                "environment_node_id",
                node_id::encode(NodeType::Environment, id),
            );
        }
    }
    if let Some(id) = job.check_run_id {
        put("check_run_id", id.to_string());
    }
    Ok(c)
}

/// A signed id-token for `job` with audience `audience` (default: the
/// repository owner's URL, like GitHub).
pub async fn mint(
    state: &AppState,
    job: &JobRow,
    audience: Option<&str>,
) -> anyhow::Result<String> {
    let base = job_claims(state, job).await?;
    let repo = db::Repository::find(&state.db, job.repo_id)
        .await?
        .context("repository deleted")?;
    let template = sub_template(&state.db, &repo).await?;
    let sub = render_sub(&template, &base);
    let aud = match audience.filter(|a| !a.is_empty()) {
        Some(a) => a.to_string(),
        None => state.urls.html(&format!(
            "/{}",
            base["repository_owner"].as_str().unwrap_or_default()
        )),
    };
    let now = chrono::Utc::now().timestamp();
    let mut claims = Map::new();
    claims.insert("jti".into(), json!(uuid::Uuid::new_v4().to_string()));
    claims.insert("sub".into(), json!(sub));
    claims.insert("aud".into(), json!(aud));
    claims.extend(base);
    claims.insert("iss".into(), json!(issuer(state)));
    claims.insert("nbf".into(), json!(now - 5));
    claims.insert("exp".into(), json!(now + TOKEN_LIFETIME_SECS));
    claims.insert("iat".into(), json!(now));
    current_key(state).await?.sign(&Value::Object(claims))
}

// ---------------------------------------------------------------------------
// HTTP: discovery, JWKS, token requests, key rotation
// ---------------------------------------------------------------------------

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/_services/token/.well-known/openid-configuration",
            get(discovery),
        )
        .route("/_services/token/.well-known/jwks", get(jwks))
        .route(
            "/_services/token/idtoken",
            get(request_token).post(request_token),
        )
        .route("/_bgh/admin/actions/oidc/rotate-key", post(rotate_key))
}

fn cacheable(body: Value) -> Response {
    let mut res = Json(body).into_response();
    res.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=300"),
    );
    res
}

async fn discovery(State(state): State<AppState>) -> Response {
    let iss = issuer(&state);
    cacheable(json!({
        "issuer": iss,
        "jwks_uri": format!("{iss}/.well-known/jwks"),
        "subject_types_supported": ["public", "pairwise"],
        "response_types_supported": ["id_token"],
        "claims_supported": CLAIMS,
        "id_token_signing_alg_values_supported": ["RS256"],
        "scopes_supported": ["openid"],
    }))
}

async fn jwks(State(state): State<AppState>) -> ApiResult<Response> {
    let set = keys(&state).await.map_err(ApiError::internal)?;
    // Newest first, like most providers.
    let keys: Vec<Value> = set.iter().rev().map(|k| k.jwk()).collect();
    Ok(cacheable(json!({ "keys": keys })))
}

#[derive(Deserialize)]
struct TokenQuery {
    audience: Option<String>,
}

/// The running job of the `GITHUB_TOKEN` in `Authorization`, with the
/// token's permissions.
async fn job_for_token(
    state: &AppState,
    headers: &HeaderMap,
) -> ApiResult<(JobRow, TokenPermissions)> {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split_once(' '))
        .filter(|(scheme, _)| {
            scheme.eq_ignore_ascii_case("bearer") || scheme.eq_ignore_ascii_case("token")
        })
        .map(|(_, t)| t.trim())
        .filter(|t| !t.is_empty())
        .ok_or_else(ApiError::requires_auth)?;
    let row: Option<(i64, Option<Value>)> = sqlx::query_as(
        "SELECT id, permissions FROM access_tokens
          WHERE token_hash = $1 AND (expires_at IS NULL OR expires_at > now())",
    )
    .bind(bgh_core::crypto::sha256_hex(token))
    .fetch_optional(&state.db)
    .await?;
    let Some((token_id, permissions)) = row else {
        return Err(ApiError::requires_auth());
    };
    let job: Option<JobRow> = sqlx::query_as(&format!(
        "SELECT {} FROM actions_jobs WHERE token_id = $1 AND status = 'in_progress'",
        JobRow::COLUMNS
    ))
    .bind(token_id)
    .fetch_optional(&state.db)
    .await?;
    let job = job.ok_or_else(ApiError::requires_auth)?;
    let permissions: TokenPermissions = permissions
        .and_then(|p| serde_json::from_value(p).ok())
        .unwrap_or_default();
    Ok((job, permissions))
}

async fn request_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
) -> ApiResult<Json<Value>> {
    let (job, permissions) = job_for_token(&state, &headers).await?;
    if !allowed(&permissions) {
        return Err(ApiError::forbidden(
            "The job's GITHUB_TOKEN lacks the id-token: write permission.",
        ));
    }
    let token = mint(&state, &job, q.audience.as_deref())
        .await
        .map_err(ApiError::internal)?;
    tracing::info!(
        job_id = job.id,
        repo_id = job.repo_id,
        "issued Actions OIDC token"
    );
    Ok(Json(json!({ "value": token })))
}

async fn rotate_key(
    State(state): State<AppState>,
    admin: RequireSiteAdmin,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let kid = rotate(&state, true)
        .await
        .map_err(ApiError::internal)?
        .unwrap_or_default();
    bgh_core::audit::log(
        &state.db,
        Some(&admin.user),
        "actions.oidc_key_rotate",
        bgh_core::audit::Target::Site,
        json!({ "kid": kid }),
    )
    .await?;
    let set = keys(&state).await.map_err(ApiError::internal)?;
    let kids: Vec<&str> = set.iter().rev().map(|k| k.kid.as_str()).collect();
    Ok((
        StatusCode::CREATED,
        Json(json!({ "kid": kid, "keys": kids })),
    ))
}

// ---------------------------------------------------------------------------
// REST: /actions/oidc/customization/sub
// ---------------------------------------------------------------------------

/// Validate template keys (422 on unknown claims).
fn validate_keys(keys: &[String]) -> ApiResult<()> {
    for k in keys {
        let ok = k == "repo"
            || k == "context"
            || (CLAIMS.contains(&k.as_str()) && !NOT_IN_TEMPLATE.contains(&k.as_str()));
        if !ok {
            return Err(ApiError::unprocessable(format!(
                "Invalid include_claim_keys entry '{k}'"
            )));
        }
    }
    Ok(())
}

pub mod api {
    use super::*;
    use crate::api::{load_org, require_org_admin};

    pub async fn repo_get(
        State(state): State<AppState>,
        auth: MaybeUser,
        Path((owner, repo)): Path<(String, String)>,
    ) -> ApiResult<Json<Value>> {
        let a = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
        let row: Option<(bool, Vec<String>)> = sqlx::query_as(
            "SELECT use_default, include_claim_keys FROM actions_oidc_sub_claims WHERE repo_id = $1",
        )
        .bind(a.repo.id)
        .fetch_optional(&state.db)
        .await?;
        Ok(Json(match row {
            Some((false, keys)) => json!({"use_default": false, "include_claim_keys": keys}),
            _ => json!({"use_default": true}),
        }))
    }

    #[derive(Deserialize)]
    pub struct RepoBody {
        use_default: Option<bool>,
        include_claim_keys: Option<Vec<String>>,
    }

    pub async fn repo_put(
        State(state): State<AppState>,
        auth: RequireUser,
        Path((owner, repo)): Path<(String, String)>,
        Json(body): Json<RepoBody>,
    ) -> ApiResult<(StatusCode, Json<Value>)> {
        let a = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
        a.require(Permission::Admin)?;
        let use_default = body.use_default.ok_or_else(|| {
            ApiError::invalid_field(FieldError::missing_field("OidcSubClaim", "use_default"))
        })?;
        let keys = if use_default {
            Vec::new()
        } else {
            body.include_claim_keys.unwrap_or_default()
        };
        validate_keys(&keys)?;
        sqlx::query(
            "INSERT INTO actions_oidc_sub_claims (repo_id, use_default, include_claim_keys)
             VALUES ($1, $2, $3)
             ON CONFLICT (repo_id) WHERE repo_id IS NOT NULL
             DO UPDATE SET use_default = $2, include_claim_keys = $3, updated_at = now()",
        )
        .bind(a.repo.id)
        .bind(use_default)
        .bind(&keys)
        .execute(&state.db)
        .await?;
        bgh_core::audit::log(
            &state.db,
            Some(&auth.user),
            "repo.actions_oidc_sub_update",
            bgh_core::audit::Target::Repo {
                id: a.repo.id,
                org_id: a.owner.is_org().then_some(a.owner.id),
            },
            json!({"use_default": use_default, "include_claim_keys": keys}),
        )
        .await?;
        Ok((StatusCode::CREATED, Json(json!({}))))
    }

    pub async fn org_get(
        State(state): State<AppState>,
        auth: RequireUser,
        Path(org): Path<String>,
    ) -> ApiResult<Json<Value>> {
        let o = load_org(&state, &org).await?;
        if !auth.user.site_admin
            && bgh_core::perms::org_role(&state.db, o.id, auth.user.id)
                .await?
                .is_none()
        {
            return Err(ApiError::NotFound);
        }
        let keys: Option<Vec<String>> = sqlx::query_scalar(
            "SELECT include_claim_keys FROM actions_oidc_sub_claims WHERE org_id = $1",
        )
        .bind(o.id)
        .fetch_optional(&state.db)
        .await?;
        let keys = keys
            .filter(|k| !k.is_empty())
            .unwrap_or_else(|| DEFAULT_TEMPLATE.iter().map(|s| s.to_string()).collect());
        Ok(Json(json!({ "include_claim_keys": keys })))
    }

    #[derive(Deserialize)]
    pub struct OrgBody {
        include_claim_keys: Option<Vec<String>>,
    }

    pub async fn org_put(
        State(state): State<AppState>,
        auth: RequireUser,
        Path(org): Path<String>,
        Json(body): Json<OrgBody>,
    ) -> ApiResult<(StatusCode, Json<Value>)> {
        let o = load_org(&state, &org).await?;
        require_org_admin(&state, &auth, &o).await?;
        let keys = body.include_claim_keys.ok_or_else(|| {
            ApiError::invalid_field(FieldError::missing_field(
                "OidcSubClaim",
                "include_claim_keys",
            ))
        })?;
        validate_keys(&keys)?;
        sqlx::query(
            "INSERT INTO actions_oidc_sub_claims (org_id, use_default, include_claim_keys)
             VALUES ($1, false, $2)
             ON CONFLICT (org_id) WHERE org_id IS NOT NULL
             DO UPDATE SET include_claim_keys = $2, updated_at = now()",
        )
        .bind(o.id)
        .bind(&keys)
        .execute(&state.db)
        .await?;
        bgh_core::audit::log(
            &state.db,
            Some(&auth.user),
            "org.actions_oidc_sub_update",
            bgh_core::audit::Target::Org(o.id),
            json!({"include_claim_keys": keys}),
        )
        .await?;
        Ok((StatusCode::CREATED, Json(json!({}))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claims() -> Map<String, Value> {
        json!({
            "repository": "o/r",
            "ref": "refs/heads/main",
            "event_name": "push",
            "job_workflow_ref": "o/r/.github/workflows/ci.yml@refs/heads/main",
            "repository_owner_id": "7",
        })
        .as_object()
        .unwrap()
        .clone()
    }

    fn t(keys: &[&str]) -> Vec<String> {
        keys.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn default_sub_contexts() {
        let mut c = claims();
        assert_eq!(
            render_sub(&t(DEFAULT_TEMPLATE), &c),
            "repo:o/r:ref:refs/heads/main"
        );
        c.insert("event_name".into(), json!("pull_request"));
        assert_eq!(
            render_sub(&t(DEFAULT_TEMPLATE), &c),
            "repo:o/r:pull_request"
        );
        c.insert("environment".into(), json!("prod"));
        assert_eq!(
            render_sub(&t(DEFAULT_TEMPLATE), &c),
            "repo:o/r:environment:prod"
        );
    }

    #[test]
    fn custom_template() {
        assert_eq!(
            render_sub(
                &t(&["repository_owner_id", "context", "job_workflow_ref"]),
                &claims()
            ),
            "repository_owner_id:7:ref:refs/heads/main:job_workflow_ref:o/r/.github/workflows/ci.yml@refs/heads/main"
        );
    }

    #[test]
    fn template_validation() {
        assert!(validate_keys(&t(&["repo", "context", "actor", "environment"])).is_ok());
        assert!(validate_keys(&t(&["aud"])).is_err());
        assert!(validate_keys(&t(&["nope"])).is_err());
    }
}
