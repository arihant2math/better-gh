//! Who may do what with a package.
//!
//! * A package linked to a repository inherits that repository's access
//!   (read → pull, write → push, admin → delete and settings).
//! * Unlinked packages: the owning user, org admins and the publisher
//!   (`created_by`) are admins; org members can read.
//! * Public packages can be pulled by anyone, `internal` ones by any
//!   signed-in user.
//! * Creating a package (first push) needs to be the owner or an org
//!   member.
//! * Personal access tokens additionally need `read:packages` (beyond
//!   public content), `write:packages` and `delete:packages`.
//! * Actions job tokens (`GITHUB_TOKEN`) reach only the packages linked to
//!   their repository (write unless the token is read-only), may create new
//!   packages under the repository owner (linked on first push), and see
//!   other packages like an anonymous caller.

use bgh_core::auth::AuthContext;
use bgh_core::models::db;
use bgh_core::perms::{self, Permission};
use bgh_core::{ApiResult, AppState};

use crate::model::PackageRow;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    None,
    Read,
    Write,
    Admin,
}

/// What the caller may do with one package (token scopes applied).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Caps {
    pub read: bool,
    pub write: bool,
    pub admin: bool,
}

impl Caps {
    /// OCI token actions (`pull`, `push`, `delete`).
    pub fn allows(&self, action: &str) -> bool {
        match action {
            "pull" => self.read,
            "push" => self.write,
            "delete" => self.admin,
            "*" => self.read && self.write && self.admin,
            _ => false,
        }
    }
}

fn floor(auth: Option<&AuthContext>, pkg: Option<&PackageRow>) -> Role {
    match pkg {
        Some(p) if p.is_public() => Role::Read,
        Some(p) if p.visibility == "internal" && auth.is_some() => Role::Read,
        _ => Role::None,
    }
}

fn from_repo_permission(p: Permission) -> Role {
    match p {
        Permission::Admin => Role::Admin,
        Permission::Write | Permission::Maintain => Role::Write,
        Permission::Read | Permission::Triage => Role::Read,
        Permission::None => Role::None,
    }
}

/// The caller's role, ignoring token scopes. `pkg = None` asks whether the
/// caller may create `owner`'s package (Write or Admin).
pub async fn role(
    state: &AppState,
    auth: Option<&AuthContext>,
    owner: &db::User,
    pkg: Option<&PackageRow>,
) -> ApiResult<Role> {
    let base = floor(auth, pkg);
    let Some(auth) = auth else { return Ok(base) };
    let user = &auth.user;

    if let Some(job_repo) = perms::job_token_repo(auth) {
        let read_only = auth
            .scopes
            .as_ref()
            .is_some_and(|s| s.iter().any(|s| s == perms::JOB_TOKEN_READ_ONLY_SCOPE));
        let cap = if read_only { Role::Read } else { Role::Write };
        let Some(repo) = db::Repository::find(&state.db, job_repo).await? else {
            return Ok(base);
        };
        return Ok(match pkg {
            Some(p) if p.repo_id == Some(job_repo) => {
                linked_role(state, user.id, repo).await?.min(cap).max(base)
            }
            Some(_) => base,
            None if repo.owner_id == owner.id && !read_only => Role::Write,
            None => Role::None,
        });
    }

    if user.site_admin || user.id == owner.id {
        return Ok(Role::Admin);
    }
    let org_role = if owner.is_org() {
        perms::org_role(&state.db, owner.id, user.id).await?
    } else {
        None
    };
    if org_role.is_some_and(|r| r.is_admin()) {
        return Ok(Role::Admin);
    }
    let Some(p) = pkg else {
        return Ok(if org_role.is_some() {
            Role::Write
        } else {
            Role::None
        });
    };
    if p.created_by == Some(user.id) && p.repo_id.is_none() {
        return Ok(Role::Admin);
    }
    if let Some(repo_id) = p.repo_id
        && let Some(repo) = db::Repository::find(&state.db, repo_id).await?
    {
        return Ok(linked_role(state, user.id, repo).await?.max(base));
    }
    Ok(if org_role.is_some() {
        Role::Read.max(base)
    } else {
        base
    })
}

/// Role inherited from a linked repository. Only real grants count: a
/// public repository's read-for-everyone doesn't make a private package
/// public (that's the package's own visibility).
async fn linked_role(state: &AppState, user_id: i64, mut repo: db::Repository) -> ApiResult<Role> {
    repo.visibility = "private".into();
    let raw = perms::repo_permission(&state.db, Some(user_id), &repo).await?;
    Ok(from_repo_permission(raw))
}

/// Apply token scopes to a role.
pub fn caps(auth: Option<&AuthContext>, pkg: Option<&PackageRow>, role: Role) -> Caps {
    let scoped = |scope: &str| match auth {
        None => true,
        // Job tokens are limited by `role` already.
        Some(a) if perms::job_token_repo(a).is_some() => true,
        Some(a) => a.has_scope(scope),
    };
    let base = floor(auth, pkg);
    Caps {
        read: role >= Role::Read && (base >= Role::Read || scoped("read:packages")),
        write: role >= Role::Write && scoped("write:packages"),
        admin: role >= Role::Admin && scoped("delete:packages"),
    }
}

/// Role and scopes in one call.
pub async fn package_caps(
    state: &AppState,
    auth: Option<&AuthContext>,
    owner: &db::User,
    pkg: Option<&PackageRow>,
) -> ApiResult<Caps> {
    let r = role(state, auth, owner, pkg).await?;
    Ok(caps(auth, pkg, r))
}
