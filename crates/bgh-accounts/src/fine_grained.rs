//! Fine-grained personal access tokens (P47) and organization token
//! policies. Enforcement lives in `bgh_core::pat`.
//!
//! Web JSON (browser session):
//! * `GET|POST /_bgh/fine-grained-tokens`, `GET|DELETE /_bgh/fine-grained-tokens/{id}`
//! * `GET /_bgh/fine-grained-tokens/owners`, `GET /_bgh/fine-grained-tokens/permissions`
//! * `GET|PATCH /_bgh/orgs/{org}/pat-policy` (org admins)
//!
//! GitHub REST (org admins):
//! * `GET|POST /orgs/{org}/personal-access-token-requests`,
//!   `POST /orgs/{org}/personal-access-token-requests/{id}`,
//!   `GET /orgs/{org}/personal-access-token-requests/{id}/repositories`
//! * `GET|POST /orgs/{org}/personal-access-tokens`,
//!   `POST /orgs/{org}/personal-access-tokens/{id}`,
//!   `GET /orgs/{org}/personal-access-tokens/{id}/repositories`
//!
//! A request (and a grant) is the token itself: its id is the token id.

use std::collections::{BTreeMap, HashMap};

use axum::extract::State;
use axum::http::StatusCode;
use bgh_core::audit;
use bgh_core::crypto;
use bgh_core::pat::{self, FineGrainedPermissions, Group, PatPolicy, PermissionDef};
use bgh_core::prelude::*;
use bgh_core::time::ts;
use bgh_core::views;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::apps::administers;
use crate::util;

/// Most repositories a token may select (GitHub's limit).
const MAX_SELECTED_REPOS: usize = 50;

#[derive(Debug, Clone, sqlx::FromRow)]
struct TokenRow {
    id: i64,
    user_id: i64,
    name: String,
    description: String,
    token_last_eight: String,
    resource_owner_id: Option<i64>,
    repository_selection: Option<String>,
    approval_status: Option<String>,
    approval_reason: Option<String>,
    permissions: Option<sqlx::types::Json<FineGrainedPermissions>>,
    expires_at: Option<DateTime<Utc>>,
    last_used_at: Option<DateTime<Utc>>,
    reviewed_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
}

const COLUMNS: &str = "id, user_id, name, description, token_last_eight, resource_owner_id, \
    repository_selection, approval_status, approval_reason, permissions, expires_at, \
    last_used_at, reviewed_at, created_at";

impl TokenRow {
    fn perms(&self) -> FineGrainedPermissions {
        self.permissions
            .as_ref()
            .map(|p| p.0.clone())
            .unwrap_or_default()
    }

    fn selection(&self) -> &str {
        self.repository_selection.as_deref().unwrap_or("public")
    }

    fn expired(&self) -> bool {
        self.expires_at.is_some_and(|e| e <= Utc::now())
    }
}

fn require_session(auth: &AuthContext) -> ApiResult<()> {
    if auth.is_session() {
        Ok(())
    } else {
        Err(ApiError::forbidden(
            "Token management requires a browser session.",
        ))
    }
}

