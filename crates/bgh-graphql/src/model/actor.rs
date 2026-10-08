//! Users, organizations, bots, teams.

use std::sync::Arc;

use async_graphql::{Context, ID, Object};
use bgh_core::node_id::NodeType;
use bgh_core::prelude::*;

use super::enums::{
    ProjectOrder, ProjectState, ProjectV2Order, RepositoryAffiliation, RepositoryOrder,
    RepositoryPrivacy, TeamOrder,
};
use super::misc::ProjectConnection;
use super::project::{self, ProjectV2, ProjectV2Connection};
use super::repo::{self, Repository, RepositoryConnection};
use super::{Actor, nid};
use crate::conn::{ConnArgs, Page, connection};
use crate::ctx::{GResult, OrGql, gql};
use crate::loaders::one;
use crate::scalars::{DateTime, URI, dt};

fn avatar(ctx: &Context<'_>, u: &db::User, size: Option<i32>) -> URI {
    let url = gql(ctx).state.urls.avatar(u.id, u.avatar_url.as_deref());
    match size {
        Some(s) => {
            let sep = if url.contains('?') { '&' } else { '?' };
            URI(format!("{url}{sep}s={s}"))
        }
        None => URI(url),
    }
}

/// A user is an individual's account on GitHub.
#[derive(Clone)]
pub struct User(pub Arc<db::User>);

