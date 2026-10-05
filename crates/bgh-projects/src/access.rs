//! Project permissions.
//!
//! * site admin, the owning user, admins of the owning org → `Admin`
//! * members of the owning org → `Write`
//! * anyone (including anonymous) on a public project → `Read`
//!
//! Tokens need the `project` scope to write and `read:project` (implied by
//! `project`) to read non-public projects.

use bgh_core::perms;
use bgh_core::prelude::*;

use crate::model::{ProjectRow, owner_scope};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    Read,
    Write,
    Admin,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Admin => "admin",
        }
    }
}

/// The caller's role on projects of `owner`, ignoring visibility (`None`
/// when only public projects are visible).
pub async fn owner_role(
    state: &AppState,
    auth: Option<&AuthContext>,
    owner: &db::User,
) -> ApiResult<Option<Role>> {
    let Some(auth) = auth else {
        return Ok(None);
    };
    let role = if auth.user.site_admin || auth.user.id == owner.id {
        Some(Role::Admin)
    } else if owner.is_org() {
        match perms::org_role(&state.db, owner.id, auth.user.id)
            .await?
            .as_deref()
        {
            Some("admin") => Some(Role::Admin),
            Some(_) => Some(Role::Write),
            None => None,
        }
    } else {
        None
    };
    // Token scopes cap what a PAT may do.
    Ok(match role {
        Some(_) if !auth.has_scope("read:project") => None,
        Some(r) if r > Role::Read && !auth.has_scope("project") => Some(Role::Read),
        r => r,
    })
}

/// Effective role on a project given the owner role and visibility.
pub fn project_role(owner_role: Option<Role>, public: bool) -> Option<Role> {
    owner_role.or(public.then_some(Role::Read))
}

/// A project resolved for the current caller.
#[derive(Debug, Clone)]
pub struct ProjectAccess {
    pub project: ProjectRow,
    pub owner: db::User,
    pub role: Role,
}

impl ProjectAccess {
    /// Load by id; 404 unless the caller can read it.
    pub async fn load(state: &AppState, auth: Option<&AuthContext>, id: i64) -> ApiResult<Self> {
        let project = ProjectRow::find(&state.db, id)
            .await?
            .ok_or(ApiError::NotFound)?;
        let owner = db::User::find(&state.db, project.owner_id)
            .await?
            .ok_or(ApiError::NotFound)?;
        Self::resolve(state, auth, project, owner).await
    }

    /// Load by owner login + project number.
    pub async fn load_by_number(
        state: &AppState,
        auth: Option<&AuthContext>,
        owner: &str,
        number: i64,
    ) -> ApiResult<Self> {
        let owner = db::User::find_by_login(&state.db, owner)
            .await?
            .ok_or(ApiError::NotFound)?;
        let project: ProjectRow = sqlx::query_as(&format!(
            "{} WHERE p.owner_id = $1 AND p.number = $2",
            ProjectRow::SELECT
        ))
        .bind(owner.id)
        .bind(number)
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::NotFound)?;
        Self::resolve(state, auth, project, owner).await
    }

    async fn resolve(
        state: &AppState,
        auth: Option<&AuthContext>,
        project: ProjectRow,
        owner: db::User,
    ) -> ApiResult<Self> {
        let role = project_role(owner_role(state, auth, &owner).await?, project.public)
            .ok_or(ApiError::NotFound)?;
        Ok(Self {
            project,
            owner,
            role,
        })
    }

    /// 403 for callers with a lower role (they can already see the project).
    pub fn require(&self, needed: Role) -> ApiResult<()> {
        if self.role >= needed {
            Ok(())
        } else {
            Err(ApiError::forbidden(match needed {
                Role::Admin => "Must have admin rights to Project.",
                _ => "Must have write access to Project.",
            }))
        }
    }

    pub fn id(&self) -> i64 {
        self.project.id
    }

    /// Sync scope of the owner.
    pub fn scope(&self) -> String {
        owner_scope(self.owner.id, self.owner.is_org())
    }
}
