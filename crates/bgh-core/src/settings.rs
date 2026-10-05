//! Site settings (`site_settings` table), edited by site admins through
//! `bgh-admin` and read everywhere through the typed [`SiteSettings`].
//!
//! ```ignore
//! let s = settings::load(&state).await?;          // cached (5 s) per process
//! settings::check_signup(&state, email).await?;    // 403 unless allowed
//! if !s.can_create_org(&user) { ... }
//! ```
//!
//! Storage: one `site_settings` row per section (`signup`, `repositories`,
//! `organizations`, `announcement`, `rate_limits`, `auth_providers`, `smtp`,
//! `maintenance`, `git`, `actions`, `privacy`), each a JSON object. Missing rows or fields take the
//! defaults below, so new fields never need a migration.
//!
//! Also here: the maintenance-mode middleware (503 for everyone but site
//! admins) and storage quota checks used by git receive-pack.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::extract::{Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgExecutor;

use crate::config::Config;
use crate::error::{ApiError, ApiResult};
use crate::models::db;
use crate::state::AppState;

/// How long a process trusts its cached copy of the settings.
const CACHE_TTL: Duration = Duration::from_secs(5);

/// Who may create accounts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SignupPolicy {
    /// Anyone (subject to `allowed_email_domains`).
    #[default]
    Open,
    /// Only emails with a pending organization invitation.
    Invite,
    /// Only site admins create accounts.
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SignupSettings {
    pub policy: SignupPolicy,
    /// Lower-case domains (`example.com`); empty = any domain.
    pub allowed_email_domains: Vec<String>,
}

impl Default for SignupSettings {
    fn default() -> Self {
        Self {
            policy: SignupPolicy::Open,
            allowed_email_domains: Vec::new(),
        }
    }
}

impl SignupSettings {
    /// Whether `email`'s domain is allowed.
    pub fn email_domain_allowed(&self, email: &str) -> bool {
        if self.allowed_email_domains.is_empty() {
            return true;
        }
        let Some((_, domain)) = email.rsplit_once('@') else {
            return false;
        };
        let domain = domain.to_ascii_lowercase();
        self.allowed_email_domains
            .iter()
            .any(|d| d.eq_ignore_ascii_case(&domain))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RepositorySettings {
    /// `public` | `private` | `internal` (internal falls back to private
    /// for user-owned repositories).
    pub default_visibility: String,
    /// Max size of one repository in MB (checked on push); `None` = no limit.
    pub max_repo_size_mb: Option<i64>,
}

impl Default for RepositorySettings {
    fn default() -> Self {
        Self {
            default_visibility: "public".into(),
            max_repo_size_mb: None,
        }
    }
}

/// Push hardening (`git` section): receive-side fsck and size limits.
/// `None` disables a limit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GitSettings {
    /// Check pushed objects with `git fsck` rules (`receive.fsckObjects`).
    pub fsck_on_push: bool,
    /// Pushes containing a file larger than this are rejected (GH001).
    pub max_object_size_mb: Option<i64>,
    /// Pushes containing a file larger than this get a warning.
    pub warn_object_size_mb: Option<i64>,
    /// Largest pack one push may send (`receive.maxInputSize`).
    pub max_push_size_mb: Option<i64>,
}

impl Default for GitSettings {
    fn default() -> Self {
        Self {
            fsck_on_push: true,
            max_object_size_mb: Some(100),
            warn_object_size_mb: Some(50),
            max_push_size_mb: Some(2048),
        }
    }
}

/// Repository visibilities, in display order.
pub const VISIBILITIES: &[&str] = &["public", "internal", "private"];

/// Access policy (`privacy` section): private mode, the anonymous user
/// directory and the repository visibilities owners may choose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PrivacySettings {
    /// Require sign-in for everything (pages, API, git, raw files, avatars)
    /// except the sign-in flows, static assets, `/healthz` and `/api/v3/meta`.
    pub private_mode: bool,
    /// Whether anonymous callers may list `GET /users` and
    /// `GET /organizations` (always refused in private mode).
    pub allow_anonymous_directory: bool,
    /// Subset of [`VISIBILITIES`] new or changed repositories may use.
    pub allowed_visibilities: Vec<String>,
}

