//! Private endpoints (`/_bgh/metadata-imports`) and the service used by
//! them and the CLI.
//!
//! Who: site administrators (any owner) and organization owners (their
//! organization). Mannequins are site-wide accounts, so personal accounts
//! are import targets only for site administrators. The source token is
//! write-only: sealed with `bgh_core::secretbox`, never returned, logged or
//! audited.

use std::collections::{BTreeMap, HashMap};

use axum::extract::State;
use axum::http::StatusCode;
use bgh_core::prelude::*;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::client::{GitHub, HttpError};
use crate::pipeline::{RunImport, STALE_AFTER_SECS};
use crate::row::{self, ImportRow, Options};

pub const DEFAULT_API_URL: &str = "https://api.github.com";

/// `POST /_bgh/metadata-imports`
#[derive(Debug, Default, Deserialize)]
pub struct CreateBody {
    /// `https://api.github.com` (default) or `https://HOST/api/v3`.
    pub api_url: Option<String>,
    /// `owner/name` on the source.
    pub source_repo: Option<String>,
    pub token: Option<String>,
    /// Target owner login (organization, or a user for site admins).
    pub owner: Option<String>,
    /// Target name (default: the source name).
    pub name: Option<String>,
    /// `public` | `private` | `internal` (default: the source's).
    pub visibility: Option<String>,
    pub git: Option<bool>,
    pub settings: Option<bool>,
    pub labels: Option<bool>,
    pub milestones: Option<bool>,
    pub issues: Option<bool>,
    pub releases: Option<bool>,
    pub teams: Option<bool>,
    pub include_lfs: Option<bool>,
    /// Source login → local login.
    #[serde(default)]
    pub user_map: BTreeMap<String, String>,
}

fn invalid(field: &str, message: &str) -> ApiError {
    ApiError::invalid_field(FieldError::custom("Import", field, message))
}

fn valid_source_repo(s: &str) -> bool {
    let mut parts = s.split('/');
    let ok = |p: Option<&str>| {
        p.is_some_and(|p| {
            !p.is_empty()
                && p != "."
                && p != ".."
                && p.len() <= 100
                && p.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        })
    };
    ok(parts.next()) && ok(parts.next()) && parts.next().is_none()
}

/// May `user` start an import into `owner`?
async fn may_import_into(state: &AppState, user: &db::User, owner: &db::User) -> ApiResult<bool> {
    if user.site_admin {
        return Ok(true);
    }
    if owner.kind == "Organization" {
        return Ok(bgh_core::perms::org_role(&state.db, owner.id, user.id)
            .await?
            .as_deref()
            == Some("admin"));
    }
    Ok(false)
}

/// May `user` see and control `row`?
async fn may_manage(state: &AppState, user: &db::User, row: &ImportRow) -> ApiResult<bool> {
    if user.site_admin || row.created_by == Some(user.id) {
        return Ok(true);
    }
    Ok(bgh_core::perms::org_role(&state.db, row.owner_id, user.id)
        .await?
        .as_deref()
        == Some("admin"))
}