#[Object]
impl User {
    pub async fn id(&self) -> ID {
        nid(NodeType::User, self.0.id)
    }
    pub async fn database_id(&self) -> Option<i64> {
        Some(self.0.id)
    }
    pub async fn login(&self) -> String {
        self.0.login.clone()
    }
    pub async fn name(&self) -> Option<String> {
        self.0.name.clone()
    }
    /// The user's publicly visible profile email ("" when hidden).
    pub async fn email(&self) -> String {
        self.0.email.clone().unwrap_or_default()
    }
    pub async fn avatar_url(&self, ctx: &Context<'_>, size: Option<i32>) -> URI {
        avatar(ctx, &self.0, size)
    }
    pub async fn url(&self, ctx: &Context<'_>) -> URI {
        URI(gql(ctx).state.urls.user_html(&self.0.login))
    }
    pub async fn resource_path(&self) -> URI {
        URI(format!("/{}", self.0.login))
    }
    pub async fn bio(&self) -> Option<String> {
        self.0.bio.clone()
    }
    pub async fn company(&self) -> Option<String> {
        self.0.company.clone()
    }
    pub async fn location(&self) -> Option<String> {
        self.0.location.clone()
    }
    pub async fn website_url(&self) -> Option<URI> {
        self.0.blog.clone().filter(|b| !b.is_empty()).map(URI)
    }
    pub async fn twitter_username(&self) -> Option<String> {
        self.0.twitter_username.clone()
    }
    pub async fn created_at(&self) -> DateTime {
        dt(self.0.created_at)
    }
    pub async fn updated_at(&self) -> DateTime {
        dt(self.0.updated_at)
    }
    pub async fn is_site_admin(&self) -> bool {
        self.0.site_admin
    }
    pub async fn is_hireable(&self) -> bool {
        self.0.hireable.unwrap_or(false)
    }
    pub async fn is_viewer(&self, ctx: &Context<'_>) -> bool {
        gql(ctx).viewer_id() == Some(self.0.id)
    }
    pub async fn is_employee(&self) -> bool {
        false
    }
    pub async fn is_bounty_hunter(&self) -> bool {
        false
    }
    pub async fn is_campus_expert(&self) -> bool {
        false
    }
    pub async fn is_developer_program_member(&self) -> bool {
        false
    }
    pub async fn status(&self) -> Option<UserStatus> {
        None
    }
    /// Find a repository owned by this user by name.
    pub async fn repository(
        &self,
        ctx: &Context<'_>,
        name: String,
        #[graphql(default)] follow_renames: bool,
    ) -> async_graphql::Result<Option<Repository>> {
        let _ = follow_renames;
        repo::by_owner_and_name(ctx, &self.0, &name).await
    }
    pub async fn repositories(
        &self,
        ctx: &Context<'_>,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
        privacy: Option<RepositoryPrivacy>,
        is_fork: Option<bool>,
        is_archived: Option<bool>,
        is_locked: Option<bool>,
        owner_affiliations: Option<Vec<Option<RepositoryAffiliation>>>,
        affiliations: Option<Vec<Option<RepositoryAffiliation>>>,
        order_by: Option<RepositoryOrder>,
    ) -> async_graphql::Result<RepositoryConnection> {
        let _ = (is_locked, affiliations);
        repo::owner_repositories(
            ctx,
            &self.0,
            ConnArgs::new(first, last, after, before),
            repo::RepoFilter {
                privacy,
                is_fork,
                is_archived,
                owner_affiliations: owner_affiliations
                    .map(|v| v.into_iter().flatten().collect())
                    .unwrap_or_default(),
                order_by,
            },
        )
        .await
    }
    pub async fn organizations(
        &self,
        ctx: &Context<'_>,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
    ) -> GResult<OrganizationConnection> {
        let g = gql(ctx);
        let orgs: Vec<db::User> = sqlx::query_as(&format!(
            "SELECT {} FROM org_members m JOIN users u ON u.id = m.org_id
              WHERE m.user_id = $1 ORDER BY lower(u.login)",
            db::prefixed("u", db::User::COLUMNS)
        ))
        .bind(self.0.id)
        .fetch_all(&g.state.db)
        .await
        .gql()?;
        let items = orgs
            .into_iter()
            .map(|o| Organization(Arc::new(o)))
            .collect();
        Ok(Page::from_vec(items, &ConnArgs::new(first, last, after, before))?.into())
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
    ) -> GResult<ProjectV2Connection> {
        project::owner_projects(
            ctx,
            &self.0,
            ConnArgs::new(first, last, after, before),
            query,
            order_by,
        )
        .await
    }
    /// A project by number (NOT_FOUND when missing or not visible).
    #[graphql(name = "projectV2")]
    pub async fn project_v2(&self, ctx: &Context<'_>, number: i32) -> GResult<Option<ProjectV2>> {
        project::owner_project(ctx, &self.0, number).await
    }
    pub async fn followers(&self, ctx: &Context<'_>) -> GResult<CountOnly> {
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM follows WHERE following_id = $1")
            .bind(self.0.id)
            .fetch_one(&gql(ctx).state.db)
            .await
            .unwrap_or(0);
        Ok(CountOnly(n))
    }
    pub async fn following(&self, ctx: &Context<'_>) -> GResult<CountOnly> {
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM follows WHERE follower_id = $1")
            .bind(self.0.id)
            .fetch_one(&gql(ctx).state.db)
            .await
            .unwrap_or(0);
        Ok(CountOnly(n))
    }
    pub async fn viewer_can_follow(&self, ctx: &Context<'_>) -> bool {
        gql(ctx).viewer_id().is_some_and(|v| v != self.0.id)
    }
    pub async fn viewer_is_following(&self, ctx: &Context<'_>) -> GResult<bool> {
        let Some(v) = gql(ctx).viewer_id() else {
            return Ok(false);
        };
        sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM follows WHERE follower_id = $1 AND following_id = $2)",
        )
        .bind(v)
        .bind(self.0.id)
        .fetch_one(&gql(ctx).state.db)
        .await
        .gql()
    }
}

/// `{ totalCount }`-only connection (followers, following, watchers, ...).
pub struct CountOnly(pub i64);

#[Object(name = "FollowerConnection")]
impl CountOnly {
    pub async fn total_count(&self) -> i32 {
        i32::try_from(self.0).unwrap_or(i32::MAX)
    }
}

