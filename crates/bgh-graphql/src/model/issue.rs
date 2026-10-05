//! Issues, the conversation fields shared with pull requests, comments,
//! labels and milestones.

use std::sync::Arc;

use async_graphql::{Context, ID, MergedObject, Object, Union};
use bgh_core::markdown::{self, RenderContext};
use bgh_core::node_id::NodeType;
use bgh_core::prelude::*;
use sqlx::{Postgres, QueryBuilder};

use super::actor::{self, User, UserConnection};
use super::enums::*;
use super::misc::{
    ProjectCardConnection, ProjectV2ItemConnection, ReactionGroup, load_reaction_groups,
};
use super::pull::{PullRequest, PullRequestConnection};
use super::repo::{self, Repository};
use super::{Actor, nid};
use crate::conn::{ConnArgs, Page, connection, wants};
use crate::ctx::{GResult, OrGql, gql};
use crate::loaders::{CommentsKey, Loaders, many, one};
use crate::scalars::{DateTime, HTML, URI, dt, odt};

// ---------------------------------------------------------------------------
// Shared conversation fields (issues and pull requests)
// ---------------------------------------------------------------------------

/// Fields common to issues and pull requests (both live in `issues`).
#[derive(Clone)]
pub struct Conversation {
    pub i: Arc<db::Issue>,
}

impl Conversation {
    fn html_path(&self, repo: &Repository) -> String {
        let kind = if self.i.is_pull_request {
            "pull"
        } else {
            "issues"
        };
        format!("/{}/{kind}/{}", repo.row().full_name(), self.i.number)
    }
}

pub async fn association(
    ctx: &Context<'_>,
    repo_id: i64,
    user_id: Option<i64>,
) -> GResult<CommentAuthorAssociation> {
    let Some(uid) = user_id else {
        return Ok(CommentAuthorAssociation::None);
    };
    let l = ctx.data_unchecked::<Loaders>();
    Ok(match one(&l.associations, (repo_id, uid)).await? {
        Some("OWNER") => CommentAuthorAssociation::Owner,
        Some("MEMBER") => CommentAuthorAssociation::Member,
        Some("COLLABORATOR") => CommentAuthorAssociation::Collaborator,
        Some("CONTRIBUTOR") => CommentAuthorAssociation::Contributor,
        _ => CommentAuthorAssociation::None,
    })
}

pub fn render_html(ctx: &Context<'_>, repo: &Repository, body: &str) -> HTML {
    let g = gql(ctx);
    let r = repo.row();
    HTML(markdown::render(
        body,
        &RenderContext::new(&g.state.config.base_url).with_repo(&r.owner.login, &r.repo.name),
    ))
}

pub fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
        .trim()
        .to_string()
}

fn edited(created: chrono::DateTime<chrono::Utc>, updated: chrono::DateTime<chrono::Utc>) -> bool {
    (updated - created).num_seconds() > 1
}