/// Selected repositories of `tokens`, keyed by token id.
async fn token_repos(
    state: &AppState,
    ids: &[i64],
) -> ApiResult<HashMap<i64, Vec<db::Repository>>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        token_id: i64,
        #[sqlx(flatten)]
        repo: db::Repository,
    }
    let rows: Vec<Row> = sqlx::query_as(&format!(
        "SELECT tr.token_id, {} FROM access_token_repos tr
           JOIN repositories r ON r.id = tr.repo_id
          WHERE tr.token_id = ANY($1) ORDER BY lower(r.name), r.id",
        db::prefixed("r", db::Repository::COLUMNS)
    ))
    .bind(ids)
    .fetch_all(&state.db)
    .await?;
    let mut out: HashMap<i64, Vec<db::Repository>> = HashMap::new();
    for row in rows {
        out.entry(row.token_id).or_default().push(row.repo);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Web JSON
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct TokenRepo {
    pub id: i64,
    pub name: String,
    pub full_name: String,
    pub private: bool,
}

#[derive(Debug, Serialize)]
pub struct FineGrainedToken {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub token_last_eight: String,
    pub resource_owner: Option<api::SimpleUser>,
    pub repository_selection: String,
    pub repositories: Vec<TokenRepo>,
    pub permissions: FineGrainedPermissions,
    /// `active` | `pending` | `denied` | `revoked`
    pub status: String,
    pub approval_reason: Option<String>,
    pub expires_at: Option<Timestamp>,
    pub last_used_at: Option<Timestamp>,
    pub created_at: Timestamp,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

fn status_name(row: &TokenRow) -> String {
    match row.approval_status.as_deref() {
        Some("approved") | None => "active",
        Some(other) => other,
    }
    .to_string()
}

async fn render(
    state: &AppState,
    rows: &[TokenRow],
    token: Option<String>,
) -> ApiResult<Vec<FineGrainedToken>> {
    let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
    let mut repos = token_repos(state, &ids).await?;
    let owners = views::users_by_id(state, rows.iter().map(|r| r.resource_owner_id)).await?;
    let repo_owners =
        views::users_by_id(state, repos.values().flatten().map(|r| Some(r.owner_id))).await?;
    Ok(rows
        .iter()
        .map(|row| FineGrainedToken {
            id: row.id,
            name: row.name.clone(),
            description: row.description.clone(),
            token_last_eight: row.token_last_eight.clone(),
            resource_owner: row
                .resource_owner_id
                .and_then(|id| owners.get(&id))
                .map(|u| api::SimpleUser::new(&state.urls, u)),
            repository_selection: row.selection().to_string(),
            repositories: repos
                .remove(&row.id)
                .unwrap_or_default()
                .into_iter()
                .map(|r| TokenRepo {
                    full_name: format!(
                        "{}/{}",
                        repo_owners
                            .get(&r.owner_id)
                            .map(|u| u.login.as_str())
                            .unwrap_or("ghost"),
                        r.name
                    ),
                    id: r.id,
                    private: r.is_private(),
                    name: r.name,
                })
                .collect(),
            permissions: row.perms(),
            status: status_name(row),
            approval_reason: row.approval_reason.clone(),
            expires_at: ts(row.expires_at),
            last_used_at: ts(row.last_used_at),
            created_at: row.created_at.into(),
            token: token.clone(),
        })
        .collect())
}

/// `GET /_bgh/fine-grained-tokens`
pub async fn list(
    State(state): State<AppState>,
    auth: RequireUser,
) -> ApiResult<Json<Vec<FineGrainedToken>>> {
    require_session(&auth)?;
    let rows: Vec<TokenRow> = sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM access_tokens
          WHERE user_id = $1 AND kind = 'fine_grained' ORDER BY id DESC"
    ))
    .bind(auth.user.id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(render(&state, &rows, None).await?))
}

async fn own_token(state: &AppState, user_id: i64, id: i64) -> ApiResult<TokenRow> {
    sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM access_tokens
          WHERE id = $1 AND user_id = $2 AND kind = 'fine_grained'"
    ))
    .bind(id)
    .bind(user_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)
}

/// `GET /_bgh/fine-grained-tokens/{id}`
pub async fn get(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(id): Path<i64>,
) -> ApiResult<Json<FineGrainedToken>> {
    require_session(&auth)?;
    let row = own_token(&state, auth.user.id, id).await?;
    let mut out = render(&state, std::slice::from_ref(&row), None).await?;
    Ok(Json(out.remove(0)))
}

