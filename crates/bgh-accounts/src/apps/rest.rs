//! GitHub's App REST endpoints.
//!
//! App JWT (`Authorization: Bearer <jwt>`):
//! * `GET /app`
//! * `GET /app/installations`, `GET|DELETE /app/installations/{id}`,
//!   `PUT|DELETE /app/installations/{id}/suspended`
//! * `POST /app/installations/{id}/access_tokens`
//! * `GET /orgs/{org}/installation`, `/repos/{o}/{r}/installation`,
//!   `/users/{u}/installation`
//!
//! Any caller: `GET /apps/{slug}` (private apps: the app itself and
//! administrators of its owner).
//!
//! Installation token: `GET /installation/repositories`,
//! `DELETE /installation/token`.

use std::collections::{BTreeMap, HashMap};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use bgh_core::apps::{
    self as core_apps, AppRow, INSTALLATION_TOKEN_TTL_SECS, Installation, InstallationRow,
    JWT_REQUIRED,
};
use bgh_core::auth::AuthMethod;
use bgh_core::crypto;
use bgh_core::prelude::*;
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};

use super::{
    administers, app_by_id, app_by_slug, installation_by_id, installation_repos,
    installations_json, level, wrapped,
};

/// The app of a JWT-authenticated request, else 401 like GitHub.
async fn jwt_app(state: &AppState, auth: &MaybeUser) -> ApiResult<AppRow> {
    let app_id = auth
        .as_ref()
        .and_then(core_apps::jwt_app_id)
        .ok_or_else(|| ApiError::Unauthorized {
            message: JWT_REQUIRED.into(),
            www_authenticate: None,
        })?;
    app_by_id(&state.db, app_id).await
}

/// Installation `id` of the JWT's app, else 404.
async fn app_installation(state: &AppState, app: &AppRow, id: i64) -> ApiResult<InstallationRow> {
    let inst = installation_by_id(&state.db, id).await?;
    if inst.app_id != app.id {
        return Err(ApiError::NotFound);
    }
    Ok(inst)
}

async fn one(state: &AppState, inst: &InstallationRow) -> ApiResult<Installation> {
    installations_json(state, std::slice::from_ref(inst))
        .await?
        .pop()
        .ok_or(ApiError::NotFound)
}

/// `GET /app`
pub async fn get_app(
    State(state): State<AppState>,
    auth: MaybeUser,
) -> ApiResult<Json<api::Integration>> {
    let app = jwt_app(&state, &auth).await?;
    Ok(Json(super::integration(&state, &app, true).await?))
}

/// `GET /apps/{app_slug}`
pub async fn get_app_by_slug(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path(slug): Path<String>,
) -> ApiResult<Json<api::Integration>> {
    let app = app_by_slug(&state.db, &slug).await?;
    if !app.public {
        let Some(a) = auth.as_ref() else {
            return Err(ApiError::NotFound);
        };
        let own = a.user.id == app.bot_user_id;
        let owner = db::User::find(&state.db, app.owner_id)
            .await?
            .ok_or(ApiError::NotFound)?;
        if !own && (core_apps::is_integration(a) || !administers(&state, &a.user, &owner).await?) {
            return Err(ApiError::NotFound);
        }
    }
    Ok(Json(super::integration(&state, &app, false).await?))
}

#[derive(Debug, Deserialize)]
pub struct InstallationsQuery {
    pub since: Option<String>,
    pub outdated: Option<String>,
}

/// `GET /app/installations[?since=]`
pub async fn list_installations(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Query(q): Query<InstallationsQuery>,
) -> ApiResult<Page<Installation>> {
    let app = jwt_app(&state, &auth).await?;
    let since = q
        .since
        .as_deref()
        .map(|s| {
            chrono::DateTime::parse_from_rfc3339(s)
                .map(|d| d.with_timezone(&Utc))
                .map_err(|_| ApiError::invalid_field(FieldError::invalid("Installation", "since")))
        })
        .transpose()?;
    // `outdated`: only installations that haven't accepted the app's
    // current permissions / events.
    let outdated = q
        .outdated
        .as_deref()
        .is_some_and(|v| v != "false" && v != "0");
    let rows: Vec<InstallationRow> = sqlx::query_as(&format!(
        "SELECT {} FROM app_installations
          WHERE app_id = $1 AND ($2::timestamptz IS NULL OR updated_at >= $2)
            AND (NOT $5 OR permissions <> $6 OR NOT (events @> $7 AND events <@ $7))
          ORDER BY id LIMIT $3 OFFSET $4",
        InstallationRow::COLUMNS
    ))
    .bind(app.id)
    .bind(since)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .bind(outdated)
    .bind(&app.permissions)
    .bind(&app.events)
    .fetch_all(&state.db)
    .await?;
    let page = p.page(rows);
    let items = installations_json(&state, &page.items).await?;
    Ok(Page {
        items,
        link: page.link,
    })
}