#[Object]
impl Conversation {
    pub async fn database_id(&self) -> Option<i64> {
        Some(self.i.id)
    }
    pub async fn full_database_id(&self) -> Option<String> {
        Some(self.i.id.to_string())
    }
    pub async fn number(&self) -> i32 {
        self.i.number as i32
    }
    pub async fn title(&self) -> String {
        self.i.title.clone()
    }
    #[graphql(name = "titleHTML")]
    pub async fn title_html(&self) -> String {
        repo::html_escape(&self.i.title)
    }
    pub async fn body(&self) -> String {
        self.i.body.clone().unwrap_or_default()
    }
    #[graphql(name = "bodyHTML")]
    pub async fn body_html(&self, ctx: &Context<'_>) -> GResult<HTML> {
        let repo = repo::load_unchecked(ctx, self.i.repo_id).await?;
        Ok(render_html(
            ctx,
            &repo,
            self.i.body.as_deref().unwrap_or(""),
        ))
    }
    pub async fn body_text(&self, ctx: &Context<'_>) -> GResult<String> {
        let repo = repo::load_unchecked(ctx, self.i.repo_id).await?;
        let html = render_html(ctx, &repo, self.i.body.as_deref().unwrap_or(""));
        Ok(html_to_text(&html.0))
    }
    pub async fn url(&self, ctx: &Context<'_>) -> GResult<URI> {
        let repo = repo::load_unchecked(ctx, self.i.repo_id).await?;
        Ok(URI(gql(ctx).state.urls.html(&self.html_path(&repo))))
    }
    pub async fn resource_path(&self, ctx: &Context<'_>) -> GResult<URI> {
        let repo = repo::load_unchecked(ctx, self.i.repo_id).await?;
        Ok(URI(self.html_path(&repo)))
    }
    pub async fn closed(&self) -> bool {
        self.i.state == "closed"
    }
    pub async fn closed_at(&self) -> Option<DateTime> {
        odt(self.i.closed_at)
    }
    pub async fn created_at(&self) -> DateTime {
        dt(self.i.created_at)
    }
    pub async fn updated_at(&self) -> DateTime {
        dt(self.i.updated_at)
    }
    pub async fn last_edited_at(&self) -> Option<DateTime> {
        None
    }
    pub async fn published_at(&self) -> Option<DateTime> {
        Some(dt(self.i.created_at))
    }
    pub async fn includes_created_edit(&self) -> bool {
        false
    }
    pub async fn author(&self, ctx: &Context<'_>) -> GResult<Option<Actor>> {
        actor::actor(ctx, self.i.author_id).await
    }
    pub async fn editor(&self) -> Option<Actor> {
        None
    }
    pub async fn author_association(&self, ctx: &Context<'_>) -> GResult<CommentAuthorAssociation> {
        association(ctx, self.i.repo_id, self.i.author_id).await
    }
    pub async fn locked(&self) -> bool {
        self.i.locked
    }
    pub async fn active_lock_reason(&self) -> Option<LockReason> {
        self.i
            .active_lock_reason
            .as_deref()
            .and_then(LockReason::from_db)
    }
    pub async fn repository(&self, ctx: &Context<'_>) -> GResult<Repository> {
        repo::load_unchecked(ctx, self.i.repo_id).await
    }
    pub async fn assignees(
        &self,
        ctx: &Context<'_>,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
    ) -> GResult<UserConnection> {
        let users = assignees(ctx, self.i.id).await?;
        Ok(Page::from_vec(users, &ConnArgs::new(first, last, after, before))?.into())
    }
    pub async fn labels(
        &self,
        ctx: &Context<'_>,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
        order_by: Option<LabelOrder>,
    ) -> GResult<LabelConnection> {
        let _ = order_by;
        let l = ctx.data_unchecked::<Loaders>();
        let labels = one(&l.issue_labels, self.i.id).await?.unwrap_or_default();
        let items = labels.iter().map(|x| Label(Arc::new(x.clone()))).collect();
        Ok(Page::from_vec(items, &ConnArgs::new(first, last, after, before))?.into())
    }
    pub async fn milestone(&self, ctx: &Context<'_>) -> GResult<Option<Milestone>> {
        let Some(id) = self.i.milestone_id else {
            return Ok(None);
        };
        let l = ctx.data_unchecked::<Loaders>();
        Ok(one(&l.milestones, id).await?.map(Milestone))
    }
    #[allow(clippy::too_many_arguments)]
    pub async fn comments(
        &self,
        ctx: &Context<'_>,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
    ) -> GResult<IssueCommentConnection> {
        let args = ConnArgs::new(first, last, after, before);
        let total = self.i.comments_count;
        if !wants(ctx).nodes {
            return Ok(Page::count_only(total).into());
        }
        let w = args.window(Some(total))?;
        let l = ctx.data_unchecked::<Loaders>();
        let rows = one(
            &l.comments,
            CommentsKey {
                issue_id: self.i.id,
                offset: w.offset,
                limit: w.limit,
            },
        )
        .await?
        .unwrap_or_default();
        let items: Vec<IssueComment> = rows
            .iter()
            .map(|c| IssueComment(Arc::new(c.clone())))
            .collect();
        let has_next = w.offset + (items.len() as i64) < total;
        Ok(Page {
            items,
            offset: w.offset,
            has_next,
            total,
        }
        .into())
    }
    pub async fn reaction_groups(&self, ctx: &Context<'_>) -> GResult<Option<Vec<ReactionGroup>>> {
        Ok(Some(load_reaction_groups(ctx, "issue", self.i.id).await?))
    }
    pub async fn project_cards(
        &self,
        first: Option<i32>,
        after: Option<String>,
        archived_states: Option<Vec<Option<String>>>,
    ) -> ProjectCardConnection {
        let _ = (first, after, archived_states);
        ProjectCardConnection
    }
    pub async fn project_items(
        &self,
        first: Option<i32>,
        after: Option<String>,
        include_archived: Option<bool>,
    ) -> ProjectV2ItemConnection {
        let _ = (first, after, include_archived);
        ProjectV2ItemConnection
    }
    pub async fn assigned_actors(
        &self,
        ctx: &Context<'_>,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
    ) -> GResult<AssigneeConnection> {
        let users = assignees(ctx, self.i.id).await?;
        let items = users.into_iter().map(Assignee::User).collect();
        Ok(Page::from_vec(items, &ConnArgs::new(first, last, after, before))?.into())
    }
    pub async fn participants(
        &self,
        ctx: &Context<'_>,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
    ) -> GResult<UserConnection> {
        let g = gql(ctx);
        let ids: Vec<i64> = sqlx::query_scalar(
            "SELECT DISTINCT u FROM (
               SELECT author_id AS u FROM issues WHERE id = $1
               UNION SELECT author_id FROM comments WHERE issue_id = $1
             ) x WHERE u IS NOT NULL",
        )
        .bind(self.i.id)
        .fetch_all(&g.state.db)
        .await
        .gql()?;
        let l = ctx.data_unchecked::<Loaders>();
        let found = many(&l.users, ids.clone()).await?;
        let users = ids
            .iter()
            .filter_map(|i| found.get(i))
            .map(|u| User(u.clone()))
            .collect();
        Ok(Page::from_vec(users, &ConnArgs::new(first, last, after, before))?.into())
    }
    pub async fn viewer_did_author(&self, ctx: &Context<'_>) -> bool {
        gql(ctx).viewer_id().is_some() && gql(ctx).viewer_id() == self.i.author_id
    }
    pub async fn viewer_can_react(&self, ctx: &Context<'_>) -> bool {
        gql(ctx).auth.is_some()
    }
    pub async fn viewer_can_update(&self, ctx: &Context<'_>) -> GResult<bool> {
        let g = gql(ctx);
        if g.viewer_id().is_none() {
            return Ok(false);
        }
        if g.viewer_id() == self.i.author_id {
            return Ok(true);
        }
        let repo = repo::load_unchecked(ctx, self.i.repo_id).await?;
        Ok(repo.row().perm >= Permission::Triage)
    }
    pub async fn viewer_subscription(
        &self,
        ctx: &Context<'_>,
    ) -> GResult<Option<SubscriptionState>> {
        let g = gql(ctx);
        let Some(v) = g.viewer_id() else {
            return Ok(None);
        };
        let subscribed: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM thread_subscriptions WHERE user_id = $1 AND subject_id = $2)",
        )
        .bind(v)
        .bind(self.i.id)
        .fetch_one(&g.state.db)
        .await
        .unwrap_or(false);
        Ok(Some(if subscribed {
            SubscriptionState::Subscribed
        } else {
            SubscriptionState::Unsubscribed
        }))
    }
}

