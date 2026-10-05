//! Repositories and refs.

use std::sync::Arc;

use async_graphql::{Context, ID, Object, SimpleObject};
use bgh_core::node_id::{self, NodeType};
use bgh_core::perms::Permission;
use bgh_core::prelude::*;
use sqlx::{Postgres, QueryBuilder};

use super::actor::{CountOnly, User, UserConnection};
use super::enums::*;
use super::git::{self, GitObject};
use super::issue::{
    self, Issue, IssueConnection, Label, LabelConnection, Milestone, MilestoneConnection,
};
use super::misc::{
    CodeOfConduct, FundingLink, IssueTemplate, Language, LanguageConnection, License,
    ProjectConnection, PullRequestTemplate, RepositoryContactLink, RepositoryTopicConnection,
};
use super::pull::{self, PullRequest, PullRequestConnection};
use super::release::{self, Release, ReleaseConnection};
use super::{RepositoryOwner, nid};
use crate::conn::{ConnArgs, Page, connection, wants};
use crate::ctx::{GResult, OrGql, gql};
use crate::loaders::{Loaders, RepoLoader, RepoRow, one};
use crate::scalars::{DateTime, GitObjectID, GitSSHRemote, HTML, URI, dt, odt};

/// A repository contains the content for a project.
#[derive(Clone)]
pub struct Repository(pub Arc<RepoRow>);

impl Repository {
    pub fn row(&self) -> &RepoRow {
        &self.0
    }
    pub fn rid(&self) -> i64 {
        self.0.repo.id
    }
    fn r(&self) -> &db::Repository {
        &self.0.repo
    }
    fn owner_login(&self) -> &str {
        &self.0.owner.login
    }
    async fn extra(&self, ctx: &Context<'_>) -> GResult<Arc<crate::loaders::RepoExtra>> {
        let l = ctx.data_unchecked::<Loaders>();
        Ok(one(&l.repo_extra, self.rid()).await?.unwrap_or_default())
    }
}

/// Load a repository by id for the viewer (`None` without read access).
pub async fn load(ctx: &Context<'_>, id: i64) -> GResult<Option<Repository>> {
    let l = ctx.data_unchecked::<Loaders>();
    Ok(one(&l.repos, id)
        .await?
        .filter(|r| r.readable())
        .map(Repository))
}

/// Load a repository by id without the read check (for objects reached
/// through an already-authorized parent, e.g. an issue's repository).
pub async fn load_unchecked(ctx: &Context<'_>, id: i64) -> GResult<Repository> {
    let l = ctx.data_unchecked::<Loaders>();
    one(&l.repos, id)
        .await?
        .map(Repository)
        .ok_or_else(|| crate::ctx::not_found("Could not resolve to a Repository."))
}

pub async fn by_owner_and_name(
    ctx: &Context<'_>,
    owner: &db::User,
    name: &str,
) -> GResult<Option<Repository>> {
    let g = gql(ctx);
    let name = name.strip_suffix(".git").unwrap_or(name);
    let Some(repo) = db::Repository::find_by_name(&g.state.db, owner.id, name)
        .await
        .gql()?
    else {
        return Ok(None);
    };
    load(ctx, repo.id).await
}

/// `repository(owner:, name:)`.
pub async fn by_nwo(ctx: &Context<'_>, owner: &str, name: &str) -> GResult<Option<Repository>> {
    let g = gql(ctx);
    let Some(o) = db::User::find_by_login(&g.state.db, owner).await.gql()? else {
        return Ok(None);
    };
    by_owner_and_name(ctx, &o, name).await
}

/// Filters for owner repository lists.
#[derive(Default)]
pub struct RepoFilter {
    pub privacy: Option<RepositoryPrivacy>,
    pub is_fork: Option<bool>,
    pub is_archived: Option<bool>,
    pub owner_affiliations: Vec<RepositoryAffiliation>,
    pub order_by: Option<RepositoryOrder>,
}