impl Default for PrivacySettings {
    fn default() -> Self {
        Self {
            private_mode: false,
            allow_anonymous_directory: true,
            allowed_visibilities: VISIBILITIES.iter().map(|v| v.to_string()).collect(),
        }
    }
}

impl PrivacySettings {
    /// Whether repositories may be given `visibility`.
    pub fn visibility_allowed(&self, visibility: &str) -> bool {
        self.allowed_visibilities.iter().any(|v| v == visibility)
    }

    /// 422 (GitHub validation error on `visibility`) unless `visibility` is
    /// allowed by the site policy.
    pub fn check_visibility(&self, visibility: &str) -> ApiResult<()> {
        if self.visibility_allowed(visibility) {
            Ok(())
        } else {
            Err(ApiError::invalid_field(crate::error::FieldError::custom(
                "Repository",
                "visibility",
                format!("{visibility} repositories are not allowed on this instance"),
            )))
        }
    }

    /// Whether anonymous callers may enumerate users and organizations.
    pub fn anonymous_directory(&self) -> bool {
        !self.private_mode && self.allow_anonymous_directory
    }
}

/// Who may create organizations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OrgCreation {
    #[default]
    All,
    AdminsOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct OrganizationSettings {
    pub creation: OrgCreation,
}

/// Site-wide announcement banner (GHES `/enterprise/announcement`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Announcement {
    pub message: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
    pub user_dismissible: bool,
}

impl Announcement {
    /// The banner text if set and not expired.
    pub fn active_message(&self) -> Option<&str> {
        let msg = self.message.as_deref().filter(|m| !m.trim().is_empty())?;
        match self.expires_at {
            Some(t) if t <= Utc::now() => None,
            _ => Some(msg),
        }
    }
}

/// API rate limits (`crate::ratelimit`). Budgets are always counted and
/// reported (`X-RateLimit-*`, `GET /rate_limit`); `enabled` turns on
/// enforcement (off by default, like GHES). Defaults come from the
/// `BGH_RATE_LIMIT*` environment ([`crate::Config::rate_limits`]); stored
/// fields override them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RateLimitSettings {
    pub enabled: bool,
    /// `core` requests per hour per authenticated user.
    pub authenticated_per_hour: i64,
    /// `core` requests per hour per client IP for anonymous callers (also
    /// the anonymous `graphql` budget).
    pub unauthenticated_per_hour: i64,
    /// `search` requests per minute per authenticated user.
    pub search_authenticated_per_minute: i64,
    /// `search` requests per minute per client IP for anonymous callers.
    pub search_unauthenticated_per_minute: i64,
    /// `graphql` requests per hour per authenticated user.
    pub graphql_per_hour: i64,
}

impl Default for RateLimitSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            authenticated_per_hour: 5000,
            unauthenticated_per_hour: 60,
            search_authenticated_per_minute: 30,
            search_unauthenticated_per_minute: 10,
            graphql_per_hour: 5000,
        }
    }
}

/// A generic OIDC provider (consumed by the SSO login in bgh-accounts,
/// `bgh_accounts::sso`). `BGH_OIDC_*` provide one by default
/// ([`crate::Config::oidc`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OidcProvider {
    /// Short identifier used in URLs (`/_bgh/sso/{name}`).
    pub name: String,
    pub display_name: Option<String>,
    pub issuer: String,
    pub client_id: String,
    /// Secret; never returned by the admin API (write-only).
    pub client_secret: Option<String>,
    /// Requested scopes; empty = `openid profile email`.
    pub scopes: Vec<String>,
    /// Create accounts on first login.
    pub auto_create_users: bool,
    /// ID-token / userinfo claim proposing the login of new accounts
    /// (default `preferred_username`).
    pub login_claim: Option<String>,
    /// Lower-case email domains allowed to sign in; empty = any.
    pub allowed_domains: Vec<String>,
}