pub async fn assignees(ctx: &Context<'_>, issue_id: i64) -> GResult<Vec<User>> {
    let l = ctx.data_unchecked::<Loaders>();
    let ids = one(&l.issue_assignees, issue_id).await?.unwrap_or_default();
    let found = many(&l.users, ids.iter().copied()).await?;
    Ok(ids
        .iter()
        .filter_map(|i| found.get(i))
        .map(|u| User(u.clone()))
        .collect())
}

// ---------------------------------------------------------------------------
// Issue
// ---------------------------------------------------------------------------

/// Issue-only fields.
#[derive(Clone)]
pub struct IssueOnly {
    pub i: Arc<db::Issue>,
}

#[Object]
impl IssueOnly {
    pub async fn id(&self) -> ID {
        nid(NodeType::Issue, self.i.id)
    }
    pub async fn state(&self) -> IssueState {
        if self.i.state == "closed" {
            IssueState::Closed
        } else {
            IssueState::Open
        }
    }
    pub async fn state_reason(&self) -> Option<IssueStateReason> {
        self.i
            .state_reason
            .as_deref()
            .and_then(IssueStateReason::from_db)
    }
    pub async fn is_pinned(&self, ctx: &Context<'_>) -> GResult<Option<bool>> {
        let g = gql(ctx);
        let pinned: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM information_schema.tables WHERE table_name = 'pinned_issues')",
        )
        .fetch_one(&g.state.db)
        .await
        .gql()?;
        if !pinned {
            return Ok(Some(false));
        }
        let v: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pinned_issues WHERE issue_id = $1)")
                .bind(self.i.id)
                .fetch_one(&g.state.db)
                .await
                .unwrap_or(false);
        Ok(Some(v))
    }
    pub async fn closed_by_pull_requests_references(
        &self,
        ctx: &Context<'_>,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
        #[graphql(default)] include_closed_prs: bool,
    ) -> GResult<PullRequestConnection> {
        let _ = include_closed_prs;
        // Open pull requests of the same repository whose body closes this issue.
        let g = gql(ctx);
        let candidates: Vec<db::Issue> = sqlx::query_as(&format!(
            "SELECT {} FROM issues WHERE repo_id = $1 AND is_pull_request
                AND body ~* ('(close[sd]?|fix(e[sd])?|resolve[sd]?):?\\s+#' || $2::text || '\\M')
              ORDER BY number",
            db::Issue::COLUMNS
        ))
        .bind(self.i.repo_id)
        .bind(self.i.number.to_string())
        .fetch_all(&g.state.db)
        .await
        .gql()?;
        let items = super::pull::from_issues(ctx, candidates).await?;
        Ok(Page::from_vec(items, &ConnArgs::new(first, last, after, before))?.into())
    }
    pub async fn linked_branches(
        &self,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
    ) -> LinkedBranchConnection {
        let _ = (first, last, after, before);
        LinkedBranchConnection
    }
    pub async fn tracked_issues(&self) -> crate::model::actor::CountOnly {
        crate::model::actor::CountOnly(0)
    }
}

/// An Issue is a place to discuss ideas, enhancements, tasks, and bugs.
#[derive(MergedObject, Clone)]
pub struct Issue(pub IssueOnly, pub Conversation);

impl Issue {
    pub fn new(i: Arc<db::Issue>) -> Self {
        Self(IssueOnly { i: i.clone() }, Conversation { i })
    }
    pub fn row(&self) -> &db::Issue {
        &self.1.i
    }
    /// `Node.id` (MergedObject types need it as an inherent method).
    pub async fn id(&self, _ctx: &Context<'_>) -> GResult<ID> {
        Ok(nid(NodeType::Issue, self.1.i.id))
    }
}

connection!(IssueConnection, IssueEdge, Issue);