/// `DELETE /_bgh/fine-grained-tokens/{id}` → 204.
pub async fn delete(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    require_session(&auth)?;
    let mut tx = Tx::begin(&state).await?;
    let deleted = sqlx::query(
        "DELETE FROM access_tokens WHERE id = $1 AND user_id = $2 AND kind = 'fine_grained'",
    )
    .bind(id)
    .bind(auth.user.id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if deleted == 0 {
        return Err(ApiError::NotFound);
    }
    audit::log(
        &mut *tx,
        Some(&auth.user),
        "personal_access_token.destroy",
        audit::Target::Token(id),
        json!({ "fine_grained": true }),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
pub struct CreateBody {
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// Login of the user or organization; defaults to the caller.
    pub resource_owner: Option<String>,
    pub expires_in_days: Option<i64>,
    pub repository_selection: Option<String>,
    #[serde(default)]
    pub repository_ids: Vec<i64>,
    #[serde(default)]
    pub repositories: Vec<String>,
    #[serde(default)]
    pub permissions: FineGrainedPermissions,
    pub reason: Option<String>,
}

fn invalid(field: &str, message: impl Into<String>) -> ApiError {
    ApiError::invalid_field(FieldError::custom(
        "FineGrainedPersonalAccessToken",
        field,
        message.into(),
    ))
}

/// `POST /_bgh/fine-grained-tokens` → 201 with the plaintext token (shown
/// once). Tokens for an organization requiring approval start `pending`.
pub async fn create(
    State(state): State<AppState>,
    auth: RequireUser,
    Json(body): Json<CreateBody>,
) -> ApiResult<(StatusCode, Json<FineGrainedToken>)> {
    require_session(&auth)?;
    let name = body.name.unwrap_or_default().trim().to_string();
    if name.is_empty() {
        return Err(ApiError::invalid_field(FieldError::missing_field(
            "FineGrainedPersonalAccessToken",
            "name",
        )));
    }
    if name.chars().count() > 255 {
        return Err(invalid(
            "name",
            "name is too long (maximum is 255 characters)",
        ));
    }
    let description = body.description.unwrap_or_default().trim().to_string();
    // Resource owner: the caller or an organization they belong to.
    let owner = match body.resource_owner.as_deref() {
        None => auth.user.clone(),
        Some(login) if login.eq_ignore_ascii_case(&auth.user.login) => auth.user.clone(),
        Some(login) => {
            let org = util::find_account(&state, login)
                .await
                .map_err(|_| invalid("resource_owner", "unknown resource owner"))?;
            let member = org.is_org()
                && bgh_core::perms::org_role(&state.db, org.id, auth.user.id)
                    .await?
                    .is_some();
            if !member {
                return Err(invalid(
                    "resource_owner",
                    "resource owner must be you or an organization you belong to",
                ));
            }
            org
        }
    };
    let policy = if owner.is_org() {
        PatPolicy::load(&state.db, owner.id).await?
    } else {
        PatPolicy::default()
    };
    if !policy.fine_grained_allowed {
        return Err(invalid(
            "resource_owner",
            format!(
                "{} does not allow access via fine-grained personal access tokens",
                owner.login
            ),
        ));
    }
    let max_days = policy.max_lifetime_days();
    let days = body
        .expires_in_days
        .ok_or_else(|| invalid("expires_in_days", "an expiration is required"))?;
    if !(1..=max_days).contains(&days) {
        return Err(invalid(
            "expires_in_days",
            format!("expiration must be between 1 and {max_days} days"),
        ));
    }
    let expires_at = Utc::now() + Duration::days(days);
    let permissions = body
        .permissions
        .validated(owner.is_org())
        .map_err(|p| invalid("permissions", format!("invalid permission {p}")))?;
    let selection = body.repository_selection.unwrap_or_else(|| "public".into());
    let repos: Vec<db::Repository> = match selection.as_str() {
        "all" | "public" => Vec::new(),
        "selected" => {
            select_repos(
                &state,
                &auth,
                &owner,
                &body.repository_ids,
                &body.repositories,
            )
            .await?
        }
        _ => {
            return Err(invalid(
                "repository_selection",
                "must be one of all, selected, public",
            ));
        }
    };
    if selection == "public"
        && permissions
            .repository
            .iter()
            .any(|(k, v)| k != "metadata" && v == "write")
    {
        return Err(invalid(
            "permissions",
            "public repository access is read-only",
        ));
    }
    // Organizations requiring approval: owners' tokens and public-only
    // tokens without organization permissions are approved right away.
    let admin = owner.is_org() && administers(&state, &auth.user, &owner).await?;
    let needs_approval = owner.is_org()
        && policy.fine_grained_require_approval
        && !admin
        && !(selection == "public" && permissions.organization.is_empty());
    let repo_ids: Vec<i64> = repos.iter().map(|r| r.id).collect();
    let scopes = pat::token_scopes(
        owner.id,
        &selection,
        &repo_ids,
        !needs_approval,
        &permissions,
    );
    let reason = body
        .reason
        .map(|r| r.trim().to_string())
        .filter(|r| !r.is_empty());

    let token = crypto::new_fine_grained_pat();
    let mut tx = Tx::begin(&state).await?;
    let row: TokenRow = sqlx::query_as(&format!(
        "INSERT INTO access_tokens
            (user_id, kind, name, description, token_hash, token_last_eight, scopes, expires_at,
             resource_owner_id, repository_selection, approval_status, approval_reason, permissions)
         VALUES ($1, 'fine_grained', $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
         RETURNING {COLUMNS}"
    ))
    .bind(auth.user.id)
    .bind(&name)
    .bind(&description)
    .bind(crypto::sha256_hex(&token))
    .bind(&token[token.len() - 8..])
    .bind(&scopes)
    .bind(expires_at)
    .bind(owner.id)
    .bind(&selection)
    .bind(if needs_approval {
        "pending"
    } else {
        "approved"
    })
    .bind(&reason)
    .bind(sqlx::types::Json(&permissions))
    .fetch_one(&mut *tx)
    .await?;
    if !repo_ids.is_empty() {
        sqlx::query(
            "INSERT INTO access_token_repos (token_id, repo_id) SELECT $1, unnest($2::bigint[])",
        )
        .bind(row.id)
        .bind(&repo_ids)
        .execute(&mut *tx)
        .await?;
    }
    audit::log(
        &mut *tx,
        Some(&auth.user),
        "personal_access_token.create",
        audit::Target::Token(row.id),
        json!({
            "fine_grained": true,
            "resource_owner": owner.login,
            "repository_selection": selection,
            "permissions": permissions,
        }),
    )
    .await?;
    if needs_approval {
        audit::log(
            &mut *tx,
            Some(&auth.user),
            "personal_access_token.request_created",
            audit::Target::Org(owner.id),
            json!({ "token_id": row.id, "token_name": name }),
        )
        .await?;
    }
    tx.commit().await?;
    let mut out = render(&state, std::slice::from_ref(&row), Some(token)).await?;
    Ok((StatusCode::CREATED, Json(out.remove(0))))
}

/// Resolve the selected repositories (ids and names) of `owner` the caller
/// can read.
async fn select_repos(
    state: &AppState,
    auth: &AuthContext,
    owner: &db::User,
    ids: &[i64],
    names: &[String],
) -> ApiResult<Vec<db::Repository>> {
    let lower: Vec<String> = names
        .iter()
        .map(|n| n.rsplit('/').next().unwrap_or(n).to_lowercase())
        .collect();
    let repos: Vec<db::Repository> = sqlx::query_as(&format!(
        "SELECT {} FROM repositories
          WHERE owner_id = $1 AND (id = ANY($2) OR lower(name) = ANY($3)) ORDER BY id",
        db::Repository::COLUMNS
    ))
    .bind(owner.id)
    .bind(ids)
    .bind(&lower)
    .fetch_all(&state.db)
    .await?;
    let perms = bgh_core::perms::repo_permissions(&state.db, Some(auth.user.id), &repos).await?;
    let found_ids = ids.iter().all(|id| repos.iter().any(|r| r.id == *id));
    let found_names = lower
        .iter()
        .all(|n| repos.iter().any(|r| r.name.to_lowercase() == *n));
    let readable = repos
        .iter()
        .all(|r| perms.get(&r.id).copied().unwrap_or(Permission::None) >= Permission::Read);
    if !found_ids || !found_names || !readable {
        return Err(invalid(
            "repositories",
            format!(
                "at least one repository does not exist or is not accessible in {}",
                owner.login
            ),
        ));
    }
    if repos.is_empty() {
        return Err(invalid("repositories", "select at least one repository"));
    }
    if repos.len() > MAX_SELECTED_REPOS {
        return Err(invalid(
            "repositories",
            format!("select at most {MAX_SELECTED_REPOS} repositories"),
        ));
    }
    Ok(repos)
}

#[derive(Debug, Serialize)]
pub struct OwnerChoice {
    pub id: i64,
    pub login: String,
    pub avatar_url: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub fine_grained_allowed: bool,
    pub requires_approval: bool,
    pub max_lifetime_days: i64,
}

/// `GET /_bgh/fine-grained-tokens/owners` → the caller, then their
/// organizations, with each one's policy.
pub async fn owners(
    State(state): State<AppState>,
    auth: RequireUser,
) -> ApiResult<Json<Vec<OwnerChoice>>> {
    require_session(&auth)?;
    #[derive(sqlx::FromRow)]
    struct Row {
        #[sqlx(flatten)]
        org: db::User,
        role: bgh_core::perms::OrgRole,
        fine_grained_allowed: Option<bool>,
        fine_grained_require_approval: Option<bool>,
        fine_grained_max_lifetime_days: Option<i32>,
    }
    let rows: Vec<Row> = sqlx::query_as(&format!(
        "SELECT {}, m.role, p.fine_grained_allowed, p.fine_grained_require_approval,
                p.fine_grained_max_lifetime_days
           FROM org_members m JOIN users u ON u.id = m.org_id
           LEFT JOIN org_pat_policies p ON p.org_id = m.org_id
          WHERE m.user_id = $1 ORDER BY lower(u.login)",
        db::prefixed("u", db::User::COLUMNS)
    ))
    .bind(auth.user.id)
    .fetch_all(&state.db)
    .await?;
    let me = api::SimpleUser::new(&state.urls, &auth.user);
    let mut out = vec![OwnerChoice {
        id: auth.user.id,
        login: auth.user.login.clone(),
        avatar_url: me.avatar_url,
        kind: auth.user.kind.clone(),
        fine_grained_allowed: true,
        requires_approval: false,
        max_lifetime_days: pat::DEFAULT_MAX_LIFETIME_DAYS,
    }];
    for row in rows {
        let policy = PatPolicy {
            fine_grained_allowed: row.fine_grained_allowed.unwrap_or(true),
            fine_grained_require_approval: row.fine_grained_require_approval.unwrap_or(false),
            fine_grained_max_lifetime_days: row.fine_grained_max_lifetime_days,
            ..PatPolicy::default()
        };
        let simple = api::SimpleUser::new(&state.urls, &row.org);
        out.push(OwnerChoice {
            id: row.org.id,
            login: row.org.login.clone(),
            avatar_url: simple.avatar_url,
            kind: row.org.kind.clone(),
            fine_grained_allowed: policy.fine_grained_allowed,
            requires_approval: policy.fine_grained_require_approval && !row.role.is_admin(),
            max_lifetime_days: policy.max_lifetime_days(),
        });
    }
    Ok(Json(out))
}

#[derive(Debug, Serialize)]
pub struct Catalog {
    pub repository: &'static [PermissionDef],
    pub organization: &'static [PermissionDef],
    pub account: &'static [PermissionDef],
}

/// `GET /_bgh/fine-grained-tokens/permissions` → the permission catalog.
pub async fn permissions(_auth: RequireUser) -> Json<Catalog> {
    Json(Catalog {
        repository: pat::catalog(Group::Repository),
        organization: pat::catalog(Group::Organization),
        account: pat::catalog(Group::Account),
    })
}

// ---------------------------------------------------------------------------
// Organization policy (web JSON)
// ---------------------------------------------------------------------------

async fn admin_org(state: &AppState, auth: &AuthContext, org: &str) -> ApiResult<db::User> {
    let org = util::find_org(state, org).await?;
    if !administers(state, &auth.user, &org).await? {
        // Members see that the org exists; others don't.
        return if bgh_core::perms::org_role(&state.db, org.id, auth.user.id)
            .await?
            .is_some()
        {
            Err(ApiError::forbidden("You must be an organization owner."))
        } else {
            Err(ApiError::NotFound)
        };
    }
    Ok(org)
}

/// `GET /_bgh/orgs/{org}/pat-policy`
pub async fn get_policy(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(org): Path<String>,
) -> ApiResult<Json<PatPolicy>> {
    auth.require_scope("read:org")?;
    let org = admin_org(&state, &auth, &org).await?;
    Ok(Json(PatPolicy::load(&state.db, org.id).await?))
}

fn lifetime(v: &Value, field: &str, max: i32) -> ApiResult<Option<i32>> {
    match v {
        Value::Null => Ok(None),
        Value::Number(n) => n
            .as_i64()
            .filter(|d| (1..=i64::from(max)).contains(d))
            .map(|d| Some(d as i32))
            .ok_or_else(|| {
                ApiError::invalid_field(FieldError::custom(
                    "PatPolicy",
                    field,
                    format!("must be between 1 and {max} days, or null"),
                ))
            }),
        _ => Err(ApiError::invalid_field(FieldError::invalid(
            "PatPolicy",
            field,
        ))),
    }
}

fn flag(v: &Value, field: &str) -> ApiResult<bool> {
    v.as_bool()
        .ok_or_else(|| ApiError::invalid_field(FieldError::invalid("PatPolicy", field)))
}

/// `PATCH /_bgh/orgs/{org}/pat-policy` (partial) → the new policy.
pub async fn update_policy(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(org): Path<String>,
    Json(body): Json<BTreeMap<String, Value>>,
) -> ApiResult<Json<PatPolicy>> {
    auth.require_scope("admin:org")?;
    let org = admin_org(&state, &auth, &org).await?;
    let mut policy = PatPolicy::load(&state.db, org.id).await?;
    for (k, v) in &body {
        match k.as_str() {
            "fine_grained_allowed" => policy.fine_grained_allowed = flag(v, k)?,
            "fine_grained_require_approval" => policy.fine_grained_require_approval = flag(v, k)?,
            "fine_grained_max_lifetime_days" => {
                policy.fine_grained_max_lifetime_days = lifetime(v, k, 366)?
            }
            "classic_allowed" => policy.classic_allowed = flag(v, k)?,
            "classic_max_lifetime_days" => policy.classic_max_lifetime_days = lifetime(v, k, 3650)?,
            _ => {}
        }
    }
    let mut tx = Tx::begin(&state).await?;
    sqlx::query(
        "INSERT INTO org_pat_policies (org_id, fine_grained_allowed, fine_grained_require_approval,
             fine_grained_max_lifetime_days, classic_allowed, classic_max_lifetime_days)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (org_id) DO UPDATE SET
             fine_grained_allowed = EXCLUDED.fine_grained_allowed,
             fine_grained_require_approval = EXCLUDED.fine_grained_require_approval,
             fine_grained_max_lifetime_days = EXCLUDED.fine_grained_max_lifetime_days,
             classic_allowed = EXCLUDED.classic_allowed,
             classic_max_lifetime_days = EXCLUDED.classic_max_lifetime_days,
             updated_at = now()",
    )
    .bind(org.id)
    .bind(policy.fine_grained_allowed)
    .bind(policy.fine_grained_require_approval)
    .bind(policy.fine_grained_max_lifetime_days)
    .bind(policy.classic_allowed)
    .bind(policy.classic_max_lifetime_days)
    .execute(&mut *tx)
    .await?;
    audit::log(
        &mut *tx,
        Some(&auth.user),
        "org.personal_access_token_policy_update",
        audit::Target::Org(org.id),
        json!(policy),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(policy))
}

// ---------------------------------------------------------------------------
// GitHub REST: requests and grants
// ---------------------------------------------------------------------------

/// GitHub's `organization-programmatic-access-grant(-request)`.
#[derive(Debug, Serialize)]
pub struct Grant {
    pub id: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<Option<String>>,
    pub owner: api::SimpleUser,
    /// `none` (public repositories only) | `all` | `subset`
    pub repository_selection: &'static str,
    pub repositories_url: String,
    pub permissions: GrantPermissions,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<Timestamp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access_granted_at: Option<Timestamp>,
    pub token_id: i64,
    pub token_name: String,
    pub token_expired: bool,
    pub token_expires_at: Option<Timestamp>,
    pub token_last_used_at: Option<Timestamp>,
}

#[derive(Debug, Serialize)]
pub struct GrantPermissions {
    pub organization: BTreeMap<String, String>,
    pub repository: BTreeMap<String, String>,
    pub other: BTreeMap<String, String>,
}

/// Which list a REST endpoint serves.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Requests,
    Grants,
}

impl Kind {
    fn status(self) -> &'static str {
        match self {
            Self::Requests => "pending",
            Self::Grants => "approved",
        }
    }

    fn path(self) -> &'static str {
        match self {
            Self::Requests => "personal-access-token-requests",
            Self::Grants => "personal-access-tokens",
        }
    }
}