/// Repositories of a user or organization visible to the viewer.
pub async fn owner_repositories(
    ctx: &Context<'_>,
    owner: &db::User,
    args: ConnArgs,
    f: RepoFilter,
) -> GResult<RepositoryConnection> {
    let g = gql(ctx);
    let w = wants(ctx);
    // Affiliations other than OWNER (collaborator / org member) widen the
    // list for the viewer's own account, like GitHub.
    let widen = g.viewer_id() == Some(owner.id)
        && !f.owner_affiliations.is_empty()
        && f.owner_affiliations
            .iter()
            .any(|a| *a != RepositoryAffiliation::Owner);
    let mut qb: QueryBuilder<Postgres> = QueryBuilder::new(format!(
        "SELECT {} FROM repositories r WHERE ",
        db::prefixed("r", db::Repository::COLUMNS)
    ));
    if widen {
        qb.push("(r.owner_id = ")
            .push_bind(owner.id)
            .push(" OR r.id IN (SELECT repo_id FROM collaborators WHERE user_id = ")
            .push_bind(owner.id)
            .push(") OR r.owner_id IN (SELECT org_id FROM org_members WHERE user_id = ")
            .push_bind(owner.id)
            .push("))");
    } else {
        qb.push("r.owner_id = ").push_bind(owner.id);
    }
    match f.privacy {
        Some(RepositoryPrivacy::Public) => {
            qb.push(" AND r.visibility = 'public'");
        }
        Some(RepositoryPrivacy::Private) => {
            qb.push(" AND r.visibility <> 'public'");
        }
        None => {}
    }
    if let Some(fork) = f.is_fork {
        qb.push(" AND r.fork = ").push_bind(fork);
    }
    if let Some(a) = f.is_archived {
        qb.push(" AND r.archived = ").push_bind(a);
    }
    let order = f.order_by.unwrap_or(RepositoryOrder {
        field: RepositoryOrderField::Name,
        direction: OrderDirection::Asc,
    });
    let col = match order.field {
        RepositoryOrderField::CreatedAt => "r.created_at",
        RepositoryOrderField::UpdatedAt => "r.updated_at",
        RepositoryOrderField::PushedAt => "coalesce(r.pushed_at, r.created_at)",
        RepositoryOrderField::Name => "lower(r.name)",
        RepositoryOrderField::Stargazers => "r.stargazers_count",
    };
    qb.push(format!(
        " ORDER BY {col} {dir}, r.id {dir}",
        dir = order.direction.sql()
    ));
    // Visibility depends on the viewer's permissions, which are computed in
    // one batch for the owner's repositories; the list is then paginated in
    // memory (owners rarely have more than a few thousand repositories).
    let repos: Vec<db::Repository> = qb.build_query_as().fetch_all(&g.state.db).await.gql()?;
    let rows = RepoLoader::rows(&g.state, g.auth.as_ref(), repos)
        .await
        .gql()?;
    let visible: Vec<Repository> = rows
        .into_iter()
        .filter(|r| r.readable())
        .map(|r| Repository(Arc::new(r)))
        .collect();
    if !w.nodes {
        return Ok(Page::count_only(visible.len() as i64).into());
    }
    Ok(Page::from_vec(visible, &args)?.into())
}

connection!(RepositoryConnection, RepositoryEdge, Repository);

