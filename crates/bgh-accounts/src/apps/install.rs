//! Installing GitHub Apps on accounts, and managing installations.
//!
//! Web-client JSON (writes need a browser session):
//! * `GET /_bgh/apps/{slug}/install`: the install page (app summary and the
//!   accounts the viewer can install it on);
//!   `POST /_bgh/apps/{slug}/installations` installs it.
//! * `GET /_bgh/installations?account=login`, `GET|PATCH|DELETE
//!   /_bgh/installations/{id}`, `PUT|DELETE /_bgh/installations/{id}/suspended`,
//!   `POST /_bgh/installations/{id}/accept_permissions`.
//!
//! GitHub REST: `GET /user/installations`,
//! `GET /user/installations/{id}/repositories`,
//! `PUT|DELETE /user/installations/{id}/repositories/{repository_id}`,
//! `GET /orgs/{org}/installations`.
//!
//! Installation webhooks (`installation`, `installation_repositories`)
//! and the permission-upgrade notification are P46.

use std::collections::{BTreeMap, HashMap};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use bgh_core::apps::{AppRow, Installation, InstallationRow};
use bgh_core::audit;
use bgh_core::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{
    administers, app_by_id, app_by_slug, installation_by_id, installation_repos,
    installations_json, revoke_repo, revoke_tokens, wrapped,
};
use crate::util;

const RESOURCE: &str = "Installation";
/// Most repositories one request may select.
const MAX_SELECTED: usize = 500;

/// Whether the viewer may see the app at all (public, or administers the
/// owner).
async fn visible_app(state: &AppState, auth: &AuthContext, slug: &str) -> ApiResult<AppRow> {
    let app = app_by_slug(&state.db, slug).await?;
    if !app.public {
        let owner = db::User::find(&state.db, app.owner_id)
            .await?
            .ok_or(ApiError::NotFound)?;
        if !administers(state, &auth.user, &owner).await? {
            return Err(ApiError::NotFound);
        }
    }
    Ok(app)
}

/// Accounts `user` can install apps on: itself and organizations it
/// administers.
async fn installable_accounts(state: &AppState, user: &db::User) -> ApiResult<Vec<db::User>> {
    let mut out = vec![user.clone()];
    let orgs: Vec<db::User> = sqlx::query_as(&format!(
        "SELECT {} FROM users u JOIN org_members m ON m.org_id = u.id
          WHERE m.user_id = $1 AND m.role = 'admin' ORDER BY lower(u.login)",
        db::prefixed("u", db::User::COLUMNS)
    ))
    .bind(user.id)
    .fetch_all(&state.db)
    .await?;
    out.extend(orgs);
    Ok(out)
}

