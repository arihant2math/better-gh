//! Secret scanning REST API: repository and organization alert lists,
//! alert detail and resolution, locations, push protection bypasses and
//! scan history; plus the web client's internal endpoints (settings,
//! push block detail, manual scan).

use std::collections::HashMap;

use axum::extract::State;
use axum::http::StatusCode;
use bgh_core::audit::{self, Target};
use bgh_core::pagination::Page;
use bgh_core::prelude::*;
use bgh_core::secret_scanning::{self as ss, AlertRow, LocationRow};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::settings::{self, Effective};

const RESOLUTIONS: &[&str] = &["false_positive", "wont_fix", "revoked", "used_in_tests"];

/// Repository admin with secret scanning on (404 otherwise).
async fn load(
    state: &AppState,
    auth: Option<&AuthContext>,
    owner: &str,
    repo: &str,
) -> ApiResult<(RepoAccess, Effective)> {
    let auth = auth.ok_or_else(ApiError::requires_auth)?;
    let access = RepoAccess::load(state, Some(auth), owner, repo).await?;
    access.require(Permission::Admin)?;
    let eff = settings::effective(state, access.repo.id).await?;
    settings::require_enabled(&eff)?;
    Ok((access, eff))
}

fn invalid(field: &str) -> ApiError {
    ApiError::invalid_field(FieldError::invalid("SecretScanningAlert", field))
}

#[derive(Debug, Default, Deserialize)]
pub struct ListQuery {
    pub state: Option<String>,
    pub secret_type: Option<String>,
    pub resolution: Option<String>,
    pub sort: Option<String>,
    pub direction: Option<String>,
}

/// Filters shared by the repository and organization lists, as SQL
/// conditions on alias `a` (binds `$2..$4`) and an `ORDER BY`.
struct Filters {
    state: Option<String>,
    types: Option<Vec<String>>,
    resolutions: Option<Vec<String>>,
    order: &'static str,
}

fn csv(v: &Option<String>) -> Option<Vec<String>> {
    v.as_deref()
        .map(|s| {
            s.split(',')
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect::<Vec<_>>()
        })
        .filter(|v| !v.is_empty())
}

impl Filters {
    fn parse(q: &ListQuery) -> ApiResult<Self> {
        let state = match q.state.as_deref() {
            None | Some("") => None,
            Some(s @ ("open" | "resolved")) => Some(s.to_string()),
            Some(_) => return Err(invalid("state")),
        };
        let resolutions = csv(&q.resolution);
        if let Some(rs) = &resolutions
            && rs.iter().any(|r| {
                !RESOLUTIONS.contains(&r.as_str())
                    && r != "pattern_deleted"
                    && r != "pattern_edited"
            })
        {
            return Err(invalid("resolution"));
        }
        let desc = match q.direction.as_deref() {
            None | Some("desc") => true,
            Some("asc") => false,
            Some(_) => return Err(invalid("direction")),
        };
        let order = match (q.sort.as_deref(), desc) {
            (None | Some("created"), true) => "a.created_at DESC, a.id DESC",
            (None | Some("created"), false) => "a.created_at ASC, a.id ASC",
            (Some("updated"), true) => "a.updated_at DESC, a.id DESC",
            (Some("updated"), false) => "a.updated_at ASC, a.id ASC",
            _ => return Err(invalid("sort")),
        };
        Ok(Self {
            state,
            types: csv(&q.secret_type),
            resolutions,
            order,
        })
    }

    const WHERE: &'static str = "($2::text IS NULL OR a.state = $2)
          AND ($3::text[] IS NULL OR a.secret_type = ANY($3))
          AND ($4::text[] IS NULL OR a.resolution = ANY($4))";
}

fn prefixed_columns() -> String {
    db::prefixed("a", AlertRow::COLUMNS)
}

