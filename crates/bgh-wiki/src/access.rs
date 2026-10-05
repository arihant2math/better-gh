//! Who may read and edit a wiki.
//!
//! * Read: repository read access and `has_wiki` (otherwise 404).
//! * Edit (web and `git push`): repository write permission, or any
//!   signed-in reader when `wiki_anyone_can_edit` is on (tokens need the
//!   `public_repo` scope). Archived/disabled repositories are read-only.

use bgh_core::prelude::*;
use bgh_git::RepoStore;

/// A repository whose wiki the caller can read.
#[derive(Debug, Clone)]
pub struct WikiAccess {
    pub access: RepoAccess,
    pub anyone_can_edit: bool,
    /// Whether the caller may edit (ignoring archived/disabled).
    pub may_edit: bool,
}

pub fn store(state: &AppState) -> RepoStore {
    RepoStore::from_config(&state.config).wiki()
}

impl WikiAccess {
    /// Load without checking `has_wiki` (settings).
    pub async fn load_any(
        state: &AppState,
        auth: Option<&AuthContext>,
        owner: &str,
        repo: &str,
    ) -> ApiResult<Self> {
        let access = RepoAccess::load(state, auth, owner, repo).await?;
        Self::for_access(state, auth, access).await
    }

    /// Load for wiki reads: 404 unless readable and `has_wiki`.
    pub async fn load(
        state: &AppState,
        auth: Option<&AuthContext>,
        owner: &str,
        repo: &str,
    ) -> ApiResult<Self> {
        let w = Self::load_any(state, auth, owner, repo).await?;
        if !w.access.repo.has_wiki {
            return Err(ApiError::NotFound);
        }
        Ok(w)
    }

    pub async fn for_access(
        state: &AppState,
        auth: Option<&AuthContext>,
        access: RepoAccess,
    ) -> ApiResult<Self> {
        let anyone_can_edit: bool =
            sqlx::query_scalar("SELECT wiki_anyone_can_edit FROM repositories WHERE id = $1")
                .bind(access.repo.id)
                .fetch_one(&state.db)
                .await?;
        let may_edit = access.permission >= Permission::Write
            || (anyone_can_edit
                && access.permission >= Permission::Read
                && auth.is_some_and(|a| a.has_scope("public_repo")));
        Ok(Self {
            access,
            anyone_can_edit,
            may_edit,
        })
    }

    pub fn repo_id(&self) -> i64 {
        self.access.repo.id
    }

    pub fn owner(&self) -> &str {
        &self.access.owner.login
    }

    pub fn name(&self) -> &str {
        &self.access.repo.name
    }

    /// `canEdit` as shown to the client.
    pub fn can_edit(&self) -> bool {
        self.may_edit && !self.access.repo.archived && !self.access.repo.disabled
    }

    /// Require edit rights for a web write.
    pub fn require_edit(&self, auth: Option<&AuthContext>) -> ApiResult<()> {
        if auth.is_none() {
            return Err(ApiError::requires_auth());
        }
        if !self.may_edit {
            return Err(ApiError::forbidden(
                "You do not have permission to edit this wiki.",
            ));
        }
        if self.access.repo.disabled {
            return Err(ApiError::forbidden("Repository access blocked."));
        }
        self.access.require_not_archived()
    }
}
