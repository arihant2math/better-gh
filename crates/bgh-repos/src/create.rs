//! `POST /user/repos` and `POST /orgs/{org}/repos`.

use axum::extract::State;
use axum::http::StatusCode;
use bgh_core::audit;
use bgh_core::error::unique_violation;
use bgh_core::models::api::Repository;
use bgh_core::perms::{self, RepoAccess};
use bgh_core::prelude::*;
use bgh_git::write::{self, CommitRequest, FileChange};
use serde::Deserialize;
use serde_json::json;

use crate::json::full_repo;

/// Default branch for new repositories.
pub const DEFAULT_BRANCH: &str = "main";

/// Request body (subset of GitHub's create-repository parameters).
#[derive(Debug, Default, Deserialize)]
pub struct CreateRepoBody {
    pub name: Option<String>,
    pub description: Option<String>,
    pub homepage: Option<String>,
    pub private: Option<bool>,
    /// `public` | `private` | `internal` (orgs only); overrides `private`.
    pub visibility: Option<String>,
    pub has_issues: Option<bool>,
    pub has_projects: Option<bool>,
    pub has_wiki: Option<bool>,
    pub has_discussions: Option<bool>,
    pub is_template: Option<bool>,
    /// Create an initial commit with a README.
    pub auto_init: Option<bool>,
    pub allow_squash_merge: Option<bool>,
    pub allow_merge_commit: Option<bool>,
    pub allow_rebase_merge: Option<bool>,
    pub allow_auto_merge: Option<bool>,
    pub allow_update_branch: Option<bool>,
    pub delete_branch_on_merge: Option<bool>,
    pub use_squash_pr_title_as_default: Option<bool>,
    /// `.gitignore` template name (`GET /gitignore/templates`); implies an
    /// initial commit.
    pub gitignore_template: Option<String>,
    /// License key (`GET /licenses`); implies an initial commit.
    pub license_template: Option<String>,
    /// Organization team granted access (its default permission).
    pub team_id: Option<i64>,
}

/// Initial files requested by `auto_init` / the templates.
#[derive(Debug, Default)]
struct InitFiles {
    readme: bool,
    gitignore: Option<&'static str>,
    license: Option<&'static bgh_core::licenses::License>,
}

impl InitFiles {
    fn from_body(body: &CreateRepoBody) -> ApiResult<Self> {
        let gitignore = match body.gitignore_template.as_deref().filter(|s| !s.is_empty()) {
            None => None,
            Some(name) => Some(
                crate::gitignore::find(name)
                    .ok_or_else(|| {
                        ApiError::invalid_field(FieldError::invalid(
                            "Repository",
                            "gitignore_template",
                        ))
                    })?
                    .1,
            ),
        };
        let license = match body.license_template.as_deref().filter(|s| !s.is_empty()) {
            None => None,
            Some(key) => Some(bgh_core::licenses::find(key).ok_or_else(|| {
                ApiError::invalid_field(FieldError::invalid("Repository", "license_template"))
            })?),
        };
        Ok(Self {
            readme: body.auto_init.unwrap_or(false),
            gitignore,
            license,
        })
    }

    fn any(&self) -> bool {
        self.readme || self.gitignore.is_some() || self.license.is_some()
    }
}

/// GitHub repository name rules: `[A-Za-z0-9._-]{1,100}`, not `.`/`..`,
/// not ending in `.git`.
pub fn is_valid_repo_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 100
        && name != "."
        && name != ".."
        && !name.to_ascii_lowercase().ends_with(".git")
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

/// `POST /user/repos`
pub async fn create_for_user(
    State(state): State<AppState>,
    auth: RequireUser,
    Json(body): Json<CreateRepoBody>,
) -> ApiResult<(StatusCode, Json<Repository>)> {
    let owner = auth.user.clone();
    create(&state, &auth, owner, body).await
}

/// `POST /orgs/{org}/repos`: org admins, or members when the org allows it.
pub async fn create_for_org(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(org): Path<String>,
    Json(body): Json<CreateRepoBody>,
) -> ApiResult<(StatusCode, Json<Repository>)> {
    let org = db::User::find_by_login(&state.db, &org)
        .await?
        .filter(db::User::is_org)
        .ok_or(ApiError::NotFound)?;
    authorize_org(&state, &auth, &org, &body).await?;
    create(&state, &auth, org, body).await
}

/// Whether `auth` may create the repository described by `body` in `org`.
pub(crate) async fn authorize_org(
    state: &AppState,
    auth: &AuthContext,
    org: &db::User,
    body: &CreateRepoBody,
) -> ApiResult<()> {
    let role = perms::org_role(&state.db, org.id, auth.user.id).await?;
    let allowed = match role {
        Some(perms::OrgRole::Admin) => true,
        Some(_) => {
            let s = db::OrgSettings::find(&state.db, org.id)
                .await?
                .ok_or(ApiError::NotFound)?;
            s.members_can_create_repositories
                && match body.visibility.as_deref() {
                    Some("internal") => s.members_can_create_internal_repositories,
                    _ if wants_private(body) => s.members_can_create_private_repositories,
                    _ => s.members_can_create_public_repositories,
                }
        }
        None if auth.user.site_admin => true,
        None => return Err(ApiError::NotFound),
    };
    if !allowed {
        return Err(ApiError::forbidden(
            "You need admin access to the organization before adding a repository to it.",
        ));
    }
    Ok(())
}