/// Users (and bots) assigned to an item.
#[derive(Union, Clone)]
pub enum Assignee {
    User(User),
    Bot(super::Bot),
    Mannequin(super::Mannequin),
    Organization(super::Organization),
}

connection!(AssigneeConnection, AssigneeEdge, Assignee);

/// Branches linked to an issue (`gh issue develop`); none are tracked.
#[derive(Default)]
pub struct LinkedBranchConnection;

#[Object]
impl LinkedBranchConnection {
    pub async fn nodes(&self) -> Vec<LinkedBranch> {
        vec![]
    }
    pub async fn total_count(&self) -> i32 {
        0
    }
}

#[derive(async_graphql::SimpleObject, Clone)]
pub struct LinkedBranch {
    pub id: ID,
    #[graphql(name = "ref")]
    pub ref_: Option<super::Ref>,
}

/// Either an issue or a pull request.
#[derive(Union, Clone)]
pub enum IssueOrPullRequest {
    Issue(Issue),
    PullRequest(PullRequest),
}

pub async fn by_number(
    ctx: &Context<'_>,
    repo: &Repository,
    number: i64,
    allow_pr: bool,
) -> GResult<Option<Issue>> {
    let g = gql(ctx);
    let row: Option<db::Issue> = sqlx::query_as(&format!(
        "SELECT {} FROM issues WHERE repo_id = $1 AND number = $2",
        db::Issue::COLUMNS
    ))
    .bind(repo.rid())
    .bind(number)
    .fetch_optional(&g.state.db)
    .await
    .gql()?;
    match row {
        Some(i) if !i.is_pull_request || allow_pr => Ok(Some(Issue::new(Arc::new(i)))),
        _ => Err(crate::ctx::not_found(format!(
            "Could not resolve to an Issue with the number of {number}."
        ))),
    }
}

pub async fn issue_or_pull(
    ctx: &Context<'_>,
    repo: &Repository,
    number: i64,
) -> GResult<Option<IssueOrPullRequest>> {
    let g = gql(ctx);
    let row: Option<db::Issue> = sqlx::query_as(&format!(
        "SELECT {} FROM issues WHERE repo_id = $1 AND number = $2",
        db::Issue::COLUMNS
    ))
    .bind(repo.rid())
    .bind(number)
    .fetch_optional(&g.state.db)
    .await
    .gql()?;
    let Some(i) = row else {
        return Err(crate::ctx::not_found(format!(
            "Could not resolve to an issue or pull request with the number of {number}."
        )));
    };
    if i.is_pull_request {
        Ok(super::pull::from_issues(ctx, vec![i])
            .await?
            .into_iter()
            .next()
            .map(IssueOrPullRequest::PullRequest))
    } else {
        Ok(Some(IssueOrPullRequest::Issue(Issue::new(Arc::new(i)))))
    }
}

/// Common filter → SQL for issue/PR lists on `issues i`.
pub struct ListFilter {
    pub is_pr: bool,
    pub states: Vec<&'static str>,
    pub merged: Option<bool>,
    pub labels: Vec<String>,
    pub assignee: Option<String>,
    pub created_by: Option<String>,
    pub mentioned: Option<String>,
    pub milestone: Option<String>,
    pub milestone_number: Option<String>,
    pub since: Option<chrono::DateTime<chrono::Utc>>,
    pub viewer_subscribed: bool,
    pub head_ref: Option<String>,
    pub base_ref: Option<String>,
    pub order: Option<IssueOrder>,
}

impl ListFilter {
    pub fn new(is_pr: bool) -> Self {
        Self {
            is_pr,
            states: vec![],
            merged: None,
            labels: vec![],
            assignee: None,
            created_by: None,
            mentioned: None,
            milestone: None,
            milestone_number: None,
            since: None,
            viewer_subscribed: false,
            head_ref: None,
            base_ref: None,
            order: None,
        }
    }

