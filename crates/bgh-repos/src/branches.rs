//! Branches.
//!
//! * `GET /repos/{o}/{r}/branches` (`protected=true|false`)
//! * `GET /repos/{o}/{r}/branches/{branch}` (branch names may contain `/`)
//! * `POST /repos/{o}/{r}/branches/{branch}/rename`
//! * `/repos/{o}/{r}/branches/{branch}/protection[/...]` → [`crate::protection_api`]
//! * `POST /repos/{o}/{r}/merges`
//! * `POST /repos/{o}/{r}/merge-upstream`
//!
//! All ref changes go through [`crate::refs::write_ref`] (branch protection,
//! post-receive processing).

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use bgh_core::perms::RepoAccess;
use bgh_core::prelude::*;
use bgh_core::urls::encode_path;
use bgh_git::ops::MergeOutcome;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::commits::{render_commits, resolve};
use crate::gitjson::{CommitJson, RepoRef, ShaUrl, short_commit};
use crate::protection::{ProtectionRow, RepoRules};
use crate::refs::write_ref;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/repos/{owner}/{repo}/branches", get(list_branches))
        .route(
            "/repos/{owner}/{repo}/branches/{*rest}",
            get(branch_path)
                .post(branch_path)
                .put(branch_path)
                .patch(branch_path)
                .delete(branch_path),
        )
        .route("/repos/{owner}/{repo}/merges", post(merge))
        .route("/repos/{owner}/{repo}/merge-upstream", post(merge_upstream))
}

/// `protection` summary of branch lists (`enabled` + status checks).
pub fn protection_summary(rule: Option<&ProtectionRow>) -> Value {
    match rule {
        None => json!({
            "enabled": false,
            "required_status_checks": {"enforcement_level": "off", "contexts": [], "checks": []},
        }),
        Some(p) => crate::protection_api::protection_summary(p),
    }
}

/// `short-branch`.
#[derive(Debug, Serialize)]
pub struct ShortBranch {
    pub name: String,
    pub commit: ShaUrl,
    pub protected: bool,
    pub protection: Value,
    pub protection_url: String,
}

fn protection_url(r: &RepoRef, branch: &str) -> String {
    r.api(&format!("/branches/{}/protection", encode_path(branch)))
}

#[derive(Debug, Default, Deserialize)]
pub struct ListQuery {
    pub protected: Option<bool>,
}

/// `GET /repos/{owner}/{repo}/branches`
async fn list_branches(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo)): Path<(String, String)>,
    Query(q): Query<ListQuery>,
) -> ApiResult<Page<ShortBranch>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let branches = crate::store(&state)
        .read(access.repo.id, |r| r.branches())
        .await?;
    let rules = RepoRules::load(&state.db, &access.repo).await?;
    let r = RepoRef::new(&state.urls, &access);
    let mut items: Vec<ShortBranch> = branches
        .iter()
        .map(|b| {
            let name = b.short_name().to_string();
            let rule = rules.protection_for(&name);
            let protected = rule.is_some() || rules.rulesets_for(&b.name).next().is_some();
            ShortBranch {
                commit: short_commit(&r, &b.peeled),
                protected,
                protection: protection_summary(rule),
                protection_url: protection_url(&r, &name),
                name,
            }
        })
        .filter(|b| q.protected.is_none_or(|want| b.protected == want))
        .collect();
    let total = items.len() as i64;
    let items: Vec<ShortBranch> = items
        .drain(..)
        .skip(p.offset() as usize)
        .take(p.limit() as usize)
        .collect();
    Ok(p.page_with_total(items, total))
}

/// `branch-with-protection`.
#[derive(Debug, Serialize)]
pub struct Branch {
    pub name: String,
    pub commit: CommitJson,
    #[serde(rename = "_links")]
    pub links: Value,
    pub protected: bool,
    pub protection: Value,
    pub protection_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required_approving_review_count: Option<i64>,
}