#[Object]
impl Repository {
    pub async fn id(&self) -> ID {
        nid(NodeType::Repository, self.rid())
    }
    pub async fn database_id(&self) -> Option<i64> {
        Some(self.rid())
    }
    pub async fn name(&self) -> String {
        self.r().name.clone()
    }
    pub async fn name_with_owner(&self) -> String {
        self.0.full_name()
    }
    pub async fn owner(&self) -> RepositoryOwner {
        RepositoryOwner::from_user(Arc::new(self.0.owner.clone()))
    }
    pub async fn description(&self) -> Option<String> {
        self.r().description.clone()
    }
    #[graphql(name = "descriptionHTML")]
    pub async fn description_html(&self) -> HTML {
        let d = self.r().description.as_deref().unwrap_or("");
        HTML(format!("<div>{}</div>", html_escape(d)))
    }
    #[graphql(name = "shortDescriptionHTML")]
    pub async fn short_description_html(&self, #[graphql(default = 200)] limit: i32) -> HTML {
        let d: String = self
            .r()
            .description
            .as_deref()
            .unwrap_or("")
            .chars()
            .take(limit.max(0) as usize)
            .collect();
        HTML(html_escape(&d))
    }
    pub async fn homepage_url(&self) -> Option<URI> {
        self.r().homepage.clone().filter(|h| !h.is_empty()).map(URI)
    }
    pub async fn url(&self, ctx: &Context<'_>) -> URI {
        URI(gql(ctx)
            .state
            .urls
            .repo_html(self.owner_login(), &self.r().name))
    }
    pub async fn resource_path(&self) -> URI {
        URI(format!("/{}", self.0.full_name()))
    }
    pub async fn ssh_url(&self, ctx: &Context<'_>) -> GitSSHRemote {
        GitSSHRemote(
            gql(ctx)
                .state
                .urls
                .ssh_url(self.owner_login(), &self.r().name),
        )
    }
    pub async fn mirror_url(&self) -> Option<URI> {
        self.r().mirror_url.clone().map(URI)
    }
    pub async fn open_graph_image_url(&self, ctx: &Context<'_>) -> URI {
        URI(gql(ctx)
            .state
            .urls
            .avatar(self.0.owner.id, self.0.owner.avatar_url.as_deref()))
    }
    pub async fn uses_custom_open_graph_image(&self) -> bool {
        false
    }
    pub async fn security_policy_url(&self) -> Option<URI> {
        None
    }
    pub async fn created_at(&self) -> DateTime {
        dt(self.r().created_at)
    }
    pub async fn pushed_at(&self) -> Option<DateTime> {
        odt(self.r().pushed_at)
    }
    pub async fn updated_at(&self) -> DateTime {
        dt(self.r().updated_at)
    }
    pub async fn archived_at(&self) -> Option<DateTime> {
        self.r().archived.then(|| dt(self.r().updated_at))
    }
    pub async fn is_blank_issues_enabled(&self) -> bool {
        true
    }
    pub async fn is_security_policy_enabled(&self) -> Option<bool> {
        Some(false)
    }
    pub async fn has_issues_enabled(&self) -> bool {
        self.r().has_issues
    }
    pub async fn has_projects_enabled(&self) -> bool {
        self.r().has_projects
    }
    pub async fn has_wiki_enabled(&self) -> bool {
        self.r().has_wiki
    }
    pub async fn has_discussions_enabled(&self) -> bool {
        self.r().has_discussions
    }
    pub async fn has_vulnerability_alerts_enabled(&self) -> bool {
        false
    }
    pub async fn has_sponsorships_enabled(&self) -> bool {
        false
    }
    pub async fn merge_commit_allowed(&self) -> bool {
        self.r().allow_merge_commit
    }
    pub async fn squash_merge_allowed(&self) -> bool {
        self.r().allow_squash_merge
    }
    pub async fn rebase_merge_allowed(&self) -> bool {
        self.r().allow_rebase_merge
    }
    pub async fn auto_merge_allowed(&self) -> bool {
        self.r().allow_auto_merge
    }
    pub async fn delete_branch_on_merge(&self) -> bool {
        self.r().delete_branch_on_merge
    }
    pub async fn allow_update_branch(&self) -> bool {
        self.r().allow_update_branch
    }
    pub async fn forking_allowed(&self) -> bool {
        self.r().allow_forking
    }
    pub async fn web_commit_signoff_required(&self) -> bool {
        self.r().web_commit_signoff_required
    }
    pub async fn squash_merge_commit_title(&self) -> Option<String> {
        Some(self.r().squash_merge_commit_title.clone())
    }
    pub async fn squash_merge_commit_message(&self) -> Option<String> {
        Some(self.r().squash_merge_commit_message.clone())
    }
    pub async fn merge_commit_title(&self) -> Option<String> {
        Some(self.r().merge_commit_title.clone())
    }
    pub async fn merge_commit_message(&self) -> Option<String> {
        Some(self.r().merge_commit_message.clone())
    }
    pub async fn fork_count(&self) -> i32 {
        self.r().forks_count as i32
    }
    pub async fn stargazer_count(&self) -> i32 {
        self.r().stargazers_count as i32
    }
    pub async fn stargazers(&self) -> CountOnly {
        CountOnly(self.r().stargazers_count)
    }
    pub async fn watchers(&self, ctx: &Context<'_>) -> GResult<CountOnly> {
        Ok(CountOnly(self.extra(ctx).await?.watchers))
    }
    pub async fn forks(
        &self,
        ctx: &Context<'_>,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
    ) -> GResult<RepositoryConnection> {
        let g = gql(ctx);
        let repos: Vec<db::Repository> = sqlx::query_as(&format!(
            "SELECT {} FROM repositories WHERE parent_id = $1 ORDER BY created_at, id",
            db::Repository::COLUMNS
        ))
        .bind(self.rid())
        .fetch_all(&g.state.db)
        .await
        .gql()?;
        let rows = RepoLoader::rows(&g.state, g.auth.as_ref(), repos)
            .await
            .gql()?;
        let visible: Vec<Repository> = rows
            .into_iter()
            .filter(|r| r.readable())
            .map(|r| Repository(Arc::new(r)))
            .collect();
        Ok(Page::from_vec(visible, &ConnArgs::new(first, last, after, before))?.into())
    }
    pub async fn disk_usage(&self) -> Option<i32> {
        Some(self.r().size as i32)
    }
    pub async fn is_archived(&self) -> bool {
        self.r().archived
    }
    pub async fn is_disabled(&self) -> bool {
        self.r().disabled
    }
    pub async fn is_empty(&self, ctx: &Context<'_>) -> GResult<bool> {
        git::is_empty(ctx, self.rid()).await
    }
    pub async fn is_fork(&self) -> bool {
        self.r().fork
    }
    pub async fn is_in_organization(&self) -> bool {
        self.0.owner.is_org()
    }
    pub async fn is_locked(&self) -> bool {
        false
    }
    pub async fn lock_reason(&self) -> Option<RepositoryLockReason> {
        None
    }
    pub async fn is_mirror(&self) -> bool {
        self.r().mirror_url.is_some()
    }
    pub async fn is_private(&self) -> bool {
        self.r().is_private()
    }
    pub async fn visibility(&self) -> RepositoryVisibility {
        RepositoryVisibility::from_db(&self.r().visibility)
    }
    pub async fn is_template(&self) -> bool {
        self.r().is_template
    }
    pub async fn is_user_configuration_repository(&self) -> bool {
        self.r().name.eq_ignore_ascii_case(self.owner_login())
    }
    pub async fn license_info(&self) -> Option<License> {
        self.r().license_spdx_id.as_deref().map(License::from_spdx)
    }
    pub async fn code_of_conduct(&self) -> Option<CodeOfConduct> {
        None
    }
    pub async fn contact_links(&self) -> Option<Vec<RepositoryContactLink>> {
        Some(vec![])
    }
    pub async fn funding_links(&self) -> Vec<FundingLink> {
        vec![]
    }
    pub async fn viewer_can_administer(&self) -> bool {
        self.0.perm >= Permission::Admin
    }
    pub async fn viewer_can_update_topics(&self) -> bool {
        self.0.perm >= Permission::Maintain
    }
    pub async fn viewer_can_create_projects(&self) -> bool {
        self.0.perm >= Permission::Write
    }
    pub async fn viewer_can_subscribe(&self, ctx: &Context<'_>) -> bool {
        gql(ctx).auth.is_some()
    }
    pub async fn viewer_permission(&self, ctx: &Context<'_>) -> Option<RepositoryPermission> {
        gql(ctx).auth.as_ref()?;
        RepositoryPermission::from_perm(self.0.perm)
    }
    pub async fn viewer_default_commit_email(&self, ctx: &Context<'_>) -> Option<String> {
        let a = gql(ctx).auth.as_ref()?;
        Some(a.user.email.clone().unwrap_or_else(|| {
            format!(
                "{}+{}@users.noreply.{}",
                a.user.id,
                a.user.login,
                gql(ctx).state.config.hostname()
            )
        }))
    }
    pub async fn viewer_default_merge_method(&self) -> PullRequestMergeMethod {
        let r = self.r();
        if r.allow_merge_commit {
            PullRequestMergeMethod::Merge
        } else if r.allow_squash_merge {
            PullRequestMergeMethod::Squash
        } else {
            PullRequestMergeMethod::Rebase
        }
    }
    pub async fn viewer_possible_commit_emails(
        &self,
        ctx: &Context<'_>,
    ) -> GResult<Option<Vec<String>>> {
        let g = gql(ctx);
        let Some(a) = g.auth.as_ref() else {
            return Ok(None);
        };
        let emails: Vec<String> = sqlx::query_scalar(
            "SELECT email FROM user_emails WHERE user_id = $1 AND verified ORDER BY is_primary DESC, email",
        )
        .bind(a.user.id)
        .fetch_all(&g.state.db)
        .await
        .gql()?;
        Ok(Some(emails))
    }
    pub async fn viewer_has_starred(&self, ctx: &Context<'_>) -> GResult<bool> {
        if gql(ctx).viewer_id().is_none() {
            return Ok(false);
        }
        Ok(self.extra(ctx).await?.starred)
    }
    pub async fn viewer_subscription(
        &self,
        ctx: &Context<'_>,
    ) -> GResult<Option<SubscriptionState>> {
        if gql(ctx).viewer_id().is_none() {
            return Ok(None);
        }
        Ok(Some(match self.extra(ctx).await?.watch {
            Some((_, true)) => SubscriptionState::Ignored,
            Some((true, _)) => SubscriptionState::Subscribed,
            _ => SubscriptionState::Unsubscribed,
        }))
    }
    pub async fn repository_topics(
        &self,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
    ) -> GResult<RepositoryTopicConnection> {
        RepositoryTopicConnection::new(
            self.r().topics.clone(),
            &ConnArgs::new(first, last, after, before),
        )
    }
    pub async fn primary_language(&self) -> Option<Language> {
        self.r().language.clone().map(Language::new)
    }
    pub async fn languages(
        &self,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
    ) -> GResult<LanguageConnection> {
        let langs: Vec<(String, i64)> = self
            .r()
            .language
            .clone()
            .map(|l| vec![(l, (self.r().size * 1024).max(1))])
            .unwrap_or_default();
        LanguageConnection::new(langs, &ConnArgs::new(first, last, after, before))
    }
    pub async fn issue_templates(&self) -> Option<Vec<IssueTemplate>> {
        Some(vec![])
    }
    pub async fn pull_request_templates(&self) -> Option<Vec<PullRequestTemplate>> {
        Some(vec![])
    }
    pub async fn projects(
        &self,
        first: Option<i32>,
        after: Option<String>,
        states: Option<Vec<ProjectState>>,
        order_by: Option<ProjectOrder>,
    ) -> ProjectConnection {
        let _ = (first, after, states, order_by);
        ProjectConnection
    }
    #[graphql(name = "projectsV2")]
    pub async fn projects_v2(
        &self,
        ctx: &Context<'_>,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
        query: Option<String>,
        order_by: Option<ProjectV2Order>,
    ) -> GResult<super::project::ProjectV2Connection> {
        super::project::repo_projects(
            ctx,
            self.rid(),
            ConnArgs::new(first, last, after, before),
            query,
            order_by,
        )
        .await
    }
    pub async fn parent(&self, ctx: &Context<'_>) -> GResult<Option<Repository>> {
        match self.r().parent_id {
            Some(id) => load(ctx, id).await,
            None => Ok(None),
        }
    }
    pub async fn template_repository(&self, ctx: &Context<'_>) -> GResult<Option<Repository>> {
        match self.r().template_repository_id {
            Some(id) => load(ctx, id).await,
            None => Ok(None),
        }
    }