async fn grants_json(
    state: &AppState,
    org: &db::User,
    kind: Kind,
    rows: &[TokenRow],
) -> ApiResult<Vec<Grant>> {
    let users = views::users_by_id(state, rows.iter().map(|r| Some(r.user_id))).await?;
    Ok(rows
        .iter()
        .filter_map(|row| {
            let user = users.get(&row.user_id)?;
            let perms = row.perms();
            Some(Grant {
                id: row.id,
                reason: (kind == Kind::Requests).then(|| row.approval_reason.clone()),
                owner: api::SimpleUser::new(&state.urls, user),
                repository_selection: match row.selection() {
                    "all" => "all",
                    "selected" => "subset",
                    _ => "none",
                },
                repositories_url: state.urls.api(&format!(
                    "/orgs/{}/{}/{}/repositories",
                    org.login,
                    kind.path(),
                    row.id
                )),
                permissions: GrantPermissions {
                    organization: perms.organization,
                    repository: perms.repository,
                    other: perms.account,
                },
                created_at: (kind == Kind::Requests).then(|| row.created_at.into()),
                access_granted_at: (kind == Kind::Grants)
                    .then(|| row.reviewed_at.unwrap_or(row.created_at).into()),
                token_id: row.id,
                token_name: row.name.clone(),
                token_expired: row.expired(),
                token_expires_at: ts(row.expires_at),
                token_last_used_at: ts(row.last_used_at),
            })
        })
        .collect())
}