impl Default for OidcProvider {
    fn default() -> Self {
        Self {
            name: String::new(),
            display_name: None,
            issuer: String::new(),
            client_id: String::new(),
            client_secret: None,
            scopes: Vec::new(),
            auto_create_users: true,
            login_claim: None,
            allowed_domains: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AuthProviderSettings {
    /// Built-in username/password login.
    pub password_login: bool,
    pub oidc: Vec<OidcProvider>,
}

impl Default for AuthProviderSettings {
    fn default() -> Self {
        Self {
            password_login: true,
            oidc: Vec::new(),
        }
    }
}

/// Outgoing mail through an SMTP relay (`crate::mail`); when disabled,
/// `BGH_SMTP_URL` / the dev transport are used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SmtpSettings {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    /// Secret; never returned by the admin API (write-only).
    pub password: Option<String>,
    /// `From:` address, e.g. `Better GitHub <noreply@example.com>`.
    pub from: String,
    /// `none` | `starttls` | `tls`
    pub tls: String,
}

impl Default for SmtpSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            host: String::new(),
            port: 587,
            username: None,
            password: None,
            from: String::new(),
            tls: "starttls".into(),
        }
    }
}

/// Maintenance mode: every request but site admins' gets a 503.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct MaintenanceSettings {
    pub enabled: bool,
    pub message: Option<String>,
    /// Announced start (informational, shown in the banner).
    pub scheduled_at: Option<DateTime<Utc>>,
}

/// GitHub Actions defaults (GHES enterprise policies; repo and org
/// overrides come later).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ActionsSettings {
    /// Default `GITHUB_TOKEN` permissions for workflows without
    /// `permissions:`: `read` (contents and packages read, GitHub's
    /// restricted default) or `write` (read/write to every category).
    pub default_workflow_permissions: String,
    /// Whether `GITHUB_TOKEN` may approve pull request reviews.
    pub can_approve_pull_request_reviews: bool,
}

impl Default for ActionsSettings {
    fn default() -> Self {
        Self {
            default_workflow_permissions: "read".into(),
            can_approve_pull_request_reviews: false,
        }
    }
}

/// Scheduled, fork-network-aware git maintenance (`repos.maintenance`
/// service) and archive-cache housekeeping.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GitMaintenanceSettings {
    /// Run the scheduled maintenance service.
    pub enabled: bool,
    /// Unreachable objects younger than this are never pruned (git's gc
    /// grace period; protects in-flight pushes). At least 1.
    pub prune_grace_days: u32,
    /// Minimum hours between incremental runs (commit-graph + geometric
    /// repack) of one repository.
    pub interval_hours: u32,
    /// Days between full repacks of one repository.
    pub full_interval_days: u32,
    /// Repack early once this many loose objects pile up.
    pub loose_objects_threshold: i64,
    /// Repack early once this many packs pile up.
    pub pack_count_threshold: i64,
    /// Upper bound of repositories handled per pass (one pass per minute).
    pub max_repos_per_pass: i64,
    /// Cached source archives unused for this many days are removed.
    pub archive_cache_max_age_days: u32,
    /// The archive cache is trimmed (oldest first) to this size.
    pub archive_cache_max_size_mb: u64,
}

impl Default for GitMaintenanceSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            prune_grace_days: 14,
            interval_hours: 24,
            full_interval_days: 7,
            loose_objects_threshold: 1000,
            pack_count_threshold: 16,
            max_repos_per_pass: 20,
            archive_cache_max_age_days: 7,
            archive_cache_max_size_mb: 2048,
        }
    }
}

/// Data retention (`admin.retention` service; expired sessions are always
/// deleted). A window of 0 keeps those rows forever.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RetentionSettings {
    /// Run the retention service (hourly).
    pub enabled: bool,
    /// Notification threads not updated for this many days are deleted
    /// (GitHub keeps about five months).
    pub notifications_days: u32,
    /// Webhook deliveries older than this lose their request/response
    /// bodies (redelivery needs them).
    pub webhook_payload_days: u32,
    /// Webhook delivery metadata older than this is deleted.
    pub webhook_delivery_days: u32,
    /// Events API / activity rows older than this are deleted.
    pub activity_days: u32,
}

impl Default for RetentionSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            notifications_days: 150,
            webhook_payload_days: 30,
            webhook_delivery_days: 90,
            activity_days: 90,
        }
    }
}