/// The user's description of what they're currently doing.
#[derive(async_graphql::SimpleObject, Clone)]
pub struct UserStatus {
    pub message: Option<String>,
    pub emoji: Option<String>,
    pub indicates_limited_availability: bool,
}

/// An account on GitHub, with one or more owners, that has repositories,
/// members and teams.
#[derive(Clone)]
pub struct Organization(pub Arc<db::User>);

#[Object]
impl Organization {
    pub async fn id(&self) -> ID {
        nid(NodeType::Organization, self.0.id)
    }
    pub async fn database_id(&self) -> Option<i64> {
        Some(self.0.id)
    }
    pub async fn login(&self) -> String {
        self.0.login.clone()
    }
    pub async fn name(&self) -> Option<String> {
        self.0.name.clone()
    }
    pub async fn email(&self) -> Option<String> {
        self.0.email.clone()
    }
    pub async fn description(&self, ctx: &Context<'_>) -> GResult<Option<String>> {
        let s = db::OrgSettings::find(&gql(ctx).state.db, self.0.id)
            .await
            .gql()?;
        Ok(s.and_then(|s| s.description))
    }
    pub async fn avatar_url(&self, ctx: &Context<'_>, size: Option<i32>) -> URI {
        avatar(ctx, &self.0, size)
    }
    pub async fn url(&self, ctx: &Context<'_>) -> URI {
        URI(gql(ctx).state.urls.user_html(&self.0.login))
    }
    pub async fn resource_path(&self) -> URI {
        URI(format!("/{}", self.0.login))
    }
    pub async fn location(&self) -> Option<String> {
        self.0.location.clone()
    }
    pub async fn website_url(&self) -> Option<URI> {
        self.0.blog.clone().filter(|b| !b.is_empty()).map(URI)
    }
    pub async fn created_at(&self) -> DateTime {
        dt(self.0.created_at)
    }
    pub async fn updated_at(&self) -> DateTime {
        dt(self.0.updated_at)
    }
    pub async fn viewer_is_a_member(&self, ctx: &Context<'_>) -> GResult<bool> {
        let Some(v) = gql(ctx).viewer_id() else {
            return Ok(false);
        };
        Ok(bgh_core::perms::org_role(&gql(ctx).state.db, self.0.id, v)
            .await
            .gql()?
            .is_some())
    }
    pub async fn viewer_can_administer(&self, ctx: &Context<'_>) -> GResult<bool> {
        let Some(v) = gql(ctx).viewer_id() else {
            return Ok(false);
        };
        Ok(bgh_core::perms::org_role(&gql(ctx).state.db, self.0.id, v)
            .await
            .gql()?
            .is_some_and(|r| r.is_admin()))
    }
    #[allow(clippy::too_many_arguments)]
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
        let _ = include_parents;
        super::ruleset::org_rulesets(
            ctx,
            self,
            ConnArgs::new(first, last, after, before),
            targets,
        )
        .await
    }
    pub async fn viewer_can_create_repositories(&self, ctx: &Context<'_>) -> GResult<bool> {
        let Some(v) = gql(ctx).viewer_id() else {
            return Ok(false);
        };
        Ok(bgh_core::perms::org_role(&gql(ctx).state.db, self.0.id, v)
            .await
            .gql()?
            .is_some())
    }
    pub async fn repository(
        &self,
        ctx: &Context<'_>,
        name: String,
        #[graphql(default)] follow_renames: bool,
    ) -> async_graphql::Result<Option<Repository>> {
        let _ = follow_renames;
        repo::by_owner_and_name(ctx, &self.0, &name).await
    }
    pub async fn repositories(
        &self,
        ctx: &Context<'_>,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
        privacy: Option<RepositoryPrivacy>,
        is_fork: Option<bool>,
        is_archived: Option<bool>,
        is_locked: Option<bool>,
        owner_affiliations: Option<Vec<Option<RepositoryAffiliation>>>,
        affiliations: Option<Vec<Option<RepositoryAffiliation>>>,
        order_by: Option<RepositoryOrder>,
    ) -> async_graphql::Result<RepositoryConnection> {
        let _ = (is_locked, affiliations, owner_affiliations);
        repo::owner_repositories(
            ctx,
            &self.0,
            ConnArgs::new(first, last, after, before),
            repo::RepoFilter {
                privacy,
                is_fork,
                is_archived,
                owner_affiliations: vec![],
                order_by,
            },
        )
        .await
    }
    /// Whether the viewer may create projects for this organization
    /// (members can).
    pub async fn viewer_can_create_projects(&self, ctx: &Context<'_>) -> GResult<bool> {
        let g = gql(ctx);
        Ok(
            bgh_projects::access::owner_role(&g.state, g.auth.as_ref(), &self.0)
                .await
                .gql()?
                .is_some_and(|r| r >= bgh_projects::access::Role::Write),
        )
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
    ) -> GResult<ProjectV2Connection> {
        project::owner_projects(
            ctx,
            &self.0,
            ConnArgs::new(first, last, after, before),
            query,
            order_by,
        )
        .await
    }
    /// A project by number (NOT_FOUND when missing or not visible).
    #[graphql(name = "projectV2")]
    pub async fn project_v2(&self, ctx: &Context<'_>, number: i32) -> GResult<Option<ProjectV2>> {
        project::owner_project(ctx, &self.0, number).await
    }
    /// Teams in this organization (visible ones).
    pub async fn teams(
        &self,
        ctx: &Context<'_>,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
        query: Option<String>,
        order_by: Option<TeamOrder>,
    ) -> GResult<TeamConnection> {
        let _ = order_by;
        let g = gql(ctx);
        let member = match g.viewer_id() {
            Some(v) => bgh_core::perms::org_role(&g.state.db, self.0.id, v)
                .await
                .gql()?
                .is_some(),
            None => false,
        };
        let rows: Vec<db::Team> = sqlx::query_as(&format!(
            "SELECT {} FROM teams WHERE org_id = $1
                AND ($2 OR privacy = 'closed')
                AND ($3::text IS NULL OR lower(name) LIKE '%' || lower($3) || '%'
                     OR lower(slug) LIKE '%' || lower($3) || '%')
              ORDER BY lower(name), id",
            db::Team::COLUMNS
        ))
        .bind(self.0.id)
        .bind(member)
        .bind(query)
        .fetch_all(&g.state.db)
        .await
        .gql()?;
        let org_login = self.0.login.clone();
        let items = rows
            .into_iter()
            .map(|t| {
                Team(Arc::new(crate::loaders::TeamRow {
                    team: t,
                    org_login: org_login.clone(),
                }))
            })
            .collect();
        Ok(Page::from_vec(items, &ConnArgs::new(first, last, after, before))?.into())
    }
    pub async fn team(&self, ctx: &Context<'_>, slug: String) -> GResult<Option<Team>> {
        let g = gql(ctx);
        let row: Option<db::Team> = sqlx::query_as(&format!(
            "SELECT {} FROM teams WHERE org_id = $1 AND lower(slug) = lower($2)",
            db::Team::COLUMNS
        ))
        .bind(self.0.id)
        .bind(slug)
        .fetch_optional(&g.state.db)
        .await
        .gql()?;
        Ok(row.map(|t| {
            Team(Arc::new(crate::loaders::TeamRow {
                team: t,
                org_login: self.0.login.clone(),
            }))
        }))
    }
}

