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
    let role = perms::org_role(&state.db, org.id, auth.user.id).await?;
    let allowed = match role.as_deref() {
        Some("admin") => true,
        Some(_) => {
            let s = db::OrgSettings::find(&state.db, org.id)
                .await?
                .ok_or(ApiError::NotFound)?;
            let private = wants_private(&body);
            s.members_can_create_repositories
                && if private {
                    s.members_can_create_private_repositories
                } else {
                    s.members_can_create_public_repositories
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
    create(&state, &auth, org, body).await
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
    mut body: CreateRepoBody,
) -> ApiResult<(StatusCode, Json<Repository>)> {
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
    let finished = finish_create(
        state,
        auth,
        &store,
        tx,
        repo,
        body.auto_init.unwrap_or(false),
    )
    .await;
    let repo = match finished {
        Ok(repo) => repo,
        Err(e) => {
            let _ = store.delete(repo_id).await;
            return Err(e);
        }
    };

    let access = RepoAccess {
        repo,
        owner,
        permission: Permission::Admin,
        authenticated: true,
    };
    let json = full_repo(state, Some(auth), &access).await?;
    Ok((StatusCode::CREATED, Json(json)))
}

/// Steps after the git repository exists on disk; any error makes the
/// caller remove the directory (the transaction rolls back on drop).
async fn finish_create(
    state: &AppState,
    auth: &AuthContext,
    store: &bgh_git::RepoStore,
    mut tx: Tx,
    mut repo: db::Repository,
    auto_init: bool,
) -> ApiResult<db::Repository> {
    if auto_init {
        let author = crate::identity::default_identity(state, &auth.user).await?;
        let mut readme = format!("# {}\n", repo.name);
        if let Some(d) = &repo.description {
            readme.push_str(&format!("\n{d}\n"));
        }
        write::commit_changes(
            store,
            repo.id,
            CommitRequest {
                branch: &repo.default_branch,
                parent: None,
                changes: &[FileChange::write("README.md", readme)],
                message: "Initial commit",
                author: &author,
                committer: None,
            },
        )
        .await?;
        repo = sqlx::query_as(&format!(
            "UPDATE repositories SET pushed_at = now() WHERE id = $1 RETURNING {}",
            db::Repository::COLUMNS
        ))
        .bind(repo.id)
        .fetch_one(&mut *tx)
        .await?;
    }
    bgh_core::labels::create_defaults(&mut tx, repo.id).await?;
    tx.sync_model(SyncModel::Repo, repo.id, SyncAction::Insert)
        .await?;
    tx.sync_viewer_repo(auth.user.id, repo.id).await?;
    tx.emit(Event::RepositoryCreated {
        repo_id: repo.id,
        actor_id: auth.user.id,
    });
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
