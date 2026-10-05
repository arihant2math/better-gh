//! Import a repository from a URL (`POST /_bgh/imports`).
//!
//! The repository is created right away (empty) together with a
//! `repo_imports` row; the `repos.import` job then fetches every branch and
//! tag (plus LFS objects when asked) in a background task, reporting
//! progress in the row. Clients poll `GET /_bgh/repos/{o}/{r}/import`
//! (status changes are also recorded as `repoImport` sync deltas in the
//! repository scope); `…/import/cancel` and `…/import/retry` control it.
//! With `mirror: true` the repository becomes a pull mirror (see
//! [`crate::mirrors`]).
//!
//! Remote access goes through [`bgh_core::ssrf`] (same allow-list as
//! webhooks) and credentials are sealed with [`bgh_core::secretbox`]; they
//! are never returned, logged or put into URLs or process arguments.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use base64::Engine;
use bgh_core::audit;
use bgh_core::events::{PushEvent, RefUpdate};
use bgh_core::jobs::JobPayload;
use bgh_core::prelude::*;
use bgh_core::ssrf;
use bgh_git::fetch::{self, Progress, Remote};
use bgh_git::lfs::Pointer;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::create::{self, CreateRepoBody};
use crate::jobs::PostReceive;

/// Time limit of one import fetch (imports run outside the job timeout).
const IMPORT_TIMEOUT: Duration = Duration::from_secs(6 * 3600);
/// An `importing` row not touched for this long belongs to a dead process.
pub const STALE_AFTER_SECS: i64 = 120;

pub fn web_routes() -> Router<AppState> {
    Router::new()
        .route("/_bgh/imports", post(create_import))
        .route("/_bgh/repos/{owner}/{repo}/import", get(get_import))
        .route("/_bgh/repos/{owner}/{repo}/import/cancel", post(cancel))
        .route("/_bgh/repos/{owner}/{repo}/import/retry", post(retry))
}

// ---------------------------------------------------------------------------
// Remote URLs and credentials
// ---------------------------------------------------------------------------

/// Username + password/token for a remote.
#[derive(Clone)]
pub struct Credentials {
    pub username: String,
    pub secret: String,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Credentials(..)")
    }
}

impl Credentials {
    /// From request fields; a token alone gets a placeholder username
    /// (GitHub, GitLab and Better GitHub accept any username with a token).
    pub fn from_parts(username: Option<&str>, secret: Option<&str>) -> Option<Self> {
        let secret = secret.filter(|s| !s.is_empty())?;
        let username = username
            .map(str::trim)
            .filter(|u| !u.is_empty())
            .unwrap_or("x-access-token");
        Some(Self {
            username: username.to_string(),
            secret: secret.to_string(),
        })
    }

    fn authorization(&self) -> String {
        let raw = format!("{}:{}", self.username, self.secret);
        format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(raw)
        )
    }

    pub fn seal(&self, state: &AppState) -> ApiResult<Vec<u8>> {
        bgh_core::secretbox::seal(state, &format!("{}\0{}", self.username, self.secret))
    }

    pub fn open(state: &AppState, sealed: &[u8]) -> ApiResult<Self> {
        let plain = bgh_core::secretbox::open(state, sealed)?;
        let (u, s) = plain.split_once('\0').unwrap_or(("x-access-token", &plain));
        Ok(Self {
            username: u.to_string(),
            secret: s.to_string(),
        })
    }
}

fn url_error(field: &str, message: impl Into<String>) -> ApiError {
    ApiError::invalid_field(FieldError::custom("Import", field, message))
}

