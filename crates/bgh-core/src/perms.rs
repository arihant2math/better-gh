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
    site_admin: bool,
    collab: Option<String>,
    org_role: Option<String>,
    org_base: Option<String>,
    team_perms: Option<Vec<String>>,
}

/// Compute `user_id`'s permission on `repo`, ignoring token scopes.
pub async fn repo_permission(
    db: impl PgExecutor<'_>,
    user_id: Option<i64>,
    repo: &db::Repository,
) -> Result<Permission, sqlx::Error> {
    let public_floor = if repo.is_private() {
        Permission::None
    } else {
        Permission::Read
    };
    let Some(uid) = user_id else {
        return Ok(public_floor);
    };
    if uid == repo.owner_id {
        return Ok(Permission::Admin);
    }
    let row: Option<PermRow> = sqlx::query_as(
        r#"
        WITH RECURSIVE user_teams AS (
            SELECT t.id, t.parent_id
              FROM team_members tm JOIN teams t ON t.id = tm.team_id
             WHERE tm.user_id = $2 AND t.org_id = $3
            UNION
            SELECT p.id, p.parent_id
              FROM teams p JOIN user_teams ut ON p.id = ut.parent_id
        )
        SELECT u.site_admin,
               (SELECT permission FROM collaborators WHERE repo_id = $1 AND user_id = $2) AS collab,
               (SELECT role FROM org_members WHERE org_id = $3 AND user_id = $2) AS org_role,
               (SELECT default_repository_permission FROM org_settings WHERE org_id = $3) AS org_base,
               (SELECT array_agg(tr.permission) FROM team_repos tr
                 WHERE tr.repo_id = $1 AND tr.team_id IN (SELECT id FROM user_teams)) AS team_perms
          FROM users u WHERE u.id = $2
        "#,
    )
    .bind(repo.id)
    .bind(uid)
    .bind(repo.owner_id)
    .fetch_optional(db)
    .await?;
    let Some(row) = row else {
        return Ok(public_floor);
    };
    let mut best = public_floor;
    let mut raise = |p: Option<Permission>| {
        if let Some(p) = p {
            best = best.max(p);
        }
    };
    if row.site_admin {
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
    Ok(best)
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
