//! GitHub Apps, part 1: registration (owned by a user or an organization),
//! private keys, the app JWT endpoints, installations (install flow and
//! user endpoints) and installation access tokens.
//!
//! * [`manage`]: web-client JSON for registering and editing apps and
//!   their keys (`/_bgh/apps…`).
//! * [`install`]: the install flow and installation settings
//!   (`/_bgh/apps/{slug}/install`, `/_bgh/installations…`) and GitHub's user
//!   endpoints (`/user/installations…`, `/orgs/{org}/installations`).
//! * [`rest`]: GitHub's app endpoints (`/app…`, `/apps/{slug}`, the
//!   `…/installation` lookups, `/installation/…`).
//!
//! Authentication (JWT, `bghs_` tokens) and enforcement live in
//! `bgh_core::apps`. Webhook delivery and installation events: P46.

pub mod install;
pub mod manage;
pub mod rest;

use std::collections::{BTreeMap, HashMap};

use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use bgh_core::apps::{AppRow, Installation, InstallationRow, Integration};
use bgh_core::prelude::*;
use serde::Serialize;

/// Permissions an app can request (GitHub's names) and the highest level
/// each accepts.
pub const PERMISSIONS: &[(&str, &str)] = &[
    // Repository permissions.
    ("actions", "write"),
    ("administration", "write"),
    ("attestations", "write"),
    ("checks", "write"),
    ("codespaces", "write"),
    ("contents", "write"),
    ("dependabot_secrets", "write"),
    ("deployments", "write"),
    ("discussions", "write"),
    ("environments", "write"),
    ("issues", "write"),
    ("merge_queues", "write"),
    ("metadata", "read"),
    ("packages", "write"),
    ("pages", "write"),
    ("pull_requests", "write"),
    ("repository_custom_properties", "write"),
    ("repository_hooks", "write"),
    ("repository_projects", "admin"),
    ("secret_scanning_alerts", "write"),
    ("secrets", "write"),
    ("security_events", "write"),
    ("single_file", "write"),
    ("statuses", "write"),
    ("vulnerability_alerts", "write"),
    ("workflows", "write"),
    // Organization permissions.
    ("members", "write"),
    ("organization_administration", "write"),
    ("organization_announcement_banners", "write"),
    ("organization_custom_roles", "write"),
    ("organization_events", "read"),
    ("organization_hooks", "write"),
    ("organization_packages", "write"),
    ("organization_personal_access_token_requests", "write"),
    ("organization_personal_access_tokens", "write"),
    ("organization_plan", "read"),
    ("organization_projects", "admin"),
    ("organization_secrets", "write"),
    ("organization_self_hosted_runners", "write"),
    ("organization_user_blocking", "write"),
    ("team_discussions", "write"),
    // Account permissions.
    ("email_addresses", "write"),
    ("followers", "write"),
    ("git_ssh_keys", "write"),
    ("gpg_keys", "write"),
    ("interaction_limits", "write"),
    ("profile", "write"),
    ("starring", "write"),
];

/// Webhook events an app can subscribe to.
pub const EVENTS: &[&str] = &[
    "branch_protection_rule",
    "check_run",
    "check_suite",
    "commit_comment",
    "create",
    "delete",
    "deployment",
    "deployment_status",
    "discussion",
    "discussion_comment",
    "fork",
    "gollum",
    "issue_comment",
    "issues",
    "label",
    "member",
    "membership",
    "merge_group",
    "meta",
    "milestone",
    "organization",
    "package",
    "page_build",
    "project",
    "project_card",
    "project_column",
    "public",
    "pull_request",
    "pull_request_review",
    "pull_request_review_comment",
    "pull_request_review_thread",
    "push",
    "registry_package",
    "release",
    "repository",
    "repository_dispatch",
    "star",
    "status",
    "team",
    "team_add",
    "watch",
    "workflow_dispatch",
    "workflow_job",
    "workflow_run",
];

/// Rank of an access level (`read` < `write` < `admin`).
pub fn level(access: &str) -> Option<u8> {
    match access {
        "read" => Some(1),
        "write" => Some(2),
        "admin" => Some(3),
        _ => None,
    }
}