    // --- git ------------------------------------------------------------

    pub async fn default_branch_ref(&self, ctx: &Context<'_>) -> GResult<Option<Ref>> {
        let name = format!("refs/heads/{}", self.r().default_branch);
        Ref::load(ctx, self.clone(), &name).await
    }
    #[graphql(name = "ref")]
    pub async fn ref_(&self, ctx: &Context<'_>, qualified_name: String) -> GResult<Option<Ref>> {
        let full = if qualified_name.starts_with("refs/") {
            qualified_name
        } else {
            format!("refs/heads/{qualified_name}")
        };
        Ref::load(ctx, self.clone(), &full).await
    }
    pub async fn refs(
        &self,
        ctx: &Context<'_>,
        ref_prefix: String,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
        query: Option<String>,
        order_by: Option<RefOrder>,
        direction: Option<OrderDirection>,
    ) -> GResult<RefConnection> {
        let _ = direction;
        let mut refs = git::list_refs(ctx, self.rid(), &ref_prefix).await?;
        if let Some(q) = query.filter(|q| !q.is_empty()) {
            let q = q.to_lowercase();
            refs.retain(|r| r.name.to_lowercase().contains(&q));
        }
        if let Some(o) = order_by {
            if o.field == RefOrderField::Alphabetical {
                refs.sort_by(|a, b| a.name.cmp(&b.name));
            }
            if o.direction == OrderDirection::Desc {
                refs.reverse();
            }
        }
        let items: Vec<Ref> = refs
            .into_iter()
            .map(|info| Ref {
                repo: self.clone(),
                name: info.name,
                target: info.target,
            })
            .collect();
        Ok(Page::from_vec(items, &ConnArgs::new(first, last, after, before))?.into())
    }
    /// A Git object in the repository.
    pub async fn object(
        &self,
        ctx: &Context<'_>,
        oid: Option<GitObjectID>,
        expression: Option<String>,
    ) -> GResult<Option<GitObject>> {
        let rev = match (oid, expression) {
            (Some(o), _) => o.0,
            (None, Some(e)) => e,
            (None, None) => return Ok(None),
        };
        git::object(ctx, self.clone(), &rev).await
    }