/// Render one branch (404 `Branch not found` if missing).
pub async fn branch_json(state: &AppState, access: &RepoAccess, name: &str) -> ApiResult<Branch> {
    let refname = format!("refs/heads/{name}");
    let found = crate::store(state)
        .read(access.repo.id, move |r| r.find_ref(&refname))
        .await?
        .ok_or_else(branch_not_found)?;
    let git = crate::store(state).cli(access.repo.id)?;
    let commit = git.commit(&found.peeled).await?;
    let r = RepoRef::new(&state.urls, access);
    let commit = render_commits(state, &r, std::slice::from_ref(&commit))
        .await?
        .remove(0);
    let rules = RepoRules::load(&state.db, &access.repo).await?;
    let rule = rules.protection_for(name);
    let protected = rule.is_some() || rules.rulesets_for(&found.name).next().is_some();
    Ok(Branch {
        name: name.to_string(),
        commit,
        links: json!({
            "self": r.api(&format!("/branches/{}", encode_path(name))),
            "html": r.html(&format!("/tree/{}", encode_path(name))),
        }),
        protected,
        protection: protection_summary(rule),
        protection_url: protection_url(&r, name),
        pattern: rule.map(|p| p.pattern.clone()),
        required_approving_review_count: rule.and_then(|p| {
            p.required_pull_request_reviews
                .as_ref()
                .map(|v| v["required_approving_review_count"].as_i64().unwrap_or(0))
        }),
    })
}

fn branch_not_found() -> ApiError {
    ApiError::Status(StatusCode::NOT_FOUND, "Branch not found".into())
}

/// Catch-all for `/branches/{*rest}`: branch, rename and protection paths.
async fn branch_path(
    State(state): State<AppState>,
    auth: MaybeUser,
    method: Method,
    Path((owner, repo, rest)): Path<(String, String, String)>,
    body: Bytes,
) -> ApiResult<Response> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    if let Some((branch, sub)) = crate::protection_api::split_protection_path(&rest) {
        let sub: Vec<&str> = sub.iter().map(String::as_str).collect();
        return crate::protection_api::dispatch(
            &state,
            auth.as_ref(),
            &access,
            &method,
            &branch,
            &sub,
            &body,
        )
        .await;
    }
    if method == Method::POST
        && let Some(branch) = rest.strip_suffix("/rename")
    {
        let auth = auth.0.ok_or_else(ApiError::requires_auth)?;
        let body: RenameBody = bgh_core::extract::parse_json(&body)?;
        let (status, json) = rename(&state, &auth, &access, branch, body).await?;
        return Ok((status, Json(json)).into_response());
    }
    if method == Method::GET {
        return Ok(Json(branch_json(&state, &access, &rest).await?).into_response());
    }
    Err(ApiError::NotFound)
}

// ----- rename ----------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct RenameBody {
    pub new_name: Option<String>,
}

