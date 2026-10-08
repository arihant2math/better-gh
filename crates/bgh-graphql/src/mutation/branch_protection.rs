//! `create/update/deleteBranchProtectionRule`, backed by the classic
//! protection storage (`bgh_repos::protection_api` pattern rules) and
//! guarded by the REST rule: repository admins only.

use std::sync::Arc;

use async_graphql::{Context, ID, InputObject, Object, SimpleObject};
use bgh_core::node_id::{self, NodeType};
use bgh_core::perms::RepoAccess;
use bgh_repos::protection::ProtectionRow;
use bgh_repos::protection_api::{self as api, RuleActors, RuleInput};

use super::{decode, guard, repo_by_id, repo_by_node};
use crate::ctx::{GResult, OrGql, err, gql, not_found};
use crate::loaders::RepoRow;
use crate::model::Repository;
use crate::model::branch_protection::BranchProtectionRule;

/// Specifies the attributes for a new or updated required status check.
#[derive(InputObject)]
pub struct RequiredStatusCheckInput {
    pub context: String,
    pub app_id: Option<ID>,
}

/// The rule settings shared by the create and update inputs.
macro_rules! rule_input {
    ($name:ident, $doc:literal, { $($head:tt)* }) => {
        #[doc = $doc]
        #[derive(InputObject)]
        pub struct $name {
            $($head)*
            pub requires_approving_reviews: Option<bool>,
            pub required_approving_review_count: Option<i32>,
            pub requires_commit_signatures: Option<bool>,
            pub requires_linear_history: Option<bool>,
            pub blocks_creations: Option<bool>,
            pub allows_force_pushes: Option<bool>,
            pub allows_deletions: Option<bool>,
            pub is_admin_enforced: Option<bool>,
            pub requires_status_checks: Option<bool>,
            pub requires_strict_status_checks: Option<bool>,
            pub requires_code_owner_reviews: Option<bool>,
            pub dismisses_stale_reviews: Option<bool>,
            pub restricts_review_dismissals: Option<bool>,
            pub review_dismissal_actor_ids: Option<Vec<ID>>,
            pub bypass_pull_request_actor_ids: Option<Vec<ID>>,
            pub bypass_force_push_actor_ids: Option<Vec<ID>>,
            pub restricts_pushes: Option<bool>,
            pub push_actor_ids: Option<Vec<ID>>,
            pub required_status_check_contexts: Option<Vec<String>>,
            pub required_status_checks: Option<Vec<RequiredStatusCheckInput>>,
            pub requires_deployments: Option<bool>,
            pub required_deployment_environments: Option<Vec<String>>,
            pub requires_conversation_resolution: Option<bool>,
            pub require_last_push_approval: Option<bool>,
            pub lock_branch: Option<bool>,
            pub lock_allows_fetch_and_merge: Option<bool>,
            pub client_mutation_id: Option<String>,
        }

        impl $name {
            fn rule_input(&self, pattern: Option<String>) -> GResult<RuleInput> {
                if self.bypass_force_push_actor_ids.as_ref().is_some_and(|v| !v.is_empty()) {
                    return Err(err(
                        "UNPROCESSABLE",
                        "bypassForcePushActorIds is not supported by this server.",
                    ));
                }
                let checks = match (&self.required_status_checks, &self.required_status_check_contexts) {
                    (Some(c), _) => Some(
                        c.iter()
                            .map(|c| Ok((c.context.clone(), app_id(c.app_id.as_ref())?)))
                            .collect::<GResult<Vec<_>>>()?,
                    ),
                    (None, Some(c)) => Some(c.iter().map(|c| (c.clone(), None)).collect()),
                    (None, None) => None,
                };
                Ok(RuleInput {
                    pattern,
                    requires_approving_reviews: self.requires_approving_reviews,
                    required_approving_review_count: self.required_approving_review_count.map(i64::from),
                    dismisses_stale_reviews: self.dismisses_stale_reviews,
                    requires_code_owner_reviews: self.requires_code_owner_reviews,
                    require_last_push_approval: self.require_last_push_approval,
                    restricts_review_dismissals: self.restricts_review_dismissals,
                    review_dismissal_actors: actors(self.review_dismissal_actor_ids.as_deref())?,
                    bypass_pull_request_actors: actors(self.bypass_pull_request_actor_ids.as_deref())?,
                    requires_status_checks: self.requires_status_checks,
                    requires_strict_status_checks: self.requires_strict_status_checks,
                    required_status_checks: checks,
                    restricts_pushes: self.restricts_pushes,
                    push_actors: actors(self.push_actor_ids.as_deref())?,
                    is_admin_enforced: self.is_admin_enforced,
                    requires_commit_signatures: self.requires_commit_signatures,
                    requires_linear_history: self.requires_linear_history,
                    requires_conversation_resolution: self.requires_conversation_resolution,
                    allows_force_pushes: self.allows_force_pushes,
                    allows_deletions: self.allows_deletions,
                    blocks_creations: self.blocks_creations,
                    lock_branch: self.lock_branch,
                    lock_allows_fetch_and_merge: self.lock_allows_fetch_and_merge,
                    requires_deployments: self.requires_deployments,
                    required_deployment_environments: self.required_deployment_environments.clone(),
                })
            }
        }
    };
}