    // --- issues & pull requests -----------------------------------------

    pub async fn issues(
        &self,
        ctx: &Context<'_>,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
        states: Option<Vec<IssueState>>,
        labels: Option<Vec<String>>,
        order_by: Option<IssueOrder>,
        filter_by: Option<IssueFilters>,
    ) -> GResult<IssueConnection> {
        issue::list_for_repo(
            ctx,
            self,
            ConnArgs::new(first, last, after, before),
            states,
            labels,
            order_by,
            filter_by.unwrap_or_default(),
        )
        .await
    }
    pub async fn issue(&self, ctx: &Context<'_>, number: i32) -> GResult<Option<Issue>> {
        issue::by_number(ctx, self, i64::from(number), false).await
    }
    pub async fn issue_or_pull_request(
        &self,
        ctx: &Context<'_>,
        number: i32,
    ) -> GResult<Option<issue::IssueOrPullRequest>> {
        issue::issue_or_pull(ctx, self, i64::from(number)).await
    }
    pub async fn pull_requests(
        &self,
        ctx: &Context<'_>,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
        states: Option<Vec<PullRequestState>>,
        labels: Option<Vec<String>>,
        head_ref_name: Option<String>,
        base_ref_name: Option<String>,
        order_by: Option<IssueOrder>,
    ) -> GResult<PullRequestConnection> {
        pull::list_for_repo(
            ctx,
            self,
            ConnArgs::new(first, last, after, before),
            pull::PullFilter {
                states,
                labels,
                head_ref_name,
                base_ref_name,
                order_by,
            },
        )
        .await
    }
    pub async fn pull_request(
        &self,
        ctx: &Context<'_>,
        number: i32,
    ) -> GResult<Option<PullRequest>> {
        pull::by_number(ctx, self, i64::from(number)).await
    }

