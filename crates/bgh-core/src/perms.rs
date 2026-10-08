//! Repository permissions.
//!
//! A user's permission on a repository is the maximum of:
//! * owner of a user-owned repo → Admin; site admin → Admin
//! * org admin of the owning org → Admin; org member → org base permission
//! * team grants (`team_repos`) of teams the user belongs to, including
//!   grants inherited from parent teams
//! * direct collaborator permission
//! * public visibility → Read (for everyone, including anonymous)
//! * internal visibility → Read for every signed-in, non-suspended user
//!   (GHES semantics; never for anonymous callers or Actions job tokens of
//!   other repositories)
//!
//! Token scopes further restrict what a PAT can do (see [`effective`]).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use sqlx::PgExecutor;

use crate::auth::AuthContext;
use crate::error::{ApiError, ApiResult};
use crate::models::db;
use crate::state::AppState;

/// Repository role, ordered from least to most privileged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Permission {
    None,
    Read,
    Triage,
    Write,
    Maintain,
    Admin,
}

impl Permission {
    /// Parse a role name; accepts GitHub's legacy names (`pull`, `push`).
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "none" => Self::None,
            "read" | "pull" => Self::Read,
            "triage" => Self::Triage,
            "write" | "push" => Self::Write,
            "maintain" => Self::Maintain,
            "admin" => Self::Admin,
            _ => return None,
        })
    }

    /// Role name as stored in the database and returned as `role_name`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Read => "read",
            Self::Triage => "triage",
            Self::Write => "write",
            Self::Maintain => "maintain",
            Self::Admin => "admin",
        }
    }

    /// Legacy names used by team/collaborator APIs (`pull`, `push`, ...).
    pub fn legacy_name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Read => "pull",
            Self::Triage => "triage",
            Self::Write => "push",
            Self::Maintain => "maintain",
            Self::Admin => "admin",
        }
    }

    /// The coarse `permission` of `GET /repos/{o}/{r}/collaborators/{u}/permission`
    /// (`admin` | `write` | `read` | `none`).
    pub fn coarse_name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Read | Self::Triage => "read",
            Self::Write | Self::Maintain => "write",
            Self::Admin => "admin",
        }
    }
}

impl std::fmt::Display for Permission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(sqlx::FromRow)]
struct PermRow {
    repo_id: i64,
    active: Option<bool>,
    collab: Option<String>,
    org_role: Option<OrgRole>,
    org_base: Option<String>,
    team_perms: Option<Vec<String>>,
}

/// What an anonymous caller gets: Read on public repositories.
fn public_floor(repo: &db::Repository) -> Permission {
    if repo.is_private() {
        Permission::None
    } else {
        Permission::Read
    }
}

/// What any caller gets from visibility alone: [`public_floor`], plus Read
/// on internal repositories for active (signed-in, non-suspended) users.
pub fn visibility_floor(repo: &db::Repository, active_user: bool) -> Permission {
    if active_user && repo.is_internal() {
        Permission::Read
    } else {
        public_floor(repo)
    }
}

/// Compute `user_id`'s permission on `repo`, ignoring token scopes.
pub async fn repo_permission(
    db: impl PgExecutor<'_>,
    user_id: Option<i64>,
    repo: &db::Repository,
) -> Result<Permission, sqlx::Error> {
    let map = repo_permissions(db, user_id, std::slice::from_ref(repo)).await?;
    Ok(map.get(&repo.id).copied().unwrap_or(Permission::None))
}