/// Validate a remote URL: `http(s)`, a host, and (for literal addresses)
/// the SSRF policy. Userinfo is moved into the returned credentials and
/// stripped from the URL.
pub async fn parse_remote_url(
    state: &AppState,
    field: &str,
    raw: &str,
) -> ApiResult<(String, Option<Credentials>)> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(ApiError::invalid_field(FieldError::missing_field(
            "Import", field,
        )));
    }
    let mut url = url::Url::parse(raw).map_err(|_| url_error(field, "is not a valid URL"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(url_error(field, "must be an http:// or https:// URL"));
    }
    let creds = (!url.username().is_empty()).then(|| {
        let user = percent_decode(url.username());
        match url.password() {
            Some(p) => Credentials {
                username: user,
                secret: percent_decode(p),
            },
            // `https://TOKEN@host/…`
            None => Credentials {
                username: "x-access-token".into(),
                secret: user,
            },
        }
    });
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_fragment(None);
    let policy = ssrf::Policy::load(state).await;
    let url = ssrf::validate_url(&policy, url.as_str()).map_err(|e| url_error(field, e))?;
    Ok((url.to_string(), creds))
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(b);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Resolve `url` under the SSRF policy into a pinned [`Remote`].
pub async fn remote(
    state: &AppState,
    url: &str,
    creds: Option<&Credentials>,
    timeout: Duration,
) -> Result<Remote, String> {
    let policy = ssrf::Policy::load(state).await;
    let target = ssrf::resolve(&policy, url).await?;
    let addrs: Vec<String> = target
        .addrs
        .iter()
        .map(|a| match a.ip() {
            std::net::IpAddr::V6(v6) => format!("[{v6}]"),
            v4 => v4.to_string(),
        })
        .collect();
    let port = target.url.port_or_known_default().unwrap_or(80);
    Ok(Remote {
        url: target.url.to_string(),
        authorization: creds.map(Credentials::authorization),
        resolve: vec![format!("{}:{port}:{}", target.host, addrs.join(","))],
        timeout: Some(timeout),
    })
}

/// Turn a git error into a message safe to show (and store).
pub fn describe_error(e: &bgh_git::GitError) -> String {
    match e {
        bgh_git::GitError::Command { stderr, status, .. } => {
            if stderr.trim().is_empty() {
                format!("git exited with {status}")
            } else {
                stderr.trim().to_string()
            }
        }
        other => other.to_string(),
    }
}

// ---------------------------------------------------------------------------
// LFS
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct LfsBatchResponse {
    #[serde(default)]
    objects: Vec<LfsBatchObject>,
}

#[derive(Deserialize)]
struct LfsBatchObject {
    oid: String,
    size: i64,
    #[serde(default)]
    actions: Option<LfsActions>,
    #[serde(default)]
    error: Option<LfsObjectError>,
}

#[derive(Deserialize)]
struct LfsActions {
    download: Option<LfsAction>,
}

#[derive(Deserialize)]
struct LfsAction {
    href: String,
    #[serde(default)]
    header: std::collections::BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct LfsObjectError {
    message: String,
}

/// HTTP client pinned to the checked addresses of `url`.
async fn pinned_client(
    policy: &ssrf::Policy,
    url: &str,
) -> anyhow::Result<(reqwest::Client, url::Url)> {
    let target = ssrf::resolve(policy, url)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(3600))
        .resolve_to_addrs(&target.host, &target.addrs)
        .build()?;
    Ok((client, target.url))
}

/// LFS endpoint of a git remote (git-lfs' default: `<url>.git/info/lfs`).
fn lfs_endpoint(url: &str) -> String {
    let base = url.trim_end_matches('/');
    if base.ends_with(".git") {
        format!("{base}/info/lfs")
    } else {
        format!("{base}.git/info/lfs")
    }
}

/// Download the LFS objects behind `pointers` that `repo_id` doesn't have
/// yet into the object store and link them. Calls `on_object(done, total)`.
pub async fn fetch_lfs(
    state: &AppState,
    repo_id: i64,
    url: &str,
    creds: Option<&Credentials>,
    pointers: Vec<Pointer>,
    uploader_id: Option<i64>,
    mut on_object: impl FnMut(i64, i64),
) -> anyhow::Result<()> {
    let mut missing = Vec::new();
    for p in pointers {
        if !crate::lfs::has_object(state, repo_id, &p.oid).await? {
            missing.push(p);
        }
    }
    let total = missing.len() as i64;
    on_object(0, total);
    if missing.is_empty() {
        return Ok(());
    }
    let policy = ssrf::Policy::load(state).await;
    let batch_url = format!("{}/objects/batch", lfs_endpoint(url));
    let (client, batch_url) = pinned_client(&policy, &batch_url).await?;
    let store = crate::lfs::object_store(state);
    let mut done = 0i64;
    for chunk in missing.chunks(100) {
        let mut req = client
            .post(batch_url.clone())
            .header("Accept", "application/vnd.git-lfs+json")
            .header("Content-Type", "application/vnd.git-lfs+json")
            .json(&json!({
                "operation": "download",
                "transfers": ["basic"],
                "objects": chunk.iter().map(|p| json!({"oid": p.oid, "size": p.size})).collect::<Vec<_>>(),
                "hash_algo": "sha256",
            }));
        if let Some(c) = creds {
            req = req.header("Authorization", c.authorization());
        }
        let res = req.send().await?;
        if !res.status().is_success() {
            anyhow::bail!("LFS batch request failed: HTTP {}", res.status().as_u16());
        }
        let body: LfsBatchResponse = res.json().await?;
        for obj in body.objects {
            if let Some(err) = obj.error {
                anyhow::bail!("LFS object {} unavailable: {}", obj.oid, err.message);
            }
            let Some(action) = obj.actions.and_then(|a| a.download) else {
                // Already present on the remote side's terms; nothing to do.
                continue;
            };
            if !bgh_git::lfs::is_valid_oid(&obj.oid) || obj.size < 0 {
                anyhow::bail!("LFS server returned an invalid object");
            }
            let (dl, href) = pinned_client(&policy, &action.href).await?;
            let mut req = dl.get(href);
            for (k, v) in &action.header {
                req = req.header(k.as_str(), v.as_str());
            }
            let res = req.send().await?;
            if !res.status().is_success() {
                anyhow::bail!(
                    "LFS download of {} failed: HTTP {}",
                    obj.oid,
                    res.status().as_u16()
                );
            }
            let stream = futures::TryStreamExt::map_err(res.bytes_stream(), std::io::Error::other);
            let mut reader = tokio_util::io::StreamReader::new(stream);
            store
                .put(&obj.oid, obj.size as u64, &mut reader)
                .await
                .map_err(|e| anyhow::anyhow!("LFS object {}: {e}", obj.oid))?;
            crate::lfs::objects::link(state, repo_id, &obj.oid, obj.size, uploader_id).await?;
            done += 1;
            on_object(done, total);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Rows and JSON
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ImportRow {
    pub id: i64,
    pub repo_id: i64,
    pub source_url: String,
    pub enc_credentials: Option<Vec<u8>>,
    pub mirror: bool,
    pub include_lfs: bool,
    pub status: String,
    pub phase: String,
    pub objects_received: i64,
    pub objects_total: i64,
    pub bytes_received: i64,
    pub lfs_received: i64,
    pub lfs_total: i64,
    pub error: Option<String>,
    pub attempts: i32,
    pub creator_id: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    /// Git step of a metadata import (P18): fetched refs emit `Push` with
    /// origin [`PushEvent::ORIGIN_METADATA_IMPORT`].
    pub quiet: bool,
}

impl ImportRow {
    pub const COLUMNS: &'static str = "id, repo_id, source_url, enc_credentials, mirror, \
        include_lfs, status, phase, objects_received, objects_total, bytes_received, \
        lfs_received, lfs_total, error, attempts, creator_id, created_at, updated_at, completed_at, quiet";

    async fn for_repo(db: impl sqlx::PgExecutor<'_>, repo_id: i64) -> ApiResult<Option<Self>> {
        Ok(sqlx::query_as(&format!(
            "SELECT {} FROM repo_imports WHERE repo_id = $1",
            Self::COLUMNS
        ))
        .bind(repo_id)
        .fetch_optional(db)
        .await?)
    }

    fn in_progress(&self) -> bool {
        matches!(self.status.as_str(), "queued" | "importing")
    }
}

#[derive(Debug, Serialize)]
pub struct ImportRepoJson {
    pub id: i64,
    pub name: String,
    pub full_name: String,
    pub owner: String,
    pub private: bool,
    pub html_url: String,
    pub url: String,
}

/// Import status (credentials are never included).
#[derive(Debug, Serialize)]
pub struct ImportJson {
    pub id: i64,
    pub status: String,
    pub phase: String,
    pub source_url: String,
    pub mirror: bool,
    pub include_lfs: bool,
    pub has_credentials: bool,
    pub objects_received: i64,
    pub objects_total: i64,
    pub bytes_received: i64,
    pub lfs_objects_received: i64,
    pub lfs_objects_total: i64,
    pub error: Option<String>,
    pub attempts: i32,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub completed_at: Option<Timestamp>,
    pub repository: ImportRepoJson,
}

impl ImportJson {
    fn new(state: &AppState, owner: &str, repo: &db::Repository, row: &ImportRow) -> Self {
        Self {
            id: row.id,
            status: row.status.clone(),
            phase: row.phase.clone(),
            source_url: row.source_url.clone(),
            mirror: row.mirror,
            include_lfs: row.include_lfs,
            has_credentials: row.enc_credentials.is_some(),
            objects_received: row.objects_received,
            objects_total: row.objects_total,
            bytes_received: row.bytes_received,
            lfs_objects_received: row.lfs_received,
            lfs_objects_total: row.lfs_total,
            error: row.error.clone(),
            attempts: row.attempts,
            created_at: row.created_at.into(),
            updated_at: row.updated_at.into(),
            completed_at: bgh_core::time::ts(row.completed_at),
            repository: ImportRepoJson {
                id: repo.id,
                name: repo.name.clone(),
                full_name: format!("{owner}/{}", repo.name),
                owner: owner.to_string(),
                private: repo.is_private(),
                html_url: state.urls.repo_html(owner, &repo.name),
                url: state.urls.repo(owner, &repo.name),
            },
        }
    }

    /// Compact delta for `repoImport` sync actions.
    fn sync_data(row: &ImportRow) -> serde_json::Value {
        json!({
            "id": row.id, "repoId": row.repo_id, "status": row.status, "phase": row.phase,
            "error": row.error,
        })
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// Request body of `POST /_bgh/imports`.
#[derive(Debug, Default, Deserialize)]
pub struct CreateImportBody {
    pub source_url: Option<String>,
    pub username: Option<String>,
    pub password_or_token: Option<String>,
    /// Owner login (user or organization); defaults to the caller.
    pub owner: Option<String>,
    /// Defaults to the last path segment of the source URL.
    pub name: Option<String>,
    pub description: Option<String>,
    /// `public` | `private` | `internal`.
    pub visibility: Option<String>,
    pub private: Option<bool>,
    #[serde(default)]
    pub mirror: bool,
    #[serde(default)]
    pub include_lfs: bool,
    /// Mirror sync interval (default 480).
    pub mirror_interval_minutes: Option<i32>,
}

/// What [`create::create_with`] records for an import.
pub struct NewImport {
    pub source_url: String,
    pub credentials: Option<Vec<u8>>,
    pub mirror: bool,
    pub include_lfs: bool,
    pub interval_minutes: i32,
    /// Metadata import (P18): no webhooks/notifications/activity for the
    /// fetched refs.
    pub quiet: bool,
}

/// Default name for a repository imported from `url`.
fn name_from_url(url: &str) -> String {
    url.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .trim_end_matches(".git")
        .to_string()
}

pub fn validate_interval(v: i32) -> ApiResult<i32> {
    if (10..=43200).contains(&v) {
        Ok(v)
    } else {
        Err(ApiError::invalid_field(FieldError::custom(
            "Mirror",
            "interval_minutes",
            "must be between 10 and 43200",
        )))
    }
}

/// `POST /_bgh/imports`
pub async fn create_import(
    State(state): State<AppState>,
    auth: RequireUser,
    Json(body): Json<CreateImportBody>,
) -> ApiResult<(StatusCode, Json<ImportJson>)> {
    let (source_url, url_creds) = parse_remote_url(
        &state,
        "source_url",
        body.source_url.as_deref().unwrap_or(""),
    )
    .await?;
    let creds =
        Credentials::from_parts(body.username.as_deref(), body.password_or_token.as_deref())
            .or(url_creds);
    let interval = validate_interval(body.mirror_interval_minutes.unwrap_or(480))?;
    let owner = match body
        .owner
        .as_deref()
        .map(str::trim)
        .filter(|o| !o.is_empty())
    {
        None => auth.user.clone(),
        Some(login) if login.eq_ignore_ascii_case(&auth.user.login) => auth.user.clone(),
        Some(login) => db::User::find_by_login(&state.db, login)
            .await?
            .filter(db::User::is_org)
            .ok_or_else(|| {
                ApiError::invalid_field(FieldError::custom(
                    "Import",
                    "owner",
                    "must be you or an organization you can create repositories in",
                ))
            })?,
    };
    let name = body
        .name
        .clone()
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| name_from_url(&source_url));
    let repo_body = CreateRepoBody {
        name: Some(name),
        description: body.description.clone(),
        visibility: body.visibility.clone(),
        private: body.private,
        ..Default::default()
    };
    if owner.is_org() {
        create::authorize_org(&state, &auth, &owner, &repo_body).await?;
    }
    let sealed = creds.as_ref().map(|c| c.seal(&state)).transpose()?;
    let access = create::create_with(
        &state,
        &auth,
        owner,
        repo_body,
        Some(NewImport {
            source_url,
            credentials: sealed,
            mirror: body.mirror,
            include_lfs: body.include_lfs,
            interval_minutes: interval,
            quiet: false,
        }),
    )
    .await?;
    let row = ImportRow::for_repo(&state.db, access.repo.id)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok((
        StatusCode::CREATED,
        Json(ImportJson::new(
            &state,
            &access.owner.login,
            &access.repo,
            &row,
        )),
    ))
}

/// Inside the repository-creation transaction: the import row, the mirror
/// configuration and the job.
pub(crate) async fn record(
    tx: &mut Tx,
    auth: &AuthContext,
    mut repo: db::Repository,
    import: NewImport,
) -> ApiResult<db::Repository> {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO repo_imports (repo_id, source_url, enc_credentials, mirror, include_lfs, creator_id, quiet)
         VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id",
    )
    .bind(repo.id)
    .bind(&import.source_url)
    .bind(&import.credentials)
    .bind(import.mirror)
    .bind(import.include_lfs)
    .bind(auth.user.id)
    .bind(import.quiet)
    .fetch_one(&mut **tx)
    .await?;
    if import.mirror {
        sqlx::query(
            "INSERT INTO repo_mirrors (repo_id, url, enc_credentials, interval_minutes, include_lfs,
                                       creator_id, next_sync_at)
             VALUES ($1, $2, $3, $4, $5, $6, now() + make_interval(mins => $4))",
        )
        .bind(repo.id)
        .bind(&import.source_url)
        .bind(&import.credentials)
        .bind(import.interval_minutes)
        .bind(import.include_lfs)
        .bind(auth.user.id)
        .execute(&mut **tx)
        .await?;
        repo = sqlx::query_as(&format!(
            "UPDATE repositories SET mirror_url = $2 WHERE id = $1 RETURNING {}",
            db::Repository::COLUMNS
        ))
        .bind(repo.id)
        .bind(&import.source_url)
        .fetch_one(&mut **tx)
        .await?;
    }
    audit::log(
        &mut **tx,
        Some(&auth.user),
        "repo.import",
        audit::Target::Repo {
            id: repo.id,
            org_id: None,
        },
        json!({
            "source_url": import.source_url, "mirror": import.mirror,
            "include_lfs": import.include_lfs,
        }),
    )
    .await?;
    tx.enqueue(&RunImport { import_id: id }).await?;
    Ok(repo)
}

async fn load_for(
    state: &AppState,
    auth: Option<&AuthContext>,
    owner: &str,
    repo: &str,
) -> ApiResult<(RepoAccess, ImportRow)> {
    let access = RepoAccess::load(state, auth, owner, repo).await?;
    let row = ImportRow::for_repo(&state.db, access.repo.id)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok((access, row))
}

/// `GET /_bgh/repos/{owner}/{repo}/import` (readers of the repository).
pub async fn get_import(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<Json<ImportJson>> {
    let (access, row) = load_for(&state, auth.as_ref(), &owner, &repo).await?;
    Ok(Json(ImportJson::new(
        &state,
        &access.owner.login,
        &access.repo,
        &row,
    )))
}

/// `POST /_bgh/repos/{owner}/{repo}/import/cancel` (admins).
pub async fn cancel(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<Json<ImportJson>> {
    let (access, row) = load_for(&state, Some(&auth), &owner, &repo).await?;
    access.require(Permission::Admin)?;
    if !row.in_progress() {
        return Err(ApiError::unprocessable("The import is not in progress."));
    }
    let mut tx = Tx::begin(&state).await?;
    let row: ImportRow = sqlx::query_as(&format!(
        "UPDATE repo_imports SET status = 'cancelled', phase = 'cancelled', updated_at = now(),
                completed_at = now()
          WHERE id = $1 RETURNING {}",
        ImportRow::COLUMNS
    ))
    .bind(row.id)
    .fetch_one(&mut *tx)
    .await?;
    audit::log(
        &mut *tx,
        Some(&auth.user),
        "repo.import_cancel",
        audit::Target::Repo {
            id: access.repo.id,
            org_id: None,
        },
        json!({}),
    )
    .await?;
    sync_import(&mut tx, &row).await?;
    tx.commit().await?;
    Ok(Json(ImportJson::new(
        &state,
        &access.owner.login,
        &access.repo,
        &row,
    )))
}

/// Body of `POST …/import/retry`: optionally replace the credentials.
#[derive(Debug, Default, Deserialize)]
pub struct RetryBody {
    pub username: Option<String>,
    pub password_or_token: Option<String>,
}

/// `POST /_bgh/repos/{owner}/{repo}/import/retry` (admins; failed or
/// cancelled imports).
pub async fn retry(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo)): Path<(String, String)>,
    body: axum::body::Bytes,
) -> ApiResult<Json<ImportJson>> {
    let (access, row) = load_for(&state, Some(&auth), &owner, &repo).await?;
    access.require(Permission::Admin)?;
    if !matches!(row.status.as_str(), "failed" | "cancelled") {
        return Err(ApiError::unprocessable(
            "Only failed or cancelled imports can be retried.",
        ));
    }
    let body: RetryBody = if body.iter().all(u8::is_ascii_whitespace) {
        RetryBody::default()
    } else {
        serde_json::from_slice(&body)
            .map_err(|_| ApiError::BadRequest("Problems parsing JSON".into()))?
    };
    let sealed =
        Credentials::from_parts(body.username.as_deref(), body.password_or_token.as_deref())
            .map(|c| c.seal(&state))
            .transpose()?;
    let mut tx = Tx::begin(&state).await?;
    let row: ImportRow = sqlx::query_as(&format!(
        "UPDATE repo_imports
            SET status = 'queued', phase = 'queued', error = NULL, objects_received = 0,
                objects_total = 0, bytes_received = 0, lfs_received = 0, lfs_total = 0,
                enc_credentials = coalesce($2, enc_credentials), updated_at = now(),
                completed_at = NULL
          WHERE id = $1 RETURNING {}",
        ImportRow::COLUMNS
    ))
    .bind(row.id)
    .bind(&sealed)
    .fetch_one(&mut *tx)
    .await?;
    if sealed.is_some() && row.mirror {
        sqlx::query("UPDATE repo_mirrors SET enc_credentials = $2 WHERE repo_id = $1")
            .bind(access.repo.id)
            .bind(&sealed)
            .execute(&mut *tx)
            .await?;
    }
    audit::log(
        &mut *tx,
        Some(&auth.user),
        "repo.import_retry",
        audit::Target::Repo {
            id: access.repo.id,
            org_id: None,
        },
        json!({ "credentials_changed": sealed.is_some() }),
    )
    .await?;
    sync_import(&mut tx, &row).await?;
    tx.enqueue(&RunImport { import_id: row.id }).await?;
    tx.commit().await?;
    Ok(Json(ImportJson::new(
        &state,
        &access.owner.login,
        &access.repo,
        &row,
    )))
}

async fn sync_import(tx: &mut Tx, row: &ImportRow) -> ApiResult<()> {
    tx.sync(
        &bgh_core::sync::repo_scope(row.repo_id),
        "repoImport",
        row.id,
        SyncAction::Update,
        &ImportJson::sync_data(row),
    )
    .await
}

/// Refuse pushes while an import is queued or running.
pub async fn require_not_importing(state: &AppState, repo_id: i64) -> ApiResult<()> {
    let busy: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM repo_imports
                         WHERE repo_id = $1 AND status IN ('queued', 'importing'))",
    )
    .bind(repo_id)
    .fetch_one(&state.db)
    .await?;
    if busy {
        Err(ApiError::forbidden(
            "This repository is being imported; try again when the import is complete.",
        ))
    } else {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Job
// ---------------------------------------------------------------------------

/// Run (or re-run) an import.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunImport {
    pub import_id: i64,
}

impl JobPayload for RunImport {
    const KIND: &'static str = "repos.import";
    const MAX_ATTEMPTS: i32 = 3;
}

/// Claim the import and run it in a background task (an import can take
/// longer than the job timeout). Progress writes keep `updated_at` fresh;
/// [`sweep_stale`] fails imports whose process died.
pub async fn run_import_job(state: AppState, job: RunImport) -> anyhow::Result<()> {
    let claimed: Option<ImportRow> = sqlx::query_as(&format!(
        "UPDATE repo_imports
            SET status = 'importing', phase = 'connecting', attempts = attempts + 1,
                updated_at = now()
          WHERE id = $1 AND status = 'queued' RETURNING {}",
        ImportRow::COLUMNS
    ))
    .bind(job.import_id)
    .fetch_optional(&state.db)
    .await?;
    let Some(row) = claimed else {
        return Ok(()); // cancelled, already running or done
    };
    record_status(&state, &row).await?;
    tokio::spawn(async move {
        let id = row.id;
        if let Err(e) = run_import(&state, row).await {
            tracing::warn!(import_id = id, error = %e, "import failed");
            let _ = finish(&state, id, "failed", Some(&e.to_string())).await;
        }
    });
    Ok(())
}

async fn record_status(state: &AppState, row: &ImportRow) -> anyhow::Result<()> {
    let mut tx = Tx::begin(state).await?;
    sync_import(&mut tx, row).await?;
    tx.commit().await?;
    Ok(())
}

/// Set a final status (only from `importing`, so a cancel wins).
async fn finish(
    state: &AppState,
    id: i64,
    status: &str,
    error: Option<&str>,
) -> anyhow::Result<()> {
    let row: Option<ImportRow> = sqlx::query_as(&format!(
        "UPDATE repo_imports SET status = $2, phase = $2, error = $3, updated_at = now(),
                completed_at = now()
          WHERE id = $1 AND status = 'importing' RETURNING {}",
        ImportRow::COLUMNS
    ))
    .bind(id)
    .bind(status)
    .bind(error)
    .fetch_optional(&state.db)
    .await?;
    if let Some(row) = row {
        record_status(state, &row).await?;
    }
    Ok(())
}

#[derive(Default)]
struct Shared {
    progress: Progress,
    lfs: (i64, i64),
}

/// Persist progress every second while the import runs; cancel the fetch
/// when the row stops being `importing` (cancelled by a user).
fn spawn_progress_writer(
    state: &AppState,
    id: i64,
    shared: Arc<Mutex<Shared>>,
    cancel: CancellationToken,
    done: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    let state = state.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                _ = tick.tick() => {}
                _ = done.cancelled() => return,
            }
            let (p, lfs) = {
                let s = shared.lock().expect("progress");
                (s.progress.clone(), s.lfs)
            };
            let res = sqlx::query(
                "UPDATE repo_imports
                    SET phase = CASE WHEN $2 = '' THEN phase ELSE $2 END,
                        objects_received = $3, objects_total = $4, bytes_received = $5,
                        lfs_received = $6, lfs_total = $7, updated_at = now()
                  WHERE id = $1 AND status = 'importing'",
            )
            .bind(id)
            .bind(&p.phase)
            .bind(p.objects_received)
            .bind(p.objects_total)
            .bind(p.bytes_received)
            .bind(lfs.0)
            .bind(lfs.1)
            .execute(&state.db)
            .await;
            if let Ok(r) = res
                && r.rows_affected() == 0
            {
                cancel.cancel();
                return;
            }
        }
    })
}

