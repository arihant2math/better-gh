//! The `Query` root.

use std::sync::Arc;

use async_graphql::{Context, ID, Object};
use bgh_core::node_id::{self, NodeType};
use bgh_core::prelude::*;

use crate::conn::ConnArgs;
use crate::ctx::{GResult, OrGql, gql, not_found};
use crate::loaders::{Loaders, ReleaseRow, ReviewRow, one};
use crate::model::enums::SearchType;
use crate::model::misc::RateLimit;
use crate::model::pull::{PullRequestReview, from_issues};
use crate::model::{
    Bot, Issue, IssueComment, Label, Milestone, Node, Organization, Ref, Release, Repository,
    RepositoryOwner, Team, User, repo,
};
use crate::scalars::DateTime;
use crate::search::{self, SearchResultItemConnection};

#[derive(Default)]
pub struct Query;

#[Object]
impl Query {
    /// The currently authenticated user.
    pub async fn viewer(&self, ctx: &Context<'_>) -> GResult<User> {
        let a = gql(ctx).require_auth()?;
        Ok(User(Arc::new(a.user.clone())))
    }

    /// Lookup a given repository by the owner and repository name.
    pub async fn repository(
        &self,
        ctx: &Context<'_>,
        owner: String,
        name: String,
        #[graphql(default)] follow_renames: bool,
    ) -> GResult<Option<Repository>> {
        let _ = follow_renames;
        match repo::by_nwo(ctx, &owner, &name).await? {
            Some(r) => Ok(Some(r)),
            None => Err(not_found(format!(
                "Could not resolve to a Repository with the name '{owner}/{name}'."
            ))),
        }
    }

    /// Lookup a repository owner (ie. either a User or an Organization) by login.
    pub async fn repository_owner(
        &self,
        ctx: &Context<'_>,
        login: String,
    ) -> GResult<Option<RepositoryOwner>> {
        let u = db::User::find_by_login(&gql(ctx).state.db, &login)
            .await
            .gql()?;
        Ok(u.filter(|u| u.kind != "Bot")
            .map(|u| RepositoryOwner::from_user(Arc::new(u))))
    }

    /// Lookup a user by login.
    pub async fn user(&self, ctx: &Context<'_>, login: String) -> GResult<Option<User>> {
        let u = db::User::find_by_login(&gql(ctx).state.db, &login)
            .await
            .gql()?;
        match u.filter(|u| u.kind == "User") {
            Some(u) => Ok(Some(User(Arc::new(u)))),
            None => Err(not_found(format!(
                "Could not resolve to a User with the login of '{login}'."
            ))),
        }
    }

    /// Lookup an organization by login.
    pub async fn organization(
        &self,
        ctx: &Context<'_>,
        login: String,
    ) -> GResult<Option<Organization>> {
        let u = db::User::find_by_login(&gql(ctx).state.db, &login)
            .await
            .gql()?;
        match u.filter(|u| u.is_org()) {
            Some(u) => Ok(Some(Organization(Arc::new(u)))),
            None => Err(not_found(format!(
                "Could not resolve to an Organization with the login of '{login}'."
            ))),
        }
    }

    /// Fetches an object given its ID.
    pub async fn node(&self, ctx: &Context<'_>, id: ID) -> GResult<Option<Node>> {
        match resolve_node(ctx, &id).await? {
            Some(n) => Ok(Some(n)),
            None => Err(not_found(format!(
                "Could not resolve to a node with the global id of '{}'",
                id.0
            ))),
        }
    }

    /// Lookup nodes by a list of IDs.
    pub async fn nodes(&self, ctx: &Context<'_>, ids: Vec<ID>) -> GResult<Vec<Option<Node>>> {
        let mut out = Vec::with_capacity(ids.len());
        for id in &ids {
            out.push(resolve_node(ctx, id).await?);
        }
        Ok(out)
    }

    /// Perform a search across resources.
    #[allow(clippy::too_many_arguments)]
    pub async fn search(
        &self,
        ctx: &Context<'_>,
        query: String,
        #[graphql(name = "type")] kind: SearchType,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
    ) -> GResult<SearchResultItemConnection> {
        search::search(ctx, &query, kind, ConnArgs::new(first, last, after, before)).await
    }

    /// The client's rate limit information.
    pub async fn rate_limit(&self, #[graphql(default)] dry_run: bool) -> RateLimit {
        let _ = dry_run;
        let reset = chrono::Utc::now() + chrono::Duration::hours(1);
        RateLimit {
            cost: 1,
            limit: 5000,
            node_count: 0,
            remaining: 4999,
            reset_at: DateTime(reset),
            used: 1,
        }
    }
}