    // --- rulesets ----------------------------------------------------------

    pub async fn rulesets(
        &self,
        ctx: &Context<'_>,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
        #[graphql(default = true)] include_parents: bool,
        targets: Option<Vec<super::ruleset::RepositoryRulesetTarget>>,
    ) -> GResult<super::ruleset::RepositoryRulesetConnection> {
        super::ruleset::repo_rulesets(
            ctx,
            self,
            ConnArgs::new(first, last, after, before),
            include_parents,
            targets,
        )
        .await
    }

    // --- labels, milestones, people --------------------------------------

    pub async fn labels(
        &self,
        ctx: &Context<'_>,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
        query: Option<String>,
        order_by: Option<LabelOrder>,
    ) -> GResult<LabelConnection> {
        issue::repo_labels(
            ctx,
            self,
            ConnArgs::new(first, last, after, before),
            query,
            order_by,
        )
        .await
    }
    pub async fn label(&self, ctx: &Context<'_>, name: String) -> GResult<Option<Label>> {
        issue::repo_label(ctx, self, &name).await
    }
    pub async fn milestones(
        &self,
        ctx: &Context<'_>,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
        states: Option<Vec<MilestoneState>>,
        order_by: Option<MilestoneOrder>,
        query: Option<String>,
    ) -> GResult<MilestoneConnection> {
        issue::repo_milestones(
            ctx,
            self,
            ConnArgs::new(first, last, after, before),
            states,
            order_by,
            query,
        )
        .await
    }
    pub async fn milestone(&self, ctx: &Context<'_>, number: i32) -> GResult<Option<Milestone>> {
        issue::repo_milestone(ctx, self, i64::from(number)).await
    }
    pub async fn assignable_users(
        &self,
        ctx: &Context<'_>,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
        query: Option<String>,
    ) -> GResult<UserConnection> {
        let users = people(ctx, self, query, true).await?;
        Ok(Page::from_vec(users, &ConnArgs::new(first, last, after, before))?.into())
    }
    pub async fn mentionable_users(
        &self,
        ctx: &Context<'_>,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
        query: Option<String>,
    ) -> GResult<UserConnection> {
        let users = people(ctx, self, query, false).await?;
        Ok(Page::from_vec(users, &ConnArgs::new(first, last, after, before))?.into())
    }
    pub async fn collaborators(
        &self,
        ctx: &Context<'_>,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
        query: Option<String>,
    ) -> GResult<UserConnection> {
        let users = people(ctx, self, query, true).await?;
        Ok(Page::from_vec(users, &ConnArgs::new(first, last, after, before))?.into())
    }