/// A special type of user which takes actions on behalf of integrations.
#[derive(Clone)]
pub struct Bot(pub Arc<db::User>);

#[Object]
impl Bot {
    pub async fn id(&self) -> ID {
        nid(NodeType::Bot, self.0.id)
    }
    pub async fn database_id(&self) -> Option<i64> {
        Some(self.0.id)
    }
    pub async fn login(&self) -> String {
        self.0.login.clone()
    }
    pub async fn avatar_url(&self, ctx: &Context<'_>, size: Option<i32>) -> URI {
        avatar(ctx, &self.0, size)
    }
    pub async fn url(&self, ctx: &Context<'_>) -> URI {
        URI(gql(ctx).state.urls.user_html(&self.0.login))
    }
    pub async fn resource_path(&self) -> URI {
        URI(format!("/{}", self.0.login))
    }
    pub async fn created_at(&self) -> DateTime {
        dt(self.0.created_at)
    }
}

/// A placeholder user for attribution of imported data (never produced).
#[derive(Clone)]
pub struct Mannequin(pub Arc<db::User>);

#[Object]
impl Mannequin {
    pub async fn id(&self) -> ID {
        nid(NodeType::User, self.0.id)
    }
    pub async fn login(&self) -> String {
        self.0.login.clone()
    }
    pub async fn email(&self) -> Option<String> {
        None
    }
    pub async fn avatar_url(&self, ctx: &Context<'_>, size: Option<i32>) -> URI {
        avatar(ctx, &self.0, size)
    }
    pub async fn url(&self, ctx: &Context<'_>) -> URI {
        URI(gql(ctx).state.urls.user_html(&self.0.login))
    }
    pub async fn resource_path(&self) -> URI {
        URI(format!("/{}", self.0.login))
    }
}