rule_input!(
    CreateBranchProtectionRuleInput,
    "Autogenerated input type of CreateBranchProtectionRule",
    {
        pub repository_id: ID,
        pub pattern: String,
    }
);

rule_input!(
    UpdateBranchProtectionRuleInput,
    "Autogenerated input type of UpdateBranchProtectionRule",
    {
        pub branch_protection_rule_id: ID,
        pub pattern: Option<String>,
    }
);

#[derive(InputObject)]
pub struct DeleteBranchProtectionRuleInput {
    pub branch_protection_rule_id: ID,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
pub struct CreateBranchProtectionRulePayload {
    pub branch_protection_rule: Option<BranchProtectionRule>,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
pub struct UpdateBranchProtectionRulePayload {
    pub branch_protection_rule: Option<BranchProtectionRule>,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
pub struct DeleteBranchProtectionRulePayload {
    pub client_mutation_id: Option<String>,
}

/// Users and teams named by global ids (apps are not supported).
fn actors(ids: Option<&[ID]>) -> GResult<Option<RuleActors>> {
    let Some(ids) = ids else { return Ok(None) };
    let mut out = RuleActors::default();
    for id in ids {
        match node_id::decode(&id.0) {
            Some((NodeType::User, n)) => out.users.push(n),
            Some((NodeType::Team, n)) => out.teams.push(n),
            _ => {
                return Err(not_found(format!(
                    "Could not resolve to a User or Team with the global id of '{}'.",
                    id.0
                )));
            }
        }
    }
    Ok(Some(out))
}

fn app_id(id: Option<&ID>) -> GResult<Option<i64>> {
    id.map(|id| decode(id, &[NodeType::Integration], "an App"))
        .transpose()
}

async fn access(ctx: &Context<'_>, repo: &RepoRow) -> GResult<RepoAccess> {
    let g = gql(ctx);
    RepoAccess::for_repo(
        &g.state,
        g.auth.as_ref(),
        repo.repo.clone(),
        repo.owner.clone(),
    )
    .await
    .gql()
}

/// The rule and its repository; NOT_FOUND unless the caller can read it.
async fn rule_by_node(ctx: &Context<'_>, id: &ID) -> GResult<(i64, Arc<RepoRow>)> {
    let n = decode(
        id,
        &[NodeType::BranchProtectionRule],
        "a BranchProtectionRule",
    )?;
    let repo_id: Option<(i64,)> =
        sqlx::query_as("SELECT repo_id FROM branch_protections WHERE id = $1")
            .bind(n)
            .fetch_optional(&gql(ctx).state.db)
            .await
            .gql()?;
    let missing = || {
        not_found(format!(
            "Could not resolve to a node with the global id of '{}'.",
            id.0
        ))
    };
    let (repo_id,) = repo_id.ok_or_else(missing)?;
    let repo = repo_by_id(ctx, repo_id).await.map_err(|_| missing())?;
    Ok((n, repo))
}

fn rule(repo: Arc<RepoRow>, row: ProtectionRow) -> BranchProtectionRule {
    BranchProtectionRule {
        row: Arc::new(row),
        repo: Repository(repo),
    }
}

#[derive(Default)]
pub struct BranchProtectionMutations;

#[Object]
impl BranchProtectionMutations {
    /// Create a new branch protection rule
    pub async fn create_branch_protection_rule(
        &self,
        ctx: &Context<'_>,
        input: CreateBranchProtectionRuleInput,
    ) -> GResult<CreateBranchProtectionRulePayload> {
        let a = guard(ctx)?;
        let repo = repo_by_node(ctx, &input.repository_id).await?;
        let access = access(ctx, &repo).await?;
        let rule_input = input.rule_input(Some(input.pattern.clone()))?;
        let row = api::create_rule(&gql(ctx).state, &access, &a.user, &rule_input)
            .await
            .gql()?;
        Ok(CreateBranchProtectionRulePayload {
            branch_protection_rule: Some(rule(repo, row)),
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Update a branch protection rule
    pub async fn update_branch_protection_rule(
        &self,
        ctx: &Context<'_>,
        input: UpdateBranchProtectionRuleInput,
    ) -> GResult<UpdateBranchProtectionRulePayload> {
        let a = guard(ctx)?;
        let (id, repo) = rule_by_node(ctx, &input.branch_protection_rule_id).await?;
        let access = access(ctx, &repo).await?;
        let rule_input = input.rule_input(input.pattern.clone())?;
        let row = api::update_rule(&gql(ctx).state, &access, &a.user, id, &rule_input)
            .await
            .gql()?;
        Ok(UpdateBranchProtectionRulePayload {
            branch_protection_rule: Some(rule(repo, row)),
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Delete a branch protection rule
    pub async fn delete_branch_protection_rule(
        &self,
        ctx: &Context<'_>,
        input: DeleteBranchProtectionRuleInput,
    ) -> GResult<DeleteBranchProtectionRulePayload> {
        let a = guard(ctx)?;
        let (id, repo) = rule_by_node(ctx, &input.branch_protection_rule_id).await?;
        let access = access(ctx, &repo).await?;
        api::delete_rule(&gql(ctx).state, &access, &a.user, id)
            .await
            .gql()?;
        Ok(DeleteBranchProtectionRulePayload {
            client_mutation_id: input.client_mutation_id,
        })
    }
}
