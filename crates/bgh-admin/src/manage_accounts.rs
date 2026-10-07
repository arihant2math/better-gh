//! Admin UI: user and organization management (`/_bgh/admin/users`,
//! `/_bgh/admin/orgs`), storage quotas, password reset, 2FA reset.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use bgh_core::audit::Target;
use bgh_core::crypto;
use bgh_core::prelude::*;
use bgh_core::time::ts;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::FromRow;

use crate::common::{self, account_target, direction, like_escape, log};
use crate::service;

/// Accounts without activity for this long count as dormant.
const DORMANT_DAYS: i32 = 90;

/// `(id, text, text, created, optional time)` rows.
type IdNameTimes = (i64, String, String, DateTime<Utc>, Option<DateTime<Utc>>);

#[derive(Debug, FromRow)]
struct AccountRow {
    id: i64,
    login: String,
    #[sqlx(rename = "type")]
    kind: String,
    name: Option<String>,
    email: Option<String>,
    site_admin: bool,
    suspended_at: Option<DateTime<Utc>>,
    suspended_reason: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    last_active_at: Option<DateTime<Utc>>,
    repos_count: i64,
    disk_usage_kb: i64,
    two_factor_enabled: bool,
    members_count: Option<i64>,
}

/// Account summary row of the admin UI lists.
#[derive(Debug, Serialize)]
pub struct AccountSummary {
    pub id: i64,
    pub login: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub name: Option<String>,
    pub email: Option<String>,
    pub avatar_url: String,
    pub html_url: String,
    pub site_admin: bool,
    pub suspended: bool,
    pub suspended_at: Option<Timestamp>,
    pub suspended_reason: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub last_active_at: Option<Timestamp>,
    pub repos_count: i64,
    pub disk_usage_kb: i64,
    pub two_factor_enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub members_count: Option<i64>,
}

fn summary(state: &AppState, r: AccountRow) -> AccountSummary {
    AccountSummary {
        avatar_url: state.urls.avatar(r.id, None),
        html_url: state.urls.user_html(&r.login),
        id: r.id,
        login: r.login,
        kind: r.kind,
        name: r.name,
        email: r.email,
        site_admin: r.site_admin,
        suspended: r.suspended_at.is_some(),
        suspended_at: ts(r.suspended_at),
        suspended_reason: r.suspended_reason,
        created_at: r.created_at.into(),
        updated_at: r.updated_at.into(),
        last_active_at: ts(r.last_active_at),
        repos_count: r.repos_count,
        disk_usage_kb: r.disk_usage_kb,
        two_factor_enabled: r.two_factor_enabled,
        members_count: r.members_count,
    }
}

/// Columns of [`AccountRow`] for `users u` (aggregates via LATERAL joins).
const ACCOUNT_SELECT: &str = "
    SELECT u.id, u.login, u.type, u.name,
           coalesce((SELECT e.email FROM user_emails e WHERE e.user_id = u.id AND e.is_primary),
                    u.email) AS email,
           u.site_admin, u.suspended_at, u.suspended_reason, u.created_at, u.updated_at,
           act.last_active_at,
           coalesce(rs.repos_count, 0) AS repos_count,
           coalesce(rs.disk_usage_kb, 0) AS disk_usage_kb,
           EXISTS (SELECT 1 FROM user_two_factor t WHERE t.user_id = u.id AND t.enabled_at IS NOT NULL) AS two_factor_enabled,
           CASE WHEN u.type = 'Organization'
                THEN (SELECT count(*) FROM org_members m WHERE m.org_id = u.id) END AS members_count
      FROM users u
      LEFT JOIN LATERAL (
          SELECT greatest(
              (SELECT max(s.last_seen_at) FROM sessions s WHERE s.user_id = u.id),
              (SELECT max(t.last_used_at) FROM access_tokens t WHERE t.user_id = u.id)
          ) AS last_active_at) act ON true
      LEFT JOIN LATERAL (
          SELECT count(*) AS repos_count, coalesce(sum(r.size), 0)::bigint AS disk_usage_kb
            FROM repositories r WHERE r.owner_id = u.id) rs ON true";