/// Validate, check the source, create the target repository (with P11's
/// git import unless disabled) and queue the run.
pub async fn create_import(
    state: &AppState,
    auth: &AuthContext,
    body: CreateBody,
) -> ApiResult<ImportRow> {
    auth.require_scope("repo")?;
    let source_repo = body
        .source_repo
        .as_deref()
        .map(|s| s.trim().trim_end_matches(".git").trim_matches('/'))
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ApiError::invalid_field(FieldError::missing_field("Import", "source_repo")))?
        .to_string();
    if !valid_source_repo(&source_repo) {
        return Err(invalid("source_repo", "must be owner/name"));
    }
    let api_url = body
        .api_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_API_URL)
        .trim_end_matches('/')
        .to_string();
    let policy = bgh_core::ssrf::Policy::load(state).await;
    bgh_core::ssrf::validate_url(&policy, &api_url).map_err(|e| invalid("api_url", &e))?;

    let owner_login = body
        .owner
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ApiError::invalid_field(FieldError::missing_field("Import", "owner")))?;
    let owner = db::User::find_by_login(&state.db, owner_login)
        .await?
        .ok_or_else(|| invalid("owner", "does not exist"))?;
    if !may_import_into(state, &auth.user, &owner).await? {
        return Err(ApiError::forbidden(
            "Must be an organization owner or a site administrator.",
        ));
    }

    let token = body
        .token
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty());
    let gh = GitHub::new(state, &api_url, token.map(str::to_string), None)
        .await
        .map_err(|e| invalid("api_url", &e.to_string()))?;
    let src = match gh.get(&format!("/repos/{source_repo}")).await {
        Ok((v, _)) => v,
        Err(e) => {
            return Err(match e.downcast_ref::<HttpError>() {
                Some(h) if h.status == 401 => invalid("token", "Bad credentials"),
                Some(h) if h.status == 404 || h.status == 403 => invalid(
                    "source_repo",
                    "not found on the source, or the token can't read it",
                ),
                _ => invalid("api_url", &format!("the source API is unreachable: {e}")),
            });
        }
    };

    let options = Options {
        git: body.git.unwrap_or(true),
        settings: body.settings.unwrap_or(true),
        labels: body.labels.unwrap_or(true),
        milestones: body.milestones.unwrap_or(true),
        issues: body.issues.unwrap_or(true),
        releases: body.releases.unwrap_or(true),
        teams: body.teams.unwrap_or(false),
        include_lfs: body.include_lfs.unwrap_or(false),
        user_map: body.user_map,
    };
    let name = body
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            source_repo
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .to_string()
        });
    let visibility = body
        .visibility
        .clone()
        .or_else(|| src["visibility"].as_str().map(str::to_string))
        .unwrap_or_else(|| {
            if src["private"].as_bool().unwrap_or(true) {
                "private".into()
            } else {
                "public".into()
            }
        });
    // `internal` only exists for organizations.
    let visibility = if visibility == "internal" && owner.kind != "Organization" {
        "private".to_string()
    } else {
        visibility
    };

    let git = if options.git {
        let clone_url = src["clone_url"]
            .as_str()
            .ok_or_else(|| invalid("source_repo", "the source has no clone_url"))?;
        let (url, _) = bgh_repos::import::parse_remote_url(state, "source_repo", clone_url).await?;
        let credentials = match token {
            Some(t) => Some(
                bgh_repos::import::Credentials::from_parts(Some("x-access-token"), Some(t))
                    .expect("both parts")
                    .seal(state)?,
            ),
            None => None,
        };
        Some(bgh_repos::import::NewImport {
            source_url: url,
            credentials,
            mirror: false,
            include_lfs: options.include_lfs,
            interval_minutes: 480,
            quiet: true,
        })
    } else {
        None
    };
    let access = bgh_repos::create::create_with(
        state,
        auth,
        owner.clone(),
        bgh_repos::create::CreateRepoBody {
            name: Some(name.clone()),
            description: src["description"].as_str().map(str::to_string),
            visibility: Some(visibility.clone()),
            ..Default::default()
        },
        git,
    )
    .await?;

    let sealed = match token {
        Some(t) => Some(bgh_core::secretbox::seal(state, t)?),
        None => None,
    };
    let mut tx = Tx::begin(state).await?;
    if options.labels {
        // The new repository's default labels: the source's set replaces
        // them (only now, while nothing can reference them yet).
        let seeded: Vec<i64> =
            sqlx::query_scalar("DELETE FROM labels WHERE repo_id = $1 RETURNING id")
                .bind(access.repo.id)
                .fetch_all(&mut *tx)
                .await?;
        let scope = bgh_core::sync::repo_scope(access.repo.id);
        for id in seeded {
            tx.sync_delete(&scope, SyncModel::Label, id).await?;
        }
    }
    let row: ImportRow = sqlx::query_as(&format!(
        "INSERT INTO imports (api_url, source_repo, enc_token, owner_id, repo_name, repo_id,
                              visibility, options, created_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING {}",
        ImportRow::COLUMNS
    ))
    .bind(&api_url)
    .bind(&source_repo)
    .bind(&sealed)
    .bind(owner.id)
    .bind(&access.repo.name)
    .bind(access.repo.id)
    .bind(&access.repo.visibility)
    .bind(serde_json::to_value(&options).map_err(ApiError::internal)?)
    .bind(auth.user.id)
    .fetch_one(&mut *tx)
    .await?;
    bgh_core::audit::log(
        &mut *tx,
        Some(&auth.user),
        "repo.metadata_import",
        bgh_core::audit::Target::Repo {
            id: access.repo.id,
            org_id: (owner.kind == "Organization").then_some(owner.id),
        },
        json!({"import_id": row.id, "api_url": api_url, "source_repo": source_repo}),
    )
    .await?;
    tx.enqueue(&RunImport { import_id: row.id }).await?;
    tx.commit().await?;
    row::log(
        &state.db,
        row.id,
        "info",
        &format!("import of {source_repo} from {api_url} queued"),
    )
    .await;
    Ok(row)
}