/// `GET /app/installations/{installation_id}`
pub async fn get_installation(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path(id): Path<i64>,
) -> ApiResult<Json<Installation>> {
    let app = jwt_app(&state, &auth).await?;
    let inst = app_installation(&state, &app, id).await?;
    Ok(Json(one(&state, &inst).await?))
}

async fn account_of(state: &AppState, inst: &InstallationRow) -> ApiResult<db::User> {
    db::User::find(&state.db, inst.account_id)
        .await?
        .ok_or(ApiError::NotFound)
}

/// `DELETE /app/installations/{installation_id}` → 204 (the app
/// uninstalls itself).
pub async fn delete_installation(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    let app = jwt_app(&state, &auth).await?;
    let inst = app_installation(&state, &app, id).await?;
    let account = account_of(&state, &inst).await?;
    let bot = &auth.as_ref().ok_or(ApiError::NotFound)?.user;
    super::install::uninstall(&state, bot, &inst, &account).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn suspend_impl(
    state: &AppState,
    auth: &MaybeUser,
    id: i64,
    on: bool,
) -> ApiResult<StatusCode> {
    let app = jwt_app(state, auth).await?;
    let inst = app_installation(state, &app, id).await?;
    let account = account_of(state, &inst).await?;
    let bot = &auth.as_ref().ok_or(ApiError::NotFound)?.user;
    super::install::set_suspended(state, bot, &inst, &account, on).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `PUT /app/installations/{installation_id}/suspended` → 204.
pub async fn suspend(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    suspend_impl(&state, &auth, id, true).await
}

/// `DELETE /app/installations/{installation_id}/suspended` → 204.
pub async fn unsuspend(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    suspend_impl(&state, &auth, id, false).await
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct TokenBody {
    /// Repository names (of the installation's account).
    pub repositories: Option<Vec<String>>,
    pub repository_ids: Option<Vec<i64>>,
    pub permissions: Option<BTreeMap<String, String>>,
}

#[derive(Debug, Serialize)]
pub struct TokenJson {
    pub token: String,
    pub expires_at: Timestamp,
    pub permissions: BTreeMap<String, String>,
    pub repository_selection: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repositories: Option<Vec<api::Repository>>,
}

const NOT_IN_INSTALLATION: &str = "There is at least one repository that does not exist or is not accessible to the parent installation.";
const NOT_GRANTED: &str = "The permissions requested are not granted to this installation.";

/// `POST /app/installations/{installation_id}/access_tokens` → 201.
/// Narrowed by `repositories` / `repository_ids` (within the installation)
/// and `permissions` (at most the installation's).
pub async fn create_access_token(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path(id): Path<i64>,
    body: axum::body::Bytes,
) -> ApiResult<(StatusCode, Json<TokenJson>)> {
    let app = jwt_app(&state, &auth).await?;
    let inst = app_installation(&state, &app, id).await?;
    let body: TokenBody = crate::util::optional_json(&body)?;
    if inst.suspended_at.is_some() {
        return Err(ApiError::forbidden("This installation has been suspended"));
    }
    let account = account_of(&state, &inst).await?;
    // Repositories.
    let names = body.repositories.unwrap_or_default();
    let ids = body.repository_ids.unwrap_or_default();
    let narrowed = !names.is_empty() || !ids.is_empty();
    if names.len() + ids.len() > 500 {
        return Err(ApiError::unprocessable(
            "Too many repositories requested (maximum is 500).",
        ));
    }
    let selected: Option<Vec<db::Repository>> = if narrowed {
        let lower: Vec<String> = names.iter().map(|n| n.to_lowercase()).collect();
        let rows: Vec<db::Repository> = sqlx::query_as(&format!(
            "SELECT {} FROM repositories r
              WHERE r.owner_id = $1 AND (lower(r.name) = ANY($2) OR r.id = ANY($3))
                AND ($4 OR r.id IN (SELECT repo_id FROM app_installation_repos
                                     WHERE installation_id = $5))
              ORDER BY r.id",
            db::prefixed("r", db::Repository::COLUMNS)
        ))
        .bind(account.id)
        .bind(&lower)
        .bind(&ids)
        .bind(inst.all_repositories())
        .bind(inst.id)
        .fetch_all(&state.db)
        .await?;
        let all_found = lower
            .iter()
            .all(|n| rows.iter().any(|r| r.name.to_lowercase() == *n))
            && ids.iter().all(|id| rows.iter().any(|r| r.id == *id));
        if !all_found {
            return Err(ApiError::unprocessable(NOT_IN_INSTALLATION));
        }
        Some(rows)
    } else if !inst.all_repositories() {
        Some(installation_repos(&state, &inst, i64::MAX, 0).await?.0)
    } else {
        None
    };
    // Permissions.
    let granted = &inst.permissions.0;
    // An empty map (PyGithub's default) narrows nothing, like GitHub.
    let permissions = match body.permissions.filter(|p| !p.is_empty()) {
        Some(requested) => {
            let mut out = BTreeMap::new();
            for (k, v) in requested {
                if v == "none" {
                    continue;
                }
                let have = granted.get(&k).and_then(|g| level(g));
                match (have, level(&v)) {
                    (Some(have), Some(want)) if want <= have => {
                        out.insert(k, v);
                    }
                    // metadata: read is always granted.
                    _ if k == "metadata" && v == "read" => {}
                    _ => return Err(ApiError::unprocessable(NOT_GRANTED)),
                }
            }
            out
        }
        None => granted.clone(),
    };
    let permissions = core_apps::with_metadata(&permissions);
    // Scopes: installation, repositories, permission map.
    let repo_ids: Vec<i64> = selected
        .as_ref()
        .map(|rows| rows.iter().map(|r| r.id).collect())
        .unwrap_or_default();
    let mut scopes = vec![format!(
        "{}{}",
        core_apps::INSTALLATION_SCOPE_PREFIX,
        inst.id
    )];
    scopes.extend(core_apps::repo_scopes(
        account.id,
        selected.is_none(),
        &repo_ids,
    ));
    scopes.extend(permissions.iter().map(|(k, v)| {
        // `admin` (projects) is `write` for enforcement.
        let v = if v == "admin" { "write" } else { v.as_str() };
        format!("{}{k}:{v}", core_apps::PERMISSION_SCOPE_PREFIX)
    }));
    let token = crypto::new_installation_token();
    let expires_at = Utc::now() + Duration::seconds(INSTALLATION_TOKEN_TTL_SECS);
    sqlx::query(
        "INSERT INTO access_tokens (user_id, kind, name, token_hash, token_last_eight, scopes,
                expires_at, installation_id, permissions)
         VALUES ($1, 'app', $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(app.bot_user_id)
    .bind(format!("installation {}", inst.id))
    .bind(crypto::sha256_hex(&token))
    .bind(&token[token.len() - 8..])
    .bind(&scopes)
    .bind(expires_at)
    .bind(inst.id)
    .bind(sqlx::types::Json(&permissions))
    .execute(&state.db)
    .await?;
    let repositories = selected.map(|rows| {
        let owners: HashMap<i64, db::User> = [(account.id, account.clone())].into_iter().collect();
        let write = permissions.values().any(|v| v != "read");
        let perm = if write {
            Permission::Write
        } else {
            Permission::Read
        };
        super::repos_json(&state, &rows, &owners, |_| Some(perm))
    });
    Ok((
        StatusCode::CREATED,
        Json(TokenJson {
            token,
            expires_at: expires_at.into(),
            permissions,
            repository_selection: if repositories.is_some() {
                "selected".into()
            } else {
                "all".into()
            },
            repositories,
        }),
    ))
}

async fn installation_on(
    state: &AppState,
    app: &AppRow,
    account_id: i64,
) -> ApiResult<InstallationRow> {
    sqlx::query_as(&format!(
        "SELECT {} FROM app_installations WHERE app_id = $1 AND account_id = $2",
        InstallationRow::COLUMNS
    ))
    .bind(app.id)
    .bind(account_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)
}

/// `GET /orgs/{org}/installation`
pub async fn org_installation(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path(org): Path<String>,
) -> ApiResult<Json<Installation>> {
    let app = jwt_app(&state, &auth).await?;
    let org = crate::util::find_org(&state, &org).await?;
    let inst = installation_on(&state, &app, org.id).await?;
    Ok(Json(one(&state, &inst).await?))
}

/// `GET /users/{username}/installation`
pub async fn user_installation(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path(username): Path<String>,
) -> ApiResult<Json<Installation>> {
    let app = jwt_app(&state, &auth).await?;
    let user = crate::util::find_account(&state, &username).await?;
    let inst = installation_on(&state, &app, user.id).await?;
    Ok(Json(one(&state, &inst).await?))
}

/// `GET /repos/{owner}/{repo}/installation`: the installation covering
/// the repository.
pub async fn repo_installation(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<Json<Installation>> {
    let app = jwt_app(&state, &auth).await?;
    let account = crate::util::find_account(&state, &owner).await?;
    let repo: db::Repository = sqlx::query_as(&format!(
        "SELECT {} FROM repositories WHERE owner_id = $1 AND lower(name) = lower($2)",
        db::Repository::COLUMNS
    ))
    .bind(account.id)
    .bind(&repo)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)?;
    let inst = installation_on(&state, &app, account.id).await?;
    let covered = inst.all_repositories()
        || sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM app_installation_repos
                             WHERE installation_id = $1 AND repo_id = $2)",
        )
        .bind(inst.id)
        .bind(repo.id)
        .fetch_one(&state.db)
        .await?;
    if !covered {
        return Err(ApiError::NotFound);
    }
    Ok(Json(one(&state, &inst).await?))
}

// ---------------------------------------------------------------------------
// Installation token endpoints
// ---------------------------------------------------------------------------

/// The installation of an installation-token request, else 401/403.
fn token_installation(auth: &AuthContext) -> ApiResult<i64> {
    core_apps::installation_id(auth)
        .ok_or_else(|| ApiError::forbidden("This endpoint requires an installation access token."))
}

#[derive(Serialize)]
struct RepositoryList {
    total_count: i64,
    repository_selection: String,
    repositories: Vec<api::Repository>,
}

/// `GET /installation/repositories`: the repositories the token covers.
pub async fn installation_repositories(
    State(state): State<AppState>,
    auth: RequireUser,
    p: Pagination,
) -> ApiResult<Response> {
    let inst_id = token_installation(&auth)?;
    let inst = installation_by_id(&state.db, inst_id).await?;
    let scopes = auth.scopes.clone().unwrap_or_default();
    let all = scopes
        .iter()
        .any(|s| s.starts_with(core_apps::OWNER_SCOPE_PREFIX));
    let ids: Vec<i64> = scopes
        .iter()
        .filter_map(|s| s.strip_prefix(core_apps::REPO_SCOPE_PREFIX)?.parse().ok())
        .collect();
    const FILTER: &str = "r.owner_id = $1 AND ($2 OR r.id = ANY($3))";
    let rows: Vec<db::Repository> = sqlx::query_as(&format!(
        "SELECT {} FROM repositories r WHERE {FILTER} ORDER BY r.id LIMIT $4 OFFSET $5",
        db::prefixed("r", db::Repository::COLUMNS)
    ))
    .bind(inst.account_id)
    .bind(all)
    .bind(&ids)
    .bind(p.limit())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let total: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM repositories r WHERE {FILTER}"
    ))
    .bind(inst.account_id)
    .bind(all)
    .bind(&ids)
    .fetch_one(&state.db)
    .await?;
    let owners = bgh_core::views::users_by_id(&state, [Some(inst.account_id)]).await?;
    let repositories = super::repos_json(&state, &rows, &owners, |r| {
        Some(bgh_core::perms::effective(Some(&auth), r, Permission::None))
    });
    let has_next = p.offset() + (rows.len() as i64) < total;
    Ok(wrapped(
        &p,
        has_next,
        total,
        RepositoryList {
            total_count: total,
            repository_selection: if all { "all" } else { "selected" }.into(),
            repositories,
        },
    ))
}

/// `DELETE /installation/token` → 204 (revoke the calling token).
pub async fn revoke_token(
    State(state): State<AppState>,
    auth: RequireUser,
) -> ApiResult<StatusCode> {
    token_installation(&auth)?;
    let AuthMethod::Token { token_id } = auth.method else {
        return Err(ApiError::forbidden(
            "This endpoint requires an installation access token.",
        ));
    };
    sqlx::query("DELETE FROM access_tokens WHERE id = $1")
        .bind(token_id)
        .execute(&state.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
