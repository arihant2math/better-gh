//! Forks and template repositories.
//!
//! * `GET /repos/{owner}/{repo}/forks` (`sort=newest|oldest|stargazers|watchers`)
//! * `POST /repos/{owner}/{repo}/forks` (`organization`, `name`,
//!   `default_branch_only`) → 202
//! * `POST /repos/{template_owner}/{template_repo}/generate` → 201
//!
//! Fork storage is a `clone --bare --shared` of the parent (objects are
//! borrowed through `objects/info/alternates`); deleting the parent first
//! makes its forks self-contained (`jobs::delete_storage`). Repositories
//! generated from a template get fresh single-commit histories whose
//! objects are copied (repacked) from the template, sharing nothing.

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use bgh_core::audit;
use bgh_core::error::unique_violation;
use bgh_core::models::api::{MinimalRepository, Repository};
use bgh_core::perms::{self, RepoAccess};
use bgh_core::prelude::*;
use bgh_core::sync;
use serde::Deserialize;
use serde_json::json;

use crate::create::is_valid_repo_name;
use crate::json::{full_repo, repo_sync_json};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/repos/{owner}/{repo}/forks",
            get(list_forks).post(create_fork),
        )
        .route("/repos/{owner}/{repo}/generate", post(generate))
}

#[derive(Debug, Default, Deserialize)]
pub struct ListForksQuery {
    pub sort: Option<String>,
}

/// `GET /repos/{owner}/{repo}/forks`
async fn list_forks(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo)): Path<(String, String)>,
    Query(q): Query<ListForksQuery>,
) -> ApiResult<Page<MinimalRepository>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let order = match q.sort.as_deref() {
        Some("oldest") => "created_at ASC, id ASC",
        Some("stargazers") => "stargazers_count DESC, id DESC",
        Some("watchers") => "watchers_count DESC, id DESC",
        _ => "created_at DESC, id DESC",
    };
    let rows: Vec<db::Repository> = sqlx::query_as(&format!(
        "SELECT {} FROM repositories WHERE parent_id = $1 ORDER BY {order} LIMIT $2 OFFSET $3",
        db::Repository::COLUMNS
    ))
    .bind(access.repo.id)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let page = p.page(rows);
    let items = bgh_core::views::minimal_repos(&state, auth.as_ref(), page.items).await?;
    Ok(Page {
        items,
        link: page.link,
    })
}

/// Whether `user` may create repositories owned by `target` (`private`
/// selects the org setting to check).
pub async fn can_create_in(
    state: &AppState,
    user: &db::User,
    target: &db::User,
    private: bool,
) -> ApiResult<bool> {
    if user.id == target.id || user.site_admin {
        return Ok(true);
    }
    if !target.is_org() {
        return Ok(false);
    }
    Ok(
        match perms::org_role(&state.db, target.id, user.id)
            .await?
            .as_deref()
        {
            Some("admin") => true,
            Some(_) => match db::OrgSettings::find(&state.db, target.id).await? {
                Some(s) => {
                    s.members_can_create_repositories
                        && if private {
                            s.members_can_create_private_repositories
                        } else {
                            s.members_can_create_public_repositories
                        }
                }
                None => false,
            },
            None => false,
        },
    )
}

/// First free name among `base`, `base-1`, `base-2`, ... for `owner_id`.
async fn free_name(state: &AppState, owner_id: i64, base: &str) -> ApiResult<String> {
    let taken: Vec<String> = sqlx::query_scalar(
        "SELECT lower(name) FROM repositories
          WHERE owner_id = $1 AND (lower(name) = lower($2) OR lower(name) LIKE lower($2) || '-%')",
    )
    .bind(owner_id)
    .bind(base)
    .fetch_all(&state.db)
    .await?;
    let lower = base.to_ascii_lowercase();
    if !taken.contains(&lower) {
        return Ok(base.to_string());
    }
    for i in 1.. {
        let candidate = format!("{base}-{i}");
        if !taken.contains(&candidate.to_ascii_lowercase()) {
            return Ok(candidate);
        }
    }
    unreachable!()
}