/// `GET /repos/{owner}/{repo}/secret-scanning/alerts`
pub async fn list_repo(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo)): Path<(String, String)>,
    Query(q): Query<ListQuery>,
) -> ApiResult<Page<Value>> {
    let (access, _) = load(&state, auth.as_ref(), &owner, &repo).await?;
    let f = Filters::parse(&q)?;
    let rows: Vec<AlertRow> = sqlx::query_as(&format!(
        "SELECT {} FROM secret_scanning_alerts a WHERE a.repo_id = $1 AND {}
          ORDER BY {} LIMIT $5 OFFSET $6",
        prefixed_columns(),
        Filters::WHERE,
        f.order
    ))
    .bind(access.repo.id)
    .bind(&f.state)
    .bind(&f.types)
    .bind(&f.resolutions)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let page = p.page(rows);
    let items = ss::render(&state, &access.owner.login, &access.repo.name, &page.items).await?;
    Ok(Page {
        items,
        link: page.link,
    })
}

/// `GET /orgs/{org}/secret-scanning/alerts` (organization owners).
pub async fn list_org(
    State(state): State<AppState>,
    auth: RequireUser,
    p: Pagination,
    Path(org): Path<String>,
    Query(q): Query<ListQuery>,
) -> ApiResult<Page<Value>> {
    let org = db::User::find_by_login(&state.db, &org)
        .await?
        .filter(|u| u.is_org())
        .ok_or(ApiError::NotFound)?;
    if !auth.user.site_admin {
        match bgh_core::perms::org_role(&state.db, org.id, auth.user.id).await? {
            Some(r) if r == "admin" => {}
            Some(_) => return Err(ApiError::forbidden("Must be an organization owner.")),
            None => return Err(ApiError::NotFound),
        }
    }
    if !auth.has_scope("repo") && !auth.has_scope("security_events") {
        return Err(ApiError::forbidden(
            "Resource not accessible by personal access token",
        ));
    }
    let f = Filters::parse(&q)?;
    let site = bgh_core::settings::load(&state).await?;
    if !site.secret_scanning.available {
        return Ok(p.page(Vec::new()));
    }
    let rows: Vec<AlertRow> = sqlx::query_as(&format!(
        "SELECT {} FROM secret_scanning_alerts a
           JOIN repositories r ON r.id = a.repo_id
           LEFT JOIN repo_security_settings s ON s.repo_id = r.id
          WHERE r.owner_id = $1 AND ($7 OR coalesce(s.secret_scanning, false)) AND {}
          ORDER BY {} LIMIT $5 OFFSET $6",
        prefixed_columns(),
        Filters::WHERE,
        f.order
    ))
    .bind(org.id)
    .bind(&f.state)
    .bind(&f.types)
    .bind(&f.resolutions)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .bind(site.secret_scanning.enable_all)
    .fetch_all(&state.db)
    .await?;
    let page = p.page(rows);
    // Render per repository (one batch each), then restore the order.
    let mut repo_ids: Vec<i64> = page.items.iter().map(|a| a.repo_id).collect();
    repo_ids.sort_unstable();
    repo_ids.dedup();
    let repos: Vec<db::Repository> = sqlx::query_as(&format!(
        "SELECT {} FROM repositories WHERE id = ANY($1)",
        db::Repository::COLUMNS
    ))
    .bind(&repo_ids)
    .fetch_all(&state.db)
    .await?;
    let mut rendered: HashMap<i64, Value> = HashMap::new();
    for repo in &repos {
        let group: Vec<AlertRow> = page
            .items
            .iter()
            .filter(|a| a.repo_id == repo.id)
            .cloned()
            .collect();
        let minimal =
            serde_json::to_value(api::MinimalRepository::new(&state.urls, repo, &org, None))
                .map_err(anyhow::Error::from)?;
        for (row, mut v) in group
            .iter()
            .zip(ss::render(&state, &org.login, &repo.name, &group).await?)
        {
            v["repository"] = minimal.clone();
            rendered.insert(row.id, v);
        }
    }
    let items = page
        .items
        .iter()
        .filter_map(|a| rendered.remove(&a.id))
        .collect();
    Ok(Page {
        items,
        link: page.link,
    })
}

async fn find_alert(state: &AppState, repo_id: i64, number: i64) -> ApiResult<AlertRow> {
    sqlx::query_as(&format!(
        "SELECT {} FROM secret_scanning_alerts WHERE repo_id = $1 AND number = $2",
        AlertRow::COLUMNS
    ))
    .bind(repo_id)
    .bind(number)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)
}

