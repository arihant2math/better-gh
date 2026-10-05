//! Pull mirrors: repositories that follow an upstream git URL.
//!
//! `repo_mirrors` holds the upstream URL, sealed credentials and the
//! schedule; `repositories.mirror_url` marks the repository read-only for
//! git writes ([`RepoAccess::require_not_mirror`]). The `repos.mirrors`
//! service enqueues `repos.mirror_sync` for due mirrors; a sync fetches all
//! branches and tags with `--prune`, force-updating refs, and emits `Push`
//! with origin `mirror` (search and activity run, Actions doesn't).
//!
//! Endpoints (repository admins): `GET|PATCH|DELETE /_bgh/repos/{o}/{r}/mirror`
//! (DELETE converts the mirror into a regular repository),
//! `POST /_bgh/repos/{o}/{r}/mirror/sync`; site admins:
//! `GET /_bgh/admin/mirrors?status=failed`.

use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use bgh_core::audit;
use bgh_core::events::PushEvent;
use bgh_core::jobs::JobPayload;
use bgh_core::prelude::*;
use bgh_git::fetch;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::import::{Credentials, describe_error, parse_remote_url, validate_interval};

/// Time limit of one mirror fetch (runs inside the 10-minute job limit).
const SYNC_TIMEOUT: Duration = Duration::from_secs(9 * 60);
/// How often the scheduler looks for due mirrors.
const POLL: Duration = Duration::from_secs(30);

pub fn web_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/_bgh/repos/{owner}/{repo}/mirror",
            get(get_mirror).patch(update_mirror).delete(convert),
        )
        .route("/_bgh/repos/{owner}/{repo}/mirror/sync", post(sync_now))
        .route("/_bgh/admin/mirrors", get(admin_list))
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct MirrorRow {
    pub repo_id: i64,
    pub url: String,
    pub enc_credentials: Option<Vec<u8>>,
    pub interval_minutes: i32,
    pub enabled: bool,
    pub include_lfs: bool,
    pub creator_id: Option<i64>,
    pub last_sync_at: Option<DateTime<Utc>>,
    pub next_sync_at: DateTime<Utc>,
    pub last_status: String,
    pub last_error: Option<String>,
    pub consecutive_failures: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl MirrorRow {
    pub const COLUMNS: &'static str = "repo_id, url, enc_credentials, interval_minutes, enabled, \
        include_lfs, creator_id, last_sync_at, next_sync_at, last_status, last_error, \
        consecutive_failures, created_at, updated_at";

    pub async fn find(db: impl sqlx::PgExecutor<'_>, repo_id: i64) -> sqlx::Result<Option<Self>> {
        sqlx::query_as(&format!(
            "SELECT {} FROM repo_mirrors WHERE repo_id = $1",
            Self::COLUMNS
        ))
        .bind(repo_id)
        .fetch_optional(db)
        .await
    }
}

/// Mirror settings and state (credentials are never included).
#[derive(Debug, Serialize)]
pub struct MirrorJson {
    pub url: String,
    pub interval_minutes: i32,
    pub enabled: bool,
    pub include_lfs: bool,
    pub has_credentials: bool,
    pub last_sync_at: Option<Timestamp>,
    pub next_sync_at: Option<Timestamp>,
    /// `pending` | `success` | `failed`
    pub last_status: String,
    pub last_error: Option<String>,
    pub consecutive_failures: i32,
    /// Whether a sync is queued or running.
    pub syncing: bool,
}

impl MirrorJson {
    fn new(row: &MirrorRow, syncing: bool) -> Self {
        Self {
            url: row.url.clone(),
            interval_minutes: row.interval_minutes,
            enabled: row.enabled,
            include_lfs: row.include_lfs,
            has_credentials: row.enc_credentials.is_some(),
            last_sync_at: bgh_core::time::ts(row.last_sync_at),
            next_sync_at: row.enabled.then(|| row.next_sync_at.into()),
            last_status: row.last_status.clone(),
            last_error: row.last_error.clone(),
            consecutive_failures: row.consecutive_failures,
            syncing,
        }
    }
}

async fn is_syncing(state: &AppState, repo_id: i64) -> ApiResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM jobs WHERE kind = $1 AND failed_at IS NULL
                          AND (payload->>'repo_id')::bigint = $2)",
    )
    .bind(MirrorSync::KIND)
    .bind(repo_id)
    .fetch_one(&state.db)
    .await
    .unwrap_or(false))
}