/// All site settings, with defaults for anything not stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SiteSettings {
    pub signup: SignupSettings,
    pub repositories: RepositorySettings,
    pub organizations: OrganizationSettings,
    pub announcement: Announcement,
    pub rate_limits: RateLimitSettings,
    pub auth_providers: AuthProviderSettings,
    pub smtp: SmtpSettings,
    pub maintenance: MaintenanceSettings,
    pub git_maintenance: GitMaintenanceSettings,
    pub git: GitSettings,
    pub retention: RetentionSettings,
    pub actions: ActionsSettings,
    pub privacy: PrivacySettings,
}

/// Section keys (`site_settings.key`), in display order.
pub const SECTIONS: &[&str] = &[
    "signup",
    "repositories",
    "organizations",
    "announcement",
    "rate_limits",
    "auth_providers",
    "smtp",
    "maintenance",
    "git_maintenance",
    "git",
    "retention",
    "actions",
    "privacy",
];

impl SiteSettings {
    /// Whether `user` may create an organization.
    pub fn can_create_org(&self, user: &db::User) -> bool {
        user.site_admin || self.organizations.creation == OrgCreation::All
    }

    /// Default visibility for a new repository of a user / org owner.
    /// Falls back to the most restrictive allowed visibility the owner can
    /// use when the configured default isn't allowed (`privacy`).
    pub fn default_visibility(&self, owner_is_org: bool) -> &str {
        let configured = match self.repositories.default_visibility.as_str() {
            "private" => "private",
            "internal" if owner_is_org => "internal",
            "internal" => "private",
            _ => "public",
        };
        if self.privacy.visibility_allowed(configured) {
            return configured;
        }
        ["private", "internal", "public"]
            .into_iter()
            .filter(|v| owner_is_org || *v != "internal")
            .find(|v| self.privacy.visibility_allowed(v))
            .unwrap_or(configured)
    }

    /// Validate the access-policy fields across sections (admin settings
    /// writes): known, non-empty `allowed_visibilities`, and a
    /// `default_visibility` inside that set.
    pub fn validate_policy(&self) -> Result<(), String> {
        let allowed = &self.privacy.allowed_visibilities;
        if allowed.is_empty() {
            return Err("privacy.allowed_visibilities must not be empty".into());
        }
        if let Some(bad) = allowed.iter().find(|v| !VISIBILITIES.contains(&v.as_str())) {
            return Err(format!(
                "privacy.allowed_visibilities: unknown visibility {bad:?}"
            ));
        }
        let default = self.repositories.default_visibility.as_str();
        if !VISIBILITIES.contains(&default) {
            return Err(format!(
                "repositories.default_visibility: unknown visibility {default:?}"
            ));
        }
        if !self.privacy.visibility_allowed(default) {
            return Err(format!(
                "repositories.default_visibility {default:?} is not in privacy.allowed_visibilities"
            ));
        }
        Ok(())
    }