/// `POST /repos/{o}/{r}/branches/{branch}/rename` → 201 with the renamed
/// branch. Renaming the default branch needs admin, others push access.
/// Protection rules naming the branch, the default branch and open pull
/// requests follow the rename.
async fn rename(
    state: &AppState,
    auth: &AuthContext,
    access: &RepoAccess,
    branch: &str,
    body: RenameBody,
) -> ApiResult<(StatusCode, Branch)> {
    let rules = RepoRules::load(&state.db, &access.repo).await?;
    // Default and protected branches need admin, others push access.
    let is_default = branch == access.repo.default_branch;
    access.require(if is_default || rules.protection_for(branch).is_some() {
        Permission::Admin
    } else {
        Permission::Write
    })?;
    access.require_not_archived()?;
    access.require_not_mirror()?;
    let new_name = body
        .new_name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .ok_or_else(|| ApiError::invalid_field(FieldError::missing_field("Branch", "new_name")))?
        .to_string();
    if !bgh_git::is_valid_ref_name(&new_name) {
        return Err(ApiError::invalid_field(FieldError::invalid(
            "Branch", "new_name",
        )));
    }
    let store = crate::store(state);
    let (old_ref, new_ref) = (
        format!("refs/heads/{branch}"),
        format!("refs/heads/{new_name}"),
    );
    let (old, exists) = {
        let (o, n) = (old_ref.clone(), new_ref.clone());
        store
            .read(access.repo.id, move |r| {
                Ok((r.find_ref(&o)?, r.find_ref(&n)?.is_some()))
            })
            .await?
    };
    let old = old.ok_or_else(branch_not_found)?;
    if exists {
        return Err(ApiError::invalid_field(FieldError::custom(
            "Branch",
            "new_name",
            format!("Branch {new_name} already exists"),
        )));
    }
    write_ref(
        state,
        access,
        &auth.user,
        &new_ref,
        None,
        Some(&old.target),
        true,
    )
    .await?;
    // Not a deletion in protection terms: the rule moves with the branch.
    bgh_git::write::delete_ref(&store, access.repo.id, &old_ref, Some(&old.target)).await?;
    bgh_core::jobs::enqueue_job(
        &state.db,
        &crate::jobs::PostReceive {
            repo_id: access.repo.id,
            pusher_id: Some(auth.user.id),
            updates: vec![bgh_core::events::RefUpdate {
                old: old.target.clone(),
                new: bgh_core::events::ZERO_SHA.into(),
                refname: old_ref.clone(),
            }],
        },
    )
    .await?;

    let mut tx = Tx::begin(state).await?;
    sqlx::query(
        "UPDATE branch_protections SET pattern = $3, updated_at = now()
          WHERE repo_id = $1 AND pattern = $2",
    )
    .bind(access.repo.id)
    .bind(branch)
    .bind(&new_name)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE pull_requests p SET base_ref = $3 FROM issues i
          WHERE i.id = p.issue_id AND p.repo_id = $1 AND p.base_ref = $2 AND i.state = 'open'",
    )
    .bind(access.repo.id)
    .bind(branch)
    .bind(&new_name)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE pull_requests p SET head_ref = $3 FROM issues i
          WHERE i.id = p.issue_id AND p.head_repo_id = $1 AND p.head_ref = $2 AND i.state = 'open'",
    )
    .bind(access.repo.id)
    .bind(branch)
    .bind(&new_name)
    .execute(&mut *tx)
    .await?;
    let mut access = access.clone();
    if is_default {
        let repo: db::Repository = sqlx::query_as(&format!(
            "UPDATE repositories SET default_branch = $2, updated_at = now() WHERE id = $1 RETURNING {}",
            db::Repository::COLUMNS
        ))
        .bind(access.repo.id)
        .bind(&new_name)
        .fetch_one(&mut *tx)
        .await?;
        bgh_git::write::set_head(&store, repo.id, &new_name).await?;
        tx.sync_model(SyncModel::Repo, repo.id, SyncAction::Update)
            .await?;
        access.repo = repo;
    }
    bgh_core::audit::log(
        &mut *tx,
        Some(&auth.user),
        "repo.rename_branch",
        bgh_core::audit::Target::Repo {
            id: access.repo.id,
            org_id: access.owner.is_org().then_some(access.owner.id),
        },
        json!({"from": branch, "to": new_name}),
    )
    .await?;
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        branch_json(state, &access, &new_name).await?,
    ))
}

// ----- merges ----------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct MergeBody {
    pub base: Option<String>,
    pub head: Option<String>,
    pub commit_message: Option<String>,
}

/// `POST /repos/{owner}/{repo}/merges`: merge `head` (branch or SHA) into
/// branch `base` with a merge commit. 201 commit, 204 when already merged,
/// 409 on conflicts, 404 when base/head are missing.
async fn merge(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo)): Path<(String, String)>,
    Json(body): Json<MergeBody>,
) -> ApiResult<Response> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    access.require(Permission::Write)?;
    access.require_not_archived()?;
    access.require_not_mirror()?;
    let (Some(base), Some(head)) = (body.base.as_deref(), body.head.as_deref()) else {
        return Err(ApiError::invalid_field(FieldError::missing_field(
            "Merge",
            if body.base.is_none() { "base" } else { "head" },
        )));
    };
    let git = crate::store(&state).cli(access.repo.id)?;
    let base_sha = git
        .resolve_commit(&format!("refs/heads/{base}"))
        .await?
        .ok_or_else(|| ApiError::Status(StatusCode::NOT_FOUND, "Base does not exist".into()))?;
    let head_sha = git
        .resolve_commit(head)
        .await?
        .ok_or_else(|| ApiError::Status(StatusCode::NOT_FOUND, "Head does not exist".into()))?;
    if git.is_ancestor(&head_sha, &base_sha).await? {
        return Ok(StatusCode::NO_CONTENT.into_response());
    }
    let tree = match git.merge_trees(&base_sha, &head_sha).await? {
        MergeOutcome::Clean { tree } => tree,
        MergeOutcome::Conflicts { .. } => return Err(ApiError::conflict("Merge conflict")),
    };
    let me = crate::identity::default_identity(&state, &auth.user).await?;
    let message = body
        .commit_message
        .clone()
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| format!("Merge {head} into {base}"));
    let sha = git
        .commit_tree(&tree, &[base_sha.clone(), head_sha], &message, &me, &me)
        .await?;
    write_ref(
        &state,
        &access,
        &auth.user,
        &format!("refs/heads/{base}"),
        Some(&base_sha),
        Some(&sha),
        false,
    )
    .await?;
    let commit = git.commit(&sha).await?;
    let r = RepoRef::new(&state.urls, &access);
    let json = render_commits(&state, &r, std::slice::from_ref(&commit))
        .await?
        .remove(0);
    Ok((StatusCode::CREATED, Json(json)).into_response())
}