fn wants_private(body: &CreateRepoBody) -> bool {
    match body.visibility.as_deref() {
        Some(v) => v != "public",
        None => body.private.unwrap_or(false),
    }
}

async fn create(
    state: &AppState,
    auth: &AuthContext,
    owner: db::User,
    body: CreateRepoBody,
) -> ApiResult<(StatusCode, Json<Repository>)> {
    let access = create_with(state, auth, owner, body, None).await?;
    let json = full_repo(state, Some(auth), &access).await?;
    Ok((StatusCode::CREATED, Json(json)))
}

/// Create a repository (row, storage, defaults). `import` additionally
/// records an import (and pull mirror) in the same transaction.
pub async fn create_with(
    state: &AppState,
    auth: &AuthContext,
    owner: db::User,
    mut body: CreateRepoBody,
    import: Option<crate::import::NewImport>,
) -> ApiResult<RepoAccess> {
    if body.visibility.is_none() && body.private.is_none() {
        let settings = bgh_core::settings::load(state).await?;
        body.visibility = Some(settings.default_visibility(owner.is_org()).to_string());
    }
    let private = wants_private(&body);
    auth.require_scope(if private { "repo" } else { "public_repo" })?;

    let name = body
        .name
        .as_deref()
        .map(str::trim)
        .unwrap_or_default()
        .to_string();
    if name.is_empty() {
        return Err(ApiError::invalid_field(FieldError::missing_field(
            "Repository",
            "name",
        )));
    }
    if !is_valid_repo_name(&name) {
        return Err(ApiError::invalid_field(FieldError::custom(
            "Repository",
            "name",
            "name may only contain alphanumeric characters, '.', '-' and '_'",
        )));
    }
    let visibility = match body.visibility.as_deref() {
        None => if private { "private" } else { "public" }.to_string(),
        Some(v @ ("public" | "private")) => v.to_string(),
        Some("internal") if owner.is_org() => "internal".to_string(),
        Some(_) => {
            return Err(ApiError::invalid_field(FieldError::invalid(
                "Repository",
                "visibility",
            )));
        }
    };

    bgh_core::settings::load(state)
        .await?
        .privacy
        .check_visibility(&visibility)?;

    let mut init = InitFiles::from_body(&body)?;
    if import.is_some() {
        init = InitFiles::default();
    }
    let team = match body.team_id {
        None => None,
        Some(id) => Some(
            sqlx::query_as::<_, (i64, String)>(
                "SELECT id, permission FROM teams WHERE id = $1 AND org_id = $2",
            )
            .bind(id)
            .bind(owner.id)
            .fetch_optional(&state.db)
            .await?
            .ok_or_else(|| ApiError::invalid_field(FieldError::invalid("Repository", "team_id")))?,
        ),
    };

    let mut tx = Tx::begin(state).await?;
    let repo: db::Repository = sqlx::query_as(&format!(
        "INSERT INTO repositories (
            owner_id, name, description, homepage, visibility, default_branch,
            has_issues, has_projects, has_wiki, has_discussions, is_template,
            allow_squash_merge, allow_merge_commit, allow_rebase_merge, allow_auto_merge,
            allow_update_branch, delete_branch_on_merge, use_squash_pr_title_as_default,
            watchers_count)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, 1)
         RETURNING {}",
        db::Repository::COLUMNS
    ))
    .bind(owner.id)
    .bind(&name)
    .bind(body.description.as_deref().filter(|s| !s.is_empty()))
    .bind(body.homepage.as_deref().filter(|s| !s.is_empty()))
    .bind(&visibility)
    .bind(DEFAULT_BRANCH)
    .bind(body.has_issues.unwrap_or(true))
    .bind(body.has_projects.unwrap_or(true))
    .bind(body.has_wiki.unwrap_or(true))
    .bind(body.has_discussions.unwrap_or(false))
    .bind(body.is_template.unwrap_or(false))
    .bind(body.allow_squash_merge.unwrap_or(true))
    .bind(body.allow_merge_commit.unwrap_or(true))
    .bind(body.allow_rebase_merge.unwrap_or(true))
    .bind(body.allow_auto_merge.unwrap_or(false))
    .bind(body.allow_update_branch.unwrap_or(false))
    .bind(body.delete_branch_on_merge.unwrap_or(false))
    .bind(body.use_squash_pr_title_as_default.unwrap_or(false))
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| match unique_violation(&e).as_deref() {
        // GitHub's message for this case (not "Validation Failed").
        Some("repositories_owner_name_key") => ApiError::Validation {
            message: "Repository creation failed.".into(),
            errors: vec![FieldError::custom(
                "Repository",
                "name",
                "name already exists on this account",
            )],
        },
        _ => e.into(),
    })?;

    // A new repository takes over a redirect left by a rename/transfer.
    sqlx::query(
        "DELETE FROM repo_redirects WHERE lower(owner_login) = lower($1) AND lower(name) = lower($2)",
    )
    .bind(&owner.login)
    .bind(&repo.name)
    .execute(&mut *tx)
    .await?;

    // The creator watches the new repository (like GitHub).
    sqlx::query("INSERT INTO watches (user_id, repo_id) VALUES ($1, $2)")
        .bind(auth.user.id)
        .bind(repo.id)
        .execute(&mut *tx)
        .await?;
    audit::log(
        &mut *tx,
        Some(&auth.user),
        "repo.create",
        audit::Target::Repo {
            id: repo.id,
            org_id: owner.is_org().then_some(owner.id),
        },
        json!({ "name": repo.name, "visibility": repo.visibility }),
    )
    .await?;

    // Create storage inside the transaction window: if it fails the row is
    // rolled back; if the commit fails the directory is removed.
    let store = crate::store(state);
    store.init(repo.id, &repo.default_branch).await?;
    let repo_id = repo.id;
    let finished = finish_create(state, auth, &owner, &store, tx, repo, init, team, import).await;
    let repo = match finished {
        Ok(repo) => repo,
        Err(e) => {
            let _ = store.delete(repo_id).await;
            return Err(e);
        }
    };

    Ok(RepoAccess {
        repo,
        owner,
        permission: Permission::Admin,
        authenticated: true,
    })
}