#[derive(Debug, Default, Deserialize)]
pub struct ListParams {
    /// Matches login, name or primary email (substring, case-insensitive).
    pub q: Option<String>,
    /// `user` (default for /users) | `organization` | `bot` | `all`
    #[serde(rename = "type")]
    pub kind: Option<String>,
    /// `admin` | `suspended` | `active` | `dormant` | `2fa` | `no_2fa`
    pub filter: Option<String>,
    /// `login` | `created` | `last_active` | `repos` | `disk_usage`
    pub sort: Option<String>,
    pub direction: Option<String>,
}

async fn list_accounts(
    state: &AppState,
    p: &Pagination,
    q: &ListParams,
    default_kind: &str,
) -> ApiResult<Page<AccountSummary>> {
    let kind = match q.kind.as_deref().unwrap_or(default_kind) {
        "user" | "User" => Some("User"),
        "organization" | "org" | "Organization" => Some("Organization"),
        "bot" | "Bot" => Some("Bot"),
        "all" => None,
        _ => return Err(ApiError::invalid_field(FieldError::invalid("User", "type"))),
    };
    let filter = match q.filter.as_deref() {
        None | Some("") => "true".to_string(),
        Some("admin") => "a.site_admin".into(),
        Some("suspended") => "a.suspended_at IS NOT NULL".into(),
        Some("active") => "a.suspended_at IS NULL".into(),
        Some("dormant") => format!(
            "a.type = 'User' AND coalesce(a.last_active_at, a.created_at) < now() - interval '{DORMANT_DAYS} days'"
        ),
        Some("2fa") => "a.two_factor_enabled".into(),
        Some("no_2fa") => "a.type = 'User' AND NOT a.two_factor_enabled".into(),
        Some(_) => {
            return Err(ApiError::invalid_field(FieldError::invalid(
                "User", "filter",
            )));
        }
    };
    let (order, default_desc) = match q.sort.as_deref() {
        None | Some("login") => ("lower(a.login)", false),
        Some("created") => ("a.created_at", true),
        Some("last_active") => ("a.last_active_at", true),
        Some("repos") => ("a.repos_count", true),
        Some("disk_usage") => ("a.disk_usage_kb", true),
        Some(_) => return Err(ApiError::invalid_field(FieldError::invalid("User", "sort"))),
    };
    let dir = direction(q.direction.as_deref(), default_desc);
    let pattern =
        q.q.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| format!("%{}%", like_escape(&s.to_lowercase())));
    let where_clause = format!(
        "WHERE ($1::text IS NULL OR a.type = $1)
           AND ($2::text IS NULL OR lower(a.login) LIKE $2 OR lower(coalesce(a.name, '')) LIKE $2
                OR lower(coalesce(a.email, '')) LIKE $2)
           AND {filter}"
    );
    let total: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM ({ACCOUNT_SELECT}) a {where_clause}"
    ))
    .bind(kind)
    .bind(&pattern)
    .fetch_one(&state.db)
    .await?;
    let rows: Vec<AccountRow> = sqlx::query_as(&format!(
        "SELECT * FROM ({ACCOUNT_SELECT}) a {where_clause}
          ORDER BY {order} {dir} NULLS LAST, a.id {dir} LIMIT $3 OFFSET $4"
    ))
    .bind(kind)
    .bind(&pattern)
    .bind(p.limit())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    Ok(p.page_with_total(rows, total).map(|r| summary(state, r)))
}

/// `GET /_bgh/admin/users`
pub async fn list_users(
    State(state): State<AppState>,
    _auth: RequireSiteAdmin,
    p: Pagination,
    Query(q): Query<ListParams>,
) -> ApiResult<Page<AccountSummary>> {
    list_accounts(&state, &p, &q, "user").await
}

/// `GET /_bgh/admin/orgs`
pub async fn list_orgs(
    State(state): State<AppState>,
    _auth: RequireSiteAdmin,
    p: Pagination,
    Query(q): Query<ListParams>,
) -> ApiResult<Page<AccountSummary>> {
    list_accounts(&state, &p, &q, "organization").await
}

async fn account_summary(state: &AppState, id: i64) -> ApiResult<AccountSummary> {
    let row: AccountRow = sqlx::query_as(&format!(
        "SELECT * FROM ({ACCOUNT_SELECT}) a WHERE a.id = $1"
    ))
    .bind(id)
    .fetch_one(&state.db)
    .await?;
    Ok(summary(state, row))
}