/// Render rows with their owners and repositories (two queries).
async fn render(state: &AppState, rows: &[ImportRow], with_git: bool) -> ApiResult<Vec<Value>> {
    let owner_ids: Vec<i64> = rows.iter().map(|r| r.owner_id).collect();
    let repo_ids: Vec<i64> = rows.iter().filter_map(|r| r.repo_id).collect();
    let owners: HashMap<i64, db::User> = sqlx::query_as::<_, db::User>(&format!(
        "SELECT {} FROM users WHERE id = ANY($1)",
        db::User::COLUMNS
    ))
    .bind(&owner_ids)
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .map(|u| (u.id, u))
    .collect();
    let repos: HashMap<i64, db::Repository> = sqlx::query_as::<_, db::Repository>(&format!(
        "SELECT {} FROM repositories WHERE id = ANY($1)",
        db::Repository::COLUMNS
    ))
    .bind(&repo_ids)
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .map(|r| (r.id, r))
    .collect();
    let mut gits: HashMap<i64, Value> = HashMap::new();
    if with_git {
        let rows: Vec<(i64, String, String, i64, i64, Option<String>)> = sqlx::query_as(
            "SELECT repo_id, status, phase, objects_received, objects_total, error
               FROM repo_imports WHERE repo_id = ANY($1)",
        )
        .bind(&repo_ids)
        .fetch_all(&state.db)
        .await?;
        for (repo_id, status, phase, received, total, error) in rows {
            gits.insert(
                repo_id,
                json!({"status": status, "phase": phase, "objects_received": received,
                       "objects_total": total, "error": error}),
            );
        }
    }
    Ok(rows
        .iter()
        .map(|r| {
            row::to_json(
                state,
                r,
                owners.get(&r.owner_id),
                r.repo_id.and_then(|id| repos.get(&id)),
                r.repo_id.and_then(|id| gits.remove(&id)),
            )
        })
        .collect())
}

pub async fn create(
    State(state): State<AppState>,
    auth: RequireUser,
    Json(body): Json<CreateBody>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let row = create_import(&state, &auth, body).await?;
    let json = render(&state, std::slice::from_ref(&row), true).await?;
    Ok((
        StatusCode::CREATED,
        Json(json.into_iter().next().unwrap_or_default()),
    ))
}

async fn load(state: &AppState, auth: &AuthContext, id: i64) -> ApiResult<ImportRow> {
    let row = ImportRow::find(&state.db, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if !may_manage(state, &auth.user, &row).await? {
        return Err(ApiError::NotFound);
    }
    Ok(row)
}

pub async fn get(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(id): Path<i64>,
) -> ApiResult<Json<Value>> {
    let row = load(&state, &auth, id).await?;
    let json = render(&state, std::slice::from_ref(&row), true).await?;
    Ok(Json(json.into_iter().next().unwrap_or_default()))
}

#[derive(Deserialize)]
pub struct LogQuery {
    /// Entries after this id (polling).
    #[serde(default)]
    after: i64,
}

/// `GET /_bgh/metadata-imports/{id}/log?after=N` → `{entries: [...]}`
/// (oldest first, at most 500).
pub async fn log(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(id): Path<i64>,
    Query(q): Query<LogQuery>,
) -> ApiResult<Json<Value>> {
    let row = load(&state, &auth, id).await?;
    let entries: Vec<(i64, String, String, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
        "SELECT id, level, message, created_at FROM import_log
          WHERE import_id = $1 AND id > $2 ORDER BY id LIMIT 500",
    )
    .bind(row.id)
    .bind(q.after)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(json!({
        "entries": entries
            .into_iter()
            .map(|(id, level, message, at)| json!({
                "id": id, "level": level, "message": message, "created_at": Timestamp::from(at),
            }))
            .collect::<Vec<_>>(),
    })))
}