#[derive(Debug, Serialize)]
pub struct InstallTarget {
    pub account: api::SimpleUser,
    /// Existing installation of the app on this account.
    pub installation_id: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct InstallInfo {
    pub app: api::Integration,
    pub homepage_url: String,
    pub public: bool,
    pub accounts: Vec<InstallTarget>,
}

/// `GET /_bgh/apps/{slug}/install`
pub async fn install_info(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(slug): Path<String>,
) -> ApiResult<Json<InstallInfo>> {
    let app = visible_app(&state, &auth, &slug).await?;
    let mut accounts = installable_accounts(&state, &auth.user).await?;
    if !app.public {
        accounts.retain(|a| a.id == app.owner_id);
    }
    let ids: Vec<i64> = accounts.iter().map(|a| a.id).collect();
    let installed: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT account_id, id FROM app_installations WHERE app_id = $1 AND account_id = ANY($2)",
    )
    .bind(app.id)
    .bind(&ids)
    .fetch_all(&state.db)
    .await?;
    let installed: HashMap<i64, i64> = installed.into_iter().collect();
    Ok(Json(InstallInfo {
        app: super::integration(&state, &app, false).await?,
        homepage_url: app.homepage_url.clone(),
        public: app.public,
        accounts: accounts
            .iter()
            .map(|a| InstallTarget {
                account: api::SimpleUser::new(&state.urls, a),
                installation_id: installed.get(&a.id).copied(),
            })
            .collect(),
    }))
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct InstallBody {
    /// Account login (user or organization).
    pub account: Option<String>,
    /// `all` | `selected`
    pub repository_selection: Option<String>,
    pub repository_ids: Option<Vec<i64>>,
}

/// Installation settings (account admin's view).
#[derive(Debug, Serialize)]
pub struct InstallationDetail {
    pub installation: Installation,
    pub app: api::Integration,
    /// Selected repositories (empty for `all`).
    pub repositories: Vec<api::MinimalRepository>,
    /// The app requests permissions or events the installation hasn't
    /// accepted yet.
    pub permissions_outdated: bool,
    pub requested_permissions: BTreeMap<String, String>,
    pub requested_events: Vec<String>,
    /// Where to send the user after installing (`setup_url` with
    /// `installation_id` and `setup_action`), if the app has one.
    pub setup_redirect: Option<String>,
}

async fn detail(
    state: &AppState,
    inst: &InstallationRow,
    setup_action: Option<&str>,
) -> ApiResult<InstallationDetail> {
    let app = app_by_id(&state.db, inst.app_id).await?;
    let installation = installations_json(state, std::slice::from_ref(inst))
        .await?
        .pop()
        .ok_or(ApiError::NotFound)?;
    let repositories = if inst.all_repositories() {
        Vec::new()
    } else {
        let (rows, _) = installation_repos(state, inst, MAX_SELECTED as i64, 0).await?;
        let owners = bgh_core::views::users_by_id(state, [Some(inst.account_id)]).await?;
        rows.iter()
            .filter_map(|r| {
                Some(api::MinimalRepository::new(
                    &state.urls,
                    r,
                    owners.get(&r.owner_id)?,
                    None,
                ))
            })
            .collect()
    };
    let setup_redirect = setup_action
        .zip(app.setup_url.as_deref())
        .filter(|(action, _)| *action == "install" || app.setup_on_update)
        .and_then(|(action, base)| {
            let mut u = url::Url::parse(base).ok()?;
            u.query_pairs_mut()
                .append_pair("installation_id", &inst.id.to_string())
                .append_pair("setup_action", action);
            Some(u.to_string())
        });
    Ok(InstallationDetail {
        permissions_outdated: app.permissions.0 != inst.permissions.0 || {
            let mut a = app.events.clone();
            let mut b = inst.events.clone();
            a.sort();
            b.sort();
            a != b
        },
        requested_permissions: bgh_core::apps::with_metadata(&app.permissions),
        requested_events: app.events.clone(),
        app: super::integration(state, &app, false).await?,
        installation,
        repositories,
        setup_redirect,
    })
}

/// Repositories `ids` must belong to `account`: returns them deduplicated,
/// else 422.
async fn check_repo_ids(state: &AppState, account_id: i64, ids: &[i64]) -> ApiResult<Vec<i64>> {
    let mut ids = ids.to_vec();
    ids.sort_unstable();
    ids.dedup();
    if ids.len() > MAX_SELECTED {
        return Err(ApiError::invalid_field(FieldError::custom(
            RESOURCE,
            "repository_ids",
            format!("at most {MAX_SELECTED} repositories can be selected"),
        )));
    }
    let found: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM repositories WHERE owner_id = $1 AND id = ANY($2)",
    )
    .bind(account_id)
    .bind(&ids)
    .fetch_one(&state.db)
    .await?;
    if found != ids.len() as i64 {
        return Err(ApiError::invalid_field(FieldError::invalid(
            RESOURCE,
            "repository_ids",
        )));
    }
    Ok(ids)
}

