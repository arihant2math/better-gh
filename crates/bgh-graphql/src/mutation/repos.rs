//! Repository, star and ref mutations (bgh-repos).

use async_graphql::{Context, ID, InputObject, Object, SimpleObject};
use axum::extract::State;
use bgh_core::auth::RequireUser;
use bgh_core::node_id::{self, NodeType};
use bgh_core::prelude::*;
use serde_json::json;

use super::{body, guard, into_json, owner_repo, reload_repo, repo_by_id, repo_by_node};
use crate::ctx::{GResult, err, gql, not_found};
use crate::loaders::{Loaders, one};
use crate::model::enums::RepositoryVisibility;
use crate::model::{Ref, Repository};
use crate::scalars::{GitObjectID, URI};

fn st(ctx: &Context<'_>) -> State<AppState> {
    State(gql(ctx).state.clone())
}

fn user(a: &bgh_core::auth::AuthContext) -> RequireUser {
    RequireUser(a.clone())
}

#[derive(InputObject)]
pub struct CreateRepositoryInput {
    pub name: String,
    pub owner_id: Option<ID>,
    pub description: Option<String>,
    pub visibility: RepositoryVisibility,
    pub template: Option<bool>,
    pub homepage_url: Option<URI>,
    pub has_wiki_enabled: Option<bool>,
    pub has_issues_enabled: Option<bool>,
    pub team_id: Option<ID>,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
pub struct CreateRepositoryPayload {
    pub repository: Option<Repository>,
    pub client_mutation_id: Option<String>,
}

#[derive(InputObject)]
pub struct UpdateRepositoryInput {
    pub repository_id: ID,
    pub name: Option<String>,
    pub description: Option<String>,
    pub template: Option<bool>,
    pub homepage_url: Option<URI>,
    pub has_wiki_enabled: Option<bool>,
    pub has_issues_enabled: Option<bool>,
    pub has_projects_enabled: Option<bool>,
    pub has_discussions_enabled: Option<bool>,
    pub has_sponsorships_enabled: Option<bool>,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
pub struct UpdateRepositoryPayload {
    pub repository: Option<Repository>,
    pub client_mutation_id: Option<String>,
}

#[derive(InputObject)]
pub struct CloneTemplateRepositoryInput {
    pub repository_id: ID,
    pub name: String,
    pub owner_id: ID,
    pub description: Option<String>,
    pub visibility: RepositoryVisibility,
    #[graphql(default)]
    pub include_all_branches: bool,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
pub struct CloneTemplateRepositoryPayload {
    pub repository: Option<Repository>,
    pub client_mutation_id: Option<String>,
}

#[derive(InputObject)]
pub struct ArchiveRepositoryInput {
    pub repository_id: ID,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
pub struct ArchiveRepositoryPayload {
    pub repository: Option<Repository>,
    pub client_mutation_id: Option<String>,
}

#[derive(InputObject)]
pub struct UnarchiveRepositoryInput {
    pub repository_id: ID,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
pub struct UnarchiveRepositoryPayload {
    pub repository: Option<Repository>,
    pub client_mutation_id: Option<String>,
}

#[derive(InputObject)]
pub struct AddStarInput {
    pub starrable_id: ID,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
pub struct AddStarPayload {
    pub starrable: Option<Repository>,
    pub client_mutation_id: Option<String>,
}

#[derive(InputObject)]
pub struct RemoveStarInput {
    pub starrable_id: ID,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
pub struct RemoveStarPayload {
    pub starrable: Option<Repository>,
    pub client_mutation_id: Option<String>,
}

#[derive(InputObject)]
pub struct CreateRefInput {
    pub repository_id: ID,
    pub name: String,
    pub oid: GitObjectID,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
pub struct CreateRefPayload {
    #[graphql(name = "ref")]
    pub ref_: Option<Ref>,
    pub client_mutation_id: Option<String>,
}

#[derive(InputObject)]
pub struct UpdateRefInput {
    pub ref_id: ID,
    pub oid: GitObjectID,
    #[graphql(default)]
    pub force: bool,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
pub struct UpdateRefPayload {
    #[graphql(name = "ref")]
    pub ref_: Option<Ref>,
    pub client_mutation_id: Option<String>,
}

#[derive(InputObject)]
pub struct DeleteRefInput {
    pub ref_id: ID,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
pub struct DeleteRefPayload {
    pub client_mutation_id: Option<String>,
}

/// Owner login for an owner node id (user or organization).
async fn owner_login(ctx: &Context<'_>, id: &ID) -> GResult<db::User> {
    let uid = super::decode(id, &[NodeType::User, NodeType::Organization], "an owner")?;
    let l = ctx.data_unchecked::<Loaders>();
    one(&l.users, uid)
        .await?
        .map(|u| (*u).clone())
        .ok_or_else(|| not_found("Could not resolve to an owner."))
}

/// A ref node id (`{repo_id}:{refs/...}`) → (repo, full ref name).
async fn ref_by_node(ctx: &Context<'_>, id: &ID) -> GResult<(Repository, String)> {
    let bad = || {
        not_found(format!(
            "Could not resolve to a Ref with the global id of '{}'.",
            id.0
        ))
    };
    let (ty, key) = node_id::decode_raw(&id.0).ok_or_else(bad)?;
    if ty != NodeType::Ref {
        return Err(bad());
    }
    let (repo_id, name) = key.split_once(':').ok_or_else(bad)?;
    let repo = repo_by_id(ctx, repo_id.parse().map_err(|_| bad())?).await?;
    Ok((Repository(repo), name.to_string()))
}

async fn patch_repo(
    ctx: &Context<'_>,
    repo_id: i64,
    patch: serde_json::Value,
) -> GResult<Repository> {
    let a = guard(ctx)?;
    let repo = repo_by_id(ctx, repo_id).await?;
    let (o, r) = owner_repo(&repo);
    into_json(
        bgh_repos::settings::update_repo(st(ctx), user(a), Path((o, r)), Json(body(patch)?)).await,
    )
    .await?;
    reload_repo(ctx, repo_id).await
}

#[derive(Default)]
pub struct RepoMutations;

#[Object]
impl RepoMutations {
    /// Create a new repository.
    pub async fn create_repository(
        &self,
        ctx: &Context<'_>,
        input: CreateRepositoryInput,
    ) -> GResult<CreateRepositoryPayload> {
        let a = guard(ctx)?;
        let b = body(json!({
            "name": input.name,
            "description": input.description,
            "homepage": input.homepage_url.map(|u| u.0),
            "visibility": input.visibility.rest(),
            "is_template": input.template,
            "has_wiki": input.has_wiki_enabled,
            "has_issues": input.has_issues_enabled,
        }))?;
        let owner = match &input.owner_id {
            Some(id) => Some(owner_login(ctx, id).await?),
            None => None,
        };
        let v = match owner {
            Some(o) if o.is_org() => {
                into_json(
                    bgh_repos::create::create_for_org(st(ctx), user(a), Path(o.login), Json(b))
                        .await,
                )
                .await?
            }
            Some(o) if o.id != a.user.id => {
                return Err(err(
                    "FORBIDDEN",
                    "You can only create repositories for yourself or organizations.",
                ));
            }
            _ => {
                into_json(bgh_repos::create::create_for_user(st(ctx), user(a), Json(b)).await)
                    .await?
            }
        };
        let id = v["id"]
            .as_i64()
            .ok_or_else(|| not_found("repository not created"))?;
        Ok(CreateRepositoryPayload {
            repository: Some(reload_repo(ctx, id).await?),
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Update information about a repository.
    pub async fn update_repository(
        &self,
        ctx: &Context<'_>,
        input: UpdateRepositoryInput,
    ) -> GResult<UpdateRepositoryPayload> {
        let repo = repo_by_node(ctx, &input.repository_id).await?;
        let mut p = json!({});
        if let Some(v) = input.name {
            p["name"] = json!(v);
        }
        if let Some(v) = input.description {
            p["description"] = json!(v);
        }
        if let Some(v) = input.template {
            p["is_template"] = json!(v);
        }
        if let Some(v) = input.homepage_url {
            p["homepage"] = json!(v.0);
        }
        if let Some(v) = input.has_wiki_enabled {
            p["has_wiki"] = json!(v);
        }
        if let Some(v) = input.has_issues_enabled {
            p["has_issues"] = json!(v);
        }
        if let Some(v) = input.has_projects_enabled {
            p["has_projects"] = json!(v);
        }
        if let Some(v) = input.has_discussions_enabled {
            p["has_discussions"] = json!(v);
        }
        Ok(UpdateRepositoryPayload {
            repository: Some(patch_repo(ctx, repo.repo.id, p).await?),
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Create a new repository with the same files and directory structure
    /// as a template repository.
    pub async fn clone_template_repository(
        &self,
        ctx: &Context<'_>,
        input: CloneTemplateRepositoryInput,
    ) -> GResult<CloneTemplateRepositoryPayload> {
        let a = guard(ctx)?;
        let template = repo_by_node(ctx, &input.repository_id).await?;
        let owner = owner_login(ctx, &input.owner_id).await?;
        let (o, r) = owner_repo(&template);
        let v = into_json(
            bgh_repos::forks::generate(
                st(ctx),
                user(a),
                Path((o, r)),
                Json(body(json!({
                    "owner": owner.login,
                    "name": input.name,
                    "description": input.description,
                    "include_all_branches": input.include_all_branches,
                    "private": input.visibility != RepositoryVisibility::Public,
                }))?),
            )
            .await,
        )
        .await?;
        let id = v["id"]
            .as_i64()
            .ok_or_else(|| not_found("repository not created"))?;
        Ok(CloneTemplateRepositoryPayload {
            repository: Some(reload_repo(ctx, id).await?),
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Marks a repository as archived.
    pub async fn archive_repository(
        &self,
        ctx: &Context<'_>,
        input: ArchiveRepositoryInput,
    ) -> GResult<ArchiveRepositoryPayload> {
        let repo = repo_by_node(ctx, &input.repository_id).await?;
        Ok(ArchiveRepositoryPayload {
            repository: Some(patch_repo(ctx, repo.repo.id, json!({"archived": true})).await?),
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Unarchives a repository.
    pub async fn unarchive_repository(
        &self,
        ctx: &Context<'_>,
        input: UnarchiveRepositoryInput,
    ) -> GResult<UnarchiveRepositoryPayload> {
        let repo = repo_by_node(ctx, &input.repository_id).await?;
        Ok(UnarchiveRepositoryPayload {
            repository: Some(patch_repo(ctx, repo.repo.id, json!({"archived": false})).await?),
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Adds a star to a Starrable.
    pub async fn add_star(
        &self,
        ctx: &Context<'_>,
        input: AddStarInput,
    ) -> GResult<AddStarPayload> {
        let a = guard(ctx)?;
        let repo = repo_by_node(ctx, &input.starrable_id).await?;
        let (o, r) = owner_repo(&repo);
        into_json(bgh_repos::stars::star(st(ctx), user(a), Path((o, r))).await).await?;
        Ok(AddStarPayload {
            starrable: Some(reload_repo(ctx, repo.repo.id).await?),
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Removes a star from a Starrable.
    pub async fn remove_star(
        &self,
        ctx: &Context<'_>,
        input: RemoveStarInput,
    ) -> GResult<RemoveStarPayload> {
        let a = guard(ctx)?;
        let repo = repo_by_node(ctx, &input.starrable_id).await?;
        let (o, r) = owner_repo(&repo);
        into_json(bgh_repos::stars::unstar(st(ctx), user(a), Path((o, r))).await).await?;
        Ok(RemoveStarPayload {
            starrable: Some(reload_repo(ctx, repo.repo.id).await?),
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Create a new Git Ref.
    pub async fn create_ref(
        &self,
        ctx: &Context<'_>,
        input: CreateRefInput,
    ) -> GResult<CreateRefPayload> {
        let a = guard(ctx)?;
        let repo = repo_by_node(ctx, &input.repository_id).await?;
        let (o, r) = owner_repo(&repo);
        into_json(
            bgh_repos::gitdb::create_ref(
                st(ctx),
                user(a),
                Path((o, r)),
                Json(body(json!({"ref": input.name, "sha": input.oid.0}))?),
            )
            .await,
        )
        .await?;
        let repo = Repository(repo);
        Ok(CreateRefPayload {
            ref_: Ref::load(ctx, repo, &input.name).await?,
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Update a Git Ref.
    pub async fn update_ref(
        &self,
        ctx: &Context<'_>,
        input: UpdateRefInput,
    ) -> GResult<UpdateRefPayload> {
        let a = guard(ctx)?;
        let (repo, name) = ref_by_node(ctx, &input.ref_id).await?;
        let (o, r) = owner_repo(repo.row());
        let short = name.strip_prefix("refs/").unwrap_or(&name).to_string();
        into_json(
            bgh_repos::gitdb::update_ref(
                st(ctx),
                user(a),
                Path((o, r, short)),
                Json(body(json!({"sha": input.oid.0, "force": input.force}))?),
            )
            .await,
        )
        .await?;
        Ok(UpdateRefPayload {
            ref_: Ref::load(ctx, repo, &name).await?,
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Delete a Git Ref.
    pub async fn delete_ref(
        &self,
        ctx: &Context<'_>,
        input: DeleteRefInput,
    ) -> GResult<DeleteRefPayload> {
        let a = guard(ctx)?;
        let (repo, name) = ref_by_node(ctx, &input.ref_id).await?;
        let (o, r) = owner_repo(repo.row());
        let short = name.strip_prefix("refs/").unwrap_or(&name).to_string();
        into_json(bgh_repos::gitdb::delete_ref(st(ctx), user(a), Path((o, r, short))).await)
            .await?;
        Ok(DeleteRefPayload {
            client_mutation_id: input.client_mutation_id,
        })
    }
}
