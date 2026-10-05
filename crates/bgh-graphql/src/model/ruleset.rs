//! Repository rulesets (`Repository.rulesets`, `Organization.rulesets`),
//! enough for `gh ruleset list` and rule counts. Data comes from
//! `bgh_repos::protection::RulesetRow`.

use std::sync::Arc;

use async_graphql::{Context, Enum, ID, Object, Union};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use bgh_repos::protection::RulesetRow;

use super::actor::Organization;
use super::repo::Repository;
use crate::conn::{ConnArgs, Page, connection};
use crate::ctx::{GResult, OrGql, err, gql};
use crate::scalars::{DateTime, dt};

/// The targets supported for rulesets.
#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug)]
pub enum RepositoryRulesetTarget {
    Branch,
    Tag,
    Push,
    Repository,
}

impl RepositoryRulesetTarget {
    fn as_str(self) -> &'static str {
        match self {
            Self::Branch => "branch",
            Self::Tag => "tag",
            Self::Push => "push",
            Self::Repository => "repository",
        }
    }
}

/// The level of enforcement for a rule or ruleset.
#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug)]
pub enum RuleEnforcement {
    Active,
    Disabled,
    Evaluate,
}

/// The rule types supported in rulesets.
#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug)]
pub enum RepositoryRuleType {
    Creation,
    Update,
    Deletion,
    RequiredLinearHistory,
    MergeQueue,
    RequiredDeployments,
    RequiredSignatures,
    PullRequest,
    RequiredStatusChecks,
    NonFastForward,
    CommitMessagePattern,
    CommitAuthorEmailPattern,
    CommitterEmailPattern,
    BranchNamePattern,
    TagNamePattern,
    FilePathRestriction,
    MaxFilePathLength,
    FileExtensionRestriction,
    MaxFileSize,
    Workflows,
    CodeScanning,
}

impl RepositoryRuleType {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "creation" => Self::Creation,
            "update" => Self::Update,
            "deletion" => Self::Deletion,
            "required_linear_history" => Self::RequiredLinearHistory,
            "merge_queue" => Self::MergeQueue,
            "required_deployments" => Self::RequiredDeployments,
            "required_signatures" => Self::RequiredSignatures,
            "pull_request" => Self::PullRequest,
            "required_status_checks" => Self::RequiredStatusChecks,
            "non_fast_forward" => Self::NonFastForward,
            "commit_message_pattern" => Self::CommitMessagePattern,
            "commit_author_email_pattern" => Self::CommitAuthorEmailPattern,
            "committer_email_pattern" => Self::CommitterEmailPattern,
            "branch_name_pattern" => Self::BranchNamePattern,
            "tag_name_pattern" => Self::TagNamePattern,
            "file_path_restriction" => Self::FilePathRestriction,
            "max_file_path_length" => Self::MaxFilePathLength,
            "file_extension_restriction" => Self::FileExtensionRestriction,
            "max_file_size" => Self::MaxFileSize,
            "workflows" => Self::Workflows,
            "code_scanning" => Self::CodeScanning,
            _ => return None,
        })
    }
}

/// Where a ruleset is defined.
#[derive(Union, Clone)]
pub enum RuleSource {
    Repository(Repository),
    Organization(Organization),
}

/// A repository rule.
#[derive(Clone)]
pub struct RepositoryRule {
    ruleset_id: i64,
    index: usize,
    ty: RepositoryRuleType,
}

#[Object]
impl RepositoryRule {
    pub async fn id(&self) -> ID {
        ID(format!(
            "RR_{}",
            URL_SAFE_NO_PAD.encode(format!("RepositoryRule:{}:{}", self.ruleset_id, self.index))
        ))
    }
    #[graphql(name = "type")]
    pub async fn rule_type(&self) -> RepositoryRuleType {
        self.ty
    }
}

connection!(RepositoryRuleConnection, RepositoryRuleEdge, RepositoryRule);

/// A repository ruleset.
#[derive(Clone)]
pub struct RepositoryRuleset {
    row: Arc<RulesetRow>,
    source: RuleSource,
}