    // --- releases --------------------------------------------------------

    pub async fn latest_release(&self, ctx: &Context<'_>) -> GResult<Option<Release>> {
        let l = ctx.data_unchecked::<Loaders>();
        Ok(one(&l.latest_release, self.rid())
            .await?
            .map(|r| Release::new(self.clone(), r)))
    }
    pub async fn releases(
        &self,
        ctx: &Context<'_>,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
        order_by: Option<ReleaseOrder>,
    ) -> GResult<ReleaseConnection> {
        release::list_for_repo(
            ctx,
            self,
            ConnArgs::new(first, last, after, before),
            order_by,
        )
        .await
    }
    pub async fn release(&self, ctx: &Context<'_>, tag_name: String) -> GResult<Option<Release>> {
        release::by_tag(ctx, self, &tag_name).await
    }
}

/// Users that can be assigned (triage or better) or mentioned (any
/// grant), optionally filtered by login/name. One query: the best grant per
/// user from ownership, collaborators, org membership (+ base permission)
/// and team grants (inherited from parent teams), like `bgh_core::perms`.
async fn people(
    ctx: &Context<'_>,
    repo: &Repository,
    query: Option<String>,
    write_only: bool,
) -> GResult<Vec<User>> {
    let g = gql(ctx);
    let r = repo.r();
    let min_rank: i32 = if write_only { 2 } else { 1 };
    let users: Vec<db::User> = sqlx::query_as(&format!(
        r#"
        WITH RECURSIVE
        ranks(name, rk) AS (VALUES ('read', 1), ('pull', 1), ('triage', 2), ('write', 3),
                                   ('push', 3), ('maintain', 4), ('admin', 5)),
        team_tree(team_id, granting_id) AS (
            SELECT id, id FROM teams WHERE org_id = $1
            UNION
            SELECT tt.team_id, t.parent_id FROM team_tree tt JOIN teams t ON t.id = tt.granting_id
             WHERE t.parent_id IS NOT NULL
        ),
        grants(user_id, rk) AS (
            SELECT $1::bigint, 5
            UNION ALL
            SELECT c.user_id, rn.rk FROM collaborators c JOIN ranks rn ON rn.name = c.permission
             WHERE c.repo_id = $2
            UNION ALL
            SELECT m.user_id,
                   CASE WHEN m.role = 'admin' THEN 5
                        ELSE coalesce((SELECT rk FROM ranks WHERE name = s.default_repository_permission), 0)
                   END
              FROM org_members m LEFT JOIN org_settings s ON s.org_id = m.org_id
             WHERE m.org_id = $1
            UNION ALL
            SELECT tm.user_id, rn.rk
              FROM team_members tm
              JOIN team_tree tt ON tt.team_id = tm.team_id
              JOIN team_repos tr ON tr.team_id = tt.granting_id AND tr.repo_id = $2
              JOIN ranks rn ON rn.name = tr.permission
        )
        SELECT {cols} FROM users u
          JOIN (SELECT user_id, max(rk) AS rk FROM grants GROUP BY user_id) gr ON gr.user_id = u.id
         WHERE u.type = 'User' AND u.suspended_at IS NULL AND gr.rk >= $4
           AND ($3::text IS NULL OR lower(u.login) LIKE lower($3) || '%'
                OR lower(coalesce(u.name, '')) LIKE '%' || lower($3) || '%')
         ORDER BY lower(u.login)
        "#,
        cols = db::prefixed("u", db::User::COLUMNS)
    ))
    .bind(r.owner_id)
    .bind(r.id)
    .bind(query.filter(|q| !q.is_empty()))
    .bind(min_rank)
    .fetch_all(&g.state.db)
    .await
    .gql()?;
    Ok(users.into_iter().map(|u| User(Arc::new(u))).collect())
}