/// Validate a requested permission map against [`PERMISSIONS`]; drops
/// `none` entries.
pub fn validate_permissions(
    resource: &str,
    perms: &BTreeMap<String, String>,
) -> ApiResult<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for (k, v) in perms {
        if v == "none" {
            continue;
        }
        let max = PERMISSIONS.iter().find(|(name, _)| name == k).map(|p| p.1);
        match (max.and_then(level), level(v)) {
            (Some(max), Some(l)) if l <= max => {
                out.insert(k.clone(), v.clone());
            }
            _ => {
                return Err(ApiError::invalid_field(FieldError::custom(
                    resource,
                    "permissions",
                    format!("{k} is not a valid permission or level ({v})"),
                )));
            }
        }
    }
    Ok(out)
}

/// App slug from its name: lowercase ASCII alphanumerics joined by `-`.
pub fn slugify(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

/// Login of an app's bot user.
pub fn bot_login(slug: &str) -> String {
    format!("{slug}[bot]")
}

/// App by slug, else 404.
pub async fn app_by_slug(db: impl sqlx::PgExecutor<'_>, slug: &str) -> ApiResult<AppRow> {
    sqlx::query_as(&format!(
        "SELECT {} FROM github_apps WHERE lower(slug) = lower($1)",
        AppRow::COLUMNS
    ))
    .bind(slug)
    .fetch_optional(db)
    .await?
    .ok_or(ApiError::NotFound)
}

/// App by id, else 404.
pub async fn app_by_id(db: impl sqlx::PgExecutor<'_>, id: i64) -> ApiResult<AppRow> {
    sqlx::query_as(&format!(
        "SELECT {} FROM github_apps WHERE id = $1",
        AppRow::COLUMNS
    ))
    .bind(id)
    .fetch_optional(db)
    .await?
    .ok_or(ApiError::NotFound)
}

/// Installation by id, else 404.
pub async fn installation_by_id(
    db: impl sqlx::PgExecutor<'_>,
    id: i64,
) -> ApiResult<InstallationRow> {
    sqlx::query_as(&format!(
        "SELECT {} FROM app_installations WHERE id = $1",
        InstallationRow::COLUMNS
    ))
    .bind(id)
    .fetch_optional(db)
    .await?
    .ok_or(ApiError::NotFound)
}

/// Whether `user` administers `account` (it is the user, or an org the
/// user is an admin of; site admins administer every account).
pub async fn administers(state: &AppState, user: &db::User, account: &db::User) -> ApiResult<bool> {
    if user.id == account.id || user.site_admin {
        return Ok(true);
    }
    if !account.is_org() {
        return Ok(false);
    }
    Ok(bgh_core::perms::org_role(&state.db, account.id, user.id)
        .await?
        .as_deref()
        == Some("admin"))
}

/// Render one app.
pub async fn integration(
    state: &AppState,
    app: &AppRow,
    with_count: bool,
) -> ApiResult<Integration> {
    let owner = db::User::find(&state.db, app.owner_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let count = if with_count {
        Some(
            sqlx::query_scalar("SELECT count(*) FROM app_installations WHERE app_id = $1")
                .bind(app.id)
                .fetch_one(&state.db)
                .await?,
        )
    } else {
        None
    };
    Ok(Integration::new(&state.urls, app, &owner, count))
}

/// Render installations (batch-loads apps and accounts).
pub async fn installations_json(
    state: &AppState,
    rows: &[InstallationRow],
) -> ApiResult<Vec<Installation>> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let mut app_ids: Vec<i64> = rows.iter().map(|r| r.app_id).collect();
    app_ids.sort_unstable();
    app_ids.dedup();
    let apps: Vec<AppRow> = sqlx::query_as(&format!(
        "SELECT {} FROM github_apps WHERE id = ANY($1)",
        AppRow::COLUMNS
    ))
    .bind(&app_ids)
    .fetch_all(&state.db)
    .await?;
    let apps: HashMap<i64, AppRow> = apps.into_iter().map(|a| (a.id, a)).collect();
    let users = bgh_core::views::users_by_id(
        state,
        rows.iter()
            .flat_map(|r| [Some(r.account_id), r.suspended_by_id]),
    )
    .await?;
    Ok(rows
        .iter()
        .filter_map(|r| {
            Some(Installation::new(
                &state.urls,
                r,
                apps.get(&r.app_id)?,
                users.get(&r.account_id)?,
                r.suspended_by_id.and_then(|id| users.get(&id)),
            ))
        })
        .collect())
}

/// Repositories of an installation (every repository of the account for
/// `all`), ordered by id, paginated with `limit`/`offset`.
pub async fn installation_repos(
    state: &AppState,
    inst: &InstallationRow,
    limit: i64,
    offset: i64,
) -> ApiResult<(Vec<db::Repository>, i64)> {
    const FILTER: &str = "(($2 AND r.owner_id = $1) OR (NOT $2 AND r.id IN \
        (SELECT repo_id FROM app_installation_repos WHERE installation_id = $3)))";
    let rows: Vec<db::Repository> = sqlx::query_as(&format!(
        "SELECT {} FROM repositories r WHERE {FILTER} ORDER BY r.id LIMIT $4 OFFSET $5",
        db::prefixed("r", db::Repository::COLUMNS)
    ))
    .bind(inst.account_id)
    .bind(inst.all_repositories())
    .bind(inst.id)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;
    let total: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM repositories r WHERE {FILTER}"
    ))
    .bind(inst.account_id)
    .bind(inst.all_repositories())
    .bind(inst.id)
    .fetch_one(&state.db)
    .await?;
    Ok((rows, total))
}