#[derive(Debug, Default, Deserialize)]
pub struct MergeUpstreamBody {
    pub branch: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct MergeUpstreamResult {
    pub message: String,
    pub merge_type: &'static str,
    pub base_branch: String,
}

/// `POST /repos/{owner}/{repo}/merge-upstream`: bring a fork branch up to
/// date with the same-named upstream branch (fast-forward when possible,
/// otherwise a merge commit; 409 on conflicts).
async fn merge_upstream(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo)): Path<(String, String)>,
    Json(body): Json<MergeUpstreamBody>,
) -> ApiResult<Json<MergeUpstreamResult>> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    access.require(Permission::Write)?;
    access.require_not_archived()?;
    access.require_not_mirror()?;
    let branch = body
        .branch
        .as_deref()
        .map(str::trim)
        .filter(|b| !b.is_empty())
        .ok_or_else(|| ApiError::invalid_field(FieldError::missing_field("Branch", "branch")))?
        .to_string();
    let parent_id = access
        .repo
        .parent_id
        .ok_or_else(|| ApiError::unprocessable("This repository is not a fork."))?;
    let parent = db::Repository::find(&state.db, parent_id)
        .await?
        .ok_or_else(|| ApiError::unprocessable("The upstream repository no longer exists."))?;
    let parent_owner = db::User::find(&state.db, parent.owner_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    // The caller must be able to read the upstream repository.
    let upstream = RepoAccess::for_repo(&state, Some(&auth), parent, parent_owner).await?;

    let store = crate::store(&state);
    let git = store.cli(access.repo.id)?;
    let up_git = store.cli(upstream.repo.id)?;
    let refname = format!("refs/heads/{branch}");
    let ours = resolve(&git, &refname)
        .await
        .map_err(|_| branch_not_found())?;
    let theirs = up_git.resolve_commit(&refname).await?.ok_or_else(|| {
        ApiError::unprocessable(format!("The upstream branch {branch} does not exist."))
    })?;
    let base_branch = format!("{}:{branch}", upstream.owner.login);
    if git.object_type(&theirs).await?.is_none() {
        git.fetch_objects(up_git.dir(), &refname).await?;
    }
    if git.is_ancestor(&theirs, &ours).await? {
        return Ok(Json(MergeUpstreamResult {
            message: "This branch is not behind the upstream.".into(),
            merge_type: "none",
            base_branch,
        }));
    }
    let (new, merge_type) = if git.is_ancestor(&ours, &theirs).await? {
        (theirs.clone(), "fast-forward")
    } else {
        let tree = match git.merge_trees(&ours, &theirs).await? {
            MergeOutcome::Clean { tree } => tree,
            MergeOutcome::Conflicts { .. } => {
                return Err(ApiError::conflict(
                    "There are merge conflicts with the upstream branch.",
                ));
            }
        };
        let me = crate::identity::default_identity(&state, &auth.user).await?;
        let message = format!("Merge branch '{branch}' of {}", upstream.full_name());
        let sha = git
            .commit_tree(&tree, &[ours.clone(), theirs.clone()], &message, &me, &me)
            .await?;
        (sha, "merge")
    };
    write_ref(
        &state,
        &access,
        &auth.user,
        &refname,
        Some(&ours),
        Some(&new),
        false,
    )
    .await?;
    Ok(Json(MergeUpstreamResult {
        message: format!(
            "Successfully fetched and {} from upstream {base_branch}.",
            if merge_type == "merge" {
                "merged"
            } else {
                "fast-forwarded"
            }
        ),
        merge_type,
        base_branch,
    }))
}