/// Decode a global id and load the node, enforcing read access.
pub async fn resolve_node(ctx: &Context<'_>, id: &ID) -> GResult<Option<Node>> {
    let Some((ty, key)) = node_id::decode_raw(&id.0) else {
        return Ok(None);
    };
    let g = gql(ctx);
    let l = ctx.data_unchecked::<Loaders>();
    let num = key.parse::<i64>().ok();
    Ok(match (ty, num) {
        (NodeType::User | NodeType::Organization | NodeType::Bot, Some(n)) => {
            one(&l.users, n).await?.map(|u| match u.kind.as_str() {
                "Organization" => Node::Organization(Organization(u)),
                "Bot" => Node::Bot(Bot(u)),
                _ => Node::User(User(u)),
            })
        }
        (NodeType::Repository, Some(n)) => repo::load(ctx, n).await?.map(Node::Repository),
        (NodeType::Issue | NodeType::PullRequest, Some(n)) => {
            let Some(i) = one(&l.issues, n).await? else {
                return Ok(None);
            };
            if repo::load(ctx, i.repo_id).await?.is_none() {
                return Ok(None);
            }
            if i.is_pull_request {
                from_issues(ctx, vec![(*i).clone()])
                    .await?
                    .into_iter()
                    .next()
                    .map(Node::PullRequest)
            } else {
                Some(Node::Issue(Issue::new(i)))
            }
        }
        (NodeType::IssueComment, Some(n)) => {
            let row: Option<db::Comment> = sqlx::query_as(&format!(
                "SELECT {} FROM comments WHERE id = $1",
                db::Comment::COLUMNS
            ))
            .bind(n)
            .fetch_optional(&g.state.db)
            .await
            .gql()?;
            match row {
                Some(c) if repo::load(ctx, c.repo_id).await?.is_some() => {
                    Some(Node::IssueComment(IssueComment(Arc::new(c))))
                }
                _ => None,
            }
        }
        (NodeType::Label, Some(n)) => {
            let row: Option<db::Label> = sqlx::query_as(&format!(
                "SELECT {} FROM labels WHERE id = $1",
                db::Label::COLUMNS
            ))
            .bind(n)
            .fetch_optional(&g.state.db)
            .await
            .gql()?;
            match row {
                Some(x) if repo::load(ctx, x.repo_id).await?.is_some() => {
                    Some(Node::Label(Label(Arc::new(x))))
                }
                _ => None,
            }
        }
        (NodeType::Milestone, Some(n)) => match one(&l.milestones, n).await? {
            Some(m) if repo::load(ctx, m.repo_id).await?.is_some() => {
                Some(Node::Milestone(Milestone(m)))
            }
            _ => None,
        },
        (NodeType::PullRequestReview, Some(n)) => {
            let row: Option<ReviewRow> = sqlx::query_as(&format!(
                "SELECT {} FROM pr_reviews WHERE id = $1",
                ReviewRow::COLUMNS
            ))
            .bind(n)
            .fetch_optional(&g.state.db)
            .await
            .gql()?;
            match row {
                Some(r)
                    if repo::load(ctx, r.repo_id).await?.is_some()
                        && (r.state != "PENDING" || r.user_id == g.viewer_id()) =>
                {
                    Some(Node::PullRequestReview(PullRequestReview(Arc::new(r))))
                }
                _ => None,
            }
        }
        (NodeType::Release, Some(n)) => {
            let row: Option<ReleaseRow> = sqlx::query_as(&format!(
                "SELECT {} FROM releases WHERE id = $1",
                ReleaseRow::COLUMNS
            ))
            .bind(n)
            .fetch_optional(&g.state.db)
            .await
            .gql()?;
            match row {
                Some(r) => match repo::load(ctx, r.repo_id).await? {
                    Some(repo) if !r.draft || repo.row().perm >= Permission::Write => {
                        Some(Node::Release(Release::new(repo, Arc::new(r))))
                    }
                    _ => None,
                },
                None => None,
            }
        }
        (NodeType::Team, Some(n)) => one(&l.teams, n).await?.map(|t| Node::Team(Team(t))),
        (NodeType::Ref, None) => {
            let Some((repo_id, name)) = key.split_once(':') else {
                return Ok(None);
            };
            let Some(repo) = repo::load(ctx, repo_id.parse().unwrap_or(0)).await? else {
                return Ok(None);
            };
            Ref::load(ctx, repo, name).await?.map(Node::Ref)
        }
        (NodeType::Commit, None) => {
            let Some((repo_id, sha)) = key.split_once(':') else {
                return Ok(None);
            };
            let Some(repo) = repo::load(ctx, repo_id.parse().unwrap_or(0)).await? else {
                return Ok(None);
            };
            crate::model::git::commit(ctx, repo, sha)
                .await?
                .map(Node::Commit)
        }
        _ => None,
    })
}