async fn load_admin(
    state: &AppState,
    auth: &AuthContext,
    owner: &str,
    repo: &str,
) -> ApiResult<(RepoAccess, MirrorRow)> {
    let access = RepoAccess::load(state, Some(auth), owner, repo).await?;
    access.require(Permission::Admin)?;
    let row = MirrorRow::find(&state.db, access.repo.id)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok((access, row))
}

fn target(access: &RepoAccess) -> audit::Target {
    audit::Target::Repo {
        id: access.repo.id,
        org_id: access.owner.is_org().then_some(access.owner.id),
    }
}

/// `GET /_bgh/repos/{owner}/{repo}/mirror`
pub async fn get_mirror(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<Json<MirrorJson>> {
    let (access, row) = load_admin(&state, &auth, &owner, &repo).await?;
    let syncing = is_syncing(&state, access.repo.id).await?;
    Ok(Json(MirrorJson::new(&row, syncing)))
}

/// Body of `PATCH …/mirror`. Credentials: send `username` /
/// `password_or_token` to replace them, `clear_credentials: true` to drop.
#[derive(Debug, Default, Deserialize)]
pub struct UpdateMirrorBody {
    pub url: Option<String>,
    pub username: Option<String>,
    pub password_or_token: Option<String>,
    #[serde(default)]
    pub clear_credentials: bool,
    pub interval_minutes: Option<i32>,
    pub enabled: Option<bool>,
    pub include_lfs: Option<bool>,
}

/// `PATCH /_bgh/repos/{owner}/{repo}/mirror`
pub async fn update_mirror(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo)): Path<(String, String)>,
    Json(body): Json<UpdateMirrorBody>,
) -> ApiResult<Json<MirrorJson>> {
    let (access, row) = load_admin(&state, &auth, &owner, &repo).await?;
    let (url, url_creds) = match body.url.as_deref() {
        Some(u) => {
            let (u, c) = parse_remote_url(&state, "url", u).await?;
            (Some(u), c)
        }
        None => (None, None),
    };
    let creds =
        Credentials::from_parts(body.username.as_deref(), body.password_or_token.as_deref())
            .or(url_creds);
    let sealed = creds.as_ref().map(|c| c.seal(&state)).transpose()?;
    let interval = body.interval_minutes.map(validate_interval).transpose()?;
    let mut tx = Tx::begin(&state).await?;
    let row: MirrorRow = sqlx::query_as(&format!(
        "UPDATE repo_mirrors
            SET url = coalesce($2, url),
                enc_credentials = CASE WHEN $3::bytea IS NOT NULL THEN $3
                                       WHEN $4 THEN NULL ELSE enc_credentials END,
                interval_minutes = coalesce($5, interval_minutes),
                enabled = coalesce($6, enabled),
                include_lfs = coalesce($7, include_lfs),
                next_sync_at = CASE WHEN $5 IS NOT NULL
                                    THEN coalesce(last_sync_at, now()) + make_interval(mins => $5)
                                    ELSE next_sync_at END,
                updated_at = now()
          WHERE repo_id = $1 RETURNING {}",
        MirrorRow::COLUMNS
    ))
    .bind(row.repo_id)
    .bind(&url)
    .bind(&sealed)
    .bind(body.clear_credentials)
    .bind(interval)
    .bind(body.enabled)
    .bind(body.include_lfs)
    .fetch_one(&mut *tx)
    .await?;
    if let Some(url) = &url {
        sqlx::query("UPDATE repositories SET mirror_url = $2, updated_at = now() WHERE id = $1")
            .bind(row.repo_id)
            .bind(url)
            .execute(&mut *tx)
            .await?;
        tx.sync_model(SyncModel::Repo, row.repo_id, SyncAction::Update)
            .await?;
    }
    audit::log(
        &mut *tx,
        Some(&auth.user),
        "repo.mirror_update",
        target(&access),
        json!({
            "url": url, "interval_minutes": interval, "enabled": body.enabled,
            "include_lfs": body.include_lfs,
            "credentials_changed": sealed.is_some() || body.clear_credentials,
        }),
    )
    .await?;
    tx.commit().await?;
    let syncing = is_syncing(&state, row.repo_id).await?;
    Ok(Json(MirrorJson::new(&row, syncing)))
}