#[derive(Debug, Serialize, FromRow)]
struct RepoBrief {
    id: i64,
    name: String,
    visibility: String,
    fork: bool,
    archived: bool,
    size: i64,
    pushed_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
}

async fn owned_repos(state: &AppState, owner_id: i64) -> ApiResult<Vec<Value>> {
    let rows: Vec<RepoBrief> = sqlx::query_as(
        "SELECT id, name, visibility, fork, archived, size, pushed_at, created_at
           FROM repositories WHERE owner_id = $1 ORDER BY lower(name), id LIMIT 1000",
    )
    .bind(owner_id)
    .fetch_all(&state.db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            json!({
                "id": r.id, "name": r.name, "visibility": r.visibility, "fork": r.fork,
                "archived": r.archived, "size": r.size,
                "pushed_at": ts(r.pushed_at), "created_at": Timestamp::from(r.created_at),
            })
        })
        .collect())
}

/// Storage limits and usage of an owner (MB for limits, KB for usage).
async fn quota_json(state: &AppState, owner_id: i64) -> ApiResult<Value> {
    let row: Option<(Option<i64>, Option<i64>)> = sqlx::query_as(
        "SELECT max_repo_size_mb, max_total_size_mb FROM storage_quotas WHERE owner_id = $1",
    )
    .bind(owner_id)
    .fetch_optional(&state.db)
    .await?;
    let (repo_mb, total_mb) = row.unwrap_or((None, None));
    let (eff_repo_kb, eff_total_kb) =
        bgh_core::settings::storage_limits_kb(state, owner_id).await?;
    let (used, largest): (i64, i64) = sqlx::query_as(
        "SELECT coalesce(sum(size), 0)::bigint, coalesce(max(size), 0)::bigint
           FROM repositories WHERE owner_id = $1",
    )
    .bind(owner_id)
    .fetch_one(&state.db)
    .await?;
    Ok(json!({
        "max_repo_size_mb": repo_mb,
        "max_total_size_mb": total_mb,
        "effective_max_repo_size_mb": eff_repo_kb.map(|k| k / 1024),
        "effective_max_total_size_mb": eff_total_kb.map(|k| k / 1024),
        "used_kb": used,
        "largest_repo_kb": largest,
    }))
}

/// `GET /_bgh/admin/users/{login}`: profile, emails, keys, repositories,
/// organizations, 2FA, sessions, tokens, quota.
pub async fn get_user(
    State(state): State<AppState>,
    _auth: RequireSiteAdmin,
    Path(login): Path<String>,
) -> ApiResult<Json<Value>> {
    let user = common::user(&state, &login).await?;
    let summary = account_summary(&state, user.id).await?;
    let emails: Vec<(String, bool, bool, Option<String>)> = sqlx::query_as(
        "SELECT email, verified, is_primary, visibility FROM user_emails
          WHERE user_id = $1 ORDER BY is_primary DESC, id",
    )
    .bind(user.id)
    .fetch_all(&state.db)
    .await?;
    let keys: Vec<IdNameTimes> = sqlx::query_as(
        "SELECT id, title, fingerprint, created_at, last_used_at FROM ssh_keys
          WHERE user_id = $1 ORDER BY id",
    )
    .bind(user.id)
    .fetch_all(&state.db)
    .await?;
    let gpg_keys: Vec<(i64, String, Option<DateTime<Utc>>)> = sqlx::query_as(
        "SELECT id, key_id, expires_at FROM gpg_keys WHERE user_id = $1 AND primary_key_id IS NULL ORDER BY id",
    )
    .bind(user.id)
    .fetch_all(&state.db)
    .await?;
    let orgs: Vec<(i64, String, String)> = sqlx::query_as(
        "SELECT o.id, o.login, m.role FROM org_members m JOIN users o ON o.id = m.org_id
          WHERE m.user_id = $1 ORDER BY lower(o.login)",
    )
    .bind(user.id)
    .fetch_all(&state.db)
    .await?;
    let tokens: Vec<db::AccessToken> = sqlx::query_as(&format!(
        "SELECT {} FROM access_tokens WHERE user_id = $1 ORDER BY id DESC",
        db::AccessToken::COLUMNS
    ))
    .bind(user.id)
    .fetch_all(&state.db)
    .await?;
    let sessions: (i64, Option<DateTime<Utc>>) = sqlx::query_as(
        "SELECT count(*), max(last_seen_at) FROM sessions WHERE user_id = $1 AND expires_at > now()",
    )
    .bind(user.id)
    .fetch_one(&state.db)
    .await?;
    let two_factor = bgh_core::two_factor::enabled_at(&state.db, user.id).await?;
    Ok(Json(json!({
        "user": summary,
        "emails": emails.into_iter().map(|(email, verified, primary, visibility)| json!({
            "email": email, "verified": verified, "primary": primary, "visibility": visibility,
        })).collect::<Vec<_>>(),
        "ssh_keys": keys.into_iter().map(|(id, title, fp, created, used)| json!({
            "id": id, "title": title, "fingerprint": fp,
            "created_at": Timestamp::from(created), "last_used_at": ts(used),
        })).collect::<Vec<_>>(),
        "gpg_keys": gpg_keys.into_iter().map(|(id, key_id, exp)| json!({
            "id": id, "key_id": key_id, "expires_at": ts(exp),
        })).collect::<Vec<_>>(),
        "organizations": orgs.into_iter().map(|(id, login, role)| json!({
            "id": id, "login": login, "role": role,
        })).collect::<Vec<_>>(),
        "repositories": owned_repos(&state, user.id).await?,
        "two_factor": { "enabled": two_factor.is_some(), "enabled_at": ts(two_factor) },
        "sessions": { "active": sessions.0, "last_seen_at": ts(sessions.1) },
        "tokens": tokens.iter().map(|t| json!({
            "id": t.id, "kind": t.kind, "name": t.name, "scopes": t.scopes,
            "token_last_eight": t.token_last_eight, "expires_at": ts(t.expires_at),
            "last_used_at": ts(t.last_used_at), "created_at": Timestamp::from(t.created_at),
        })).collect::<Vec<_>>(),
        "quota": quota_json(&state, user.id).await?,
    })))
}

