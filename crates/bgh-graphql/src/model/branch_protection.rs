//! Classic branch protection rules (`Repository.branchProtectionRules`,
//! `Ref.branchProtectionRule`, `node()`), with every field the Terraform
//! provider's `github_branch_protection` reads. Rows come from
//! `branch_protections` (`bgh_repos::protection::ProtectionRow`); like the
//! REST protection endpoints, only repository admins see them.

use std::sync::Arc;

use async_graphql::{Context, ID, Object, SimpleObject, Union};
use bgh_core::node_id::NodeType;
use bgh_core::perms::Permission;
use bgh_repos::protection::{ProtectionRow, pattern_matches};
use serde_json::Value;

use super::actor::{Team, User};
use super::git::App;
use super::nid;
use super::repo::{self, Repository};
use crate::conn::{ConnArgs, Page, connection};
use crate::ctx::{GResult, OrGql, gql};
use crate::loaders::{Loaders, one};

/// A branch protection rule.
#[derive(Clone)]
pub struct BranchProtectionRule {
    pub row: Arc<ProtectionRow>,
    pub repo: Repository,
}

connection!(
    BranchProtectionRuleConnection,
    BranchProtectionRuleEdge,
    BranchProtectionRule
);

/// Whether the viewer may see `repo`'s protection rules (the REST rule).
fn visible(repo: &Repository) -> bool {
    repo.row().perm >= Permission::Admin
}

async fn rows(ctx: &Context<'_>, repo_id: i64) -> GResult<Vec<ProtectionRow>> {
    sqlx::query_as(&format!(
        "SELECT {} FROM branch_protections WHERE repo_id = $1 ORDER BY id",
        ProtectionRow::COLUMNS
    ))
    .bind(repo_id)
    .fetch_all(&gql(ctx).state.db)
    .await
    .gql()
}

fn wrap(repo: &Repository, row: ProtectionRow) -> BranchProtectionRule {
    BranchProtectionRule {
        row: Arc::new(row),
        repo: repo.clone(),
    }
}

/// `Repository.branchProtectionRules`.
pub async fn list(
    ctx: &Context<'_>,
    repo: &Repository,
    args: ConnArgs,
) -> GResult<BranchProtectionRuleConnection> {
    if !visible(repo) {
        return Ok(BranchProtectionRuleConnection::empty());
    }
    let all = rows(ctx, repo.rid())
        .await?
        .into_iter()
        .map(|r| wrap(repo, r))
        .collect();
    Ok(Page::from_vec(all, &args)?.into())
}

/// `Ref.branchProtectionRule`: the rule protecting branch `name` (an exact
/// pattern beats globs, longer globs beat shorter ones).
pub async fn for_branch(
    ctx: &Context<'_>,
    repo: &Repository,
    name: &str,
) -> GResult<Option<BranchProtectionRule>> {
    if !visible(repo) {
        return Ok(None);
    }
    let rows = rows(ctx, repo.rid()).await?;
    let best = match rows.iter().position(|p| p.pattern == name) {
        Some(i) => Some(i),
        None => rows
            .iter()
            .enumerate()
            .filter(|(_, p)| pattern_matches(&p.pattern, name))
            .max_by_key(|(_, p)| (p.pattern.len(), std::cmp::Reverse(p.id)))
            .map(|(i, _)| i),
    };
    Ok(best.map(|i| wrap(repo, rows.into_iter().nth(i).expect("index in range"))))
}

/// `node()` / mutation lookup of a rule by database id.
pub async fn by_id(ctx: &Context<'_>, id: i64) -> GResult<Option<BranchProtectionRule>> {
    let row: Option<ProtectionRow> = sqlx::query_as(&format!(
        "SELECT {} FROM branch_protections WHERE id = $1",
        ProtectionRow::COLUMNS
    ))
    .bind(id)
    .fetch_optional(&gql(ctx).state.db)
    .await
    .gql()?;
    let Some(row) = row else { return Ok(None) };
    let Some(repo) = repo::load(ctx, row.repo_id).await? else {
        return Ok(None);
    };
    Ok(visible(&repo).then(|| wrap(&repo, row)))
}

fn ids(v: Option<&Value>, key: &str) -> Vec<i64> {
    v.and_then(|v| v[key].as_array())
        .into_iter()
        .flatten()
        .filter_map(Value::as_i64)
        .collect()
}

/// An actor of an allowance list.
#[derive(Clone)]
pub enum AllowanceActor {
    User(User),
    Team(Team),
}