/// `DELETE /_bgh/repos/{owner}/{repo}/mirror`: stop mirroring and make the
/// repository a regular, writable one.
pub async fn convert(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    let (access, row) = load_admin(&state, &auth, &owner, &repo).await?;
    let mut tx = Tx::begin(&state).await?;
    sqlx::query("DELETE FROM repo_mirrors WHERE repo_id = $1")
        .bind(row.repo_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE repositories SET mirror_url = NULL, updated_at = now() WHERE id = $1")
        .bind(row.repo_id)
        .execute(&mut *tx)
        .await?;
    audit::log(
        &mut *tx,
        Some(&auth.user),
        "repo.mirror_convert",
        target(&access),
        json!({ "url": row.url }),
    )
    .await?;
    tx.sync_model(SyncModel::Repo, row.repo_id, SyncAction::Update)
        .await?;
    tx.emit(Event::RepositoryUpdated {
        repo_id: row.repo_id,
        actor_id: auth.user.id,
    });
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /_bgh/repos/{owner}/{repo}/mirror/sync` → 202
pub async fn sync_now(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<(StatusCode, Json<MirrorJson>)> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    access.require(Permission::Write)?;
    let row = MirrorRow::find(&state.db, access.repo.id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if !is_syncing(&state, row.repo_id).await? {
        bgh_core::jobs::enqueue_job(
            &state.db,
            &MirrorSync {
                repo_id: row.repo_id,
            },
        )
        .await?;
    }
    Ok((StatusCode::ACCEPTED, Json(MirrorJson::new(&row, true))))
}

#[derive(Debug, Deserialize)]
pub struct AdminListQuery {
    /// `failed` (default) or `all`.
    pub status: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct AdminMirrorJson {
    pub repository: String,
    pub html_url: String,
    #[serde(flatten)]
    pub mirror: MirrorJson,
}

#[derive(sqlx::FromRow)]
struct AdminRow {
    owner_login: String,
    repo_name: String,
    #[sqlx(flatten)]
    mirror: MirrorRow,
}

/// `GET /_bgh/admin/mirrors?status=failed|all` (site admins).
pub async fn admin_list(
    State(state): State<AppState>,
    _admin: RequireSiteAdmin,
    p: Pagination,
    Query(q): Query<AdminListQuery>,
) -> ApiResult<Page<AdminMirrorJson>> {
    let failed_only = match q.status.as_deref() {
        None | Some("failed") => true,
        Some("all") => false,
        Some(_) => {
            return Err(ApiError::invalid_field(FieldError::invalid(
                "Mirror", "status",
            )));
        }
    };
    let rows: Vec<AdminRow> = sqlx::query_as(&format!(
        "SELECT o.login AS owner_login, r.name AS repo_name, {}
           FROM repo_mirrors m
           JOIN repositories r ON r.id = m.repo_id
           JOIN users o ON o.id = r.owner_id
          WHERE NOT $1 OR m.last_status = 'failed'
          ORDER BY m.last_sync_at DESC NULLS LAST, m.repo_id
          LIMIT $2 OFFSET $3",
        db::prefixed("m", MirrorRow::COLUMNS)
    ))
    .bind(failed_only)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    Ok(p.page(rows).map(|r| AdminMirrorJson {
        repository: format!("{}/{}", r.owner_login, r.repo_name),
        html_url: state.urls.repo_html(&r.owner_login, &r.repo_name),
        mirror: MirrorJson::new(&r.mirror, false),
    }))
}

// ---------------------------------------------------------------------------
// Sync
// ---------------------------------------------------------------------------

/// Fetch a mirror from upstream now.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MirrorSync {
    pub repo_id: i64,
}

impl JobPayload for MirrorSync {
    const KIND: &'static str = "repos.mirror_sync";
    // The schedule is the retry.
    const MAX_ATTEMPTS: i32 = 1;
}

pub async fn sync_job(state: AppState, job: MirrorSync) -> anyhow::Result<()> {
    let Some(row) = MirrorRow::find(&state.db, job.repo_id).await? else {
        return Ok(()); // converted or deleted
    };
    let Some(repo) = db::Repository::find(&state.db, row.repo_id).await? else {
        return Ok(());
    };
    if repo.archived || repo.disabled {
        return Ok(());
    }
    let importing: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM repo_imports WHERE repo_id = $1 AND status <> 'complete')",
    )
    .bind(repo.id)
    .fetch_one(&state.db)
    .await?;
    if importing {
        return Ok(()); // the import does the first fetch
    }
    let result = sync(&state, &row).await;
    let (status, error) = match &result {
        Ok(()) => ("success", None),
        Err(e) => ("failed", Some(e.as_str())),
    };
    sqlx::query(
        "UPDATE repo_mirrors
            SET last_sync_at = now(), last_status = $2, last_error = $3,
                consecutive_failures = CASE WHEN $2 = 'failed' THEN consecutive_failures + 1
                                            ELSE 0 END,
                next_sync_at = now() + make_interval(mins => interval_minutes),
                updated_at = now()
          WHERE repo_id = $1",
    )
    .bind(row.repo_id)
    .bind(status)
    .bind(error)
    .execute(&state.db)
    .await?;
    if let Err(e) = result {
        tracing::info!(repo_id = row.repo_id, error = %e, "mirror sync failed");
    }
    Ok(())
}

/// One sync; errors are user-facing messages.
async fn sync(state: &AppState, row: &MirrorRow) -> Result<(), String> {
    let internal = |e: &dyn std::fmt::Display| {
        tracing::warn!(repo_id = row.repo_id, error = %e, "mirror sync step failed");
        "Internal error while syncing.".to_string()
    };
    let creds = row
        .enc_credentials
        .as_deref()
        .map(|c| Credentials::open(state, c))
        .transpose()
        .map_err(|e| internal(&e))?;
    let remote = crate::import::remote(state, &row.url, creds.as_ref(), SYNC_TIMEOUT).await?;
    let store = crate::store(state);
    let updates = fetch::fetch(
        &store,
        row.repo_id,
        &remote,
        true,
        &CancellationToken::new(),
        |_| {},
    )
    .await
    .map_err(|e| describe_error(&e))?;
    if row.include_lfs {
        let tips: Vec<String> = updates
            .iter()
            .filter(|u| !u.is_delete())
            .map(|u| u.new.clone())
            .collect();
        let old: Vec<String> = updates.iter().map(|u| u.old.clone()).collect();
        let pointers = fetch::lfs_pointers(&store, row.repo_id, &tips, &old)
            .await
            .map_err(|e| internal(&e))?;
        crate::import::fetch_lfs(
            state,
            row.repo_id,
            &row.url,
            creds.as_ref(),
            pointers,
            row.creator_id,
            |_, _| {},
        )
        .await
        .map_err(|e| e.to_string())?;
    }
    crate::import::after_fetch(
        state,
        row.repo_id,
        row.creator_id,
        updates,
        PushEvent::ORIGIN_MIRROR,
    )
    .await
    .map_err(|e| internal(&e))
}

/// Enqueue syncs for due mirrors (safe to run from several processes).
pub async fn enqueue_due(state: &AppState) -> anyhow::Result<Vec<i64>> {
    let mut tx = state.db.begin().await?;
    let due: Vec<i64> = sqlx::query_scalar(
        "UPDATE repo_mirrors m
            SET next_sync_at = now() + make_interval(mins => m.interval_minutes)
          WHERE m.repo_id IN (SELECT repo_id FROM repo_mirrors
                               WHERE enabled AND next_sync_at <= now()
                               ORDER BY next_sync_at LIMIT 100
                               FOR UPDATE SKIP LOCKED)
          RETURNING m.repo_id",
    )
    .fetch_all(&mut *tx)
    .await?;
    for repo_id in &due {
        bgh_core::jobs::enqueue_job(&mut *tx, &MirrorSync { repo_id: *repo_id }).await?;
    }
    tx.commit().await?;
    Ok(due)
}

/// `repos.mirrors` service: schedule mirror syncs and fail imports whose
/// process died.
pub async fn service(state: AppState, shutdown: CancellationToken) -> anyhow::Result<()> {
    loop {
        if let Err(e) = enqueue_due(&state).await {
            tracing::warn!(error = %e, "mirror scheduling failed");
        }
        if let Err(e) = crate::import::sweep_stale(&state).await {
            tracing::warn!(error = %e, "stale import sweep failed");
        }
        tokio::select! {
            _ = tokio::time::sleep(POLL) => {}
            _ = shutdown.cancelled() => return Ok(()),
        }
    }
}