#[derive(Debug, Deserialize)]
pub struct CreateUserBody {
    #[serde(default)]
    pub login: String,
    #[serde(default)]
    pub email: String,
    /// Optional: without a password the user signs in via SSO or a reset.
    pub password: Option<String>,
    pub name: Option<String>,
    #[serde(default)]
    pub site_admin: bool,
}

/// `POST /_bgh/admin/users` → 201 account summary.
pub async fn create_user(
    State(state): State<AppState>,
    auth: RequireSiteAdmin,
    headers: HeaderMap,
    Json(body): Json<CreateUserBody>,
) -> ApiResult<(StatusCode, Json<AccountSummary>)> {
    let email = body.email.trim();
    let user = match body.password.as_deref().filter(|p| !p.is_empty()) {
        Some(password) => {
            bgh_accounts::create_user(
                &state,
                bgh_accounts::NewAccount {
                    login: body.login.trim(),
                    email,
                    password,
                    name: body.name.as_deref(),
                    site_admin: Some(body.site_admin),
                    email_verified: true,
                },
                Some(&auth.user),
            )
            .await?
        }
        None => {
            let u = service::create_user(
                &state,
                &auth,
                &headers,
                body.login.trim(),
                Some(email).filter(|e| !e.is_empty()),
                false,
            )
            .await?;
            if body.site_admin {
                service::set_site_admin(&state, &auth, &headers, &u, true).await?;
            }
            u
        }
    };
    Ok((
        StatusCode::CREATED,
        Json(account_summary(&state, user.id).await?),
    ))
}

#[derive(Debug, Default, Deserialize)]
pub struct UpdateUserBody {
    pub login: Option<String>,
    pub site_admin: Option<bool>,
    pub suspended: Option<bool>,
    pub suspended_reason: Option<String>,
}