/// The users and teams of a stored people setting, in that order.
async fn actors(ctx: &Context<'_>, v: Option<&Value>) -> GResult<Vec<AllowanceActor>> {
    let l = ctx.data_unchecked::<Loaders>();
    let mut out = Vec::new();
    for id in ids(v, "users") {
        if let Some(u) = one(&l.users, id).await? {
            out.push(AllowanceActor::User(User(u)));
        }
    }
    for id in ids(v, "teams") {
        if let Some(t) = one(&l.teams, id).await? {
            out.push(AllowanceActor::Team(Team(t)));
        }
    }
    Ok(out)
}

/// One allowance type per GitHub union: `$actor` is the union of the
/// actors it may name.
macro_rules! allowance {
    ($ty:ident, $actor:ident, $conn:ident, $edge:ident, $kind:literal, $doc:literal) => {
        #[doc = $doc]
        #[derive(Union, Clone)]
        pub enum $actor {
            App(App),
            Team(Team),
            User(User),
        }

        #[doc = $doc]
        #[derive(Clone)]
        pub struct $ty {
            rule: BranchProtectionRule,
            actor: AllowanceActor,
        }

        #[Object]
        impl $ty {
            pub async fn id(&self) -> ID {
                let (k, n) = match &self.actor {
                    AllowanceActor::User(u) => ("User", u.0.id),
                    AllowanceActor::Team(t) => ("Team", t.0.team.id),
                };
                ID(bgh_core::node_id::encode_str(
                    NodeType::BranchProtectionRule,
                    &format!("{}:{}:{k}{n}", self.rule.row.id, $kind),
                ))
            }
            pub async fn actor(&self) -> Option<$actor> {
                Some(match &self.actor {
                    AllowanceActor::User(u) => $actor::User(u.clone()),
                    AllowanceActor::Team(t) => $actor::Team(t.clone()),
                })
            }
            pub async fn branch_protection_rule(&self) -> Option<BranchProtectionRule> {
                Some(self.rule.clone())
            }
        }

        connection!($conn, $edge, $ty);

        impl $ty {
            async fn connection(
                ctx: &Context<'_>,
                rule: &BranchProtectionRule,
                v: Option<&Value>,
                args: ConnArgs,
            ) -> GResult<$conn> {
                let all = actors(ctx, v)
                    .await?
                    .into_iter()
                    .map(|actor| $ty {
                        rule: rule.clone(),
                        actor,
                    })
                    .collect();
                Ok(Page::from_vec(all, &args)?.into())
            }
        }
    };
}

allowance!(
    PushAllowance,
    PushAllowanceActor,
    PushAllowanceConnection,
    PushAllowanceEdge,
    "push",
    "A team, user, or app who can push to a protected branch."
);
allowance!(
    ReviewDismissalAllowance,
    ReviewDismissalAllowanceActor,
    ReviewDismissalAllowanceConnection,
    ReviewDismissalAllowanceEdge,
    "dismiss",
    "A user, team, or app who can dismiss reviews on a protected branch."
);
allowance!(
    BypassPullRequestAllowance,
    BranchActorAllowanceActor,
    BypassPullRequestAllowanceConnection,
    BypassPullRequestAllowanceEdge,
    "bypass_pr",
    "A user, team, or app who can bypass pull request requirements."
);

/// A user, team, or app who can bypass force-push requirements. Classic
/// rules here have no such list, so these connections are always empty.
#[derive(Clone)]
pub struct BypassForcePushAllowance {
    actor: BranchActorAllowanceActor,
}

#[Object]
impl BypassForcePushAllowance {
    pub async fn actor(&self) -> Option<BranchActorAllowanceActor> {
        Some(self.actor.clone())
    }
}

connection!(
    BypassForcePushAllowanceConnection,
    BypassForcePushAllowanceEdge,
    BypassForcePushAllowance
);

/// A required status check context.
#[derive(SimpleObject, Clone)]
pub struct RequiredStatusCheckDescription {
    pub context: Option<String>,
}

impl BranchProtectionRule {
    fn reviews(&self) -> Option<&Value> {
        self.row.required_pull_request_reviews.as_ref()
    }
    fn review_flag(&self, key: &str) -> bool {
        self.reviews()
            .and_then(|v| v[key].as_bool())
            .unwrap_or(false)
    }
    fn review_people(&self, key: &str) -> Option<&Value> {
        self.reviews()
            .and_then(|v| v.get(key))
            .filter(|v| !v.is_null())
    }
}