pub async fn cancel(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(id): Path<i64>,
) -> ApiResult<Json<Value>> {
    let row = load(&state, &auth, id).await?;
    let updated = sqlx::query(
        "UPDATE imports SET status = 'cancelled', updated_at = now(), resume_at = NULL
          WHERE id = $1 AND status IN ('queued', 'running', 'waiting')",
    )
    .bind(row.id)
    .execute(&state.db)
    .await?
    .rows_affected();
    if updated == 0 {
        return Err(ApiError::unprocessable(
            "Only a queued or running import can be cancelled",
        ));
    }
    // Stop the git step too (P11 kills a running fetch when its row
    // leaves `importing`).
    if let Some(repo_id) = row.repo_id {
        sqlx::query(
            "UPDATE repo_imports SET status = 'cancelled', phase = 'cancelled', updated_at = now(),
                    completed_at = now()
              WHERE repo_id = $1 AND status IN ('queued', 'importing')",
        )
        .bind(repo_id)
        .execute(&state.db)
        .await?;
    }
    bgh_core::audit::log(
        &state.db,
        Some(&auth.user),
        "repo.metadata_import_cancel",
        bgh_core::audit::Target::Repo {
            id: row.repo_id.unwrap_or(0),
            org_id: None,
        },
        json!({"import_id": row.id}),
    )
    .await?;
    row::log(
        &state.db,
        row.id,
        "warn",
        &format!("cancelled by {}", auth.user.login),
    )
    .await;
    let row = load(&state, &auth, id).await?;
    let json = render(&state, std::slice::from_ref(&row), true).await?;
    Ok(Json(json.into_iter().next().unwrap_or_default()))
}

#[derive(Debug, Default, Deserialize)]
pub struct ResumeBody {
    /// Replace the source token.
    pub token: Option<String>,
}

/// Resume a failed, cancelled or stale run, or rerun a complete one (a
/// rerun only imports what is new on the source).
pub async fn resume(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(id): Path<i64>,
    body: axum::body::Bytes,
) -> ApiResult<Json<Value>> {
    let row = load(&state, &auth, id).await?;
    let body: ResumeBody = if body.iter().all(u8::is_ascii_whitespace) {
        ResumeBody::default()
    } else {
        serde_json::from_slice(&body).map_err(|_| ApiError::bad_request("Problems parsing JSON"))?
    };
    resume_import(&state, &auth.user, &row, body.token.as_deref()).await?;
    let row = load(&state, &auth, id).await?;
    let json = render(&state, std::slice::from_ref(&row), true).await?;
    Ok(Json(json.into_iter().next().unwrap_or_default()))
}