/// Batch variant of [`repo_permission`] for lists: one query for all
/// `repos`. Returns a map keyed by repository id.
pub async fn repo_permissions(
    db: impl PgExecutor<'_>,
    user_id: Option<i64>,
    repos: &[db::Repository],
) -> Result<HashMap<i64, Permission>, sqlx::Error> {
    let mut out: HashMap<i64, Permission> = repos.iter().map(|r| (r.id, public_floor(r))).collect();
    let Some(uid) = user_id else {
        return Ok(out);
    };
    let mut pending = Vec::new();
    for r in repos {
        if r.owner_id == uid {
            out.insert(r.id, Permission::Admin);
        } else {
            pending.push(r.id);
        }
    }
    if pending.is_empty() {
        return Ok(out);
    }
    // One round trip: site_admin flag + per-repo grants.
    let rows: Vec<PermRow> = sqlx::query_as(
        r#"
        WITH RECURSIVE user_teams AS (
            SELECT t.id, t.parent_id
              FROM team_members tm JOIN teams t ON t.id = tm.team_id
             WHERE tm.user_id = $1
            UNION
            SELECT p.id, p.parent_id
              FROM teams p JOIN user_teams ut ON p.id = ut.parent_id
        )
        SELECT r.id AS repo_id,
               (SELECT type = 'User' AND suspended_at IS NULL FROM users WHERE id = $1) AS active,
               CASE WHEN (SELECT site_admin FROM users WHERE id = $1) THEN 'admin'
                    ELSE (SELECT permission FROM collaborators c WHERE c.repo_id = r.id AND c.user_id = $1)
               END AS collab,
               (SELECT role FROM org_members m WHERE m.org_id = r.owner_id AND m.user_id = $1) AS org_role,
               (SELECT default_repository_permission FROM org_settings s WHERE s.org_id = r.owner_id) AS org_base,
               (SELECT array_agg(tr.permission) FROM team_repos tr
                 WHERE tr.repo_id = r.id AND tr.team_id IN (SELECT id FROM user_teams)) AS team_perms
          FROM repositories r
         WHERE r.id = ANY($2)
        "#,
    )
    .bind(uid)
    .bind(&pending)
    .fetch_all(db)
    .await?;
    let internal: std::collections::HashSet<i64> = repos
        .iter()
        .filter(|r| r.is_internal())
        .map(|r| r.id)
        .collect();
    for row in rows {
        let best = out.entry(row.repo_id).or_insert(Permission::None);
        let mut raise = |p: Option<Permission>| {
            if let Some(p) = p {
                *best = (*best).max(p);
            }
        };
        if row.active == Some(true) && internal.contains(&row.repo_id) {
            raise(Some(Permission::Read));
        }
        raise(row.collab.as_deref().and_then(Permission::parse));
        match row.org_role {
            Some(OrgRole::Admin) => raise(Some(Permission::Admin)),
            Some(OrgRole::Member) => raise(row.org_base.as_deref().and_then(Permission::parse)),
            None => {}
        }
        for p in row.team_perms.unwrap_or_default() {
            raise(Permission::parse(&p));
        }
    }
    Ok(out)
}

/// Raw permission of each of `user_ids` on one `repo` (token scopes not
/// applied), in one query: the "who may see this" check used when fanning
/// out notifications and emails. Ids of unknown users get the public floor.
pub async fn users_repo_permissions(
    db: impl PgExecutor<'_>,
    repo: &db::Repository,
    user_ids: &[i64],
) -> Result<HashMap<i64, Permission>, sqlx::Error> {
    #[derive(sqlx::FromRow)]
    struct Row {
        user_id: i64,
        site_admin: bool,
        active: bool,
        collab: Option<String>,
        org_role: Option<OrgRole>,
        org_base: Option<String>,
        team_perms: Option<Vec<String>>,
    }
    let mut out: HashMap<i64, Permission> =
        user_ids.iter().map(|&u| (u, public_floor(repo))).collect();
    if user_ids.is_empty() {
        return Ok(out);
    }
    let rows: Vec<Row> = sqlx::query_as(
        r#"
        WITH RECURSIVE ut AS (
            SELECT tm.user_id, t.id, t.parent_id
              FROM team_members tm JOIN teams t ON t.id = tm.team_id
             WHERE tm.user_id = ANY($1) AND t.org_id = $3
            UNION
            SELECT ut.user_id, p.id, p.parent_id
              FROM teams p JOIN ut ON p.id = ut.parent_id
        )
        SELECT u.id AS user_id, u.site_admin,
               (u.type = 'User' AND u.suspended_at IS NULL) AS active,
               (SELECT permission FROM collaborators c
                 WHERE c.repo_id = $2 AND c.user_id = u.id) AS collab,
               (SELECT role FROM org_members m
                 WHERE m.org_id = $3 AND m.user_id = u.id) AS org_role,
               (SELECT default_repository_permission FROM org_settings s
                 WHERE s.org_id = $3) AS org_base,
               (SELECT array_agg(tr.permission) FROM team_repos tr
                 WHERE tr.repo_id = $2
                   AND tr.team_id IN (SELECT ut.id FROM ut WHERE ut.user_id = u.id)) AS team_perms
          FROM users u
         WHERE u.id = ANY($1)
        "#,
    )
    .bind(user_ids)
    .bind(repo.id)
    .bind(repo.owner_id)
    .fetch_all(db)
    .await?;
    for row in rows {
        let best = out.entry(row.user_id).or_insert(Permission::None);
        let mut raise = |p: Option<Permission>| {
            if let Some(p) = p {
                *best = (*best).max(p);
            }
        };
        if row.site_admin || row.user_id == repo.owner_id {
            raise(Some(Permission::Admin));
        }
        raise(Some(visibility_floor(repo, row.active)));
        raise(row.collab.as_deref().and_then(Permission::parse));
        match row.org_role {
            Some(OrgRole::Admin) => raise(Some(Permission::Admin)),
            Some(OrgRole::Member) => raise(row.org_base.as_deref().and_then(Permission::parse)),
            None => {}
        }
        for p in row.team_perms.unwrap_or_default() {
            raise(Permission::parse(&p));
        }
    }
    Ok(out)
}