/// `PATCH /_bgh/admin/users/{login}`: rename, promote/demote,
/// suspend/unsuspend in one call.
pub async fn update_user(
    State(state): State<AppState>,
    auth: RequireSiteAdmin,
    headers: HeaderMap,
    Path(login): Path<String>,
    Json(body): Json<UpdateUserBody>,
) -> ApiResult<Json<AccountSummary>> {
    let mut user = common::user(&state, &login).await?;
    if let Some(admin) = body.site_admin {
        service::set_site_admin(&state, &auth, &headers, &user, admin).await?;
    }
    if let Some(suspended) = body.suspended {
        user = common::user(&state, &user.login).await?;
        service::set_suspended(
            &state,
            &auth,
            &headers,
            &user,
            suspended,
            body.suspended_reason.as_deref(),
        )
        .await?;
    }
    if let Some(new_login) = body.login.as_deref().map(str::trim) {
        user = common::user(&state, &user.login).await?;
        service::rename_account(&state, &auth, &headers, &user, new_login).await?;
    }
    Ok(Json(account_summary(&state, user.id).await?))
}

#[derive(Debug, Default, Deserialize)]
pub struct DeleteParams {
    /// Login of the account receiving the owned repositories; omitted =
    /// repositories are deleted.
    pub transfer_repositories_to: Option<String>,
}