/// Full repository JSON for `repos` (all owned by one account) with the
/// given permissions.
pub fn repos_json(
    state: &AppState,
    repos: &[db::Repository],
    owners: &HashMap<i64, db::User>,
    perm: impl Fn(&db::Repository) -> Option<Permission>,
) -> Vec<api::Repository> {
    repos
        .iter()
        .filter_map(|r| {
            Some(api::Repository::new(
                &state.urls,
                r,
                owners.get(&r.owner_id)?,
                perm(r),
                Default::default(),
            ))
        })
        .collect()
}

/// A JSON object response with a pagination `Link` header (GitHub's
/// wrapped lists: `{total_count, installations}`, ...).
pub fn wrapped<T: Serialize>(p: &Pagination, has_next: bool, total: i64, body: T) -> Response {
    let mut resp = Json(body).into_response();
    if let Some(link) = p.link_header(has_next, Some(total))
        && let Ok(v) = HeaderValue::from_str(&link)
    {
        resp.headers_mut().insert(header::LINK, v);
    }
    resp
}

/// Revoke the access tokens of an installation (suspension, selection
/// changes).
pub async fn revoke_tokens(conn: &mut sqlx::PgConnection, installation_id: i64) -> ApiResult<()> {
    sqlx::query("DELETE FROM access_tokens WHERE installation_id = $1")
        .bind(installation_id)
        .execute(conn)
        .await?;
    Ok(())
}

/// Drop `repo_id` from the installation's existing tokens.
pub async fn revoke_repo(
    conn: &mut sqlx::PgConnection,
    installation_id: i64,
    repo_id: i64,
) -> ApiResult<()> {
    sqlx::query(
        "UPDATE access_tokens SET scopes = array_remove(scopes, $2) WHERE installation_id = $1",
    )
    .bind(installation_id)
    .bind(format!("{}{repo_id}", bgh_core::apps::REPO_SCOPE_PREFIX))
    .execute(conn)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        assert_eq!(slugify("My Cool App!"), "my-cool-app");
        assert_eq!(slugify("  Renovate  bot "), "renovate-bot");
        assert_eq!(slugify("ÄÖ"), "");
    }

    #[test]
    fn permission_validation() {
        let mut p = BTreeMap::new();
        p.insert("contents".to_string(), "write".to_string());
        p.insert("issues".to_string(), "none".to_string());
        let v = validate_permissions("Integration", &p).unwrap();
        assert_eq!(v.len(), 1);
        p.insert("metadata".to_string(), "write".to_string());
        assert!(validate_permissions("Integration", &p).is_err());
        let mut q = BTreeMap::new();
        q.insert("nope".to_string(), "read".to_string());
        assert!(validate_permissions("Integration", &q).is_err());
    }
}