/// Apply token scopes to a raw permission:
/// * private repos need the `repo` scope, otherwise the token sees nothing;
/// * writes to public repos need `repo` or `public_repo`, otherwise Read.
///
/// Actions job tokens (`GITHUB_TOKEN`) carry a [`JOB_TOKEN_SCOPE_PREFIX`]
/// scope: on that repository they get Write (Read when their permission map
/// has no write, see [`crate::token_permissions`]) whatever the token's
/// user (`github-actions[bot]`) could do; other repositories they see like
/// an anonymous caller. The per-category map itself is enforced by
/// [`crate::token_permissions::middleware`] and the git transport.
pub fn effective(auth: Option<&AuthContext>, repo: &db::Repository, raw: Permission) -> Permission {
    let Some(auth) = auth else {
        return raw;
    };
    // GitHub App installation tokens: their repositories and permissions.
    if let Some(cap) = crate::apps::effective_cap(auth, repo) {
        return cap;
    }
    // GitHub App user-to-server tokens: the user, limited to the app.
    if let Some(cap) = crate::apps::user_to_server_cap(auth, repo, raw) {
        return cap;
    }
    // Fine-grained personal access tokens: their repositories and
    // permissions, within the user's own role.
    if let Some(cap) = crate::pat::effective_cap(auth, repo) {
        return raw.min(cap);
    }
    if let Some(job_repo) = job_token_repo(auth) {
        let cap = if auth
            .scopes
            .as_ref()
            .is_some_and(|s| s.iter().any(|s| s == JOB_TOKEN_READ_ONLY_SCOPE))
        {
            Permission::Read
        } else {
            Permission::Write
        };
        return if job_repo != repo.id {
            public_floor(repo)
        } else {
            cap
        };
    }
    // Organizations whose token policy blocks this (classic) token.
    if crate::pat::is_blocked(auth, repo.owner_id) {
        return raw.min(public_floor(repo));
    }
    if auth.has_scope("repo") {
        return raw;
    }
    if repo.is_private() {
        // `repo:status` / `repo_deployment` reach their categories only.
        crate::pat::narrow_cap(auth).map_or(Permission::None, |cap| raw.min(cap))
    } else if auth.has_scope("public_repo") {
        raw
    } else {
        raw.min(Permission::Read)
    }
}

/// Scope prefix marking an Actions job token: `actions:repo:{repo_id}`.
pub const JOB_TOKEN_SCOPE_PREFIX: &str = "actions:repo:";

/// Extra scope making an Actions job token read-only (fork pull requests).
pub const JOB_TOKEN_READ_ONLY_SCOPE: &str = "actions:read-only";