async fn delete_account(
    state: &AppState,
    auth: &AuthContext,
    headers: &HeaderMap,
    account: db::User,
    q: DeleteParams,
) -> ApiResult<StatusCode> {
    let target = match q
        .transfer_repositories_to
        .as_deref()
        .filter(|s| !s.is_empty())
    {
        Some(login) => Some(
            db::User::find_by_login(&state.db, login)
                .await?
                .ok_or_else(|| {
                    ApiError::invalid_field(FieldError::invalid("User", "transfer_repositories_to"))
                })?,
        ),
        None => None,
    };
    service::delete_account(state, auth, headers, &account, target.as_ref()).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /_bgh/admin/users/{login}[?transfer_repositories_to=]` → 204.
/// Authored issues, comments and reviews stay, attributed to `ghost`.
pub async fn delete_user(
    State(state): State<AppState>,
    auth: RequireSiteAdmin,
    headers: HeaderMap,
    Path(login): Path<String>,
    Query(q): Query<DeleteParams>,
) -> ApiResult<StatusCode> {
    let user = common::user(&state, &login).await?;
    delete_account(&state, &auth, &headers, user, q).await
}

#[derive(Debug, Default, Deserialize)]
pub struct PasswordBody {
    /// New password; omitted = a random temporary password is generated
    /// and returned once.
    pub password: Option<String>,
}

/// `POST /_bgh/admin/users/{login}/password` → `{"password": temp|null}`:
/// sets the password and signs the user out everywhere.
pub async fn reset_password(
    State(state): State<AppState>,
    auth: RequireSiteAdmin,
    headers: HeaderMap,
    Path(login): Path<String>,
    Json(body): Json<PasswordBody>,
) -> ApiResult<Json<Value>> {
    let user = common::user(&state, &login).await?;
    let (password, generated) = match body.password {
        Some(p) => {
            if !bgh_accounts::validate::is_valid_password(&p) {
                return Err(ApiError::invalid_field(FieldError::custom(
                    "User",
                    "password",
                    format!(
                        "password must be at least {} characters",
                        bgh_accounts::validate::MIN_PASSWORD_LEN
                    ),
                )));
            }
            (p, false)
        }
        None => (crypto::random_token(20), true),
    };
    let hash = {
        let p = password.clone();
        tokio::task::spawn_blocking(move || crypto::hash_password(&p)).await??
    };
    let mut tx = Tx::begin(&state).await?;
    sqlx::query("UPDATE users SET password_hash = $2, updated_at = now() WHERE id = $1")
        .bind(user.id)
        .bind(&hash)
        .execute(&mut *tx)
        .await?;
    log(
        &mut tx,
        &auth,
        &headers,
        "user.reset_password",
        Target::User(user.id),
        json!({ "login": user.login, "generated": generated }),
    )
    .await?;
    tx.commit().await?;
    bgh_core::auth::destroy_user_sessions(&state, user.id).await?;
    Ok(Json(json!({ "password": generated.then_some(password) })))
}

/// `DELETE /_bgh/admin/users/{login}/two-factor` → 204 (404 if not enabled).
pub async fn disable_two_factor(
    State(state): State<AppState>,
    auth: RequireSiteAdmin,
    headers: HeaderMap,
    Path(login): Path<String>,
) -> ApiResult<StatusCode> {
    let user = common::user(&state, &login).await?;
    let mut tx = Tx::begin(&state).await?;
    if !bgh_core::two_factor::disable(&mut *tx, user.id).await? {
        return Err(ApiError::NotFound);
    }
    log(
        &mut tx,
        &auth,
        &headers,
        "two_factor_authentication.disabled",
        Target::User(user.id),
        json!({ "login": user.login, "by_site_admin": true }),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /_bgh/admin/users/{login}/sessions` → 204: sign out everywhere.
pub async fn revoke_sessions(
    State(state): State<AppState>,
    auth: RequireSiteAdmin,
    headers: HeaderMap,
    Path(login): Path<String>,
) -> ApiResult<StatusCode> {
    let user = common::user(&state, &login).await?;
    bgh_core::auth::destroy_user_sessions(&state, user.id).await?;
    let mut conn = state.db.acquire().await?;
    log(
        &mut conn,
        &auth,
        &headers,
        "user.revoke_sessions",
        Target::User(user.id),
        json!({ "login": user.login }),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /_bgh/admin/accounts/{login}/quota` (users and orgs).
pub async fn get_quota(
    State(state): State<AppState>,
    _auth: RequireSiteAdmin,
    Path(login): Path<String>,
) -> ApiResult<Json<Value>> {
    let account = common::account(&state, &login).await?;
    Ok(Json(quota_json(&state, account.id).await?))
}

#[derive(Debug, Default, Deserialize)]
pub struct QuotaBody {
    pub max_repo_size_mb: Option<i64>,
    pub max_total_size_mb: Option<i64>,
}

/// `PUT /_bgh/admin/accounts/{login}/quota` → quota (null = no limit).
pub async fn set_quota(
    State(state): State<AppState>,
    auth: RequireSiteAdmin,
    headers: HeaderMap,
    Path(login): Path<String>,
    Json(body): Json<QuotaBody>,
) -> ApiResult<Json<Value>> {
    let account = common::account(&state, &login).await?;
    for (field, v) in [
        ("max_repo_size_mb", body.max_repo_size_mb),
        ("max_total_size_mb", body.max_total_size_mb),
    ] {
        if v.is_some_and(|v| v <= 0) {
            return Err(ApiError::invalid_field(FieldError::invalid("Quota", field)));
        }
    }
    let mut tx = Tx::begin(&state).await?;
    sqlx::query(
        "INSERT INTO storage_quotas (owner_id, max_repo_size_mb, max_total_size_mb)
         VALUES ($1, $2, $3)
         ON CONFLICT (owner_id) DO UPDATE SET max_repo_size_mb = EXCLUDED.max_repo_size_mb,
             max_total_size_mb = EXCLUDED.max_total_size_mb, updated_at = now()",
    )
    .bind(account.id)
    .bind(body.max_repo_size_mb)
    .bind(body.max_total_size_mb)
    .execute(&mut *tx)
    .await?;
    log(
        &mut tx,
        &auth,
        &headers,
        "business.set_storage_quota",
        account_target(&account),
        json!({
            "login": account.login,
            "max_repo_size_mb": body.max_repo_size_mb,
            "max_total_size_mb": body.max_total_size_mb,
        }),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(quota_json(&state, account.id).await?))
}

/// `DELETE /_bgh/admin/accounts/{login}/quota` → 204 (site defaults apply).
pub async fn delete_quota(
    State(state): State<AppState>,
    auth: RequireSiteAdmin,
    headers: HeaderMap,
    Path(login): Path<String>,
) -> ApiResult<StatusCode> {
    let account = common::account(&state, &login).await?;
    let mut tx = Tx::begin(&state).await?;
    sqlx::query("DELETE FROM storage_quotas WHERE owner_id = $1")
        .bind(account.id)
        .execute(&mut *tx)
        .await?;
    log(
        &mut tx,
        &auth,
        &headers,
        "business.remove_storage_quota",
        account_target(&account),
        json!({ "login": account.login }),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Organizations
// ---------------------------------------------------------------------------

/// `GET /_bgh/admin/orgs/{org}`: profile, settings, members, teams,
/// repositories, quota.
pub async fn get_org(
    State(state): State<AppState>,
    _auth: RequireSiteAdmin,
    Path(login): Path<String>,
) -> ApiResult<Json<Value>> {
    let org = common::org(&state, &login).await?;
    let summary = account_summary(&state, org.id).await?;
    let settings = db::OrgSettings::find(&state.db, org.id).await?;
    let members: Vec<(i64, String, String, bool)> = sqlx::query_as(
        "SELECT u.id, u.login, m.role, u.suspended_at IS NOT NULL FROM org_members m
           JOIN users u ON u.id = m.user_id WHERE m.org_id = $1
          ORDER BY m.role, lower(u.login) LIMIT 1000",
    )
    .bind(org.id)
    .fetch_all(&state.db)
    .await?;
    let teams: Vec<(i64, String, String, i64)> = sqlx::query_as(
        "SELECT t.id, t.name, t.slug, (SELECT count(*) FROM team_members tm WHERE tm.team_id = t.id)
           FROM teams t WHERE t.org_id = $1 ORDER BY lower(t.name)",
    )
    .bind(org.id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(json!({
        "organization": summary,
        "settings": settings,
        "members": members.into_iter().map(|(id, login, role, suspended)| json!({
            "id": id, "login": login, "role": role, "suspended": suspended,
        })).collect::<Vec<_>>(),
        "teams": teams.into_iter().map(|(id, name, slug, n)| json!({
            "id": id, "name": name, "slug": slug, "members_count": n,
        })).collect::<Vec<_>>(),
        "repositories": owned_repos(&state, org.id).await?,
        "quota": quota_json(&state, org.id).await?,
    })))
}

#[derive(Debug, Deserialize)]
pub struct CreateOrgBody {
    #[serde(default)]
    pub login: String,
    /// Login of the first organization owner.
    #[serde(default)]
    pub admin: String,
    pub name: Option<String>,
}

/// `POST /_bgh/admin/orgs` → 201 account summary.
pub async fn create_org(
    State(state): State<AppState>,
    auth: RequireSiteAdmin,
    Json(body): Json<CreateOrgBody>,
) -> ApiResult<(StatusCode, Json<AccountSummary>)> {
    let admin = db::User::find_by_login(&state.db, body.admin.trim())
        .await?
        .filter(|u| !u.is_org())
        .ok_or_else(|| ApiError::invalid_field(FieldError::invalid("Organization", "admin")))?;
    let org = bgh_accounts::create_org(
        &state,
        body.login.trim(),
        body.name.as_deref(),
        &admin,
        &auth.user,
    )
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(account_summary(&state, org.id).await?),
    ))
}

#[derive(Debug, Default, Deserialize)]
pub struct UpdateOrgBody {
    pub login: Option<String>,
    /// Archive (disable) / unarchive the organization.
    pub archived: Option<bool>,
}

/// `PATCH /_bgh/admin/orgs/{org}`
pub async fn update_org(
    State(state): State<AppState>,
    auth: RequireSiteAdmin,
    headers: HeaderMap,
    Path(login): Path<String>,
    Json(body): Json<UpdateOrgBody>,
) -> ApiResult<Json<AccountSummary>> {
    let mut org = common::org(&state, &login).await?;
    if let Some(archived) = body.archived {
        let mut tx = Tx::begin(&state).await?;
        let changed = sqlx::query(
            "UPDATE org_settings SET archived_at = CASE WHEN $2 THEN coalesce(archived_at, now()) END
              WHERE org_id = $1 AND (archived_at IS NOT NULL) <> $2",
        )
        .bind(org.id)
        .bind(archived)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if changed > 0 {
            log(
                &mut tx,
                &auth,
                &headers,
                if archived {
                    "org.archive"
                } else {
                    "org.unarchive"
                },
                Target::Org(org.id),
                json!({ "login": org.login }),
            )
            .await?;
        }
        tx.commit().await?;
    }
    if let Some(new_login) = body.login.as_deref().map(str::trim) {
        org = service::rename_account(&state, &auth, &headers, &org, new_login).await?;
    }
    Ok(Json(account_summary(&state, org.id).await?))
}

/// `DELETE /_bgh/admin/orgs/{org}[?transfer_repositories_to=]` → 204.
pub async fn delete_org(
    State(state): State<AppState>,
    auth: RequireSiteAdmin,
    headers: HeaderMap,
    Path(login): Path<String>,
    Query(q): Query<DeleteParams>,
) -> ApiResult<StatusCode> {
    let org = common::org(&state, &login).await?;
    delete_account(&state, &auth, &headers, org, q).await
}