async fn render_one(state: &AppState, access: &RepoAccess, row: &AlertRow) -> ApiResult<Value> {
    let mut v = ss::render(
        state,
        &access.owner.login,
        &access.repo.name,
        std::slice::from_ref(row),
    )
    .await?;
    Ok(v.remove(0))
}

/// `GET /repos/{owner}/{repo}/secret-scanning/alerts/{alert_number}`
pub async fn get(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, number)): Path<(String, String, i64)>,
) -> ApiResult<Json<Value>> {
    let (access, _) = load(&state, auth.as_ref(), &owner, &repo).await?;
    let row = find_alert(&state, access.repo.id, number).await?;
    Ok(Json(render_one(&state, &access, &row).await?))
}

#[derive(Debug, Default, Deserialize)]
pub struct UpdateBody {
    pub state: Option<String>,
    pub resolution: Option<String>,
    pub resolution_comment: Option<String>,
}

/// `PATCH /repos/{owner}/{repo}/secret-scanning/alerts/{alert_number}`
pub async fn update(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    Json(body): Json<UpdateBody>,
) -> ApiResult<Json<Value>> {
    let (access, _) = load(&state, auth.as_ref(), &owner, &repo).await?;
    let user = &auth.as_ref().expect("checked by load").user;
    let old = find_alert(&state, access.repo.id, number).await?;
    let resolve = match body.state.as_deref() {
        Some("resolved") => true,
        Some("open") => false,
        Some(_) => return Err(invalid("state")),
        None => {
            return Err(ApiError::invalid_field(FieldError::missing_field(
                "SecretScanningAlert",
                "state",
            )));
        }
    };
    if resolve {
        match body.resolution.as_deref() {
            Some(r) if RESOLUTIONS.contains(&r) => {}
            Some(_) => return Err(invalid("resolution")),
            None => {
                return Err(ApiError::unprocessable(
                    "resolution is required when state is resolved",
                ));
            }
        }
    }
    if body
        .resolution_comment
        .as_deref()
        .is_some_and(|c| c.chars().count() > 280)
    {
        return Err(ApiError::invalid_field(FieldError::custom(
            "SecretScanningAlert",
            "resolution_comment",
            "resolution_comment is too long (maximum is 280 characters)",
        )));
    }
    let mut tx = Tx::begin(&state).await?;
    let row: AlertRow = if resolve {
        sqlx::query_as(&format!(
            "UPDATE secret_scanning_alerts SET state = 'resolved', resolution = $2,
                    resolution_comment = $3, resolved_by_id = $4,
                    resolved_at = CASE WHEN state = 'resolved' THEN resolved_at ELSE now() END,
                    updated_at = now()
              WHERE id = $1 RETURNING {}",
            AlertRow::COLUMNS
        ))
        .bind(old.id)
        .bind(&body.resolution)
        .bind(&body.resolution_comment)
        .bind(user.id)
        .fetch_one(&mut *tx)
        .await?
    } else {
        sqlx::query_as(&format!(
            "UPDATE secret_scanning_alerts SET state = 'open', resolution = NULL,
                    resolution_comment = $2, resolved_by_id = NULL, resolved_at = NULL,
                    updated_at = now()
              WHERE id = $1 RETURNING {}",
            AlertRow::COLUMNS
        ))
        .bind(old.id)
        .bind(&body.resolution_comment)
        .fetch_one(&mut *tx)
        .await?
    };
    if row.state != old.state {
        let action = if resolve { "resolved" } else { "reopened" };
        tx.emit(Event::SecretScanningAlert {
            repo_id: access.repo.id,
            alert_id: row.id,
            action: action.into(),
            actor_id: Some(user.id),
        });
        audit::log(
            &mut *tx,
            Some(user),
            &format!(
                "secret_scanning_alert.{}",
                if resolve { "resolve" } else { "reopen" }
            ),
            Target::Repo {
                id: access.repo.id,
                org_id: access.owner.is_org().then_some(access.owner.id),
            },
            json!({"number": row.number, "resolution": row.resolution}),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(Json(render_one(&state, &access, &row).await?))
}

/// `GET /repos/{owner}/{repo}/secret-scanning/alerts/{alert_number}/locations`
pub async fn locations(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo, number)): Path<(String, String, i64)>,
) -> ApiResult<Page<Value>> {
    let (access, _) = load(&state, auth.as_ref(), &owner, &repo).await?;
    let alert = find_alert(&state, access.repo.id, number).await?;
    let rows: Vec<LocationRow> = sqlx::query_as(&format!(
        "SELECT {} FROM secret_scanning_locations WHERE alert_id = $1
          ORDER BY id LIMIT $2 OFFSET $3",
        LocationRow::COLUMNS
    ))
    .bind(alert.id)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    Ok(p.page(rows)
        .map(|l| ss::location(&state, &access.owner.login, &access.repo.name, &l)))
}