/// Scope prefix recording who triggered the workflow run of an Actions job
/// token (`actions:actor:{user_id}`), for the audit log.
pub const JOB_TOKEN_ACTOR_SCOPE_PREFIX: &str = "actions:actor:";

/// The user who triggered the run an Actions job token belongs to.
pub fn job_token_actor(auth: &AuthContext) -> Option<i64> {
    job_token_repo(auth)?;
    auth.scopes
        .as_ref()?
        .iter()
        .find_map(|s| s.strip_prefix(JOB_TOKEN_ACTOR_SCOPE_PREFIX)?.parse().ok())
}

/// Repository an Actions job token is restricted to, if `auth` is one.
pub fn job_token_repo(auth: &AuthContext) -> Option<i64> {
    auth.scopes
        .as_ref()?
        .iter()
        .find_map(|s| s.strip_prefix(JOB_TOKEN_SCOPE_PREFIX)?.parse().ok())
}

/// Error message for git writes to a pull mirror.
pub const MIRROR_READ_ONLY: &str = "This repository is a mirror and is read-only";

/// A repository resolved for the current caller.
#[derive(Debug, Clone)]
pub struct RepoAccess {
    pub repo: db::Repository,
    pub owner: db::User,
    /// Effective permission of the caller (scopes applied).
    pub permission: Permission,
    /// Whether the caller is authenticated (controls `permissions` in JSON).
    pub authenticated: bool,
}

impl RepoAccess {
    /// Load `{owner}/{name}` and compute the caller's permission.
    /// Returns 404 if the repo doesn't exist or the caller can't read it.
    pub async fn load(
        state: &AppState,
        auth: Option<&AuthContext>,
        owner: &str,
        name: &str,
    ) -> ApiResult<Self> {
        // Renamed / transferred repositories and renamed owners keep
        // answering on their old names (`repo_redirects`,
        // `login_redirects`; see `crate::lifecycle`).
        let (repo, owner) = crate::lifecycle::resolve_repo(&state.db, owner, name)
            .await?
            .ok_or(ApiError::NotFound)?;
        Self::for_repo(state, auth, repo, owner).await
    }

    /// Whether this was resolved through a redirect, i.e. `owner/name` (as
    /// requested) is not the repository's current full name.
    pub fn is_redirect(&self, owner: &str, name: &str) -> bool {
        let name = name.strip_suffix(".git").unwrap_or(name);
        !(self.owner.login.eq_ignore_ascii_case(owner) && self.repo.name.eq_ignore_ascii_case(name))
    }

    /// Like [`Self::load`] when the rows are already loaded.
    pub async fn for_repo(
        state: &AppState,
        auth: Option<&AuthContext>,
        repo: db::Repository,
        owner: db::User,
    ) -> ApiResult<Self> {
        // Private mode: anonymous callers read nothing (the middleware
        // refuses them first; this covers handlers it lets through, such
        // as git transport).
        if auth.is_none() && crate::privacy::private_mode(state).await? {
            return Err(ApiError::NotFound);
        }
        let raw = repo_permission(&state.db, auth.map(|a| a.user.id), &repo).await?;
        let permission = effective(auth, &repo, raw);
        if permission < Permission::Read {
            return Err(ApiError::NotFound);
        }
        // Repositories disabled by a site admin are blocked for everyone else.
        if repo.disabled && !auth.is_some_and(|a| a.user.site_admin) {
            return Err(ApiError::forbidden("Repository access blocked"));
        }
        Ok(Self {
            repo,
            owner,
            permission,
            authenticated: auth.is_some(),
        })
    }

    /// Require at least `needed`. Callers without read access get 404 (no
    /// existence leak); readers lacking `needed` get 403.
    pub fn require(&self, needed: Permission) -> ApiResult<()> {
        if self.permission >= needed {
            Ok(())
        } else if self.permission >= Permission::Read {
            Err(ApiError::forbidden(match needed {
                Permission::Admin => "Must have admin rights to Repository.",
                _ => "Resource not accessible by integration",
            }))
        } else {
            Err(ApiError::NotFound)
        }
    }