/// Steps after the git repository exists on disk; any error makes the
/// caller remove the directory (the transaction rolls back on drop).
#[allow(clippy::too_many_arguments)]
async fn finish_create(
    state: &AppState,
    auth: &AuthContext,
    owner: &db::User,
    store: &bgh_git::RepoStore,
    mut tx: Tx,
    mut repo: db::Repository,
    init: InitFiles,
    team: Option<(i64, String)>,
    import: Option<crate::import::NewImport>,
) -> ApiResult<db::Repository> {
    if init.any() {
        let author = crate::identity::default_identity(state, &auth.user).await?;
        let mut changes = Vec::new();
        if init.readme {
            let mut readme = format!("# {}\n", repo.name);
            if let Some(d) = &repo.description {
                readme.push_str(&format!("\n{d}\n"));
            }
            changes.push(FileChange::write("README.md", readme));
        }
        if let Some(source) = init.gitignore {
            changes.push(FileChange::write(".gitignore", source));
        }
        if let Some(license) = init.license {
            use chrono::Datelike;
            let fullname = owner.name.as_deref().filter(|n| !n.is_empty());
            let text = bgh_core::licenses::render(
                license,
                chrono::Utc::now().year(),
                fullname.unwrap_or(&owner.login),
            );
            changes.push(FileChange::write("LICENSE", text));
        }
        write::commit_changes(
            store,
            repo.id,
            CommitRequest {
                branch: &repo.default_branch,
                parent: None,
                changes: &changes,
                message: "Initial commit",
                author: &author,
                committer: None,
            },
        )
        .await?;
        // The template's license is known; detection fills in the blob.
        repo = sqlx::query_as(&format!(
            "UPDATE repositories SET pushed_at = now(), license_spdx_id = $2,
                    license_blob_sha = CASE WHEN $2 IS NULL THEN '' END
              WHERE id = $1 RETURNING {}",
            db::Repository::COLUMNS
        ))
        .bind(repo.id)
        .bind(init.license.map(|l| l.spdx_id.as_str()))
        .fetch_one(&mut *tx)
        .await?;
        if init.license.is_some() {
            crate::licenses::enqueue_detect(&mut tx, repo.id).await?;
        }
    }
    bgh_core::labels::create_defaults(&mut tx, repo.id).await?;
    if let Some(import) = import {
        repo = crate::import::record(&mut tx, auth, repo, import).await?;
    }
    tx.sync_model(SyncModel::Repo, repo.id, SyncAction::Insert)
        .await?;
    tx.sync_viewer_repo(auth.user.id, repo.id).await?;
    tx.emit(Event::RepositoryCreated {
        repo_id: repo.id,
        actor_id: auth.user.id,
    });
    if let Some((team_id, permission)) = &team {
        sqlx::query(
            "INSERT INTO team_repos (team_id, repo_id, permission) VALUES ($1, $2, $3)
             ON CONFLICT (team_id, repo_id) DO NOTHING",
        )
        .bind(team_id)
        .bind(repo.id)
        .bind(permission)
        .execute(&mut *tx)
        .await?;
        tx.sync_model(SyncModel::Team, *team_id, SyncAction::Update)
            .await?;
        tx.emit(Event::TeamRepoAdded {
            org_id: owner.id,
            team_id: *team_id,
            repo_id: repo.id,
            actor_id: auth.user.id,
        });
    }
    tx.commit().await?;
    Ok(repo)
}

#[cfg(test)]
mod tests {
    #[test]
    fn repo_names() {
        use super::is_valid_repo_name as ok;
        assert!(ok("hello-world_1.0"));
        assert!(!ok(""));
        assert!(!ok(".."));
        assert!(!ok("a b"));
        assert!(!ok("x.git"));
        assert!(!ok(&"a".repeat(101)));
    }
}