// ----- push protection bypasses --------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct PushBlockRow {
    pub id: i64,
    pub placeholder_id: String,
    pub repo_id: i64,
    pub user_id: Option<i64>,
    pub secret_type: String,
    pub secret_type_display_name: String,
    pub secret_hash: String,
    pub secret_preview: String,
    pub commit_sha: String,
    pub path: String,
    pub start_line: i32,
    pub reason: Option<String>,
    pub bypassed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

const BLOCK_COLUMNS: &str = "id, placeholder_id, repo_id, user_id, secret_type, \
    secret_type_display_name, secret_hash, secret_preview, commit_sha, path, start_line, reason, \
    bypassed_at, expires_at, created_at";

/// How long a bypass lets its user push the secret (GitHub: 3 hours).
pub const BYPASS_TTL_HOURS: i64 = 3;

/// The pusher who was blocked, or a repository admin (404 for others:
/// placeholders are not enumerable).
async fn load_block(
    state: &AppState,
    auth: &AuthContext,
    owner: &str,
    repo: &str,
    placeholder: &str,
) -> ApiResult<(RepoAccess, PushBlockRow)> {
    let access = RepoAccess::load(state, Some(auth), owner, repo).await?;
    access.require(Permission::Write)?;
    let row: PushBlockRow = sqlx::query_as(&format!(
        "SELECT {BLOCK_COLUMNS} FROM secret_scanning_push_blocks
          WHERE placeholder_id = $1 AND repo_id = $2"
    ))
    .bind(placeholder)
    .bind(access.repo.id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)?;
    if row.user_id != Some(auth.user.id) && access.permission < Permission::Admin {
        return Err(ApiError::NotFound);
    }
    Ok((access, row))
}

fn block_json(state: &AppState, access: &RepoAccess, b: &PushBlockRow) -> Value {
    json!({
        "placeholder_id": b.placeholder_id,
        "secret_type": b.secret_type,
        "secret_type_display_name": b.secret_type_display_name,
        "secret_preview": b.secret_preview,
        "commit_sha": b.commit_sha,
        "path": b.path,
        "start_line": b.start_line,
        "created_at": Timestamp(b.created_at),
        "reason": b.reason,
        "bypassed_at": bgh_core::time::ts(b.bypassed_at),
        "expires_at": bgh_core::time::ts(b.expires_at),
        "unblock_url": crate::push::unblock_url(
            state, &access.owner.login, &access.repo.name, &b.placeholder_id),
    })
}

/// `GET /_bgh/repos/{owner}/{repo}/secret-scanning/push-blocks/{placeholder_id}`
pub async fn get_block(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, placeholder)): Path<(String, String, String)>,
) -> ApiResult<Json<Value>> {
    let (access, row) = load_block(&state, &auth, &owner, &repo, &placeholder).await?;
    Ok(Json(block_json(&state, &access, &row)))
}

#[derive(Debug, Default, Deserialize)]
pub struct BypassBody {
    pub reason: Option<String>,
    pub placeholder_id: Option<String>,
}