    /// Reject writes to archived repositories (403 like GitHub).
    /// Refuse git data writes (pushes, ref/contents/merge APIs) to a pull
    /// mirror: its refs only change by syncing from upstream.
    pub fn require_not_mirror(&self) -> ApiResult<()> {
        if self.repo.mirror_url.is_some() {
            Err(ApiError::forbidden(MIRROR_READ_ONLY))
        } else {
            Ok(())
        }
    }

    pub fn require_not_archived(&self) -> ApiResult<()> {
        if self.repo.archived {
            Err(ApiError::forbidden(
                "Repository was archived so is read-only.",
            ))
        } else {
            Ok(())
        }
    }

    /// Permission to show in the JSON `permissions` object (`None` when
    /// anonymous, which omits the field).
    pub fn api_permission(&self) -> Option<Permission> {
        self.authenticated.then_some(self.permission)
    }

    /// `owner/name`
    pub fn full_name(&self) -> String {
        format!("{}/{}", self.owner.login, self.repo.name)
    }

    /// Sync scope for this repository (`repo:{id}`).
    pub fn scope(&self) -> String {
        crate::sync::repo_scope(self.repo.id)
    }
}

/// A user's role in an organization (`org_members.role`).
///
/// Stored and serialized as lowercase text (`admin` | `member`). Decoding
/// any other value is an error, never a silent grant or deny.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, sqlx::Type)]
#[serde(rename_all = "lowercase")]
#[sqlx(type_name = "text", rename_all = "lowercase")]
pub enum OrgRole {
    Admin,
    Member,
}

impl OrgRole {
    pub fn as_str(self) -> &'static str {
        match self {
            OrgRole::Admin => "admin",
            OrgRole::Member => "member",
        }
    }

    pub fn is_admin(self) -> bool {
        self == OrgRole::Admin
    }
}

impl std::fmt::Display for OrgRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The caller's role in an organization, if any.
pub async fn org_role(
    db: impl PgExecutor<'_>,
    org_id: i64,
    user_id: i64,
) -> Result<Option<OrgRole>, sqlx::Error> {
    sqlx::query_scalar("SELECT role FROM org_members WHERE org_id = $1 AND user_id = $2")
        .bind(org_id)
        .bind(user_id)
        .fetch_optional(db)
        .await
}

/// The repositories a caller can read, for queries spanning many
/// repositories (search, activity feeds): every public repository, every
/// internal one when `internal` (signed-in users), plus `private_ids`, or
/// everything when `all` (site admins). Token scopes are applied: without
/// `repo` the set is public repositories only.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReadableRepos {
    pub all: bool,
    /// Non-public repositories the caller can read through explicit grants.
    pub private_ids: Vec<i64>,
    /// Every `internal` repository is readable (signed-in, `repo` scope).
    pub internal: bool,
}

impl ReadableRepos {
    pub fn can_read(&self, repo: &db::Repository) -> bool {
        self.all
            || !repo.is_private()
            || (self.internal && repo.is_internal())
            || self.private_ids.contains(&repo.id)
    }

    /// SQL condition on the `visibility` column of repositories alias
    /// `alias` matching the visibilities readable without a grant:
    /// `{alias}.visibility = 'public'`, or `IN ('public', 'internal')`.
    /// Combine with `OR {alias}.id = ANY(private_ids)` (unless `all`).
    pub fn visibility_sql(&self, alias: &str) -> String {
        if self.internal {
            format!("{alias}.visibility IN ('public', 'internal')")
        } else {
            format!("{alias}.visibility = 'public'")
        }
    }
}