fn parse_selection(s: Option<&str>, default: &str) -> ApiResult<String> {
    match s.unwrap_or(default) {
        v @ ("all" | "selected") => Ok(v.to_string()),
        _ => Err(ApiError::invalid_field(FieldError::invalid(
            RESOURCE,
            "repository_selection",
        ))),
    }
}

fn audit_target(account: &db::User) -> audit::Target {
    if account.is_org() {
        audit::Target::Org(account.id)
    } else {
        audit::Target::User(account.id)
    }
}

/// `POST /_bgh/apps/{slug}/installations` → 201 installation.
pub async fn install(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(slug): Path<String>,
    Json(body): Json<InstallBody>,
) -> ApiResult<(StatusCode, Json<InstallationDetail>)> {
    util::require_session(&auth)?;
    let app = visible_app(&state, &auth, &slug).await?;
    let account = match body.account.as_deref() {
        Some(login) => util::find_account(&state, login).await?,
        None => auth.user.clone(),
    };
    if account.kind == "Bot" || !administers(&state, &auth.user, &account).await? {
        return Err(ApiError::NotFound);
    }
    if !app.public && account.id != app.owner_id {
        return Err(ApiError::invalid_field(FieldError::custom(
            RESOURCE,
            "account",
            "this app is private and can only be installed on its owner",
        )));
    }
    let selection = parse_selection(body.repository_selection.as_deref(), "all")?;
    let repo_ids = if selection == "selected" {
        check_repo_ids(
            &state,
            account.id,
            body.repository_ids.as_deref().unwrap_or_default(),
        )
        .await?
    } else {
        Vec::new()
    };
    let mut tx = Tx::begin(&state).await?;
    let inst: Option<InstallationRow> = sqlx::query_as(&format!(
        "INSERT INTO app_installations (app_id, account_id, repository_selection, permissions,
                events, installed_by_id)
         VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT (app_id, account_id) DO NOTHING
         RETURNING {}",
        InstallationRow::COLUMNS
    ))
    .bind(app.id)
    .bind(account.id)
    .bind(&selection)
    .bind(&app.permissions)
    .bind(&app.events)
    .bind(auth.user.id)
    .fetch_optional(&mut *tx)
    .await?;
    let inst = inst.ok_or_else(|| {
        ApiError::invalid_field(FieldError::custom(
            RESOURCE,
            "account",
            "the app is already installed on this account",
        ))
    })?;
    sqlx::query(
        "INSERT INTO app_installation_repos (installation_id, repo_id)
         SELECT $1, unnest($2::bigint[])",
    )
    .bind(inst.id)
    .bind(&repo_ids)
    .execute(&mut *tx)
    .await?;
    audit::log(
        &mut *tx,
        Some(&auth.user),
        "integration_installation.create",
        audit_target(&account),
        json!({
            "integration": app.slug, "installation_id": inst.id,
            "repository_selection": selection, "repository_ids": repo_ids,
        }),
    )
    .await?;
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        Json(detail(&state, &inst, Some("install")).await?),
    ))
}