async fn run_import(state: &AppState, row: ImportRow) -> anyhow::Result<()> {
    let creds = row
        .enc_credentials
        .as_deref()
        .map(|c| Credentials::open(state, c))
        .transpose()
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let remote = match remote(state, &row.source_url, creds.as_ref(), IMPORT_TIMEOUT).await {
        Ok(r) => r,
        Err(e) => return finish(state, row.id, "failed", Some(&e)).await,
    };
    let shared = Arc::new(Mutex::new(Shared::default()));
    let cancel = CancellationToken::new();
    let done = CancellationToken::new();
    let writer = spawn_progress_writer(state, row.id, shared.clone(), cancel.clone(), done.clone());
    let result = import_git(state, &row, &remote, creds.as_ref(), &shared, &cancel).await;
    done.cancel();
    let _ = writer.await;
    let (p, lfs) = {
        let s = shared.lock().expect("progress");
        (s.progress.clone(), s.lfs)
    };
    sqlx::query(
        "UPDATE repo_imports SET objects_received = $2, objects_total = $3,
                bytes_received = $4, lfs_received = $5, lfs_total = $6
          WHERE id = $1",
    )
    .bind(row.id)
    .bind(p.objects_received)
    .bind(p.objects_total)
    .bind(p.bytes_received)
    .bind(lfs.0)
    .bind(lfs.1)
    .execute(&state.db)
    .await?;
    if cancel.is_cancelled() {
        return Ok(()); // status already `cancelled`
    }
    match result {
        Ok(()) => {
            finish(state, row.id, "complete", None).await?;
            if row.mirror {
                sqlx::query(
                    "UPDATE repo_mirrors
                        SET last_sync_at = now(), last_status = 'success', last_error = NULL,
                            consecutive_failures = 0,
                            next_sync_at = now() + make_interval(mins => interval_minutes),
                            updated_at = now()
                      WHERE repo_id = $1",
                )
                .bind(row.repo_id)
                .execute(&state.db)
                .await?;
            }
            Ok(())
        }
        Err(msg) => finish(state, row.id, "failed", Some(&msg)).await,
    }
}