/// `POST /repos/{owner}/{repo}/secret-scanning/push-protection-bypasses`
pub async fn bypass(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo)): Path<(String, String)>,
    Json(body): Json<BypassBody>,
) -> ApiResult<Json<Value>> {
    let reason = match body.reason.as_deref() {
        Some(r @ ("false_positive" | "used_in_tests" | "will_fix_later")) => r.to_string(),
        Some(_) => {
            return Err(ApiError::invalid_field(FieldError::invalid(
                "PushProtectionBypass",
                "reason",
            )));
        }
        None => {
            return Err(ApiError::invalid_field(FieldError::missing_field(
                "PushProtectionBypass",
                "reason",
            )));
        }
    };
    let placeholder = body.placeholder_id.unwrap_or_default();
    let (access, row) = load_block(&state, &auth, &owner, &repo, &placeholder).await?;
    let mut tx = Tx::begin(&state).await?;
    // An admin bypassing someone else's block allows that pusher.
    let (expires,): (chrono::DateTime<chrono::Utc>,) = sqlx::query_as(
        "UPDATE secret_scanning_push_blocks
            SET reason = $2, bypassed_at = now(),
                expires_at = now() + make_interval(hours => $3::int)
          WHERE id = $1 RETURNING expires_at",
    )
    .bind(row.id)
    .bind(&reason)
    .bind(BYPASS_TTL_HOURS as i32)
    .fetch_one(&mut *tx)
    .await?;
    audit::log(
        &mut *tx,
        Some(&auth.user),
        "secret_scanning_push_protection.bypass",
        Target::Repo {
            id: access.repo.id,
            org_id: access.owner.is_org().then_some(access.owner.id),
        },
        json!({"secret_type": row.secret_type, "reason": reason, "placeholder_id": placeholder}),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(json!({
        "reason": reason,
        "expire_at": Timestamp(expires),
        "token_type": row.secret_type,
    })))
}

// ----- settings, scans --------------------------------------------------------

/// `GET /_bgh/repos/{owner}/{repo}/secret-scanning/settings` (admins).
pub async fn get_settings(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    access.require(Permission::Admin)?;
    let e = settings::effective(&state, access.repo.id).await?;
    let open: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM secret_scanning_alerts WHERE repo_id = $1 AND state = 'open'",
    )
    .bind(access.repo.id)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(json!({
        "available": e.available,
        "secret_scanning": e.secret_scanning,
        "push_protection": e.push_protection,
        "non_provider_patterns": e.non_provider_patterns,
        "enforced_by_site": {
            "secret_scanning": e.forced_scanning,
            "push_protection": e.forced_push_protection,
        },
        "open_alerts": if e.secret_scanning { open } else { 0 },
    })))
}

/// `POST /_bgh/repos/{owner}/{repo}/secret-scanning/scan`: queue a
/// history scan now (admins).
pub async fn scan_now(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let (access, _) = load(&state, Some(&auth), &owner, &repo).await?;
    let mut tx = Tx::begin(&state).await?;
    let id = crate::jobs::enqueue_history_scan(&mut tx, access.repo.id, "backfill").await?;
    tx.commit().await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({"id": id, "status": "pending"})),
    ))
}

#[derive(sqlx::FromRow)]
struct ScanRow {
    kind: String,
    status: String,
    started_at: chrono::DateTime<chrono::Utc>,
    completed_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// `GET /repos/{owner}/{repo}/secret-scanning/scan-history` (latest 20 of
/// each kind).
pub async fn scan_history(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let (access, _) = load(&state, auth.as_ref(), &owner, &repo).await?;
    let rows: Vec<ScanRow> = sqlx::query_as(
        "SELECT kind, status, started_at, completed_at FROM (
             SELECT *, row_number() OVER (PARTITION BY kind ORDER BY started_at DESC, id DESC) AS n
               FROM secret_scanning_scans WHERE repo_id = $1) s
          WHERE n <= 20 ORDER BY started_at DESC, id DESC",
    )
    .bind(access.repo.id)
    .fetch_all(&state.db)
    .await?;
    let mut out = json!({
        "incremental_scans": [],
        "pattern_update_scans": [],
        "backfill_scans": [],
        "custom_pattern_backfill_scans": [],
    });
    for r in rows {
        let key = match r.kind.as_str() {
            "incremental" => "incremental_scans",
            "pattern_update" => "pattern_update_scans",
            "custom_pattern_backfill" => "custom_pattern_backfill_scans",
            _ => "backfill_scans",
        };
        if let Some(list) = out[key].as_array_mut() {
            list.push(json!({
                "type": if r.kind == "incremental" { "git" } else { r.kind.as_str() },
                "status": r.status,
                "started_at": Timestamp(r.started_at),
                "completed_at": bgh_core::time::ts(r.completed_at),
            }));
        }
    }
    Ok(Json(out))
}