/// Compute [`ReadableRepos`] for the caller with index-backed lookups
/// (owned, collaborator, org base permission, team grants incl. parents).
pub async fn readable_repos(
    db: impl PgExecutor<'_>,
    auth: Option<&AuthContext>,
) -> Result<ReadableRepos, sqlx::Error> {
    let Some(auth) = auth else {
        return Ok(ReadableRepos::default());
    };
    if crate::pat::is_fine_grained(auth) {
        return crate::pat::readable_repos(db, auth).await;
    }
    if !auth.has_scope("repo") {
        return Ok(ReadableRepos::default());
    }
    if auth.user.site_admin {
        return Ok(ReadableRepos {
            all: true,
            private_ids: vec![],
            internal: true,
        });
    }
    // Organizations whose token policy blocks the token are left out.
    let blocked: Vec<i64> = auth
        .scopes
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter_map(|s| s.strip_prefix(crate::pat::BLOCKED_ORG_PREFIX)?.parse().ok())
        .collect();
    let private_ids = private_readable_ids(db, auth.user.id, &blocked, None, None).await?;
    Ok(ReadableRepos {
        all: false,
        private_ids,
        internal: !auth.user.is_suspended(),
    })
}

/// Non-public repositories `user_id` can read (owned, collaborator, org
/// base permission, team grants incl. parents; site admins: all), ignoring
/// token scopes, except those owned by `exclude_owners`; limited to
/// repositories of `only_owner` and to `only_ids` when given.
pub async fn private_readable_ids(
    db: impl PgExecutor<'_>,
    user_id: i64,
    exclude_owners: &[i64],
    only_owner: Option<i64>,
    only_ids: Option<&[i64]>,
) -> Result<Vec<i64>, sqlx::Error> {
    sqlx::query_scalar(
        r#"
        WITH RECURSIVE user_teams AS (
            SELECT t.id, t.parent_id
              FROM team_members tm JOIN teams t ON t.id = tm.team_id
             WHERE tm.user_id = $1
            UNION
            SELECT p.id, p.parent_id
              FROM teams p JOIN user_teams ut ON p.id = ut.parent_id
        ), readable AS (
        SELECT id FROM repositories WHERE owner_id = $1 AND visibility <> 'public'
        UNION
        SELECT r.id FROM collaborators c JOIN repositories r ON r.id = c.repo_id
         WHERE c.user_id = $1 AND r.visibility <> 'public'
        UNION
        SELECT r.id FROM org_members m
          JOIN org_settings s ON s.org_id = m.org_id
          JOIN repositories r ON r.owner_id = m.org_id
         WHERE m.user_id = $1 AND r.visibility <> 'public'
           AND (m.role = 'admin' OR s.default_repository_permission <> 'none')
        UNION
        SELECT r.id FROM team_repos tr JOIN repositories r ON r.id = tr.repo_id
         WHERE tr.team_id IN (SELECT id FROM user_teams) AND r.visibility <> 'public'
        )
        SELECT r.id FROM repositories r
         WHERE (r.id IN (SELECT id FROM readable)
                OR (r.visibility <> 'public' AND (SELECT site_admin FROM users WHERE id = $1)))
           AND r.owner_id <> ALL($2)
           AND ($3::bigint IS NULL OR r.owner_id = $3)
           AND ($4::bigint[] IS NULL OR r.id = ANY($4))
        "#,
    )
    .bind(user_id)
    .bind(exclude_owners)
    .bind(only_owner)
    .bind(only_ids)
    .fetch_all(db)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn org_role_serde_round_trip() {
        for (role, text) in [(OrgRole::Admin, "admin"), (OrgRole::Member, "member")] {
            assert_eq!(serde_json::to_value(role).unwrap(), text);
            assert_eq!(
                serde_json::from_value::<OrgRole>(text.into()).unwrap(),
                role
            );
            assert_eq!(role.as_str(), text);
            assert_eq!(role.to_string(), text);
        }
        assert!(OrgRole::Admin.is_admin());
        assert!(!OrgRole::Member.is_admin());
        // GitHub's UI term and other near-misses are not roles.
        for bad in ["Admin", "owner", "admins", "billing_manager"] {
            assert!(
                serde_json::from_value::<OrgRole>(bad.into()).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn ordering_and_names() {
        assert!(Permission::Admin > Permission::Maintain);
        assert!(Permission::Write > Permission::Triage);
        assert_eq!(Permission::parse("push"), Some(Permission::Write));
        assert_eq!(Permission::Write.legacy_name(), "push");
        assert_eq!(Permission::Triage.coarse_name(), "read");
        assert_eq!(Permission::parse("bogus"), None);
    }
}