    /// Defaults before any stored row: the built-in defaults plus the
    /// environment's (`BGH_RATE_LIMIT*`, `BGH_OIDC_*`).
    pub fn defaults(config: &Config) -> Self {
        Self {
            rate_limits: config.rate_limits.clone(),
            auth_providers: AuthProviderSettings {
                oidc: config.oidc.clone().into_iter().collect(),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    /// Build from `(key, value)` rows over the built-in defaults; see
    /// [`Self::from_rows_with`].
    pub fn from_rows(rows: impl IntoIterator<Item = (String, Value)>) -> Self {
        Self::from_rows_with(Self::default(), rows)
    }

    /// Apply stored `(key, value)` rows to `base` ([`Self::defaults`]):
    /// stored fields override the base field by field, so fields an admin
    /// never set keep following the environment. Unknown keys and bad values
    /// are ignored (logged) so a bad row never takes the site down.
    pub fn from_rows_with(base: Self, rows: impl IntoIterator<Item = (String, Value)>) -> Self {
        let mut out = base;
        for (key, v) in rows {
            if !SECTIONS.contains(&key.as_str()) {
                continue;
            }
            let res = match &v {
                Value::Object(fields) => out.apply_section(&key, fields),
                _ => Err(serde::de::Error::custom("not an object")),
            };
            if let Err(err) = res {
                tracing::error!(section = key, %err, "invalid site setting; using defaults");
            }
        }
        out
    }

    /// Override fields of section `key` (all-or-nothing: on error `self`
    /// is unchanged).
    pub fn apply_section(
        &mut self,
        key: &str,
        fields: &serde_json::Map<String, Value>,
    ) -> Result<(), serde_json::Error> {
        let mut section = self.section(key).unwrap_or_else(|| json!({}));
        for (k, v) in fields {
            section[k] = v.clone();
        }
        match key {
            "signup" => self.signup = serde_json::from_value(section)?,
            "repositories" => self.repositories = serde_json::from_value(section)?,
            "organizations" => self.organizations = serde_json::from_value(section)?,
            "announcement" => self.announcement = serde_json::from_value(section)?,
            "rate_limits" => self.rate_limits = serde_json::from_value(section)?,
            "auth_providers" => self.auth_providers = serde_json::from_value(section)?,
            "smtp" => self.smtp = serde_json::from_value(section)?,
            "maintenance" => self.maintenance = serde_json::from_value(section)?,
            "git_maintenance" => self.git_maintenance = serde_json::from_value(section)?,
            "git" => self.git = serde_json::from_value(section)?,
            "retention" => self.retention = serde_json::from_value(section)?,
            "actions" => {
                let a: ActionsSettings = serde_json::from_value(section)?;
                if !matches!(a.default_workflow_permissions.as_str(), "read" | "write") {
                    return Err(serde::de::Error::custom(
                        "default_workflow_permissions must be read or write",
                    ));
                }
                self.actions = a;
            }
            "privacy" => self.privacy = serde_json::from_value(section)?,
            _ => {}
        }
        Ok(())
    }

    /// One section as JSON (`None` for unknown keys).
    pub fn section(&self, key: &str) -> Option<Value> {
        let v = serde_json::to_value(self).ok()?;
        v.get(key).cloned()
    }
}

type Cache = Mutex<HashMap<String, (Instant, Arc<SiteSettings>)>>;

/// Per-process cache keyed by the Redis prefix (unique per deployment and
/// per test app).
fn cache() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// The stored `(key, value)` rows (only the fields admins set).
pub async fn load_rows(db: impl PgExecutor<'_>) -> Result<Vec<(String, Value)>, sqlx::Error> {
    sqlx::query_as("SELECT key, value FROM site_settings ORDER BY key")
        .fetch_all(db)
        .await
}

/// Effective settings straight from the database (no cache): `config`'s
/// defaults overridden by the stored rows.
pub async fn load_uncached(
    config: &Config,
    db: impl PgExecutor<'_>,
) -> Result<SiteSettings, sqlx::Error> {
    let rows = load_rows(db).await?;
    Ok(SiteSettings::from_rows_with(
        SiteSettings::defaults(config),
        rows,
    ))
}

/// Current settings, cached per process for a few seconds.
pub async fn load(state: &AppState) -> ApiResult<Arc<SiteSettings>> {
    let key = &state.config.redis_prefix;
    if let Some((at, s)) = cache().lock().expect("settings cache").get(key)
        && at.elapsed() < CACHE_TTL
    {
        return Ok(s.clone());
    }
    let s = Arc::new(load_uncached(&state.config, &state.db).await?);
    cache()
        .lock()
        .expect("settings cache")
        .insert(key.clone(), (Instant::now(), s.clone()));
    Ok(s)
}

/// Drop this process's cached copy (call after writing settings).
pub fn invalidate(state: &AppState) {
    cache()
        .lock()
        .expect("settings cache")
        .remove(&state.config.redis_prefix);
}

/// Store one section (validated by deserializing into the typed struct).
/// Call [`invalidate`] after the transaction commits.
pub async fn store_section(
    db: impl PgExecutor<'_>,
    key: &str,
    value: &Value,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO site_settings (key, value, updated_at) VALUES ($1, $2, now())
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = now()",
    )
    .bind(key)
    .bind(value)
    .execute(db)
    .await?;
    Ok(())
}

/// Enforce the sign-up policy for a self-service sign-up with `email`.
/// 403 when sign-up is closed, invite-only without an invitation, or the
/// email domain isn't allowed.
pub async fn check_signup(state: &AppState, email: &str) -> ApiResult<()> {
    let s = load(state).await?;
    match s.signup.policy {
        SignupPolicy::Closed => {
            return Err(ApiError::forbidden("Sign up is disabled on this instance."));
        }
        SignupPolicy::Invite => {
            let invited: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM org_invitations
                                 WHERE lower(email) = lower($1) AND failed_at IS NULL)",
            )
            .bind(email)
            .fetch_one(&state.db)
            .await?;
            if !invited {
                return Err(ApiError::forbidden(
                    "Sign up on this instance requires an invitation.",
                ));
            }
        }
        SignupPolicy::Open => {}
    }
    if !s.signup.email_domain_allowed(email) {
        return Err(ApiError::forbidden(
            "Sign up is not allowed for this email domain.",
        ));
    }
    Ok(())
}