pub async fn resume_import(
    state: &AppState,
    actor: &db::User,
    row: &ImportRow,
    token: Option<&str>,
) -> ApiResult<()> {
    if row.repo_id.is_none() {
        return Err(ApiError::unprocessable("The target repository was deleted"));
    }
    let sealed = match token.map(str::trim).filter(|t| !t.is_empty()) {
        Some(t) => Some(bgh_core::secretbox::seal(state, t)?),
        None => None,
    };
    let mut tx = Tx::begin(state).await?;
    // A complete import reruns from the first step; others continue.
    let updated = sqlx::query(
        "UPDATE imports SET status = 'queued', error = NULL, resume_at = NULL, updated_at = now(),
                attempts = 0, enc_token = COALESCE($3, enc_token),
                step = CASE WHEN status = 'complete' THEN 'git' ELSE step END,
                cursor = CASE WHEN status = 'complete' THEN '{}'::jsonb ELSE cursor END
          WHERE id = $1 AND (status IN ('failed', 'cancelled', 'complete')
                OR (status = 'running' AND heartbeat_at < now() - make_interval(secs => $2)))",
    )
    .bind(row.id)
    .bind(STALE_AFTER_SECS as f64)
    .bind(&sealed)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if updated == 0 {
        return Err(ApiError::unprocessable(
            "The import is already queued or running",
        ));
    }
    // A failed or cancelled git step runs again (with the new token).
    let git_credentials = match token.map(str::trim).filter(|t| !t.is_empty()) {
        Some(t) => Some(
            bgh_repos::import::Credentials::from_parts(Some("x-access-token"), Some(t))
                .expect("both parts")
                .seal(state)?,
        ),
        None => None,
    };
    let git: Option<i64> = sqlx::query_scalar(
        "UPDATE repo_imports
            SET status = 'queued', phase = 'queued', error = NULL, objects_received = 0,
                objects_total = 0, bytes_received = 0, lfs_received = 0, lfs_total = 0,
                enc_credentials = COALESCE($2, enc_credentials), updated_at = now(),
                completed_at = NULL
          WHERE repo_id = $1 AND status IN ('failed', 'cancelled') RETURNING id",
    )
    .bind(row.repo_id)
    .bind(&git_credentials)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(import_id) = git {
        tx.enqueue(&bgh_repos::import::RunImport { import_id })
            .await?;
    }
    tx.enqueue(&RunImport { import_id: row.id }).await?;
    bgh_core::audit::log(
        &mut *tx,
        Some(actor),
        "repo.metadata_import_resume",
        bgh_core::audit::Target::Repo {
            id: row.repo_id.unwrap_or(0),
            org_id: None,
        },
        json!({"import_id": row.id, "new_token": sealed.is_some()}),
    )
    .await?;
    tx.commit().await?;
    row::log(
        &state.db,
        row.id,
        "info",
        &format!("resumed by {}", actor.login),
    )
    .await;
    Ok(())
}

pub async fn list_admin(
    State(state): State<AppState>,
    auth: RequireSiteAdmin,
    p: Pagination,
) -> ApiResult<Page<Value>> {
    let _ = &auth;
    let rows: Vec<ImportRow> = sqlx::query_as(&format!(
        "SELECT {} FROM imports ORDER BY id DESC LIMIT $1 OFFSET $2",
        ImportRow::COLUMNS
    ))
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let json = render(&state, &rows, false).await?;
    Ok(p.page(json))
}

pub async fn list_org(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(org): Path<String>,
    p: Pagination,
) -> ApiResult<Page<Value>> {
    let org = db::User::find_by_login(&state.db, &org)
        .await?
        .filter(|o| o.kind == "Organization")
        .ok_or(ApiError::NotFound)?;
    let role = bgh_core::perms::org_role(&state.db, org.id, auth.user.id).await?;
    if !auth.user.site_admin && role.as_deref() != Some("admin") {
        return Err(match role {
            Some(_) => ApiError::forbidden("Must be an organization owner."),
            None => ApiError::NotFound,
        });
    }
    let rows: Vec<ImportRow> = sqlx::query_as(&format!(
        "SELECT {} FROM imports WHERE owner_id = $1 ORDER BY id DESC LIMIT $2 OFFSET $3",
        ImportRow::COLUMNS
    ))
    .bind(org.id)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let json = render(&state, &rows, false).await?;
    Ok(p.page(json))
}

#[cfg(test)]
mod tests {
    #[test]
    fn source_repo_names() {
        assert!(super::valid_source_repo("octo-org/hello-world"));
        assert!(super::valid_source_repo("a/b.c_d"));
        assert!(!super::valid_source_repo("a"));
        assert!(!super::valid_source_repo("a/b/c"));
        assert!(!super::valid_source_repo("a/../b"));
        assert!(!super::valid_source_repo("/b"));
        assert!(!super::valid_source_repo("../b"));
    }
}
