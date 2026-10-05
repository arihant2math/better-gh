//! Repository permissions.
//!
//! A user's permission on a repository is the maximum of:
//! * owner of a user-owned repo → Admin; site admin → Admin
//! * org admin of the owning org → Admin; org member → org base permission
//! * team grants (`team_repos`) of teams the user belongs to, including
//!   grants inherited from parent teams
//! * direct collaborator permission
//! * public visibility → Read (for everyone, including anonymous)
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
    collab: Option<String>,
    org_role: Option<String>,
    org_base: Option<String>,
    team_perms: Option<Vec<String>>,
}

fn public_floor(repo: &db::Repository) -> Permission {
    if repo.is_private() {
        Permission::None
    } else {
        Permission::Read
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
    for row in rows {
        let best = out.entry(row.repo_id).or_insert(Permission::None);
        let mut raise = |p: Option<Permission>| {
            if let Some(p) = p {
                *best = (*best).max(p);
            }
        };
        raise(row.collab.as_deref().and_then(Permission::parse));
        match row.org_role.as_deref() {
            Some("admin") => raise(Some(Permission::Admin)),
            Some(_) => raise(row.org_base.as_deref().and_then(Permission::parse)),
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
        collab: Option<String>,
        org_role: Option<String>,
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
        raise(row.collab.as_deref().and_then(Permission::parse));
        match row.org_role.as_deref() {
            Some("admin") => raise(Some(Permission::Admin)),
            Some(_) => raise(row.org_base.as_deref().and_then(Permission::parse)),
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
pub fn effective(auth: Option<&AuthContext>, repo: &db::Repository, raw: Permission) -> Permission {
    let Some(auth) = auth else {
        return raw;
    };
    if auth.has_scope("repo") {
        return raw;
    }
    if repo.is_private() {
        Permission::None
    } else if auth.has_scope("public_repo") {
        raw
    } else {
        raw.min(Permission::Read)
    }
}

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
        let name = name.strip_suffix(".git").unwrap_or(name);
        let owner = db::User::find_by_login(&state.db, owner)
            .await?
            .ok_or(ApiError::NotFound)?;
        let repo = db::Repository::find_by_name(&state.db, owner.id, name)
            .await?
            .ok_or(ApiError::NotFound)?;
        Self::for_repo(state, auth, repo, owner).await
    }

    /// Like [`Self::load`] when the rows are already loaded.
    pub async fn for_repo(
        state: &AppState,
        auth: Option<&AuthContext>,
        repo: db::Repository,
        owner: db::User,
    ) -> ApiResult<Self> {
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

/// The caller's role in an organization (`admin` | `member`), if any.
pub async fn org_role(
    db: impl PgExecutor<'_>,
    org_id: i64,
    user_id: i64,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT role FROM org_members WHERE org_id = $1 AND user_id = $2")
        .bind(org_id)
        .bind(user_id)
        .fetch_optional(db)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

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