/// Fetch, set the default branch, fetch LFS objects and run post-receive.
/// Errors are user-facing messages.
async fn import_git(
    state: &AppState,
    row: &ImportRow,
    remote: &Remote,
    creds: Option<&Credentials>,
    shared: &Arc<Mutex<Shared>>,
    cancel: &CancellationToken,
) -> Result<(), String> {
    let internal = |e: &dyn std::fmt::Display| {
        tracing::warn!(import_id = row.id, error = %e, "import step failed");
        "Internal error while importing.".to_string()
    };
    let Some(repo) = db::Repository::find(&state.db, row.repo_id)
        .await
        .map_err(|e| internal(&e))?
    else {
        return Ok(());
    };
    bgh_core::settings::check_push_quota(state, &repo)
        .await
        .map_err(|e| e.to_string())?;
    let store = crate::store(state);
    let head = fetch::remote_head(&store, remote)
        .await
        .map_err(|e| describe_error(&e))?;
    let updates = {
        let shared = shared.clone();
        fetch::fetch(&store, repo.id, remote, false, cancel, move |p| {
            shared.lock().expect("progress").progress = p.clone();
        })
        .await
        .map_err(|e| describe_error(&e))?
    };
    // Adopt the remote's default branch.
    let branches: Vec<String> = updates
        .iter()
        .filter(|u| !u.is_delete())
        .filter_map(|u| u.branch().map(str::to_string))
        .collect();
    if let Some(head) = head.filter(|h| branches.contains(h)) {
        bgh_git::write::set_head(&store, repo.id, &head)
            .await
            .map_err(|e| internal(&e))?;
        sqlx::query("UPDATE repositories SET default_branch = $2 WHERE id = $1")
            .bind(repo.id)
            .bind(&head)
            .execute(&state.db)
            .await
            .map_err(|e| internal(&e))?;
    }
    if row.include_lfs {
        shared.lock().expect("progress").progress.phase = "lfs".into();
        let tips: Vec<String> = updates
            .iter()
            .filter(|u| !u.is_delete())
            .map(|u| u.new.clone())
            .collect();
        let pointers = bgh_git::fetch::lfs_pointers(&store, repo.id, &tips, &[])
            .await
            .map_err(|e| internal(&e))?;
        let shared2 = shared.clone();
        fetch_lfs(
            state,
            repo.id,
            &row.source_url,
            creds,
            pointers,
            row.creator_id,
            move |done, total| shared2.lock().expect("progress").lfs = (done, total),
        )
        .await
        .map_err(|e| e.to_string())?;
    }
    shared.lock().expect("progress").progress.phase = "finishing".into();
    let origin = if row.mirror {
        PushEvent::ORIGIN_MIRROR
    } else if row.quiet {
        PushEvent::ORIGIN_METADATA_IMPORT
    } else {
        PushEvent::ORIGIN_IMPORT
    };
    after_fetch(state, repo.id, row.creator_id, updates, origin)
        .await
        .map_err(|e| internal(&e))
}