pub fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

// ---------------------------------------------------------------------------
// Refs
// ---------------------------------------------------------------------------

/// Represents a Git reference.
#[derive(Clone)]
pub struct Ref {
    pub repo: Repository,
    /// Full name (`refs/heads/main`).
    pub name: String,
    /// Object the ref points at.
    pub target: String,
}

impl Ref {
    pub async fn load(ctx: &Context<'_>, repo: Repository, full: &str) -> GResult<Option<Self>> {
        let info = git::find_ref(ctx, repo.rid(), full).await?;
        Ok(info.map(|i| Ref {
            repo,
            name: i.name,
            target: i.target,
        }))
    }

    fn short(&self) -> &str {
        self.name
            .strip_prefix("refs/heads/")
            .or_else(|| self.name.strip_prefix("refs/tags/"))
            .unwrap_or(&self.name)
    }
}

#[Object]
impl Ref {
    pub async fn id(&self) -> ID {
        ID(node_id::encode_str(
            NodeType::Ref,
            &format!("{}:{}", self.repo.rid(), self.name),
        ))
    }
    pub async fn name(&self) -> String {
        self.short().to_string()
    }
    pub async fn prefix(&self) -> String {
        self.name[..self.name.len() - self.short().len()].to_string()
    }
    pub async fn repository(&self) -> Repository {
        self.repo.clone()
    }
    pub async fn target(&self, ctx: &Context<'_>) -> GResult<Option<GitObject>> {
        git::object(ctx, self.repo.clone(), &self.target).await
    }
    pub async fn branch_protection_rule(&self) -> Option<BranchProtectionRule> {
        None
    }
    pub async fn associated_pull_requests(
        &self,
        ctx: &Context<'_>,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
        states: Option<Vec<PullRequestState>>,
    ) -> GResult<PullRequestConnection> {
        pull::list_for_repo(
            ctx,
            &self.repo,
            ConnArgs::new(first, last, after, before),
            pull::PullFilter {
                states,
                head_ref_name: Some(self.short().to_string()),
                ..Default::default()
            },
        )
        .await
    }
    /// Compare this ref (base) with another ref (head).
    pub async fn compare(
        &self,
        ctx: &Context<'_>,
        head_ref: String,
    ) -> GResult<Option<Comparison>> {
        let head = if head_ref.starts_with("refs/") {
            head_ref
        } else {
            format!("refs/heads/{head_ref}")
        };
        let Some(h) = git::find_ref(ctx, self.repo.rid(), &head).await? else {
            return Ok(None);
        };
        let Some(b) = git::find_ref(ctx, self.repo.rid(), &self.name).await? else {
            return Ok(None);
        };
        let (behind, ahead) =
            pull::ahead_behind(&gql(ctx).state, self.repo.rid(), &b.peeled, &h.peeled).await?;
        Ok(Some(Comparison { ahead, behind }))
    }
}

connection!(RefConnection, RefEdge, Ref);

/// A branch protection rule (none are exposed; kept for query shape).
#[derive(SimpleObject, Clone)]
pub struct BranchProtectionRule {
    pub pattern: String,
    pub requires_strict_status_checks: bool,
    pub requires_approving_reviews: bool,
    pub required_approving_review_count: Option<i32>,
}

/// Represents a comparison between two commit revisions.
pub struct Comparison {
    ahead: i64,
    behind: i64,
}

#[Object]
impl Comparison {
    pub async fn ahead_by(&self) -> i32 {
        self.ahead as i32
    }
    pub async fn behind_by(&self) -> i32 {
        self.behind as i32
    }
    pub async fn status(&self) -> ComparisonStatus {
        match (self.ahead > 0, self.behind > 0) {
            (true, true) => ComparisonStatus::Diverged,
            (true, false) => ComparisonStatus::Ahead,
            (false, true) => ComparisonStatus::Behind,
            (false, false) => ComparisonStatus::Identical,
        }
    }
}