#[Object]
impl BranchProtectionRule {
    pub async fn id(&self) -> ID {
        nid(NodeType::BranchProtectionRule, self.row.id)
    }
    pub async fn database_id(&self) -> Option<i64> {
        Some(self.row.id)
    }
    pub async fn pattern(&self) -> &str {
        &self.row.pattern
    }
    pub async fn repository(&self) -> Option<Repository> {
        Some(self.repo.clone())
    }
    pub async fn allows_deletions(&self) -> bool {
        self.row.allow_deletions
    }
    pub async fn allows_force_pushes(&self) -> bool {
        self.row.allow_force_pushes
    }
    pub async fn blocks_creations(&self) -> bool {
        self.row.block_creations
    }
    pub async fn dismisses_stale_reviews(&self) -> bool {
        self.review_flag("dismiss_stale_reviews")
    }
    pub async fn is_admin_enforced(&self) -> bool {
        self.row.enforce_admins
    }
    pub async fn lock_allows_fetch_and_merge(&self) -> bool {
        self.row.allow_fork_syncing
    }
    pub async fn lock_branch(&self) -> bool {
        self.row.lock_branch
    }
    pub async fn require_last_push_approval(&self) -> bool {
        self.review_flag("require_last_push_approval")
    }
    pub async fn required_approving_review_count(&self) -> Option<i32> {
        self.reviews()
            .and_then(|v| v["required_approving_review_count"].as_i64())
            .map(|n| n as i32)
    }
    pub async fn required_deployment_environments(&self) -> Option<Vec<String>> {
        Some(self.row.required_deployment_environments.clone())
    }
    pub async fn required_status_check_contexts(&self) -> Option<Vec<String>> {
        Some(self.row.required_contexts())
    }
    pub async fn required_status_checks(&self) -> Option<Vec<RequiredStatusCheckDescription>> {
        Some(
            self.row
                .required_contexts()
                .into_iter()
                .map(|c| RequiredStatusCheckDescription { context: Some(c) })
                .collect(),
        )
    }
    pub async fn requires_approving_reviews(&self) -> bool {
        self.reviews().is_some()
    }
    pub async fn requires_code_owner_reviews(&self) -> bool {
        self.review_flag("require_code_owner_reviews")
    }
    pub async fn requires_commit_signatures(&self) -> bool {
        self.row.required_signatures
    }
    pub async fn requires_conversation_resolution(&self) -> bool {
        self.row.required_conversation_resolution
    }
    pub async fn requires_deployments(&self) -> bool {
        !self.row.required_deployment_environments.is_empty()
    }
    pub async fn requires_linear_history(&self) -> bool {
        self.row.required_linear_history
    }
    pub async fn requires_status_checks(&self) -> bool {
        self.row.required_status_checks.is_some()
    }
    pub async fn requires_strict_status_checks(&self) -> bool {
        self.row
            .required_status_checks
            .as_ref()
            .and_then(|v| v["strict"].as_bool())
            .unwrap_or(false)
    }
    pub async fn restricts_pushes(&self) -> bool {
        self.row.restrictions.is_some()
    }
    pub async fn restricts_review_dismissals(&self) -> bool {
        self.review_people("dismissal_restrictions").is_some()
    }
    pub async fn push_allowances(
        &self,
        ctx: &Context<'_>,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
    ) -> GResult<PushAllowanceConnection> {
        let args = ConnArgs::new(first, last, after, before);
        PushAllowance::connection(ctx, self, self.row.restrictions.as_ref(), args).await
    }
    pub async fn review_dismissal_allowances(
        &self,
        ctx: &Context<'_>,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
    ) -> GResult<ReviewDismissalAllowanceConnection> {
        let args = ConnArgs::new(first, last, after, before);
        let v = self.review_people("dismissal_restrictions");
        ReviewDismissalAllowance::connection(ctx, self, v, args).await
    }
    pub async fn bypass_pull_request_allowances(
        &self,
        ctx: &Context<'_>,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
    ) -> GResult<BypassPullRequestAllowanceConnection> {
        let args = ConnArgs::new(first, last, after, before);
        let v = self.review_people("bypass_pull_request_allowances");
        BypassPullRequestAllowance::connection(ctx, self, v, args).await
    }
    #[allow(unused_variables)]
    pub async fn bypass_force_push_allowances(
        &self,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
    ) -> BypassForcePushAllowanceConnection {
        BypassForcePushAllowanceConnection::empty()
    }
}