/// Post-receive processing for fetched refs (size, default branch, sync,
/// `Push` event with an origin).
pub async fn after_fetch(
    state: &AppState,
    repo_id: i64,
    pusher_id: Option<i64>,
    updates: Vec<RefUpdate>,
    origin: &str,
) -> anyhow::Result<()> {
    if updates.is_empty() {
        return Ok(());
    }
    crate::jobs::process_ref_updates(
        state,
        PostReceive {
            repo_id,
            pusher_id,
            updates,
        },
        Some(origin),
    )
    .await
}

/// Fail imports left `importing` by a process that died.
pub async fn sweep_stale(state: &AppState) -> anyhow::Result<u64> {
    let rows: Vec<ImportRow> = sqlx::query_as(&format!(
        "UPDATE repo_imports
            SET status = 'failed', phase = 'failed', completed_at = now(), updated_at = now(),
                error = 'The import was interrupted; retry it.'
          WHERE status = 'importing' AND updated_at < now() - make_interval(secs => $1)
          RETURNING {}",
        ImportRow::COLUMNS
    ))
    .bind(STALE_AFTER_SECS as f64)
    .fetch_all(&state.db)
    .await?;
    for row in &rows {
        record_status(state, row).await?;
    }
    Ok(rows.len() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_endpoints() {
        assert_eq!(name_from_url("https://h/o/repo.git"), "repo");
        assert_eq!(name_from_url("https://h/o/repo/"), "repo");
        assert_eq!(
            lfs_endpoint("https://h/o/r.git"),
            "https://h/o/r.git/info/lfs"
        );
        assert_eq!(lfs_endpoint("https://h/o/r"), "https://h/o/r.git/info/lfs");
        assert_eq!(percent_decode("a%40b"), "a@b");
        let c = Credentials::from_parts(None, Some("tok")).unwrap();
        assert_eq!(c.username, "x-access-token");
        assert!(Credentials::from_parts(Some("u"), Some("")).is_none());
        assert_eq!(format!("{c:?}"), "Credentials(..)");
    }
}