/// Effective storage limits for an owner, in KB: `(per_repo, total)`.
pub async fn storage_limits_kb(
    state: &AppState,
    owner_id: i64,
) -> ApiResult<(Option<i64>, Option<i64>)> {
    let s = load(state).await?;
    let quota: Option<(Option<i64>, Option<i64>)> = sqlx::query_as(
        "SELECT max_repo_size_mb, max_total_size_mb FROM storage_quotas WHERE owner_id = $1",
    )
    .bind(owner_id)
    .fetch_optional(&state.db)
    .await?;
    let (repo_mb, total_mb) = quota.unwrap_or((None, None));
    let repo_mb = repo_mb.or(s.repositories.max_repo_size_mb);
    Ok((repo_mb.map(|m| m * 1024), total_mb.map(|m| m * 1024)))
}

/// Storage left under the tightest applicable quota (see
/// [`quota_headroom`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotaHeadroom {
    /// Limit minus usage, in KB (negative when already over).
    pub remaining_kb: i64,
    pub limit_kb: i64,
    /// Whether the per-repository limit (rather than the owner's total) is
    /// the binding one.
    pub per_repo: bool,
}

impl QuotaHeadroom {
    /// GitHub-style message for a refused push or upload.
    pub fn message(&self) -> String {
        if self.per_repo {
            format!(
                "Repository is over its size limit ({} MB).",
                self.limit_kb / 1024
            )
        } else {
            format!(
                "The repository owner is over its storage quota ({} MB).",
                self.limit_kb / 1024
            )
        }
    }
}

/// Remaining storage for `repo` (git objects as recorded after the last
/// push plus LFS objects), or `None` when no quota applies.
pub async fn quota_headroom(
    state: &AppState,
    repo: &db::Repository,
) -> ApiResult<Option<QuotaHeadroom>> {
    let (per_repo, total) = storage_limits_kb(state, repo.owner_id).await?;
    let mut best: Option<QuotaHeadroom> = None;
    if let Some(limit) = per_repo {
        let used: i64 = sqlx::query_scalar(
            "SELECT (size + lfs_size / 1024)::bigint FROM repositories WHERE id = $1",
        )
        .bind(repo.id)
        .fetch_optional(&state.db)
        .await?
        .unwrap_or(repo.size);
        best = Some(QuotaHeadroom {
            remaining_kb: limit - used,
            limit_kb: limit,
            per_repo: true,
        });
    }
    if let Some(limit) = total {
        let used = owner_storage_used_kb(state, repo.owner_id).await?;
        let h = QuotaHeadroom {
            remaining_kb: limit - used,
            limit_kb: limit,
            per_repo: false,
        };
        if best
            .as_ref()
            .is_none_or(|b| h.remaining_kb < b.remaining_kb)
        {
            best = Some(h);
        }
    }
    Ok(best)
}

