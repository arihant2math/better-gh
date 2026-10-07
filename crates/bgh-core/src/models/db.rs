//! Database row structs (`#[derive(FromRow)]`) for core tables.
//!
//! Each struct has a `COLUMNS` constant listing its columns so queries can
//! select exactly what the struct needs:
//! `format!("SELECT {} FROM users WHERE id = $1", User::COLUMNS)`.
//! When joining, use [`prefixed`] to qualify column names with an alias.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

/// Qualify a `COLUMNS` list with a table alias: `prefixed("u", User::COLUMNS)`
/// → `u.id, u.login, ...`.
pub fn prefixed(alias: &str, columns: &str) -> String {
    columns
        .split(',')
        .map(|c| format!("{alias}.{}", c.trim()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// `users` row (users, organizations and bots).
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct User {
    pub id: i64,
    pub login: String,
    /// `User` | `Organization` | `Bot`
    #[sqlx(rename = "type")]
    pub kind: String,
    pub name: Option<String>,
    pub email: Option<String>,
    pub bio: Option<String>,
    pub company: Option<String>,
    pub location: Option<String>,
    pub blog: Option<String>,
    pub twitter_username: Option<String>,
    pub hireable: Option<bool>,
    pub avatar_url: Option<String>,
    pub site_admin: bool,
    pub suspended_at: Option<DateTime<Utc>>,
    #[serde(skip)]
    pub password_hash: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl User {
    pub const COLUMNS: &'static str = "id, login, type, name, email, bio, company, location, blog, \
        twitter_username, hireable, avatar_url, site_admin, suspended_at, password_hash, \
        created_at, updated_at";

    pub fn is_org(&self) -> bool {
        self.kind == "Organization"
    }

    pub fn is_suspended(&self) -> bool {
        self.suspended_at.is_some()
    }

    /// Load by id.
    pub async fn find(db: impl sqlx::PgExecutor<'_>, id: i64) -> Result<Option<Self>, sqlx::Error> {
        sqlx::query_as::<_, Self>(&format!(
            "SELECT {} FROM users WHERE id = $1",
            Self::COLUMNS
        ))
        .bind(id)
        .fetch_optional(db)
        .await
    }

    /// Load by login (case-insensitive).
    pub async fn find_by_login(
        db: impl sqlx::PgExecutor<'_>,
        login: &str,
    ) -> Result<Option<Self>, sqlx::Error> {
        sqlx::query_as::<_, Self>(&format!(
            "SELECT {} FROM users WHERE lower(login) = lower($1)",
            Self::COLUMNS
        ))
        .bind(login)
        .fetch_optional(db)
        .await
    }

    /// Load many by id (order not preserved). Use to batch-load actors and
    /// avoid N+1 queries.
    pub async fn find_many(
        db: impl sqlx::PgExecutor<'_>,
        ids: &[i64],
    ) -> Result<Vec<Self>, sqlx::Error> {
        sqlx::query_as::<_, Self>(&format!(
            "SELECT {} FROM users WHERE id = ANY($1)",
            Self::COLUMNS
        ))
        .bind(ids)
        .fetch_all(db)
        .await
    }
}

/// Fields for inserting a user account (validation is the caller's job;
/// see `bgh_accounts` for the validated sign-up flow).
#[derive(Debug, Clone, Default)]
pub struct NewUser<'a> {
    pub login: &'a str,
    /// Primary email, stored in `user_emails` when given.
    pub email: Option<&'a str>,
    /// Whether the primary email is already proven to belong to the user.
    /// Only trusted sources (site admins, LDAP, SCIM, SAML, an IdP's
    /// `email_verified`) pass `true`; self-service sign-up must pass `false`
    /// and send a verification mail. Callers inserting a verified address
    /// release other accounts' unverified claims on it first
    /// (`bgh_accounts::emails::release_unverified`).
    pub email_verified: bool,
    pub name: Option<&'a str>,
    /// Argon2 PHC hash (see `crypto::hash_password`).
    pub password_hash: Option<&'a str>,
    pub site_admin: bool,
}

impl NewUser<'_> {
    /// Insert the user (and primary email). Fails with a unique violation
    /// (`users_login_key` / `user_emails_email_key`) on duplicates.
    pub async fn insert(&self, conn: &mut sqlx::PgConnection) -> Result<User, sqlx::Error> {
        let user: User = sqlx::query_as(&format!(
            "INSERT INTO users (login, type, name, password_hash, site_admin)
             VALUES ($1, 'User', $2, $3, $4) RETURNING {}",
            User::COLUMNS
        ))
        .bind(self.login)
        .bind(self.name)
        .bind(self.password_hash)
        .bind(self.site_admin)
        .fetch_one(&mut *conn)
        .await?;
        if let Some(email) = self.email {
            sqlx::query(
                "INSERT INTO user_emails (user_id, email, verified, is_primary, visibility)
                 VALUES ($1, $2, $3, true, 'private')",
            )
            .bind(user.id)
            .bind(email)
            .bind(self.email_verified)
            .execute(&mut *conn)
            .await?;
            crate::signatures::forget_email(&mut *conn, email).await?;
        }
        Ok(user)
    }
}

/// Insert an organization (`users` row + `org_settings`) with `admin_id` as
/// its first admin member.
pub async fn insert_org(
    conn: &mut sqlx::PgConnection,
    login: &str,
    name: Option<&str>,
    admin_id: i64,
) -> Result<User, sqlx::Error> {
    let org: User = sqlx::query_as(&format!(
        "INSERT INTO users (login, type, name) VALUES ($1, 'Organization', $2) RETURNING {}",
        User::COLUMNS
    ))
    .bind(login)
    .bind(name)
    .fetch_one(&mut *conn)
    .await?;
    sqlx::query("INSERT INTO org_settings (org_id) VALUES ($1)")
        .bind(org.id)
        .execute(&mut *conn)
        .await?;
    sqlx::query("INSERT INTO org_members (org_id, user_id, role) VALUES ($1, $2, 'admin')")
        .bind(org.id)
        .bind(admin_id)
        .execute(&mut *conn)
        .await?;
    Ok(org)
}

/// `org_settings` row.
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct OrgSettings {
    pub org_id: i64,
    pub description: Option<String>,
    pub billing_email: Option<String>,
    pub is_verified: bool,
    pub default_repository_permission: String,
    pub members_can_create_repositories: bool,
    pub members_can_create_public_repositories: bool,
    pub members_can_create_private_repositories: bool,
    pub members_can_fork_private_repositories: bool,
    pub two_factor_requirement_enabled: bool,
    pub members_can_create_internal_repositories: bool,
    pub has_organization_projects: bool,
    pub has_repository_projects: bool,
    pub archived_at: Option<DateTime<Utc>>,
}

impl OrgSettings {
    pub const COLUMNS: &'static str = "org_id, description, billing_email, is_verified, \
        default_repository_permission, members_can_create_repositories, \
        members_can_create_public_repositories, members_can_create_private_repositories, \
        members_can_fork_private_repositories, two_factor_requirement_enabled, \
        members_can_create_internal_repositories, \
        has_organization_projects, has_repository_projects, archived_at";

    pub async fn find(
        db: impl sqlx::PgExecutor<'_>,
        org_id: i64,
    ) -> Result<Option<Self>, sqlx::Error> {
        sqlx::query_as::<_, Self>(&format!(
            "SELECT {} FROM org_settings WHERE org_id = $1",
            Self::COLUMNS
        ))
        .bind(org_id)
        .fetch_optional(db)
        .await
    }
}

/// `sessions` row.
#[derive(Debug, Clone, FromRow)]
pub struct Session {
    pub id: i64,
    pub token_hash: String,
    pub user_id: i64,
    pub user_agent: Option<String>,
    pub ip: Option<String>,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

/// `access_tokens` row.
#[derive(Debug, Clone, FromRow)]
pub struct AccessToken {
    pub id: i64,
    pub user_id: i64,
    pub kind: String,
    pub name: String,
    pub token_hash: String,
    pub token_last_eight: String,
    pub scopes: Vec<String>,
    pub expires_at: Option<DateTime<Utc>>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

impl AccessToken {
    pub const COLUMNS: &'static str = "id, user_id, kind, name, token_hash, token_last_eight, \
        scopes, expires_at, last_used_at, created_at";
}

/// `teams` row.
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct Team {
    pub id: i64,
    pub org_id: i64,
    pub parent_id: Option<i64>,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub privacy: String,
    pub notification_setting: String,
    pub permission: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Team {
    pub const COLUMNS: &'static str = "id, org_id, parent_id, name, slug, description, privacy, \
        notification_setting, permission, created_at, updated_at";
}

/// `repositories` row.
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct Repository {
    pub id: i64,
    pub owner_id: i64,
    pub name: String,
    pub description: Option<String>,
    pub homepage: Option<String>,
    /// `public` | `private` | `internal`
    pub visibility: String,
    pub fork: bool,
    pub parent_id: Option<i64>,
    pub source_id: Option<i64>,
    pub template_repository_id: Option<i64>,
    pub default_branch: String,
    pub archived: bool,
    pub disabled: bool,
    pub is_template: bool,
    pub allow_forking: bool,
    pub has_issues: bool,
    pub has_projects: bool,
    pub has_wiki: bool,
    pub has_discussions: bool,
    pub has_pages: bool,
    pub allow_merge_commit: bool,
    pub allow_squash_merge: bool,
    pub allow_rebase_merge: bool,
    pub allow_auto_merge: bool,
    pub allow_update_branch: bool,
    pub delete_branch_on_merge: bool,
    pub use_squash_pr_title_as_default: bool,
    pub squash_merge_commit_title: String,
    pub squash_merge_commit_message: String,
    pub merge_commit_title: String,
    pub merge_commit_message: String,
    pub web_commit_signoff_required: bool,
    pub topics: Vec<String>,
    pub language: Option<String>,
    pub license_spdx_id: Option<String>,
    pub next_issue_number: i64,
    pub size: i64,
    pub stargazers_count: i64,
    pub watchers_count: i64,
    pub forks_count: i64,
    pub open_issues_count: i64,
    pub pushed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Upstream URL of a pull mirror (no credentials); `None` otherwise.
    pub mirror_url: Option<String>,
}

impl Repository {
    pub const COLUMNS: &'static str = "id, owner_id, name, description, homepage, visibility, \
        fork, parent_id, source_id, template_repository_id, default_branch, archived, disabled, \
        is_template, allow_forking, has_issues, has_projects, has_wiki, has_discussions, \
        has_pages, allow_merge_commit, allow_squash_merge, allow_rebase_merge, allow_auto_merge, \
        allow_update_branch, delete_branch_on_merge, use_squash_pr_title_as_default, \
        squash_merge_commit_title, squash_merge_commit_message, merge_commit_title, \
        merge_commit_message, web_commit_signoff_required, topics, language, license_spdx_id, \
        next_issue_number, size, stargazers_count, watchers_count, forks_count, \
        open_issues_count, pushed_at, created_at, updated_at, mirror_url";

    pub fn is_private(&self) -> bool {
        self.visibility != "public"
    }

    /// `internal`: readable by every signed-in, non-suspended user of the
    /// instance (GHES semantics); still "private" for JSON and token scopes.
    pub fn is_internal(&self) -> bool {
        self.visibility == "internal"
    }

    pub async fn find(db: impl sqlx::PgExecutor<'_>, id: i64) -> Result<Option<Self>, sqlx::Error> {
        sqlx::query_as::<_, Self>(&format!(
            "SELECT {} FROM repositories WHERE id = $1",
            Self::COLUMNS
        ))
        .bind(id)
        .fetch_optional(db)
        .await
    }

    /// Load by `owner_id` + name (case-insensitive).
    pub async fn find_by_name(
        db: impl sqlx::PgExecutor<'_>,
        owner_id: i64,
        name: &str,
    ) -> Result<Option<Self>, sqlx::Error> {
        sqlx::query_as::<_, Self>(&format!(
            "SELECT {} FROM repositories WHERE owner_id = $1 AND lower(name) = lower($2)",
            Self::COLUMNS
        ))
        .bind(owner_id)
        .bind(name)
        .fetch_optional(db)
        .await
    }
}

/// `labels` row.
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct Label {
    pub id: i64,
    pub repo_id: i64,
    pub name: String,
    pub color: String,
    pub description: Option<String>,
    pub is_default: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Label {
    pub const COLUMNS: &'static str =
        "id, repo_id, name, color, description, is_default, created_at, updated_at";
}

/// `milestones` row.
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct Milestone {
    pub id: i64,
    pub repo_id: i64,
    pub number: i64,
    pub title: String,
    pub description: Option<String>,
    pub state: String,
    pub creator_id: Option<i64>,
    pub open_issues: i64,
    pub closed_issues: i64,
    pub due_on: Option<DateTime<Utc>>,
    pub closed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Milestone {
    pub const COLUMNS: &'static str = "id, repo_id, number, title, description, state, \
        creator_id, open_issues, closed_issues, due_on, closed_at, created_at, updated_at";
}

/// `issues` row (issues and the conversation half of pull requests).
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct Issue {
    pub id: i64,
    pub repo_id: i64,
    pub number: i64,
    pub title: String,
    pub body: Option<String>,
    pub state: String,
    pub state_reason: Option<String>,
    pub author_id: Option<i64>,
    pub is_pull_request: bool,
    pub milestone_id: Option<i64>,
    pub locked: bool,
    pub active_lock_reason: Option<String>,
    pub comments_count: i64,
    pub closed_at: Option<DateTime<Utc>>,
    pub closed_by_id: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Issue {
    pub const COLUMNS: &'static str = "id, repo_id, number, title, body, state, state_reason, \
        author_id, is_pull_request, milestone_id, locked, active_lock_reason, comments_count, \
        closed_at, closed_by_id, created_at, updated_at";
}

/// `comments` row (issue comments).
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct Comment {
    pub id: i64,
    pub issue_id: i64,
    pub repo_id: i64,
    pub author_id: Option<i64>,
    pub body: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Comment {
    pub const COLUMNS: &'static str =
        "id, issue_id, repo_id, author_id, body, created_at, updated_at";
}

/// `pull_requests` row.
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct PullRequest {
    pub issue_id: i64,
    pub repo_id: i64,
    pub head_repo_id: Option<i64>,
    pub head_ref: String,
    pub head_sha: String,
    pub base_ref: String,
    pub base_sha: String,
    pub merge_base_sha: Option<String>,
    pub merge_commit_sha: Option<String>,
    pub merged: bool,
    pub merged_at: Option<DateTime<Utc>>,
    pub merged_by_id: Option<i64>,
    pub mergeable: Option<bool>,
    pub rebaseable: Option<bool>,
    pub mergeable_state: String,
    pub draft: bool,
    pub maintainer_can_modify: bool,
    pub auto_merge: Option<serde_json::Value>,
    pub additions: i64,
    pub deletions: i64,
    pub changed_files: i64,
    pub commits: i64,
    pub review_comments_count: i64,
}

impl PullRequest {
    pub const COLUMNS: &'static str = "issue_id, repo_id, head_repo_id, head_ref, head_sha, \
        base_ref, base_sha, merge_base_sha, merge_commit_sha, merged, merged_at, merged_by_id, \
        mergeable, rebaseable, mergeable_state, draft, maintainer_can_modify, auto_merge, \
        additions, deletions, changed_files, commits, review_comments_count";
}

#[cfg(test)]
mod tests {
    #[test]
    fn prefixes_columns() {
        assert_eq!(super::prefixed("u", "id, login"), "u.id, u.login");
    }
}