/// A team of users in an organization.
#[derive(Clone)]
pub struct Team(pub Arc<crate::loaders::TeamRow>);

#[Object]
impl Team {
    pub async fn id(&self) -> ID {
        nid(NodeType::Team, self.0.team.id)
    }
    pub async fn database_id(&self) -> Option<i64> {
        Some(self.0.team.id)
    }
    pub async fn name(&self) -> String {
        self.0.team.name.clone()
    }
    pub async fn slug(&self) -> String {
        self.0.team.slug.clone()
    }
    pub async fn combined_slug(&self) -> String {
        format!("{}/{}", self.0.org_login, self.0.team.slug)
    }
    pub async fn description(&self) -> Option<String> {
        self.0.team.description.clone()
    }
    pub async fn url(&self, ctx: &Context<'_>) -> URI {
        URI(gql(ctx)
            .state
            .urls
            .team_html(&self.0.org_login, &self.0.team.slug))
    }
    pub async fn resource_path(&self) -> URI {
        URI(format!(
            "/orgs/{}/teams/{}",
            self.0.org_login, self.0.team.slug
        ))
    }
    pub async fn organization(&self, ctx: &Context<'_>) -> GResult<Organization> {
        let g = ctx.data_unchecked::<crate::loaders::Loaders>();
        let org = one(&g.users, self.0.team.org_id)
            .await?
            .ok_or_else(|| crate::ctx::not_found("organization not found"))?;
        Ok(Organization(org))
    }
    pub async fn created_at(&self) -> DateTime {
        dt(self.0.team.created_at)
    }
    pub async fn updated_at(&self) -> DateTime {
        dt(self.0.team.updated_at)
    }
}

connection!(OrganizationConnection, OrganizationEdge, Organization);
connection!(TeamConnection, TeamEdge, Team);
connection!(UserConnection, UserEdge, User);

/// Load an actor by user id (`None` for deleted users).
pub async fn actor(ctx: &Context<'_>, id: Option<i64>) -> GResult<Option<Actor>> {
    let Some(id) = id else {
        return Ok(None);
    };
    let l = ctx.data_unchecked::<crate::loaders::Loaders>();
    Ok(one(&l.users, id).await?.map(Actor::from_user))
}

/// Load a user by id.
pub async fn user(ctx: &Context<'_>, id: Option<i64>) -> GResult<Option<User>> {
    let Some(id) = id else {
        return Ok(None);
    };
    let l = ctx.data_unchecked::<crate::loaders::Loaders>();
    Ok(one(&l.users, id).await?.map(User))
}