/// Org admin check for the REST endpoints (`read:org` for reads,
/// `admin:org` for writes).
async fn rest_admin_org(
    state: &AppState,
    auth: &AuthContext,
    org: &str,
    write: bool,
) -> ApiResult<db::User> {
    let org = util::find_org(state, org).await?;
    auth.require_scope(if write { "admin:org" } else { "read:org" })?;
    if !administers(state, &auth.user, &org).await? {
        return Err(ApiError::forbidden(
            "You must be an organization owner to manage personal access tokens.",
        ));
    }
    Ok(org)
}

async fn list_kind(
    state: &AppState,
    auth: &AuthContext,
    p: &Pagination,
    org: &str,
    query: &[(String, String)],
    kind: Kind,
) -> ApiResult<Page<Grant>> {
    let org = rest_admin_org(state, auth, org, false).await?;
    let mut owners: Vec<String> = Vec::new();
    let mut asc = false;
    let mut sort_by_name = false;
    for (k, v) in query {
        match k.as_str() {
            "owner" | "owner[]" => owners.extend(
                v.split(',')
                    .map(|s| s.trim().to_lowercase())
                    .filter(|s| !s.is_empty()),
            ),
            "direction" => asc = v.eq_ignore_ascii_case("asc"),
            "sort" => sort_by_name = v == "token_name",
            _ => {}
        }
    }
    let order = match (sort_by_name, asc) {
        (true, true) => "lower(t.name) ASC, t.id ASC",
        (true, false) => "lower(t.name) DESC, t.id DESC",
        (false, true) => "t.created_at ASC, t.id ASC",
        (false, false) => "t.created_at DESC, t.id DESC",
    };
    let rows: Vec<TokenRow> = sqlx::query_as(&format!(
        "SELECT {} FROM access_tokens t
          WHERE t.resource_owner_id = $1 AND t.kind = 'fine_grained' AND t.approval_status = $2
            AND (cardinality($3::text[]) = 0
                 OR t.user_id IN (SELECT id FROM users WHERE lower(login) = ANY($3)))
          ORDER BY {order} LIMIT $4 OFFSET $5",
        db::prefixed("t", COLUMNS)
    ))
    .bind(org.id)
    .bind(kind.status())
    .bind(&owners)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let page = p.page(rows);
    let items = grants_json(state, &org, kind, &page.items).await?;
    Ok(Page {
        items,
        link: page.link,
    })
}