/// Storage an owner uses, in KB: git objects and LFS objects of its
/// repositories plus its container packages (`packages.size`, bytes).
pub async fn owner_storage_used_kb(state: &AppState, owner_id: i64) -> ApiResult<i64> {
    Ok(sqlx::query_scalar(
        "SELECT (coalesce((SELECT sum(size + lfs_size / 1024) FROM repositories WHERE owner_id = $1), 0)
               + coalesce((SELECT sum(size) FROM packages WHERE owner_id = $1), 0) / 1024)::bigint",
    )
    .bind(owner_id)
    .fetch_one(&state.db)
    .await?)
}

/// Remaining storage under an owner's total quota (packages have no
/// per-repository limit), or `None` when no total quota applies.
pub async fn owner_quota_headroom(
    state: &AppState,
    owner_id: i64,
) -> ApiResult<Option<QuotaHeadroom>> {
    let (_, total) = storage_limits_kb(state, owner_id).await?;
    let Some(limit) = total else { return Ok(None) };
    let used = owner_storage_used_kb(state, owner_id).await?;
    Ok(Some(QuotaHeadroom {
        remaining_kb: limit - used,
        limit_kb: limit,
        per_repo: false,
    }))
}

/// 403 when a push into `repo` must be refused because the repository or
/// its owner is already over quota (git and LFS storage). Pushes that would
/// cross the limit are caught later, in the pre-receive hook, by comparing
/// the quarantined objects with [`quota_headroom`].
pub async fn check_push_quota(state: &AppState, repo: &db::Repository) -> ApiResult<()> {
    match quota_headroom(state, repo).await? {
        Some(h) if h.remaining_kb < 0 => Err(ApiError::forbidden(h.message())),
        _ => Ok(()),
    }
}

/// 403 when storing `add_bytes` more for `owner_id` (user attachments)
/// would exceed the owner's total storage quota. Repositories (as recorded
/// after their last push) and existing attachments count towards it.
pub async fn check_upload_quota(state: &AppState, owner_id: i64, add_bytes: u64) -> ApiResult<()> {
    let (_, total) = storage_limits_kb(state, owner_id).await?;
    let Some(limit) = total else { return Ok(()) };
    let used: i64 = sqlx::query_scalar(
        "SELECT (SELECT coalesce(sum(size + lfs_size / 1024), 0) FROM repositories WHERE owner_id = $1)::bigint
              + ((SELECT coalesce(sum(size), 0) FROM attachments WHERE owner_id = $1) / 1024)::bigint",
    )
    .bind(owner_id)
    .fetch_one(&state.db)
    .await?;
    if used + (add_bytes / 1024) as i64 > limit {
        return Err(ApiError::forbidden(format!(
            "The owner is over its storage quota ({} MB).",
            limit / 1024
        )));
    }
    Ok(())
}

/// Paths that stay reachable in maintenance mode (status, banner, login).
fn maintenance_exempt(path: &str) -> bool {
    // Sign-in must keep working so site administrators can get in (and turn
    // maintenance off); everything a non-admin does afterwards is still 503.
    matches!(
        path,
        "/healthz"
            | "/_bgh/site"
            | "/_bgh/session"
            | "/_bgh/session/two_factor"
            | "/_bgh/boot"
            | "/_bgh/auth/login"
            | "/_bgh/auth/2fa"
            | "/_bgh/auth/logout"
    )
}

/// Requests that reach application data (API, private endpoints, git).
fn is_app_request(method: &Method, path: &str) -> bool {
    path.starts_with("/api/")
        || path.starts_with("/_bgh/")
        || path.ends_with("/info/refs")
        || path.ends_with("/git-upload-pack")
        || path.ends_with("/git-receive-pack")
        || !matches!(*method, Method::GET | Method::HEAD)
}

/// Middleware: in maintenance mode, answer 503 (GitHub JSON error +
/// `Retry-After`) to everything except site admins, the web client shell
/// and the exempt paths. Cheap when disabled (cached settings lookup).
pub async fn maintenance_middleware(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Response {
    let path = req.uri().path().to_string();
    if maintenance_exempt(&path) || !is_app_request(req.method(), &path) {
        return next.run(req).await;
    }
    let settings = match load(&state).await {
        Ok(s) => s,
        Err(err) => return err.into_response(),
    };
    if !settings.maintenance.enabled {
        return next.run(req).await;
    }
    if let Ok(Some(ctx)) = crate::auth::resolve_request(&state, &mut req).await
        && ctx.user.site_admin
    {
        return next.run(req).await;
    }
    let message = settings
        .maintenance
        .message
        .clone()
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| "This instance is undergoing maintenance.".into());
    let mut resp = ApiError::Status(StatusCode::SERVICE_UNAVAILABLE, message).into_response();
    resp.headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from_static("300"));
    resp
}