#[Object]
impl RepositoryRuleset {
    pub async fn id(&self) -> ID {
        ID(format!(
            "RRS_{}",
            URL_SAFE_NO_PAD.encode(format!(
                "Ruleset:{}:{}",
                self.row.org_id.unwrap_or(self.row.repo_id),
                self.row.id
            ))
        ))
    }
    pub async fn database_id(&self) -> Option<i64> {
        Some(self.row.id)
    }
    pub async fn name(&self) -> String {
        self.row.name.clone()
    }
    pub async fn target(&self) -> Option<RepositoryRulesetTarget> {
        Some(match self.row.target.as_str() {
            "tag" => RepositoryRulesetTarget::Tag,
            "push" => RepositoryRulesetTarget::Push,
            _ => RepositoryRulesetTarget::Branch,
        })
    }
    pub async fn enforcement(&self) -> RuleEnforcement {
        match self.row.enforcement.as_str() {
            "active" => RuleEnforcement::Active,
            "evaluate" => RuleEnforcement::Evaluate,
            _ => RuleEnforcement::Disabled,
        }
    }
    pub async fn source(&self) -> RuleSource {
        self.source.clone()
    }
    pub async fn rules(
        &self,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
        #[graphql(name = "type")] rule_type: Option<RepositoryRuleType>,
    ) -> GResult<RepositoryRuleConnection> {
        let rules: Vec<RepositoryRule> = self
            .row
            .rules
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
            .filter_map(|(index, r)| {
                let ty = RepositoryRuleType::parse(r["type"].as_str()?)?;
                Some(RepositoryRule {
                    ruleset_id: self.row.id,
                    index,
                    ty,
                })
            })
            .filter(|r| rule_type.is_none_or(|t| t == r.ty))
            .collect();
        Ok(Page::from_vec(rules, &ConnArgs::new(first, last, after, before))?.into())
    }
    pub async fn created_at(&self) -> DateTime {
        dt(self.row.created_at)
    }
    pub async fn updated_at(&self) -> DateTime {
        dt(self.row.updated_at)
    }
}

connection!(
    RepositoryRulesetConnection,
    RepositoryRulesetEdge,
    RepositoryRuleset
);

fn wanted(targets: &Option<Vec<RepositoryRulesetTarget>>, r: &RulesetRow) -> bool {
    targets
        .as_ref()
        .is_none_or(|t| t.iter().any(|t| t.as_str() == r.target))
}

/// `Repository.rulesets`: the repository's rulesets plus, with
/// `includeParents` (default), the organization rulesets selecting it.
pub async fn repo_rulesets(
    ctx: &Context<'_>,
    repo: &Repository,
    args: ConnArgs,
    include_parents: bool,
    targets: Option<Vec<RepositoryRulesetTarget>>,
) -> GResult<RepositoryRulesetConnection> {
    let row = repo.row();
    let parents = include_parents && row.owner.is_org();
    let rows: Vec<RulesetRow> = sqlx::query_as(&format!(
        "SELECT {} FROM repo_rulesets
          WHERE repo_id = $1 OR ($3 AND org_id = $2)
          ORDER BY org_id NULLS FIRST, id",
        RulesetRow::COLUMNS
    ))
    .bind(row.repo.id)
    .bind(row.owner.id)
    .bind(parents)
    .fetch_all(&gql(ctx).state.db)
    .await
    .gql()?;
    let org = Organization(Arc::new(row.owner.clone()));
    let items = rows
        .into_iter()
        .filter(|r| r.applies_to_repo(&row.repo) && wanted(&targets, r))
        .map(|r| RepositoryRuleset {
            source: if r.org_id.is_some() {
                RuleSource::Organization(org.clone())
            } else {
                RuleSource::Repository(repo.clone())
            },
            row: Arc::new(r),
        })
        .collect();
    Ok(Page::from_vec(items, &args)?.into())
}

/// `Organization.rulesets` (organization owners only).
pub async fn org_rulesets(
    ctx: &Context<'_>,
    org: &Organization,
    args: ConnArgs,
    targets: Option<Vec<RepositoryRulesetTarget>>,
) -> GResult<RepositoryRulesetConnection> {
    let g = gql(ctx);
    let auth = g.require_auth()?;
    let admin = auth.user.site_admin
        || bgh_core::perms::org_role(&g.state.db, org.0.id, auth.user.id)
            .await
            .gql()?
            .as_deref()
            == Some("admin");
    if !admin || auth.require_scope("admin:org").is_err() {
        return Err(err(
            "INSUFFICIENT_SCOPES",
            "Your token has not been granted the required scopes to execute this query. \
             The 'rulesets' field requires one of the following scopes: ['admin:org']",
        ));
    }
    let rows: Vec<RulesetRow> = sqlx::query_as(&format!(
        "SELECT {} FROM repo_rulesets WHERE org_id = $1 ORDER BY id",
        RulesetRow::COLUMNS
    ))
    .bind(org.0.id)
    .fetch_all(&g.state.db)
    .await
    .gql()?;
    let items = rows
        .into_iter()
        .filter(|r| wanted(&targets, r))
        .map(|r| RepositoryRuleset {
            row: Arc::new(r),
            source: RuleSource::Organization(org.clone()),
        })
        .collect();
    Ok(Page::from_vec(items, &args)?.into())
}