/// `GET /orgs/{org}/personal-access-token-requests`
pub async fn list_requests(
    State(state): State<AppState>,
    auth: RequireUser,
    p: Pagination,
    Path(org): Path<String>,
    Query(query): Query<Vec<(String, String)>>,
) -> ApiResult<Page<Grant>> {
    list_kind(&state, &auth, &p, &org, &query, Kind::Requests).await
}

/// `GET /orgs/{org}/personal-access-tokens`
pub async fn list_grants(
    State(state): State<AppState>,
    auth: RequireUser,
    p: Pagination,
    Path(org): Path<String>,
    Query(query): Query<Vec<(String, String)>>,
) -> ApiResult<Page<Grant>> {
    list_kind(&state, &auth, &p, &org, &query, Kind::Grants).await
}

#[derive(Debug, Deserialize)]
pub struct ReviewBody {
    pub action: Option<String>,
    pub reason: Option<String>,
    #[serde(default)]
    pub pat_request_ids: Option<Vec<i64>>,
    #[serde(default)]
    pub pat_ids: Option<Vec<i64>>,
}

/// Apply `action` to the tokens `ids` of `org` in `kind`'s state.
async fn review(
    state: &AppState,
    auth: &AuthContext,
    org: &db::User,
    kind: Kind,
    ids: &[i64],
    action: &str,
    reason: Option<&str>,
) -> ApiResult<()> {
    let (status, audit_action) = match (kind, action) {
        (Kind::Requests, "approve") => ("approved", "personal_access_token.request_approved"),
        (Kind::Requests, "deny") => ("denied", "personal_access_token.request_denied"),
        (Kind::Grants, "revoke") => ("revoked", "personal_access_token.access_revoked"),
        _ => {
            return Err(ApiError::invalid_field(FieldError::invalid(
                "PersonalAccessToken",
                "action",
            )));
        }
    };
    if ids.is_empty() || ids.len() > 100 {
        return Err(ApiError::unprocessable(
            "Between 1 and 100 ids must be given.",
        ));
    }
    let mut tx = Tx::begin(state).await?;
    // Pending marker: removed on approval, added otherwise.
    let updated: Vec<(i64, String)> = sqlx::query_as(&format!(
        "UPDATE access_tokens SET approval_status = $3, reviewed_by_id = $4,
                reviewed_at = now(), review_reason = $5,
                scopes = CASE WHEN $3 = 'approved' THEN array_remove(scopes, '{pending}')
                              WHEN '{pending}' = ANY(scopes) THEN scopes
                              ELSE array_append(scopes, '{pending}') END
          WHERE id = ANY($1) AND resource_owner_id = $2 AND kind = 'fine_grained'
            AND approval_status = $6
         RETURNING id, name",
        pending = pat::PENDING_SCOPE
    ))
    .bind(ids)
    .bind(org.id)
    .bind(status)
    .bind(auth.user.id)
    .bind(reason)
    .bind(kind.status())
    .fetch_all(&mut *tx)
    .await?;
    if updated.len() != {
        let mut unique = ids.to_vec();
        unique.sort_unstable();
        unique.dedup();
        unique.len()
    } {
        // Rolled back: some id isn't in the expected state for this org.
        return Err(ApiError::NotFound);
    }
    for (id, name) in &updated {
        audit::log(
            &mut *tx,
            Some(&auth.user),
            audit_action,
            audit::Target::Org(org.id),
            json!({ "token_id": id, "token_name": name, "reason": reason }),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// `POST /orgs/{org}/personal-access-token-requests` → 202 `{}`.
pub async fn review_requests(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(org): Path<String>,
    Json(body): Json<ReviewBody>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let org = rest_admin_org(&state, &auth, &org, true).await?;
    let ids = body.pat_request_ids.unwrap_or_default();
    let action = body.action.unwrap_or_default();
    review(
        &state,
        &auth,
        &org,
        Kind::Requests,
        &ids,
        &action,
        body.reason.as_deref(),
    )
    .await?;
    Ok((StatusCode::ACCEPTED, Json(json!({}))))
}

/// `POST /orgs/{org}/personal-access-token-requests/{pat_request_id}` → 204.
pub async fn review_request(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((org, id)): Path<(String, i64)>,
    Json(body): Json<ReviewBody>,
) -> ApiResult<StatusCode> {
    let org = rest_admin_org(&state, &auth, &org, true).await?;
    let action = body.action.unwrap_or_default();
    review(
        &state,
        &auth,
        &org,
        Kind::Requests,
        &[id],
        &action,
        body.reason.as_deref(),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /orgs/{org}/personal-access-tokens` → 202 `{}`.
pub async fn revoke_grants(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(org): Path<String>,
    Json(body): Json<ReviewBody>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let org = rest_admin_org(&state, &auth, &org, true).await?;
    let ids = body.pat_ids.unwrap_or_default();
    let action = body.action.unwrap_or_default();
    review(&state, &auth, &org, Kind::Grants, &ids, &action, None).await?;
    Ok((StatusCode::ACCEPTED, Json(json!({}))))
}

/// `POST /orgs/{org}/personal-access-tokens/{pat_id}` → 204.
pub async fn revoke_grant(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((org, id)): Path<(String, i64)>,
    Json(body): Json<ReviewBody>,
) -> ApiResult<StatusCode> {
    let org = rest_admin_org(&state, &auth, &org, true).await?;
    let action = body.action.unwrap_or_default();
    review(&state, &auth, &org, Kind::Grants, &[id], &action, None).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn repositories_kind(
    state: &AppState,
    auth: &AuthContext,
    p: &Pagination,
    org: &str,
    id: i64,
    kind: Kind,
) -> ApiResult<Page<api::MinimalRepository>> {
    let org = rest_admin_org(state, auth, org, false).await?;
    let row: TokenRow = sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM access_tokens
          WHERE id = $1 AND resource_owner_id = $2 AND kind = 'fine_grained'
            AND approval_status = $3"
    ))
    .bind(id)
    .bind(org.id)
    .bind(kind.status())
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)?;
    let rows: Vec<db::Repository> = match row.selection() {
        "selected" => {
            sqlx::query_as(&format!(
                "SELECT {} FROM repositories r JOIN access_token_repos tr ON tr.repo_id = r.id
              WHERE tr.token_id = $1 ORDER BY lower(r.name), r.id LIMIT $2 OFFSET $3",
                db::prefixed("r", db::Repository::COLUMNS)
            ))
            .bind(row.id)
            .bind(p.limit_plus_one())
            .bind(p.offset())
            .fetch_all(&state.db)
            .await?
        }
        "all" => {
            sqlx::query_as(&format!(
                "SELECT {} FROM repositories WHERE owner_id = $1 ORDER BY lower(name), id
              LIMIT $2 OFFSET $3",
                db::Repository::COLUMNS
            ))
            .bind(org.id)
            .bind(p.limit_plus_one())
            .bind(p.offset())
            .fetch_all(&state.db)
            .await?
        }
        _ => Vec::new(),
    };
    let page = p.page(rows);
    let items = views::minimal_repos(state, Some(auth), page.items).await?;
    Ok(Page {
        items,
        link: page.link,
    })
}

/// `GET /orgs/{org}/personal-access-token-requests/{pat_request_id}/repositories`
pub async fn request_repositories(
    State(state): State<AppState>,
    auth: RequireUser,
    p: Pagination,
    Path((org, id)): Path<(String, i64)>,
) -> ApiResult<Page<api::MinimalRepository>> {
    repositories_kind(&state, &auth, &p, &org, id, Kind::Requests).await
}

/// `GET /orgs/{org}/personal-access-tokens/{pat_id}/repositories`
pub async fn grant_repositories(
    State(state): State<AppState>,
    auth: RequireUser,
    p: Pagination,
    Path((org, id)): Path<(String, i64)>,
) -> ApiResult<Page<api::MinimalRepository>> {
    repositories_kind(&state, &auth, &p, &org, id, Kind::Grants).await
}