/// Installation `id` if the viewer administers its account, else 404.
pub async fn admin_installation(
    state: &AppState,
    auth: &AuthContext,
    id: i64,
) -> ApiResult<(InstallationRow, db::User)> {
    if bgh_core::apps::is_integration(auth) {
        return Err(ApiError::NotFound);
    }
    let inst = installation_by_id(&state.db, id).await?;
    let account = db::User::find(&state.db, inst.account_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if !administers(state, &auth.user, &account).await? {
        return Err(ApiError::NotFound);
    }
    Ok((inst, account))
}

#[derive(Debug, Deserialize)]
pub struct AccountQuery {
    pub account: Option<String>,
}

/// `GET /_bgh/installations[?account=login]`: installations on an account
/// the viewer administers.
pub async fn list_for_account(
    State(state): State<AppState>,
    auth: RequireUser,
    Query(q): Query<AccountQuery>,
) -> ApiResult<Json<Vec<Installation>>> {
    let account = match q.account.as_deref() {
        Some(login) => util::find_account(&state, login).await?,
        None => auth.user.clone(),
    };
    if bgh_core::apps::is_integration(&auth) || !administers(&state, &auth.user, &account).await? {
        return Err(ApiError::NotFound);
    }
    let rows: Vec<InstallationRow> = sqlx::query_as(&format!(
        "SELECT {} FROM app_installations WHERE account_id = $1 ORDER BY id",
        InstallationRow::COLUMNS
    ))
    .bind(account.id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(installations_json(&state, &rows).await?))
}

/// `GET /_bgh/installations/{id}`
pub async fn get_installation(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(id): Path<i64>,
) -> ApiResult<Json<InstallationDetail>> {
    let (inst, _) = admin_installation(&state, &auth, id).await?;
    Ok(Json(detail(&state, &inst, None).await?))
}

/// Change an installation's repository selection inside `tx`. Tokens are
/// revoked when access shrinks.
async fn set_repositories(
    tx: &mut Tx,
    inst: &InstallationRow,
    selection: &str,
    repo_ids: &[i64],
) -> ApiResult<()> {
    let previous: Vec<i64> =
        sqlx::query_scalar("SELECT repo_id FROM app_installation_repos WHERE installation_id = $1")
            .bind(inst.id)
            .fetch_all(&mut **tx)
            .await?;
    sqlx::query(
        "UPDATE app_installations SET repository_selection = $2, updated_at = now() WHERE id = $1",
    )
    .bind(inst.id)
    .bind(selection)
    .execute(&mut **tx)
    .await?;
    sqlx::query("DELETE FROM app_installation_repos WHERE installation_id = $1")
        .bind(inst.id)
        .execute(&mut **tx)
        .await?;
    sqlx::query(
        "INSERT INTO app_installation_repos (installation_id, repo_id)
         SELECT $1, unnest($2::bigint[])",
    )
    .bind(inst.id)
    .bind(repo_ids)
    .execute(&mut **tx)
    .await?;
    if inst.all_repositories() && selection == "selected" {
        revoke_tokens(tx, inst.id).await?;
    } else if selection == "selected" {
        for gone in previous.iter().filter(|r| !repo_ids.contains(r)) {
            revoke_repo(tx, inst.id, *gone).await?;
        }
    }
    Ok(())
}

/// `PATCH /_bgh/installations/{id}` `{repository_selection, repository_ids}`
pub async fn update_installation(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(id): Path<i64>,
    Json(body): Json<InstallBody>,
) -> ApiResult<Json<InstallationDetail>> {
    util::require_session(&auth)?;
    let (inst, account) = admin_installation(&state, &auth, id).await?;
    let selection = parse_selection(
        body.repository_selection.as_deref(),
        &inst.repository_selection,
    )?;
    let repo_ids = if selection == "selected" {
        check_repo_ids(
            &state,
            account.id,
            body.repository_ids.as_deref().unwrap_or_default(),
        )
        .await?
    } else {
        Vec::new()
    };
    let mut tx = Tx::begin(&state).await?;
    set_repositories(&mut tx, &inst, &selection, &repo_ids).await?;
    audit::log(
        &mut *tx,
        Some(&auth.user),
        "integration_installation.repositories_changed",
        audit_target(&account),
        json!({
            "installation_id": inst.id, "repository_selection": selection,
            "repository_ids": repo_ids,
        }),
    )
    .await?;
    tx.commit().await?;
    let inst = installation_by_id(&state.db, id).await?;
    Ok(Json(detail(&state, &inst, Some("update")).await?))
}

/// Delete an installation (its tokens cascade).
pub async fn uninstall(
    state: &AppState,
    actor: &db::User,
    inst: &InstallationRow,
    account: &db::User,
) -> ApiResult<()> {
    let mut tx = Tx::begin(state).await?;
    sqlx::query("DELETE FROM app_installations WHERE id = $1")
        .bind(inst.id)
        .execute(&mut *tx)
        .await?;
    audit::log(
        &mut *tx,
        Some(actor),
        "integration_installation.destroy",
        audit_target(account),
        json!({ "installation_id": inst.id, "app_id": inst.app_id }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Suspend (`true`) or unsuspend an installation. Suspension revokes its
/// tokens; suspended installations can't mint new ones.
pub async fn set_suspended(
    state: &AppState,
    actor: &db::User,
    inst: &InstallationRow,
    account: &db::User,
    suspend: bool,
) -> ApiResult<()> {
    let mut tx = Tx::begin(state).await?;
    sqlx::query(
        "UPDATE app_installations
            SET suspended_at = CASE WHEN $2 THEN coalesce(suspended_at, now()) END,
                suspended_by_id = CASE WHEN $2 THEN coalesce(suspended_by_id, $3) END,
                updated_at = now()
          WHERE id = $1",
    )
    .bind(inst.id)
    .bind(suspend)
    .bind(actor.id)
    .execute(&mut *tx)
    .await?;
    if suspend {
        revoke_tokens(&mut tx, inst.id).await?;
    }
    audit::log(
        &mut *tx,
        Some(actor),
        if suspend {
            "integration_installation.suspend"
        } else {
            "integration_installation.unsuspend"
        },
        audit_target(account),
        json!({ "installation_id": inst.id, "app_id": inst.app_id }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// `DELETE /_bgh/installations/{id}` → 204 (uninstall).
pub async fn delete_installation(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    util::require_session(&auth)?;
    let (inst, account) = admin_installation(&state, &auth, id).await?;
    uninstall(&state, &auth.user, &inst, &account).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `PUT /_bgh/installations/{id}/suspended` → 204.
pub async fn suspend(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    util::require_session(&auth)?;
    let (inst, account) = admin_installation(&state, &auth, id).await?;
    set_suspended(&state, &auth.user, &inst, &account, true).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /_bgh/installations/{id}/suspended` → 204.
pub async fn unsuspend(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    util::require_session(&auth)?;
    let (inst, account) = admin_installation(&state, &auth, id).await?;
    set_suspended(&state, &auth.user, &inst, &account, false).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /_bgh/installations/{id}/accept_permissions`: accept the app's
/// current permissions and events.
// TODO(P46): notify account admins of permission upgrades and deliver
// `installation.new_permissions_accepted`.
pub async fn accept_permissions(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(id): Path<i64>,
) -> ApiResult<Json<InstallationDetail>> {
    util::require_session(&auth)?;
    let (inst, account) = admin_installation(&state, &auth, id).await?;
    let app = app_by_id(&state.db, inst.app_id).await?;
    let mut tx = Tx::begin(&state).await?;
    sqlx::query(
        "UPDATE app_installations SET permissions = $2, events = $3, updated_at = now()
          WHERE id = $1",
    )
    .bind(inst.id)
    .bind(&app.permissions)
    .bind(&app.events)
    .execute(&mut *tx)
    .await?;
    audit::log(
        &mut *tx,
        Some(&auth.user),
        "integration_installation.version_update",
        audit_target(&account),
        json!({ "installation_id": inst.id, "permissions": app.permissions.0 }),
    )
    .await?;
    tx.commit().await?;
    let inst = installation_by_id(&state.db, id).await?;
    Ok(Json(detail(&state, &inst, Some("update")).await?))
}

// ---------------------------------------------------------------------------
// GitHub REST
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct InstallationList {
    total_count: i64,
    installations: Vec<Installation>,
}

#[derive(Serialize)]
struct RepositoryList {
    total_count: i64,
    repository_selection: String,
    repositories: Vec<api::Repository>,
}

/// Reject GitHub App credentials on user endpoints (GitHub needs a user
/// token for them).
fn require_user_token(auth: &AuthContext) -> ApiResult<()> {
    if bgh_core::apps::is_integration(auth) {
        Err(ApiError::forbidden(bgh_core::apps::NOT_ACCESSIBLE))
    } else {
        Ok(())
    }
}

/// `GET /user/installations`: installations on the viewer's account and on
/// organizations it belongs to.
pub async fn user_installations(
    State(state): State<AppState>,
    auth: RequireUser,
    p: Pagination,
) -> ApiResult<Response> {
    require_user_token(&auth)?;
    const FILTER: &str = "(i.account_id = $1 OR i.account_id IN
        (SELECT org_id FROM org_members WHERE user_id = $1))";
    let rows: Vec<InstallationRow> = sqlx::query_as(&format!(
        "SELECT {} FROM app_installations i WHERE {FILTER} ORDER BY i.id LIMIT $2 OFFSET $3",
        db::prefixed("i", InstallationRow::COLUMNS)
    ))
    .bind(auth.user.id)
    .bind(p.limit())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let total: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM app_installations i WHERE {FILTER}"
    ))
    .bind(auth.user.id)
    .fetch_one(&state.db)
    .await?;
    let installations = installations_json(&state, &rows).await?;
    let has_next = p.offset() + (rows.len() as i64) < total;
    Ok(wrapped(
        &p,
        has_next,
        total,
        InstallationList {
            total_count: total,
            installations,
        },
    ))
}

/// The installation `id` as seen by a user: on its account or an org it
/// belongs to, else 404.
async fn user_installation(
    state: &AppState,
    auth: &AuthContext,
    id: i64,
) -> ApiResult<(InstallationRow, db::User)> {
    require_user_token(auth)?;
    let inst = installation_by_id(&state.db, id).await?;
    let account = db::User::find(&state.db, inst.account_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let member = account.id == auth.user.id
        || auth.user.site_admin
        || bgh_core::perms::org_role(&state.db, account.id, auth.user.id)
            .await?
            .is_some();
    if !member {
        return Err(ApiError::NotFound);
    }
    Ok((inst, account))
}

/// `GET /user/installations/{id}/repositories`: the installation's
/// repositories the viewer can read.
pub async fn user_installation_repos(
    State(state): State<AppState>,
    auth: RequireUser,
    p: Pagination,
    Path(id): Path<i64>,
) -> ApiResult<Response> {
    let (inst, account) = user_installation(&state, &auth, id).await?;
    let (rows, _) = installation_repos(&state, &inst, i64::MAX, 0).await?;
    let raw = bgh_core::perms::repo_permissions(&state.db, Some(auth.user.id), &rows).await?;
    let readable: Vec<(db::Repository, Permission)> = rows
        .into_iter()
        .filter_map(|r| {
            let perm = bgh_core::perms::effective(
                Some(&auth),
                &r,
                raw.get(&r.id).copied().unwrap_or(Permission::None),
            );
            (perm >= Permission::Read).then_some((r, perm))
        })
        .collect();
    let total = readable.len() as i64;
    let page: Vec<&(db::Repository, Permission)> = readable
        .iter()
        .skip(p.offset() as usize)
        .take(p.limit() as usize)
        .collect();
    let owners: HashMap<i64, db::User> = [(account.id, account.clone())].into_iter().collect();
    let repos: Vec<db::Repository> = page.iter().map(|(r, _)| r.clone()).collect();
    let perms: HashMap<i64, Permission> = page.iter().map(|(r, p)| (r.id, *p)).collect();
    let repositories = super::repos_json(&state, &repos, &owners, |r| perms.get(&r.id).copied());
    let has_next = p.offset() + (page.len() as i64) < total;
    Ok(wrapped(
        &p,
        has_next,
        total,
        RepositoryList {
            total_count: total,
            repository_selection: inst.repository_selection.clone(),
            repositories,
        },
    ))
}

/// `PUT|DELETE /user/installations/{id}/repositories/{repository_id}`:
/// account admins add or remove a repository of a `selected` installation.
async fn change_user_installation_repo(
    state: &AppState,
    auth: &AuthContext,
    id: i64,
    repo_id: i64,
    add: bool,
) -> ApiResult<StatusCode> {
    let (inst, account) = user_installation(state, auth, id).await?;
    auth.require_scope("repo")?;
    if !administers(state, &auth.user, &account).await? {
        return Err(ApiError::forbidden(
            "You must be an admin of the installation's account.",
        ));
    }
    let repo = db::Repository::find(&state.db, repo_id)
        .await?
        .filter(|r| r.owner_id == account.id)
        .ok_or(ApiError::NotFound)?;
    if inst.all_repositories() {
        return Err(ApiError::unprocessable(
            "The installation has access to all repositories of the account.",
        ));
    }
    let mut tx = Tx::begin(state).await?;
    if add {
        sqlx::query(
            "INSERT INTO app_installation_repos (installation_id, repo_id) VALUES ($1, $2)
             ON CONFLICT DO NOTHING",
        )
        .bind(inst.id)
        .bind(repo.id)
        .execute(&mut *tx)
        .await?;
    } else {
        sqlx::query(
            "DELETE FROM app_installation_repos WHERE installation_id = $1 AND repo_id = $2",
        )
        .bind(inst.id)
        .bind(repo.id)
        .execute(&mut *tx)
        .await?;
        revoke_repo(&mut tx, inst.id, repo.id).await?;
    }
    sqlx::query("UPDATE app_installations SET updated_at = now() WHERE id = $1")
        .bind(inst.id)
        .execute(&mut *tx)
        .await?;
    audit::log(
        &mut *tx,
        Some(&auth.user),
        if add {
            "integration_installation.repositories_added"
        } else {
            "integration_installation.repositories_removed"
        },
        audit_target(&account),
        json!({ "installation_id": inst.id, "repository_ids": [repo.id] }),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `PUT /user/installations/{id}/repositories/{repository_id}` → 204.
pub async fn add_user_installation_repo(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((id, repo_id)): Path<(i64, i64)>,
) -> ApiResult<StatusCode> {
    change_user_installation_repo(&state, &auth, id, repo_id, true).await
}

/// `DELETE /user/installations/{id}/repositories/{repository_id}` → 204.
pub async fn remove_user_installation_repo(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((id, repo_id)): Path<(i64, i64)>,
) -> ApiResult<StatusCode> {
    change_user_installation_repo(&state, &auth, id, repo_id, false).await
}

/// `GET /orgs/{org}/installations` (org admins, `read:org`).
pub async fn org_installations(
    State(state): State<AppState>,
    auth: RequireUser,
    p: Pagination,
    Path(org): Path<String>,
) -> ApiResult<Response> {
    require_user_token(&auth)?;
    let org = util::find_org(&state, &org).await?;
    auth.require_scope("read:org")?;
    if !administers(&state, &auth.user, &org).await? {
        return Err(ApiError::forbidden(
            "You must be an organization owner to list its installations.",
        ));
    }
    let rows: Vec<InstallationRow> = sqlx::query_as(&format!(
        "SELECT {} FROM app_installations WHERE account_id = $1 ORDER BY id LIMIT $2 OFFSET $3",
        InstallationRow::COLUMNS
    ))
    .bind(org.id)
    .bind(p.limit())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let total: i64 =
        sqlx::query_scalar("SELECT count(*) FROM app_installations WHERE account_id = $1")
            .bind(org.id)
            .fetch_one(&state.db)
            .await?;
    let installations = installations_json(&state, &rows).await?;
    let has_next = p.offset() + (rows.len() as i64) < total;
    Ok(wrapped(
        &p,
        has_next,
        total,
        InstallationList {
            total_count: total,
            installations,
        },
    ))
}