#[derive(Debug, Default, Deserialize)]
pub struct ForkBody {
    pub organization: Option<String>,
    pub name: Option<String>,
    #[serde(default)]
    pub default_branch_only: bool,
}

/// `POST /repos/{owner}/{repo}/forks` → 202 with the fork (or the
/// caller's existing fork in the same network).
async fn create_fork(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo)): Path<(String, String)>,
    Json(body): Json<ForkBody>,
) -> ApiResult<(StatusCode, Json<Repository>)> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    let src = access.repo.clone();
    auth.require_scope(if src.is_private() {
        "repo"
    } else {
        "public_repo"
    })?;
    if src.is_private() && !src.allow_forking {
        return Err(ApiError::forbidden(
            "Forking is disabled for this repository.",
        ));
    }
    if src.is_private() && access.owner.is_org() {
        let allowed = db::OrgSettings::find(&state.db, access.owner.id)
            .await?
            .is_some_and(|s| s.members_can_fork_private_repositories);
        if !allowed && access.permission < Permission::Admin {
            return Err(ApiError::forbidden(
                "Forking private repositories is disabled for this organization.",
            ));
        }
    }
    let target = match body.organization.as_deref() {
        Some(org) => db::User::find_by_login(&state.db, org)
            .await?
            .filter(db::User::is_org)
            .ok_or_else(|| {
                ApiError::invalid_field(FieldError::invalid("Repository", "organization"))
            })?,
        None => auth.user.clone(),
    };
    if !can_create_in(&state, &auth.user, &target, src.is_private()).await? {
        return Err(ApiError::forbidden(format!(
            "You don't have the permission to create repositories on {}",
            target.login
        )));
    }
    if target.id == src.owner_id {
        return Err(ApiError::invalid_field(FieldError::custom(
            "Repository",
            "organization",
            "A repository cannot be forked into its own account.",
        )));
    }
    let network = src.source_id.unwrap_or(src.id);

    // Existing fork of this network owned by the target: return it.
    let existing: Option<db::Repository> = sqlx::query_as(&format!(
        "SELECT {} FROM repositories
          WHERE owner_id = $1 AND fork AND (source_id = $2 OR parent_id = $2) LIMIT 1",
        db::Repository::COLUMNS
    ))
    .bind(target.id)
    .bind(network)
    .fetch_optional(&state.db)
    .await?;
    if let Some(repo) = existing {
        let access = RepoAccess::for_repo(&state, Some(&auth), repo, target).await?;
        return Ok((
            StatusCode::ACCEPTED,
            Json(full_repo(&state, Some(&auth), &access).await?),
        ));
    }

    let name = match body.name.as_deref().map(str::trim) {
        Some(n) if !n.is_empty() => {
            if !is_valid_repo_name(n) {
                return Err(ApiError::invalid_field(FieldError::invalid(
                    "Repository",
                    "name",
                )));
            }
            n.to_string()
        }
        _ => free_name(&state, target.id, &src.name).await?,
    };

    let mut tx = Tx::begin(&state).await?;
    let fork: db::Repository = sqlx::query_as(&format!(
        "INSERT INTO repositories (
            owner_id, name, description, homepage, visibility, default_branch, fork,
            parent_id, source_id, has_issues, has_projects, has_wiki, has_discussions,
            allow_squash_merge, allow_merge_commit, allow_rebase_merge, allow_forking,
            language, size, pushed_at, watchers_count)
         SELECT $1, $2, description, homepage, visibility, default_branch, true,
                id, $3, false, has_projects, false, false,
                allow_squash_merge, allow_merge_commit, allow_rebase_merge, allow_forking,
                language, size, pushed_at, 1
           FROM repositories WHERE id = $4
         RETURNING {}",
        db::Repository::COLUMNS
    ))
    .bind(target.id)
    .bind(&name)
    .bind(network)
    .bind(src.id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| match unique_violation(&e).as_deref() {
        Some("repositories_owner_name_key") => ApiError::invalid_field(FieldError::custom(
            "Repository",
            "name",
            "name already exists on this account",
        )),
        _ => e.into(),
    })?;
    sqlx::query("UPDATE repositories SET forks_count = forks_count + 1 WHERE id = $1")
        .bind(src.id)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "DELETE FROM repo_redirects WHERE lower(owner_login) = lower($1) AND lower(name) = lower($2)",
    )
    .bind(&target.login)
    .bind(&fork.name)
    .execute(&mut *tx)
    .await?;
    sqlx::query("INSERT INTO watches (user_id, repo_id) VALUES ($1, $2) ON CONFLICT DO NOTHING")
        .bind(auth.user.id)
        .bind(fork.id)
        .execute(&mut *tx)
        .await?;
    audit::log(
        &mut *tx,
        Some(&auth.user),
        "repo.create",
        audit::Target::Repo {
            id: fork.id,
            org_id: target.is_org().then_some(target.id),
        },
        json!({"name": fork.name, "fork": true, "parent": access.full_name()}),
    )
    .await?;
    tx.sync(
        &sync::repo_scope(fork.id),
        "repository",
        fork.id,
        SyncAction::Insert,
        &repo_sync_json(&fork, &target.login),
    )
    .await?;
    tx.emit(Event::RepositoryCreated {
        repo_id: fork.id,
        actor_id: auth.user.id,
    });
    tx.emit(Event::RepositoryForked {
        repo_id: src.id,
        fork_id: fork.id,
        actor_id: auth.user.id,
    });

    // Storage inside the transaction window: failures roll the row back.
    let store = crate::store(&state);
    let cloned = async {
        store.fork(src.id, fork.id).await?;
        if body.default_branch_only {
            let git = store.cli(fork.id)?;
            let branches = store.read(fork.id, |r| r.branches()).await?;
            for b in branches {
                if b.short_name() != fork.default_branch {
                    git.run(&["update-ref", "-d", &b.name], &[], None).await?;
                }
            }
        }
        Ok::<_, ApiError>(())
    }
    .await;
    if let Err(e) = cloned {
        let _ = store.delete(fork.id).await;
        return Err(e);
    }
    if let Err(e) = tx.commit().await {
        let _ = store.delete(fork.id).await;
        return Err(e.into());
    }

    let access = RepoAccess::for_repo(&state, Some(&auth), fork, target).await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(full_repo(&state, Some(&auth), &access).await?),
    ))
}