    fn push_where(&self, qb: &mut QueryBuilder<'_, Postgres>, repo_id: i64, viewer: Option<i64>) {
        qb.push(" WHERE i.repo_id = ")
            .push_bind(repo_id)
            .push(" AND i.is_pull_request = ")
            .push_bind(self.is_pr);
        if !self.states.is_empty() || self.merged.is_some() {
            // PR states: OPEN, CLOSED (not merged), MERGED.
            let mut parts: Vec<String> = vec![];
            for s in &self.states {
                parts.push(match (*s, self.is_pr) {
                    ("open", _) => "i.state = 'open'".into(),
                    ("closed", true) => "(i.state = 'closed' AND NOT p.merged)".into(),
                    ("closed", false) => "i.state = 'closed'".into(),
                    ("merged", _) => "p.merged".into(),
                    _ => "false".into(),
                });
            }
            if !parts.is_empty() {
                qb.push(format!(" AND ({})", parts.join(" OR ")));
            }
        }
        if !self.labels.is_empty() {
            let lower: Vec<String> = self.labels.iter().map(|l| l.to_lowercase()).collect();
            qb.push(
                " AND EXISTS (SELECT 1 FROM issue_labels il JOIN labels l ON l.id = il.label_id
                    WHERE il.issue_id = i.id AND lower(l.name) = ANY(",
            )
            .push_bind(lower)
            .push("))");
        }
        if let Some(a) = &self.assignee {
            match a.as_str() {
                "*" => {
                    qb.push(
                        " AND EXISTS (SELECT 1 FROM issue_assignees ia WHERE ia.issue_id = i.id)",
                    );
                }
                "none" => {
                    qb.push(
                        " AND NOT EXISTS (SELECT 1 FROM issue_assignees ia WHERE ia.issue_id = i.id)",
                    );
                }
                login => {
                    qb.push(
                        " AND EXISTS (SELECT 1 FROM issue_assignees ia JOIN users u ON u.id = ia.user_id
                           WHERE ia.issue_id = i.id AND lower(u.login) = lower(",
                    )
                    .push_bind(login.to_string())
                    .push("))");
                }
            }
        }
        if let Some(c) = &self.created_by {
            qb.push(" AND i.author_id = (SELECT id FROM users WHERE lower(login) = lower(")
                .push_bind(c.clone())
                .push("))");
        }
        if let Some(m) = &self.mentioned {
            qb.push(" AND (i.body ILIKE ")
                .push_bind(format!("%@{m}%"))
                .push(" OR EXISTS (SELECT 1 FROM comments c WHERE c.issue_id = i.id AND c.body ILIKE ")
                .push_bind(format!("%@{m}%"))
                .push("))");
        }
        if let Some(n) = &self.milestone_number {
            if let Ok(n) = n.parse::<i64>() {
                qb.push(" AND i.milestone_id = (SELECT id FROM milestones WHERE repo_id = i.repo_id AND number = ")
                    .push_bind(n)
                    .push(")");
            }
        } else if let Some(m) = &self.milestone {
            match m.as_str() {
                "*" => {
                    qb.push(" AND i.milestone_id IS NOT NULL");
                }
                "none" | "null" => {
                    qb.push(" AND i.milestone_id IS NULL");
                }
                title => {
                    qb.push(
                        " AND i.milestone_id IN (SELECT id FROM milestones WHERE repo_id = i.repo_id
                           AND (lower(title) = lower(",
                    )
                    .push_bind(title.to_string())
                    .push(") OR number::text = ")
                    .push_bind(title.to_string())
                    .push("))");
                }
            }
        }
        if let Some(s) = self.since {
            qb.push(" AND i.updated_at >= ").push_bind(s);
        }
        if self.viewer_subscribed {
            qb.push(" AND EXISTS (SELECT 1 FROM thread_subscriptions ts WHERE ts.subject_id = i.id AND ts.user_id = ")
                .push_bind(viewer.unwrap_or(0))
                .push(")");
        }
        if let Some(h) = &self.head_ref {
            qb.push(" AND p.head_ref = ").push_bind(h.clone());
        }
        if let Some(b) = &self.base_ref {
            qb.push(" AND p.base_ref = ").push_bind(b.clone());
        }
    }

    fn order_sql(&self) -> String {
        let o = self.order.unwrap_or(IssueOrder {
            field: IssueOrderField::CreatedAt,
            direction: OrderDirection::Asc,
        });
        let col = match o.field {
            IssueOrderField::CreatedAt => "i.created_at",
            IssueOrderField::UpdatedAt => "i.updated_at",
            IssueOrderField::Comments => "i.comments_count",
        };
        format!(" ORDER BY {col} {d}, i.id {d}", d = o.direction.sql())
    }
}

/// Run a filtered, ordered, windowed list query over `issues i` (joined
/// with `pull_requests p` for PRs). Returns the page of issue rows.
pub async fn run_list(
    ctx: &Context<'_>,
    repo_id: i64,
    f: &ListFilter,
    args: &ConnArgs,
) -> GResult<Page<db::Issue>> {
    let g = gql(ctx);
    let w = wants(ctx);
    let join = if f.is_pr {
        " JOIN pull_requests p ON p.issue_id = i.id"
    } else {
        ""
    };
    let need_total = w.total || args.needs_total() || !w.nodes;
    let total = if need_total {
        let mut qb: QueryBuilder<Postgres> =
            QueryBuilder::new(format!("SELECT count(*) FROM issues i{join}"));
        f.push_where(&mut qb, repo_id, g.viewer_id());
        Some(
            qb.build_query_scalar::<i64>()
                .fetch_one(&g.state.db)
                .await
                .gql()?,
        )
    } else {
        None
    };
    if !w.nodes {
        return Ok(Page::count_only(total.unwrap_or(0)));
    }
    let win = args.window(total)?;
    let mut qb: QueryBuilder<Postgres> = QueryBuilder::new(format!(
        "SELECT {} FROM issues i{join}",
        db::prefixed("i", db::Issue::COLUMNS)
    ));
    f.push_where(&mut qb, repo_id, g.viewer_id());
    qb.push(f.order_sql());
    qb.push(" LIMIT ").push_bind(win.fetch());
    qb.push(" OFFSET ").push_bind(win.offset);
    let rows: Vec<db::Issue> = qb.build_query_as().fetch_all(&g.state.db).await.gql()?;
    Ok(Page::from_fetched(rows, win, total))
}

#[allow(clippy::too_many_arguments)]
pub async fn list_for_repo(
    ctx: &Context<'_>,
    repo: &Repository,
    args: ConnArgs,
    states: Option<Vec<IssueState>>,
    labels: Option<Vec<String>>,
    order_by: Option<IssueOrder>,
    filter_by: IssueFilters,
) -> GResult<IssueConnection> {
    let mut f = ListFilter::new(false);
    let states = filter_by.states.clone().or(states).unwrap_or_default();
    f.states = states
        .iter()
        .map(|s| match s {
            IssueState::Open => "open",
            IssueState::Closed => "closed",
        })
        .collect();
    f.labels = filter_by.labels.clone().or(labels).unwrap_or_default();
    f.assignee = filter_by.assignee;
    f.created_by = filter_by.created_by;
    f.mentioned = filter_by.mentioned;
    f.milestone = filter_by.milestone;
    f.milestone_number = filter_by.milestone_number;
    f.since = filter_by.since.map(|d| d.0);
    f.viewer_subscribed = filter_by.viewer_subscribed.unwrap_or(false);
    f.order = order_by;
    let page = run_list(ctx, repo.rid(), &f, &args).await?;
    Ok(page.map(|i| Issue::new(Arc::new(i))).into())
}

// ---------------------------------------------------------------------------
// Comments
// ---------------------------------------------------------------------------

/// Represents a comment on an Issue.
#[derive(Clone)]
pub struct IssueComment(pub Arc<db::Comment>);

#[Object]
impl IssueComment {
    pub async fn id(&self) -> ID {
        nid(NodeType::IssueComment, self.0.id)
    }
    pub async fn database_id(&self) -> Option<i64> {
        Some(self.0.id)
    }
    pub async fn full_database_id(&self) -> Option<String> {
        Some(self.0.id.to_string())
    }
    pub async fn author(&self, ctx: &Context<'_>) -> GResult<Option<Actor>> {
        actor::actor(ctx, self.0.author_id).await
    }
    pub async fn editor(&self) -> Option<Actor> {
        None
    }
    pub async fn author_association(&self, ctx: &Context<'_>) -> GResult<CommentAuthorAssociation> {
        association(ctx, self.0.repo_id, self.0.author_id).await
    }
    pub async fn body(&self) -> String {
        self.0.body.clone()
    }
    #[graphql(name = "bodyHTML")]
    pub async fn body_html(&self, ctx: &Context<'_>) -> GResult<HTML> {
        let repo = repo::load_unchecked(ctx, self.0.repo_id).await?;
        Ok(render_html(ctx, &repo, &self.0.body))
    }
    pub async fn body_text(&self, ctx: &Context<'_>) -> GResult<String> {
        let repo = repo::load_unchecked(ctx, self.0.repo_id).await?;
        Ok(html_to_text(&render_html(ctx, &repo, &self.0.body).0))
    }
    pub async fn created_at(&self) -> DateTime {
        dt(self.0.created_at)
    }
    pub async fn updated_at(&self) -> DateTime {
        dt(self.0.updated_at)
    }
    pub async fn published_at(&self) -> Option<DateTime> {
        Some(dt(self.0.created_at))
    }
    pub async fn last_edited_at(&self) -> Option<DateTime> {
        edited(self.0.created_at, self.0.updated_at).then(|| dt(self.0.updated_at))
    }
    pub async fn includes_created_edit(&self) -> bool {
        edited(self.0.created_at, self.0.updated_at)
    }
    pub async fn is_minimized(&self) -> bool {
        false
    }
    pub async fn minimized_reason(&self) -> Option<String> {
        None
    }
    pub async fn reaction_groups(&self, ctx: &Context<'_>) -> GResult<Option<Vec<ReactionGroup>>> {
        Ok(Some(
            load_reaction_groups(ctx, "issue_comment", self.0.id).await?,
        ))
    }
    pub async fn url(&self, ctx: &Context<'_>) -> GResult<URI> {
        let issue = self.issue_row(ctx).await?;
        let repo = repo::load_unchecked(ctx, self.0.repo_id).await?;
        let r = repo.row();
        let kind = if issue.is_pull_request {
            "pull"
        } else {
            "issues"
        };
        Ok(URI(gql(ctx).state.urls.html(&format!(
            "/{}/{}/{kind}/{}#issuecomment-{}",
            r.owner.login, r.repo.name, issue.number, self.0.id
        ))))
    }
    pub async fn viewer_did_author(&self, ctx: &Context<'_>) -> bool {
        gql(ctx).viewer_id().is_some() && gql(ctx).viewer_id() == self.0.author_id
    }
    pub async fn viewer_can_update(&self, ctx: &Context<'_>) -> GResult<bool> {
        let g = gql(ctx);
        if g.viewer_id().is_none() {
            return Ok(false);
        }
        if g.viewer_id() == self.0.author_id {
            return Ok(true);
        }
        let repo = repo::load_unchecked(ctx, self.0.repo_id).await?;
        Ok(repo.row().perm >= Permission::Write)
    }
    pub async fn viewer_can_delete(&self, ctx: &Context<'_>) -> GResult<bool> {
        let g = gql(ctx);
        if g.viewer_id().is_none() {
            return Ok(false);
        }
        if g.viewer_id() == self.0.author_id {
            return Ok(true);
        }
        let repo = repo::load_unchecked(ctx, self.0.repo_id).await?;
        Ok(repo.row().perm >= Permission::Write)
    }
    pub async fn viewer_can_react(&self, ctx: &Context<'_>) -> bool {
        gql(ctx).auth.is_some()
    }
    pub async fn viewer_can_minimize(&self) -> bool {
        false
    }
    pub async fn issue(&self, ctx: &Context<'_>) -> GResult<Issue> {
        Ok(Issue::new(self.issue_row(ctx).await?))
    }
    pub async fn pull_request(&self, ctx: &Context<'_>) -> GResult<Option<PullRequest>> {
        let issue = self.issue_row(ctx).await?;
        if !issue.is_pull_request {
            return Ok(None);
        }
        Ok(super::pull::from_issues(ctx, vec![(*issue).clone()])
            .await?
            .into_iter()
            .next())
    }
    pub async fn repository(&self, ctx: &Context<'_>) -> GResult<Repository> {
        repo::load_unchecked(ctx, self.0.repo_id).await
    }
}

impl IssueComment {
    pub async fn issue_row(&self, ctx: &Context<'_>) -> GResult<Arc<db::Issue>> {
        let l = ctx.data_unchecked::<Loaders>();
        one(&l.issues, self.0.issue_id)
            .await?
            .ok_or_else(|| crate::ctx::not_found("issue not found"))
    }
}

connection!(IssueCommentConnection, IssueCommentEdge, IssueComment);

// ---------------------------------------------------------------------------
// Labels
// ---------------------------------------------------------------------------

/// A label for categorizing Issues, Pull Requests, Milestones, or
/// Discussions with a given Repository.
#[derive(Clone)]
pub struct Label(pub Arc<db::Label>);

#[Object]
impl Label {
    pub async fn id(&self) -> ID {
        nid(NodeType::Label, self.0.id)
    }
    pub async fn name(&self) -> String {
        self.0.name.clone()
    }
    pub async fn color(&self) -> String {
        self.0.color.clone()
    }
    pub async fn description(&self) -> Option<String> {
        self.0.description.clone()
    }
    pub async fn is_default(&self) -> bool {
        self.0.is_default
    }
    pub async fn created_at(&self) -> Option<DateTime> {
        Some(dt(self.0.created_at))
    }
    pub async fn updated_at(&self) -> Option<DateTime> {
        Some(dt(self.0.updated_at))
    }
    pub async fn url(&self, ctx: &Context<'_>) -> GResult<URI> {
        let repo = repo::load_unchecked(ctx, self.0.repo_id).await?;
        Ok(URI(gql(ctx).state.urls.html(&format!(
            "/{}/labels/{}",
            repo.row().full_name(),
            bgh_core::urls::encode_segment(&self.0.name)
        ))))
    }
    pub async fn resource_path(&self, ctx: &Context<'_>) -> GResult<URI> {
        let repo = repo::load_unchecked(ctx, self.0.repo_id).await?;
        Ok(URI(format!(
            "/{}/labels/{}",
            repo.row().full_name(),
            bgh_core::urls::encode_segment(&self.0.name)
        )))
    }
    pub async fn repository(&self, ctx: &Context<'_>) -> GResult<Repository> {
        repo::load_unchecked(ctx, self.0.repo_id).await
    }
    pub async fn issues(&self, ctx: &Context<'_>) -> GResult<crate::model::actor::CountOnly> {
        let n: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM issue_labels il JOIN issues i ON i.id = il.issue_id
              WHERE il.label_id = $1 AND NOT i.is_pull_request",
        )
        .bind(self.0.id)
        .fetch_one(&gql(ctx).state.db)
        .await
        .gql()?;
        Ok(crate::model::actor::CountOnly(n))
    }
}

connection!(LabelConnection, LabelEdge, Label);

pub async fn repo_labels(
    ctx: &Context<'_>,
    repo: &Repository,
    args: ConnArgs,
    query: Option<String>,
    order_by: Option<LabelOrder>,
) -> GResult<LabelConnection> {
    let g = gql(ctx);
    let order = order_by.unwrap_or(LabelOrder {
        field: LabelOrderField::Name,
        direction: OrderDirection::Asc,
    });
    let col = match order.field {
        LabelOrderField::Name => "lower(name)",
        LabelOrderField::CreatedAt => "created_at",
    };
    let rows: Vec<db::Label> = sqlx::query_as(&format!(
        "SELECT {} FROM labels WHERE repo_id = $1
            AND ($2::text IS NULL OR lower(name) LIKE '%' || lower($2) || '%'
                 OR lower(coalesce(description, '')) LIKE '%' || lower($2) || '%')
          ORDER BY {col} {d}, id {d}",
        db::Label::COLUMNS,
        d = order.direction.sql()
    ))
    .bind(repo.rid())
    .bind(query.filter(|q| !q.is_empty()))
    .fetch_all(&g.state.db)
    .await
    .gql()?;
    let items = rows.into_iter().map(|l| Label(Arc::new(l))).collect();
    Ok(Page::from_vec(items, &args)?.into())
}

pub async fn repo_label(
    ctx: &Context<'_>,
    repo: &Repository,
    name: &str,
) -> GResult<Option<Label>> {
    let row: Option<db::Label> = sqlx::query_as(&format!(
        "SELECT {} FROM labels WHERE repo_id = $1 AND lower(name) = lower($2)",
        db::Label::COLUMNS
    ))
    .bind(repo.rid())
    .bind(name)
    .fetch_optional(&gql(ctx).state.db)
    .await
    .gql()?;
    Ok(row.map(|l| Label(Arc::new(l))))
}

// ---------------------------------------------------------------------------
// Milestones
// ---------------------------------------------------------------------------

/// Represents a Milestone object on a given repository.
#[derive(Clone)]
pub struct Milestone(pub Arc<db::Milestone>);

#[Object]
impl Milestone {
    pub async fn id(&self) -> ID {
        nid(NodeType::Milestone, self.0.id)
    }
    pub async fn number(&self) -> i32 {
        self.0.number as i32
    }
    pub async fn title(&self) -> String {
        self.0.title.clone()
    }
    pub async fn description(&self) -> Option<String> {
        self.0.description.clone()
    }
    pub async fn due_on(&self) -> Option<DateTime> {
        odt(self.0.due_on)
    }
    pub async fn state(&self) -> MilestoneState {
        if self.0.state == "closed" {
            MilestoneState::Closed
        } else {
            MilestoneState::Open
        }
    }
    pub async fn closed(&self) -> bool {
        self.0.state == "closed"
    }
    pub async fn closed_at(&self) -> Option<DateTime> {
        odt(self.0.closed_at)
    }
    pub async fn created_at(&self) -> DateTime {
        dt(self.0.created_at)
    }
    pub async fn updated_at(&self) -> DateTime {
        dt(self.0.updated_at)
    }
    pub async fn progress_percentage(&self) -> f64 {
        let total = self.0.open_issues + self.0.closed_issues;
        if total == 0 {
            0.0
        } else {
            self.0.closed_issues as f64 * 100.0 / total as f64
        }
    }
    pub async fn creator(&self, ctx: &Context<'_>) -> GResult<Option<Actor>> {
        actor::actor(ctx, self.0.creator_id).await
    }
    pub async fn url(&self, ctx: &Context<'_>) -> GResult<URI> {
        let repo = repo::load_unchecked(ctx, self.0.repo_id).await?;
        let r = repo.row();
        Ok(URI(gql(ctx).state.urls.milestone_html(
            &r.owner.login,
            &r.repo.name,
            self.0.number,
        )))
    }
    pub async fn repository(&self, ctx: &Context<'_>) -> GResult<Repository> {
        repo::load_unchecked(ctx, self.0.repo_id).await
    }
}

connection!(MilestoneConnection, MilestoneEdge, Milestone);

pub async fn repo_milestones(
    ctx: &Context<'_>,
    repo: &Repository,
    args: ConnArgs,
    states: Option<Vec<MilestoneState>>,
    order_by: Option<MilestoneOrder>,
    query: Option<String>,
) -> GResult<MilestoneConnection> {
    let g = gql(ctx);
    let states: Vec<&str> = states
        .unwrap_or_default()
        .iter()
        .map(|s| match s {
            MilestoneState::Open => "open",
            MilestoneState::Closed => "closed",
        })
        .collect();
    let order = order_by.unwrap_or(MilestoneOrder {
        field: MilestoneOrderField::Number,
        direction: OrderDirection::Asc,
    });
    let col = match order.field {
        MilestoneOrderField::DueDate => "due_on",
        MilestoneOrderField::CreatedAt => "created_at",
        MilestoneOrderField::UpdatedAt => "updated_at",
        MilestoneOrderField::Number => "number",
    };
    let rows: Vec<db::Milestone> = sqlx::query_as(&format!(
        "SELECT {} FROM milestones WHERE repo_id = $1
            AND (cardinality($2::text[]) = 0 OR state = ANY($2))
            AND ($3::text IS NULL OR lower(title) LIKE '%' || lower($3) || '%')
          ORDER BY {col} {d}, id {d}",
        db::Milestone::COLUMNS,
        d = order.direction.sql()
    ))
    .bind(repo.rid())
    .bind(&states)
    .bind(query.filter(|q| !q.is_empty()))
    .fetch_all(&g.state.db)
    .await
    .gql()?;
    let items = rows.into_iter().map(|m| Milestone(Arc::new(m))).collect();
    Ok(Page::from_vec(items, &args)?.into())
}

pub async fn repo_milestone(
    ctx: &Context<'_>,
    repo: &Repository,
    number: i64,
) -> GResult<Option<Milestone>> {
    let row: Option<db::Milestone> = sqlx::query_as(&format!(
        "SELECT {} FROM milestones WHERE repo_id = $1 AND number = $2",
        db::Milestone::COLUMNS
    ))
    .bind(repo.rid())
    .bind(number)
    .fetch_optional(&gql(ctx).state.db)
    .await
    .gql()?;
    Ok(row.map(|m| Milestone(Arc::new(m))))
}