/// Public site info for the web client banner (`GET /_bgh/site`).
pub fn public_info(state: &AppState, s: &SiteSettings) -> Value {
    json!({
        "site_name": state.config.site_name,
        "announcement": s.announcement.active_message().map(|m| json!({
            "message": m,
            "expires_at": s.announcement.expires_at.map(crate::time::Timestamp::from),
            "user_dismissible": s.announcement.user_dismissible,
        })),
        "maintenance": {
            "enabled": s.maintenance.enabled,
            "message": s.maintenance.message,
            "scheduled_at": s.maintenance.scheduled_at.map(crate::time::Timestamp::from),
        },
        "signup_policy": s.signup.policy,
        "password_login": s.auth_providers.password_login,
        "private_mode": s.privacy.private_mode,
        "repository_visibilities": {
            "allowed": s.privacy.allowed_visibilities,
            "default_user": s.default_visibility(false),
            "default_org": s.default_visibility(true),
        },
        "oidc_providers": s.auth_providers.oidc.iter().map(|p| json!({
            "name": p.name,
            "display_name": p.display_name.clone().unwrap_or_else(|| p.name.clone()),
        })).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_partial_rows() {
        let s = SiteSettings::from_rows(vec![
            ("signup".into(), json!({"policy": "invite"})),
            (
                "repositories".into(),
                json!({"default_visibility": "internal"}),
            ),
            ("bogus".into(), json!(1)),
            ("smtp".into(), json!("not an object")),
        ]);
        assert_eq!(s.signup.policy, SignupPolicy::Invite);
        assert!(s.signup.allowed_email_domains.is_empty());
        assert_eq!(s.default_visibility(true), "internal");
        assert_eq!(s.default_visibility(false), "private");
        assert_eq!(s.smtp, SmtpSettings::default());
        assert!(!s.rate_limits.enabled);
    }

    #[test]
    fn stored_fields_override_environment_defaults() {
        let mut config = Config::default();
        config.rate_limits.authenticated_per_hour = 100;
        config.rate_limits.unauthenticated_per_hour = 7;
        config.oidc = Some(OidcProvider {
            name: "env".into(),
            issuer: "https://id.example.com".into(),
            client_id: "bgh".into(),
            ..Default::default()
        });
        let base = SiteSettings::defaults(&config);
        assert_eq!(base.auth_providers.oidc[0].name, "env");
        let s = SiteSettings::from_rows_with(
            base.clone(),
            vec![(
                "rate_limits".into(),
                json!({"enabled": true, "unauthenticated_per_hour": 3}),
            )],
        );
        assert!(s.rate_limits.enabled);
        assert_eq!(s.rate_limits.authenticated_per_hour, 100);
        assert_eq!(s.rate_limits.unauthenticated_per_hour, 3);
        // Stored providers replace the environment's.
        let s = SiteSettings::from_rows_with(
            base,
            vec![("auth_providers".into(), json!({"oidc": []}))],
        );
        assert!(s.auth_providers.oidc.is_empty());
        assert!(s.auth_providers.password_login);
    }

    #[test]
    fn email_domains() {
        let s = SignupSettings {
            policy: SignupPolicy::Open,
            allowed_email_domains: vec!["example.com".into()],
        };
        assert!(s.email_domain_allowed("a@Example.COM"));
        assert!(!s.email_domain_allowed("a@evil.com"));
        assert!(SignupSettings::default().email_domain_allowed("x@y.z"));
    }

    #[test]
    fn announcement_expiry() {
        let mut a = Announcement {
            message: Some("hi".into()),
            ..Default::default()
        };
        assert_eq!(a.active_message(), Some("hi"));
        a.expires_at = Some(Utc::now() - chrono::Duration::minutes(1));
        assert_eq!(a.active_message(), None);
    }
}