#[derive(Debug, Default, Deserialize)]
pub struct GenerateBody {
    pub owner: Option<String>,
    pub name: Option<String>,
    pub description: Option<String>,
    #[serde(default)]
    pub include_all_branches: bool,
    #[serde(default)]
    pub private: bool,
}

/// `POST /repos/{template_owner}/{template_repo}/generate` → 201.
async fn generate(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo)): Path<(String, String)>,
    Json(body): Json<GenerateBody>,
) -> ApiResult<(StatusCode, Json<Repository>)> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    let template = access.repo.clone();
    if !template.is_template {
        return Err(ApiError::invalid_field(FieldError::custom(
            "Repository",
            "template",
            format!("{} is not a template repository", access.full_name()),
        )));
    }
    auth.require_scope(if body.private { "repo" } else { "public_repo" })?;
    let target = match body.owner.as_deref() {
        Some(o) => db::User::find_by_login(&state.db, o)
            .await?
            .ok_or_else(|| ApiError::invalid_field(FieldError::invalid("Repository", "owner")))?,
        None => auth.user.clone(),
    };
    if !can_create_in(&state, &auth.user, &target, body.private).await? {
        return Err(ApiError::forbidden(format!(
            "You don't have the permission to create repositories on {}",
            target.login
        )));
    }
    let name = body
        .name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .ok_or_else(|| ApiError::invalid_field(FieldError::missing_field("Repository", "name")))?
        .to_string();
    if !is_valid_repo_name(&name) {
        return Err(ApiError::invalid_field(FieldError::invalid(
            "Repository",
            "name",
        )));
    }
    let description = body
        .description
        .clone()
        .filter(|d| !d.trim().is_empty())
        .or_else(|| template.description.clone());

    let store = crate::store(&state);
    let tgit = store.cli(template.id)?;
    let branches: Vec<bgh_git::RefInfo> = store
        .read(template.id, |r| r.branches())
        .await?
        .into_iter()
        .filter(|b| body.include_all_branches || b.short_name() == template.default_branch)
        .collect();
    let mut tips = Vec::new();
    for b in &branches {
        let commit = tgit.commit(&b.peeled).await?;
        tips.push((b.short_name().to_string(), commit.tree));
    }

    let mut tx = Tx::begin(&state).await?;
    let new: db::Repository = sqlx::query_as(&format!(
        "INSERT INTO repositories (
            owner_id, name, description, homepage, visibility, default_branch,
            template_repository_id, has_issues, has_projects, has_wiki, watchers_count)
         VALUES ($1, $2, $3, NULL, $4, $5, $6, true, true, true, 1)
         RETURNING {}",
        db::Repository::COLUMNS
    ))
    .bind(target.id)
    .bind(&name)
    .bind(&description)
    .bind(if body.private { "private" } else { "public" })
    .bind(&template.default_branch)
    .bind(template.id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| match unique_violation(&e).as_deref() {
        Some("repositories_owner_name_key") => ApiError::invalid_field(FieldError::custom(
            "Repository",
            "name",
            "name already exists on this account",
        )),
        _ => e.into(),
    })?;
    sqlx::query(
        "DELETE FROM repo_redirects WHERE lower(owner_login) = lower($1) AND lower(name) = lower($2)",
    )
    .bind(&target.login)
    .bind(&new.name)
    .execute(&mut *tx)
    .await?;
    sqlx::query("INSERT INTO watches (user_id, repo_id) VALUES ($1, $2) ON CONFLICT DO NOTHING")
        .bind(auth.user.id)
        .bind(new.id)
        .execute(&mut *tx)
        .await?;
    audit::log(
        &mut *tx,
        Some(&auth.user),
        "repo.create",
        audit::Target::Repo {
            id: new.id,
            org_id: target.is_org().then_some(target.id),
        },
        json!({"name": new.name, "template": access.full_name()}),
    )
    .await?;

    let identity = crate::identity::default_identity(&state, &auth.user).await?;
    let built = async {
        store.init(new.id, &new.default_branch).await?;
        let git = store.cli(new.id)?.with_objects_of(tgit.dir());
        for (branch, tree) in &tips {
            let sha = git
                .commit_tree(tree, &[], "Initial commit", &identity, &identity)
                .await?;
            git.update_ref(&format!("refs/heads/{branch}"), &sha, None)
                .await?;
        }
        // Copy the referenced objects out of the template.
        git.dissociate().await?;
        Ok::<_, ApiError>(())
    }
    .await;
    if let Err(e) = built {
        let _ = store.delete(new.id).await;
        return Err(e);
    }
    let new = if tips.is_empty() {
        new
    } else {
        sqlx::query_as(&format!(
            "UPDATE repositories SET pushed_at = now() WHERE id = $1 RETURNING {}",
            db::Repository::COLUMNS
        ))
        .bind(new.id)
        .fetch_one(&mut *tx)
        .await?
    };
    tx.sync(
        &sync::repo_scope(new.id),
        "repository",
        new.id,
        SyncAction::Insert,
        &repo_sync_json(&new, &target.login),
    )
    .await?;
    tx.emit(Event::RepositoryCreated {
        repo_id: new.id,
        actor_id: auth.user.id,
    });
    if !tips.is_empty() {
        crate::stats::enqueue_languages(&mut tx, new.id).await?;
    }
    if let Err(e) = tx.commit().await {
        let _ = store.delete(new.id).await;
        return Err(e.into());
    }

    let access = RepoAccess::for_repo(&state, Some(&auth), new, target).await?;
    Ok((
        StatusCode::CREATED,
        Json(full_repo(&state, Some(&auth), &access).await?),
    ))
}
