//! Projects (v2): `ProjectV2`, its fields, items, field values, views and
//! draft issues, read straight from the bgh-projects tables. Visibility
//! and roles come from `bgh_projects::access`; writes live in
//! `mutation/projects.rs`.

use std::collections::HashMap;
use std::sync::Arc;

use async_graphql::{Context, Enum, ID, InputObject, Interface, Object, Union};
use bgh_core::node_id::{self, NodeType};
use bgh_core::prelude::*;
use bgh_projects::access::{Role, owner_role, project_role};
use bgh_projects::model::{FieldRow, ItemRow, ProjectRow, ViewRow};
use bgh_projects::rest::{iteration_completed, project_html_path};
use serde_json::Value;

use super::actor::{TeamConnection, UserConnection};
use super::enums::{OrderDirection, ProjectV2Order, ProjectV2OrderField};
use super::issue::LabelConnection;
use super::pull::{PullRequestConnection, RequestedReviewer, from_issues};
use super::repo::{self, RepositoryConnection};
use super::{Actor, Issue, Label, Milestone, Node, Organization, PullRequest, Team, User, nid};
use crate::conn::{ConnArgs, Page, connection};
use crate::ctx::{GResult, OrGql, gql, not_found};
use crate::loaders::{Loaders, many, one};
use crate::scalars::{Date, DateTime, HTML, URI, dt, odt};

// ---------------------------------------------------------------------------
// Enums and inputs
// ---------------------------------------------------------------------------

/// The type of a project field.
#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug)]
#[graphql(name = "ProjectV2FieldType")]
pub enum ProjectV2FieldType {
    Assignees,
    LinkedPullRequests,
    Reviewers,
    Labels,
    Milestone,
    Repository,
    Title,
    Text,
    SingleSelect,
    Number,
    Date,
    Iteration,
    Tracks,
    TrackedBy,
    ParentIssue,
    SubIssuesProgress,
    IssueType,
}

impl ProjectV2FieldType {
    pub fn of(data_type: &str) -> Self {
        match data_type {
            "title" => Self::Title,
            "assignees" => Self::Assignees,
            "labels" => Self::Labels,
            "repository" => Self::Repository,
            "milestone" => Self::Milestone,
            "number" => Self::Number,
            "date" => Self::Date,
            "iteration" => Self::Iteration,
            "status" | "single_select" => Self::SingleSelect,
            _ => Self::Text,
        }
    }
}

/// The type of a custom field that can be created.
#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug)]
#[graphql(name = "ProjectV2CustomFieldType")]
pub enum ProjectV2CustomFieldType {
    Text,
    SingleSelect,
    Number,
    Date,
    Iteration,
}

impl ProjectV2CustomFieldType {
    pub fn data_type(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::SingleSelect => "single_select",
            Self::Number => "number",
            Self::Date => "date",
            Self::Iteration => "iteration",
        }
    }
}

/// Colors of single-select options.
#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug)]
#[graphql(name = "ProjectV2SingleSelectFieldOptionColor")]
pub enum ProjectV2SingleSelectFieldOptionColor {
    Gray,
    Blue,
    Green,
    Yellow,
    Orange,
    Red,
    Pink,
    Purple,
}

impl ProjectV2SingleSelectFieldOptionColor {
    pub fn parse(s: &str) -> Self {
        match s {
            "BLUE" => Self::Blue,
            "GREEN" => Self::Green,
            "YELLOW" => Self::Yellow,
            "ORANGE" => Self::Orange,
            "RED" => Self::Red,
            "PINK" => Self::Pink,
            "PURPLE" => Self::Purple,
            _ => Self::Gray,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gray => "GRAY",
            Self::Blue => "BLUE",
            Self::Green => "GREEN",
            Self::Yellow => "YELLOW",
            Self::Orange => "ORANGE",
            Self::Red => "RED",
            Self::Pink => "PINK",
            Self::Purple => "PURPLE",
        }
    }
}

/// The type of a project item.
#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug)]
#[graphql(name = "ProjectV2ItemType")]
pub enum ProjectV2ItemType {
    Issue,
    PullRequest,
    DraftIssue,
    Redacted,
}

/// The layout of a project view.
#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug)]
#[graphql(name = "ProjectV2ViewLayout")]
pub enum ProjectV2ViewLayout {
    BoardLayout,
    TableLayout,
    RoadmapLayout,
}

#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug)]
#[graphql(name = "ProjectV2FieldOrderField")]
pub enum ProjectV2FieldOrderField {
    Position,
    CreatedAt,
    Name,
}

/// Ordering of project fields (accepted; fields are always in position order).
#[derive(InputObject, Clone, Copy, Debug)]
#[graphql(name = "ProjectV2FieldOrder")]
pub struct ProjectV2FieldOrder {
    pub field: ProjectV2FieldOrderField,
    pub direction: OrderDirection,
}

#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug)]
#[graphql(name = "ProjectV2ItemOrderField")]
pub enum ProjectV2ItemOrderField {
    Position,
}

/// Ordering of project items (accepted; items are in position order).
#[derive(InputObject, Clone, Copy, Debug)]
#[graphql(name = "ProjectV2ItemOrder")]
pub struct ProjectV2ItemOrder {
    pub field: ProjectV2ItemOrderField,
    pub direction: OrderDirection,
}

#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug)]
#[graphql(name = "ProjectV2ItemFieldValueOrderField")]
pub enum ProjectV2ItemFieldValueOrderField {
    Position,
}

/// Ordering of field values (accepted; values follow field positions).
#[derive(InputObject, Clone, Copy, Debug)]
#[graphql(name = "ProjectV2ItemFieldValueOrder")]
pub struct ProjectV2ItemFieldValueOrder {
    pub field: ProjectV2ItemFieldValueOrderField,
    pub direction: OrderDirection,
}

// ---------------------------------------------------------------------------
// Loading and visibility
// ---------------------------------------------------------------------------

/// The owner of a project.
#[derive(Interface, Clone)]
#[graphql(name = "ProjectV2Owner", field(name = "id", ty = "ID"))]
pub enum ProjectV2Owner {
    User(User),
    Organization(Organization),
}

/// A project as seen by the viewer (`role` is never below `Read`).
#[derive(Clone)]
pub struct ProjectV2 {
    pub p: Arc<ProjectRow>,
    pub owner: Arc<db::User>,
    pub role: Role,
}

/// Keep the projects the viewer can read, in input order.
pub async fn visible(ctx: &Context<'_>, rows: Vec<ProjectRow>) -> GResult<Vec<ProjectV2>> {
    let g = gql(ctx);
    let l = ctx.data_unchecked::<Loaders>();
    let owners = many(&l.users, rows.iter().map(|p| p.owner_id)).await?;
    let mut roles: HashMap<i64, Option<Role>> = HashMap::new();
    for (id, owner) in &owners {
        roles.insert(
            *id,
            owner_role(&g.state, g.auth.as_ref(), owner).await.gql()?,
        );
    }
    Ok(rows
        .into_iter()
        .filter_map(|p| {
            let owner = owners.get(&p.owner_id)?.clone();
            let role = project_role(roles.get(&p.owner_id).copied().flatten(), p.public)?;
            Some(ProjectV2 {
                p: Arc::new(p),
                owner,
                role,
            })
        })
        .collect())
}

async fn rows_where(ctx: &Context<'_>, cond: &str, id: i64) -> GResult<Vec<ProjectRow>> {
    sqlx::query_as(&format!("{} WHERE {cond}", ProjectRow::SELECT))
        .bind(id)
        .fetch_all(&gql(ctx).state.db)
        .await
        .gql()
}

impl ProjectV2 {
    /// A project by id, if the viewer can read it.
    pub async fn load(ctx: &Context<'_>, id: i64) -> GResult<Option<Self>> {
        let rows = rows_where(ctx, "p.id = $1", id).await?;
        Ok(visible(ctx, rows).await?.pop())
    }

    /// A project by node id, or a GitHub-style NOT_FOUND error.
    pub async fn by_node(ctx: &Context<'_>, id: &ID) -> GResult<Self> {
        let missing = || {
            not_found(format!(
                "Could not resolve to a ProjectV2 with the global id of '{}'.",
                id.0
            ))
        };
        match node_id::decode(&id.0) {
            Some((NodeType::ProjectV2, n)) => Self::load(ctx, n).await?.ok_or_else(missing),
            _ => Err(missing()),
        }
    }

    pub fn pid(&self) -> i64 {
        self.p.id
    }

    pub async fn field_rows(&self, ctx: &Context<'_>) -> GResult<Arc<Vec<FieldRow>>> {
        let rows = bgh_projects::rest::project_fields(&gql(ctx).state.db, self.pid())
            .await
            .gql()?;
        Ok(Arc::new(rows))
    }

    fn field_config(&self, f: FieldRow) -> ProjectV2FieldConfiguration {
        field_config(Arc::new(f), self.clone())
    }

    /// Wrap item rows (fields loaded once).
    pub async fn wrap_items(
        &self,
        ctx: &Context<'_>,
        rows: Vec<ItemRow>,
    ) -> GResult<Vec<ProjectV2Item>> {
        let fields = self.field_rows(ctx).await?;
        Ok(rows
            .into_iter()
            .map(|it| ProjectV2Item {
                it: Arc::new(it),
                project: self.clone(),
                fields: fields.clone(),
            })
            .collect())
    }

    /// One item of this project.
    pub async fn item(&self, ctx: &Context<'_>, item_id: i64) -> GResult<Option<ProjectV2Item>> {
        let row: Option<ItemRow> = sqlx::query_as(&format!(
            "{} WHERE i.id = $1 AND i.project_id = $2",
            ItemRow::SELECT
        ))
        .bind(item_id)
        .bind(self.pid())
        .fetch_optional(&gql(ctx).state.db)
        .await
        .gql()?;
        match row {
            Some(r) => Ok(self.wrap_items(ctx, vec![r]).await?.pop()),
            None => Ok(None),
        }
    }
}

/// `projectsV2(query:)` filter: `is:open`, `is:closed`, words in the title.
fn project_filter(query: Option<&str>) -> (Option<bool>, Option<String>) {
    let mut closed = None;
    let mut words = Vec::new();
    for tok in query.unwrap_or("").split_whitespace() {
        match tok.to_lowercase().as_str() {
            "is:open" => closed = Some(false),
            "is:closed" => closed = Some(true),
            t if t.contains(':') => {}
            _ => words.push(tok.trim_matches('"').to_string()),
        }
    }
    (closed, (!words.is_empty()).then(|| words.join(" ")))
}

fn project_order(order: Option<ProjectV2Order>) -> String {
    let o = order.unwrap_or(ProjectV2Order {
        field: ProjectV2OrderField::Number,
        direction: OrderDirection::Desc,
    });
    let col = match o.field {
        ProjectV2OrderField::Title => "lower(p.title)",
        ProjectV2OrderField::Number => "p.number",
        ProjectV2OrderField::UpdatedAt => "p.updated_at",
        ProjectV2OrderField::CreatedAt => "p.created_at",
    };
    format!("{col} {d}, p.id {d}", d = o.direction.sql())
}

/// `projectsV2` of an owner (`User` / `Organization`).
pub async fn owner_projects(
    ctx: &Context<'_>,
    owner: &db::User,
    args: ConnArgs,
    query: Option<String>,
    order_by: Option<ProjectV2Order>,
) -> GResult<ProjectV2Connection> {
    let g = gql(ctx);
    let role = owner_role(&g.state, g.auth.as_ref(), owner).await.gql()?;
    let (closed, title) = project_filter(query.as_deref());
    let rows: Vec<ProjectRow> = sqlx::query_as(&format!(
        "{} WHERE p.owner_id = $1 AND ($2::bool OR p.public)
           AND ($3::bool IS NULL OR p.closed = $3)
           AND ($4::text IS NULL OR p.title ILIKE '%' || $4 || '%')
         ORDER BY {}",
        ProjectRow::SELECT,
        project_order(order_by)
    ))
    .bind(owner.id)
    .bind(role.is_some())
    .bind(closed)
    .bind(title)
    .fetch_all(&g.state.db)
    .await
    .gql()?;
    let items = visible(ctx, rows).await?;
    Ok(Page::from_vec(items, &args)?.into())
}

/// `projectV2(number:)` of an owner.
pub async fn owner_project(
    ctx: &Context<'_>,
    owner: &db::User,
    number: i32,
) -> GResult<Option<ProjectV2>> {
    let rows: Vec<ProjectRow> = sqlx::query_as(&format!(
        "{} WHERE p.owner_id = $1 AND p.number = $2",
        ProjectRow::SELECT
    ))
    .bind(owner.id)
    .bind(i64::from(number))
    .fetch_all(&gql(ctx).state.db)
    .await
    .gql()?;
    match visible(ctx, rows).await?.pop() {
        Some(p) => Ok(Some(p)),
        None => Err(not_found(format!(
            "Could not resolve to a ProjectV2 with the number {number}."
        ))),
    }
}

/// `projectsV2` of a repository: projects linked to it or holding its
/// issues and pull requests.
pub async fn repo_projects(
    ctx: &Context<'_>,
    repo_id: i64,
    args: ConnArgs,
    query: Option<String>,
    order_by: Option<ProjectV2Order>,
) -> GResult<ProjectV2Connection> {
    let (closed, title) = project_filter(query.as_deref());
    let rows: Vec<ProjectRow> = sqlx::query_as(&format!(
        "{} WHERE p.id IN (
             SELECT project_id FROM project_linked_repos WHERE repo_id = $1
             UNION
             SELECT pi.project_id FROM project_items pi JOIN issues iss ON iss.id = pi.issue_id
              WHERE iss.repo_id = $1)
           AND ($2::bool IS NULL OR p.closed = $2)
           AND ($3::text IS NULL OR p.title ILIKE '%' || $3 || '%')
         ORDER BY {}",
        ProjectRow::SELECT,
        project_order(order_by)
    ))
    .bind(repo_id)
    .bind(closed)
    .bind(title)
    .fetch_all(&gql(ctx).state.db)
    .await
    .gql()?;
    let items = visible(ctx, rows).await?;
    Ok(Page::from_vec(items, &args)?.into())
}

/// `projectsV2` of an issue or pull request.
pub async fn issue_projects(
    ctx: &Context<'_>,
    issue_id: i64,
    args: ConnArgs,
    query: Option<String>,
    order_by: Option<ProjectV2Order>,
) -> GResult<ProjectV2Connection> {
    let (closed, title) = project_filter(query.as_deref());
    let rows: Vec<ProjectRow> = sqlx::query_as(&format!(
        "{} WHERE p.id IN (SELECT project_id FROM project_items WHERE issue_id = $1)
           AND ($2::bool IS NULL OR p.closed = $2)
           AND ($3::text IS NULL OR p.title ILIKE '%' || $3 || '%')
         ORDER BY {}",
        ProjectRow::SELECT,
        project_order(order_by)
    ))
    .bind(issue_id)
    .bind(closed)
    .bind(title)
    .fetch_all(&gql(ctx).state.db)
    .await
    .gql()?;
    let items = visible(ctx, rows).await?;
    Ok(Page::from_vec(items, &args)?.into())
}

/// `projectItems` of an issue or pull request (items in readable projects).
pub async fn issue_items(
    ctx: &Context<'_>,
    issue_id: i64,
    args: ConnArgs,
    include_archived: bool,
) -> GResult<ProjectV2ItemConnection> {
    let g = gql(ctx);
    let items: Vec<ItemRow> = sqlx::query_as(&format!(
        "{} WHERE i.issue_id = $1 AND ($2 OR NOT i.archived) ORDER BY i.id",
        ItemRow::SELECT
    ))
    .bind(issue_id)
    .bind(include_archived)
    .fetch_all(&g.state.db)
    .await
    .gql()?;
    let ids: Vec<i64> = items.iter().map(|i| i.project_id).collect();
    let rows: Vec<ProjectRow> =
        sqlx::query_as(&format!("{} WHERE p.id = ANY($1)", ProjectRow::SELECT))
            .bind(&ids)
            .fetch_all(&g.state.db)
            .await
            .gql()?;
    let projects: HashMap<i64, ProjectV2> = visible(ctx, rows)
        .await?
        .into_iter()
        .map(|p| (p.pid(), p))
        .collect();
    let mut out = Vec::new();
    for it in items {
        if let Some(p) = projects.get(&it.project_id) {
            out.extend(p.wrap_items(ctx, vec![it]).await?);
        }
    }
    Ok(Page::from_vec(out, &args)?.into())
}

/// `node(id:)` for project types.
pub async fn resolve_node(ctx: &Context<'_>, ty: NodeType, id: i64) -> GResult<Option<Node>> {
    let db = &gql(ctx).state.db;
    let project_of = |table: &'static str| async move {
        let pid: Option<i64> =
            sqlx::query_scalar(&format!("SELECT project_id FROM {table} WHERE id = $1"))
                .bind(id)
                .fetch_optional(db)
                .await
                .gql()?;
        match pid {
            Some(pid) => ProjectV2::load(ctx, pid).await,
            None => Ok(None),
        }
    };
    Ok(match ty {
        NodeType::ProjectV2 => ProjectV2::load(ctx, id).await?.map(Node::ProjectV2),
        NodeType::ProjectV2Item | NodeType::DraftIssue => {
            let Some(p) = project_of("project_items").await? else {
                return Ok(None);
            };
            let Some(item) = p.item(ctx, id).await? else {
                return Ok(None);
            };
            if ty == NodeType::ProjectV2Item {
                Some(Node::ProjectV2Item(item))
            } else if item.it.is_draft() {
                Some(Node::DraftIssue(DraftIssue(item)))
            } else {
                None
            }
        }
        NodeType::ProjectV2Field => {
            let Some(p) = project_of("project_fields").await? else {
                return Ok(None);
            };
            let fields = p.field_rows(ctx).await?;
            fields
                .iter()
                .find(|f| f.id == id)
                .map(|f| match p.field_config(f.clone()) {
                    ProjectV2FieldConfiguration::Field(x) => Node::ProjectV2Field(x),
                    ProjectV2FieldConfiguration::SingleSelect(x) => {
                        Node::ProjectV2SingleSelectField(x)
                    }
                    ProjectV2FieldConfiguration::Iteration(x) => Node::ProjectV2IterationField(x),
                })
        }
        NodeType::ProjectV2View => {
            let Some(p) = project_of("project_views").await? else {
                return Ok(None);
            };
            let row: Option<ViewRow> = sqlx::query_as(&format!(
                "SELECT {} FROM project_views WHERE id = $1",
                ViewRow::COLUMNS
            ))
            .bind(id)
            .fetch_optional(db)
            .await
            .gql()?;
            row.map(|v| {
                Node::ProjectV2View(ProjectV2View {
                    v: Arc::new(v),
                    project: p,
                })
            })
        }
        _ => None,
    })
}

// ---------------------------------------------------------------------------
// ProjectV2
// ---------------------------------------------------------------------------

#[Object(name = "ProjectV2")]
impl ProjectV2 {
    pub async fn id(&self) -> ID {
        nid(NodeType::ProjectV2, self.p.id)
    }
    pub async fn database_id(&self) -> Option<i64> {
        Some(self.p.id)
    }
    pub async fn full_database_id(&self) -> Option<String> {
        Some(self.p.id.to_string())
    }
    pub async fn number(&self) -> i32 {
        self.p.number as i32
    }
    pub async fn title(&self) -> &str {
        &self.p.title
    }
    pub async fn short_description(&self) -> Option<&str> {
        self.p.short_description.as_deref()
    }
    pub async fn readme(&self) -> Option<&str> {
        self.p.readme.as_deref()
    }
    pub async fn public(&self) -> bool {
        self.p.public
    }
    pub async fn closed(&self) -> bool {
        self.p.closed
    }
    pub async fn closed_at(&self) -> Option<DateTime> {
        odt(self.p.closed_at)
    }
    pub async fn template(&self) -> bool {
        false
    }
    pub async fn created_at(&self) -> DateTime {
        dt(self.p.created_at)
    }
    pub async fn updated_at(&self) -> DateTime {
        dt(self.p.updated_at)
    }
    pub async fn url(&self, ctx: &Context<'_>) -> URI {
        URI(gql(ctx)
            .state
            .urls
            .html(&project_html_path(&self.owner, self.p.number)))
    }
    pub async fn resource_path(&self) -> URI {
        URI(project_html_path(&self.owner, self.p.number))
    }
    pub async fn owner(&self) -> ProjectV2Owner {
        if self.owner.is_org() {
            ProjectV2Owner::Organization(Organization(self.owner.clone()))
        } else {
            ProjectV2Owner::User(User(self.owner.clone()))
        }
    }
    pub async fn creator(&self, ctx: &Context<'_>) -> GResult<Option<Actor>> {
        let Some(id) = self.p.creator_id else {
            return Ok(None);
        };
        let l = ctx.data_unchecked::<Loaders>();
        Ok(one(&l.users, id).await?.map(Actor::from_user))
    }
    pub async fn viewer_can_update(&self) -> bool {
        self.role >= Role::Write
    }
    pub async fn viewer_can_close(&self) -> bool {
        self.role >= Role::Admin
    }
    pub async fn viewer_can_reopen(&self) -> bool {
        self.role >= Role::Admin
    }
    pub async fn fields(
        &self,
        ctx: &Context<'_>,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
        order_by: Option<ProjectV2FieldOrder>,
    ) -> GResult<ProjectV2FieldConfigurationConnection> {
        let _ = order_by;
        let fields = self.field_rows(ctx).await?;
        let items = fields
            .iter()
            .map(|f| self.field_config(f.clone()))
            .collect();
        Ok(Page::from_vec(items, &ConnArgs::new(first, last, after, before))?.into())
    }
    /// A field by name (case-insensitive).
    pub async fn field(
        &self,
        ctx: &Context<'_>,
        name: String,
    ) -> GResult<Option<ProjectV2FieldConfiguration>> {
        let fields = self.field_rows(ctx).await?;
        Ok(fields
            .iter()
            .find(|f| f.name.eq_ignore_ascii_case(&name))
            .map(|f| self.field_config(f.clone())))
    }
    /// Items in position order; `query` uses the project filter syntax
    /// (archived items only with `is:archived`).
    pub async fn items(
        &self,
        ctx: &Context<'_>,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
        query: Option<String>,
        order_by: Option<ProjectV2ItemOrder>,
    ) -> GResult<ProjectV2ItemConnection> {
        let _ = order_by;
        let g = gql(ctx);
        let rows = bgh_projects::rest::filter_items(
            &g.state,
            g.auth.as_ref(),
            self.pid(),
            query.as_deref(),
            false,
        )
        .await
        .gql()?;
        let page = Page::from_vec(rows, &ConnArgs::new(first, last, after, before))?;
        let items = self.wrap_items(ctx, page.items).await?;
        Ok(Page {
            items,
            offset: page.offset,
            has_next: page.has_next,
            total: page.total,
        }
        .into())
    }
    pub async fn views(
        &self,
        ctx: &Context<'_>,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
    ) -> GResult<ProjectV2ViewConnection> {
        let rows: Vec<ViewRow> = sqlx::query_as(&format!(
            "SELECT {} FROM project_views WHERE project_id = $1 ORDER BY position, id",
            ViewRow::COLUMNS
        ))
        .bind(self.pid())
        .fetch_all(&gql(ctx).state.db)
        .await
        .gql()?;
        let items = rows
            .into_iter()
            .map(|v| ProjectV2View {
                v: Arc::new(v),
                project: self.clone(),
            })
            .collect();
        Ok(Page::from_vec(items, &ConnArgs::new(first, last, after, before))?.into())
    }
    pub async fn view(&self, ctx: &Context<'_>, number: i32) -> GResult<Option<ProjectV2View>> {
        let row: Option<ViewRow> = sqlx::query_as(&format!(
            "SELECT {} FROM project_views WHERE project_id = $1 AND number = $2",
            ViewRow::COLUMNS
        ))
        .bind(self.pid())
        .bind(i64::from(number))
        .fetch_optional(&gql(ctx).state.db)
        .await
        .gql()?;
        Ok(row.map(|v| ProjectV2View {
            v: Arc::new(v),
            project: self.clone(),
        }))
    }
    /// Linked repositories the viewer can read.
    pub async fn repositories(
        &self,
        ctx: &Context<'_>,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
    ) -> GResult<RepositoryConnection> {
        let mut items = Vec::new();
        for id in &self.p.linked_repo_ids {
            if let Some(r) = repo::load(ctx, *id).await? {
                items.push(r);
            }
        }
        Ok(Page::from_vec(items, &ConnArgs::new(first, last, after, before))?.into())
    }
    /// Linked teams.
    pub async fn teams(
        &self,
        ctx: &Context<'_>,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
    ) -> GResult<TeamConnection> {
        let ids: Vec<i64> = sqlx::query_scalar(
            "SELECT team_id FROM project_linked_teams WHERE project_id = $1 ORDER BY team_id",
        )
        .bind(self.pid())
        .fetch_all(&gql(ctx).state.db)
        .await
        .gql()?;
        let l = ctx.data_unchecked::<Loaders>();
        let teams = many(&l.teams, ids.iter().copied()).await?;
        let items = ids
            .iter()
            .filter_map(|id| teams.get(id).cloned().map(Team))
            .collect();
        Ok(Page::from_vec(items, &ConnArgs::new(first, last, after, before))?.into())
    }
}

connection!(ProjectV2Connection, ProjectV2Edge, ProjectV2);

// ---------------------------------------------------------------------------
// Fields
// ---------------------------------------------------------------------------

/// A single-select option.
#[derive(Clone)]
pub struct ProjectV2SingleSelectFieldOption(pub Value);

impl ProjectV2SingleSelectFieldOption {
    fn s(&self, k: &str) -> String {
        self.0
            .get(k)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    }
}

#[Object(name = "ProjectV2SingleSelectFieldOption")]
impl ProjectV2SingleSelectFieldOption {
    pub async fn id(&self) -> String {
        self.s("id")
    }
    pub async fn name(&self) -> String {
        self.s("name")
    }
    #[graphql(name = "nameHTML")]
    pub async fn name_html(&self) -> HTML {
        HTML(bgh_core::mail::escape_html(&self.s("name")))
    }
    pub async fn color(&self) -> ProjectV2SingleSelectFieldOptionColor {
        ProjectV2SingleSelectFieldOptionColor::parse(&self.s("color"))
    }
    pub async fn description(&self) -> String {
        self.s("description")
    }
    #[graphql(name = "descriptionHTML")]
    pub async fn description_html(&self) -> HTML {
        HTML(bgh_core::mail::escape_html(&self.s("description")))
    }
}

/// One iteration of an iteration field.
#[derive(Clone)]
pub struct ProjectV2IterationFieldIteration(pub Value);

impl ProjectV2IterationFieldIteration {
    fn s(&self, k: &str) -> String {
        self.0
            .get(k)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    }
    fn duration_days(&self) -> i64 {
        self.0.get("duration").and_then(Value::as_i64).unwrap_or(0)
    }
    fn completed(&self) -> bool {
        iteration_completed(&self.s("startDate"), self.duration_days())
    }
}

#[Object(name = "ProjectV2IterationFieldIteration")]
impl ProjectV2IterationFieldIteration {
    pub async fn id(&self) -> String {
        self.s("id")
    }
    pub async fn title(&self) -> String {
        self.s("title")
    }
    #[graphql(name = "titleHTML")]
    pub async fn title_html(&self) -> String {
        bgh_core::mail::escape_html(&self.s("title"))
    }
    pub async fn start_date(&self) -> Date {
        Date(self.s("startDate"))
    }
    pub async fn duration(&self) -> i32 {
        self.duration_days() as i32
    }
}

/// Iteration field settings.
#[derive(Clone)]
pub struct ProjectV2IterationFieldConfiguration(pub Value);

impl ProjectV2IterationFieldConfiguration {
    fn all(&self) -> Vec<ProjectV2IterationFieldIteration> {
        self.0
            .get("iterations")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .cloned()
                    .map(ProjectV2IterationFieldIteration)
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[Object(name = "ProjectV2IterationFieldConfiguration")]
impl ProjectV2IterationFieldConfiguration {
    pub async fn duration(&self) -> i32 {
        self.0.get("duration").and_then(Value::as_i64).unwrap_or(14) as i32
    }
    /// Day of the week the iterations start on (1 = Monday ... 7 = Sunday).
    pub async fn start_day(&self) -> i32 {
        self.0
            .get("startDate")
            .and_then(Value::as_str)
            .and_then(|s| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
            .map(|d| chrono::Datelike::weekday(&d).number_from_monday() as i32)
            .unwrap_or(1)
    }
    /// Current and upcoming iterations.
    pub async fn iterations(&self) -> Vec<ProjectV2IterationFieldIteration> {
        self.all().into_iter().filter(|i| !i.completed()).collect()
    }
    pub async fn completed_iterations(&self) -> Vec<ProjectV2IterationFieldIteration> {
        self.all().into_iter().filter(|i| i.completed()).collect()
    }
}

/// Shared state of the three field types.
#[derive(Clone)]
pub struct FieldBase {
    pub f: Arc<FieldRow>,
    pub project: ProjectV2,
}

macro_rules! field_type {
    ($ty:ident, $name:literal, { $($extra:tt)* }) => {
        #[derive(Clone)]
        pub struct $ty(pub FieldBase);

        #[Object(name = $name)]
        impl $ty {
            pub async fn id(&self) -> ID {
                nid(NodeType::ProjectV2Field, self.0.f.id)
            }
            pub async fn database_id(&self) -> Option<i64> {
                Some(self.0.f.id)
            }
            pub async fn name(&self) -> &str {
                &self.0.f.name
            }
            pub async fn data_type(&self) -> ProjectV2FieldType {
                ProjectV2FieldType::of(&self.0.f.data_type)
            }
            pub async fn project(&self) -> &ProjectV2 {
                &self.0.project
            }
            pub async fn created_at(&self) -> DateTime {
                dt(self.0.f.created_at)
            }
            pub async fn updated_at(&self) -> DateTime {
                dt(self.0.f.updated_at)
            }
            $($extra)*
        }
    };
}

field_type!(ProjectV2Field, "ProjectV2Field", {});
field_type!(ProjectV2SingleSelectField, "ProjectV2SingleSelectField", {
    pub async fn options(
        &self,
        names: Option<Vec<String>>,
    ) -> Vec<ProjectV2SingleSelectFieldOption> {
        self.0
            .f
            .options
            .as_ref()
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter(|o| {
                        names.as_ref().is_none_or(|n| {
                            n.iter().any(|x| Some(x.as_str()) == o["name"].as_str())
                        })
                    })
                    .cloned()
                    .map(ProjectV2SingleSelectFieldOption)
                    .collect()
            })
            .unwrap_or_default()
    }
});
field_type!(ProjectV2IterationField, "ProjectV2IterationField", {
    pub async fn configuration(&self) -> ProjectV2IterationFieldConfiguration {
        ProjectV2IterationFieldConfiguration(self.0.f.iterations.clone().unwrap_or(Value::Null))
    }
});

/// Any project field.
#[derive(Union, Clone)]
#[graphql(name = "ProjectV2FieldConfiguration")]
pub enum ProjectV2FieldConfiguration {
    Field(ProjectV2Field),
    SingleSelect(ProjectV2SingleSelectField),
    Iteration(ProjectV2IterationField),
}

pub fn field_config(f: Arc<FieldRow>, project: ProjectV2) -> ProjectV2FieldConfiguration {
    let kind = f.data_type.clone();
    let base = FieldBase { f, project };
    match kind.as_str() {
        "single_select" | "status" => {
            ProjectV2FieldConfiguration::SingleSelect(ProjectV2SingleSelectField(base))
        }
        "iteration" => ProjectV2FieldConfiguration::Iteration(ProjectV2IterationField(base)),
        _ => ProjectV2FieldConfiguration::Field(ProjectV2Field(base)),
    }
}

connection!(
    ProjectV2FieldConfigurationConnection,
    ProjectV2FieldConfigurationEdge,
    ProjectV2FieldConfiguration
);

// ---------------------------------------------------------------------------
// Views
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct ProjectV2View {
    pub v: Arc<ViewRow>,
    pub project: ProjectV2,
}

#[Object(name = "ProjectV2View")]
impl ProjectV2View {
    pub async fn id(&self) -> ID {
        nid(NodeType::ProjectV2View, self.v.id)
    }
    pub async fn database_id(&self) -> Option<i64> {
        Some(self.v.id)
    }
    pub async fn number(&self) -> i32 {
        self.v.number as i32
    }
    pub async fn name(&self) -> &str {
        &self.v.name
    }
    pub async fn layout(&self) -> ProjectV2ViewLayout {
        match self.v.layout.as_str() {
            "board" => ProjectV2ViewLayout::BoardLayout,
            "roadmap" => ProjectV2ViewLayout::RoadmapLayout,
            _ => ProjectV2ViewLayout::TableLayout,
        }
    }
    pub async fn filter(&self) -> Option<&str> {
        Some(self.v.filter.as_str()).filter(|f| !f.is_empty())
    }
    pub async fn project(&self) -> &ProjectV2 {
        &self.project
    }
    pub async fn created_at(&self) -> DateTime {
        dt(self.v.created_at)
    }
    pub async fn updated_at(&self) -> DateTime {
        dt(self.v.updated_at)
    }
}

connection!(ProjectV2ViewConnection, ProjectV2ViewEdge, ProjectV2View);

// ---------------------------------------------------------------------------
// Items
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct ProjectV2Item {
    pub it: Arc<ItemRow>,
    pub project: ProjectV2,
    /// All fields of the project (position order).
    pub fields: Arc<Vec<FieldRow>>,
}

/// What an item holds.
#[derive(Union, Clone)]
#[graphql(name = "ProjectV2ItemContent")]
pub enum ProjectV2ItemContent {
    DraftIssue(DraftIssue),
    Issue(Issue),
    PullRequest(PullRequest),
}

impl ProjectV2Item {
    /// The issue row, if this is an issue / PR item the viewer can read.
    async fn issue(&self, ctx: &Context<'_>) -> GResult<Option<Arc<db::Issue>>> {
        let Some(id) = self.it.issue_id else {
            return Ok(None);
        };
        let l = ctx.data_unchecked::<Loaders>();
        let Some(i) = one(&l.issues, id).await? else {
            return Ok(None);
        };
        if repo::load(ctx, i.repo_id).await?.is_none() {
            return Ok(None);
        }
        Ok(Some(i))
    }

    pub async fn creator_actor(&self, ctx: &Context<'_>) -> GResult<Option<Actor>> {
        let Some(id) = self.it.creator_id else {
            return Ok(None);
        };
        let l = ctx.data_unchecked::<Loaders>();
        Ok(one(&l.users, id).await?.map(Actor::from_user))
    }

    pub async fn content_value(&self, ctx: &Context<'_>) -> GResult<Option<ProjectV2ItemContent>> {
        if self.it.is_draft() {
            return Ok(Some(ProjectV2ItemContent::DraftIssue(DraftIssue(
                self.clone(),
            ))));
        }
        let Some(i) = self.issue(ctx).await? else {
            return Ok(None);
        };
        if i.is_pull_request {
            Ok(from_issues(ctx, vec![(*i).clone()])
                .await?
                .into_iter()
                .next()
                .map(ProjectV2ItemContent::PullRequest))
        } else {
            Ok(Some(ProjectV2ItemContent::Issue(Issue::new(i))))
        }
    }

    fn base(&self, f: &FieldRow) -> ValueBase {
        ValueBase {
            field: field_config(Arc::new(f.clone()), self.project.clone()),
            item: self.clone(),
            field_id: f.id,
        }
    }

    /// Values of the item's set fields, in field order.
    pub async fn values(&self, ctx: &Context<'_>) -> GResult<Vec<ProjectV2ItemFieldValue>> {
        let l = ctx.data_unchecked::<Loaders>();
        let issue = self.issue(ctx).await?;
        if !self.it.is_draft() && issue.is_none() {
            return Ok(vec![]);
        }
        let mut out = Vec::new();
        for f in self.fields.iter() {
            let base = self.base(f);
            let stored = self.it.field_values.get(f.id.to_string());
            let v = match f.data_type.as_str() {
                "title" => {
                    let title = match &issue {
                        Some(i) => i.title.clone(),
                        None => self.it.title.clone().unwrap_or_default(),
                    };
                    Some(ProjectV2ItemFieldValue::Text(ProjectV2ItemFieldTextValue {
                        base,
                        text: Some(title),
                    }))
                }
                "assignees" => {
                    let ids: Vec<i64> = match &issue {
                        Some(i) => one(&l.issue_assignees, i.id)
                            .await?
                            .map(|v| (*v).clone())
                            .unwrap_or_default(),
                        None => self.it.assignee_ids.clone(),
                    };
                    let users = many(&l.users, ids.iter().copied()).await?;
                    let users: Vec<User> = ids
                        .iter()
                        .filter_map(|id| users.get(id).cloned().map(User))
                        .collect();
                    (!users.is_empty()).then(|| {
                        ProjectV2ItemFieldValue::User(ProjectV2ItemFieldUserValue { base, users })
                    })
                }
                "labels" => match &issue {
                    Some(i) => {
                        let labels: Vec<Label> = one(&l.issue_labels, i.id)
                            .await?
                            .map(|v| v.iter().cloned().map(|x| Label(Arc::new(x))).collect())
                            .unwrap_or_default();
                        (!labels.is_empty()).then(|| {
                            ProjectV2ItemFieldValue::Label(ProjectV2ItemFieldLabelValue {
                                base,
                                labels,
                            })
                        })
                    }
                    None => None,
                },
                "milestone" => match issue.as_ref().and_then(|i| i.milestone_id) {
                    Some(mid) => one(&l.milestones, mid).await?.map(|m| {
                        ProjectV2ItemFieldValue::Milestone(ProjectV2ItemFieldMilestoneValue {
                            base,
                            milestone: Some(Milestone(m)),
                        })
                    }),
                    None => None,
                },
                "repository" => match &issue {
                    Some(i) => repo::load(ctx, i.repo_id).await?.map(|r| {
                        ProjectV2ItemFieldValue::Repository(ProjectV2ItemFieldRepositoryValue {
                            base,
                            repository: Some(r),
                        })
                    }),
                    None => None,
                },
                _ => stored.and_then(|v| custom_value(f, v, base)),
            };
            out.extend(v);
        }
        Ok(out)
    }
}

fn custom_value(f: &FieldRow, v: &Value, base: ValueBase) -> Option<ProjectV2ItemFieldValue> {
    let find = |list: Option<&Value>| {
        let id = v.as_str()?;
        list.and_then(Value::as_array)?
            .iter()
            .find(|o| o["id"].as_str() == Some(id))
            .cloned()
    };
    Some(match f.data_type.as_str() {
        "text" => ProjectV2ItemFieldValue::Text(ProjectV2ItemFieldTextValue {
            base,
            text: v.as_str().map(String::from),
        }),
        "number" => ProjectV2ItemFieldValue::Number(ProjectV2ItemFieldNumberValue {
            base,
            number: v.as_f64(),
        }),
        "date" => ProjectV2ItemFieldValue::Date(ProjectV2ItemFieldDateValue {
            base,
            date: v.as_str().map(Date::from),
        }),
        "single_select" | "status" => {
            ProjectV2ItemFieldValue::SingleSelect(ProjectV2ItemFieldSingleSelectValue {
                base,
                option: ProjectV2SingleSelectFieldOption(find(f.options.as_ref())?),
            })
        }
        "iteration" => ProjectV2ItemFieldValue::Iteration(ProjectV2ItemFieldIterationValue {
            base,
            iteration: ProjectV2IterationFieldIteration(find(
                f.iterations.as_ref().and_then(|c| c.get("iterations")),
            )?),
        }),
        _ => return None,
    })
}

#[Object(name = "ProjectV2Item")]
impl ProjectV2Item {
    pub async fn id(&self) -> ID {
        nid(NodeType::ProjectV2Item, self.it.id)
    }
    pub async fn database_id(&self) -> Option<i64> {
        Some(self.it.id)
    }
    pub async fn full_database_id(&self) -> Option<String> {
        Some(self.it.id.to_string())
    }
    #[graphql(name = "type")]
    pub async fn kind(&self, ctx: &Context<'_>) -> GResult<ProjectV2ItemType> {
        Ok(match self.it.content_type.as_str() {
            "DraftIssue" => ProjectV2ItemType::DraftIssue,
            _ if self.issue(ctx).await?.is_none() => ProjectV2ItemType::Redacted,
            "PullRequest" => ProjectV2ItemType::PullRequest,
            _ => ProjectV2ItemType::Issue,
        })
    }
    pub async fn content(&self, ctx: &Context<'_>) -> GResult<Option<ProjectV2ItemContent>> {
        self.content_value(ctx).await
    }
    pub async fn is_archived(&self) -> bool {
        self.it.archived
    }
    pub async fn project(&self) -> &ProjectV2 {
        &self.project
    }
    pub async fn creator(&self, ctx: &Context<'_>) -> GResult<Option<Actor>> {
        let Some(id) = self.it.creator_id else {
            return Ok(None);
        };
        let l = ctx.data_unchecked::<Loaders>();
        Ok(one(&l.users, id).await?.map(Actor::from_user))
    }
    pub async fn created_at(&self) -> DateTime {
        dt(self.it.created_at)
    }
    pub async fn updated_at(&self) -> DateTime {
        dt(self.it.updated_at)
    }
    pub async fn field_values(
        &self,
        ctx: &Context<'_>,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
        order_by: Option<ProjectV2ItemFieldValueOrder>,
    ) -> GResult<ProjectV2ItemFieldValueConnection> {
        let _ = order_by;
        let values = self.values(ctx).await?;
        Ok(Page::from_vec(values, &ConnArgs::new(first, last, after, before))?.into())
    }
    /// The value of the field named `name` (case-insensitive), if set.
    pub async fn field_value_by_name(
        &self,
        ctx: &Context<'_>,
        name: String,
    ) -> GResult<Option<ProjectV2ItemFieldValue>> {
        let Some(field) = self
            .fields
            .iter()
            .find(|f| f.name.eq_ignore_ascii_case(&name))
        else {
            return Ok(None);
        };
        Ok(self
            .values(ctx)
            .await?
            .into_iter()
            .find(|v| v.field_id() == field.id))
    }
}

connection!(ProjectV2ItemConnection, ProjectV2ItemEdge, ProjectV2Item);

/// A draft issue (the content of a draft item; its id is `DI_...`).
#[derive(Clone)]
pub struct DraftIssue(pub ProjectV2Item);

#[Object(name = "DraftIssue")]
impl DraftIssue {
    pub async fn id(&self) -> ID {
        nid(NodeType::DraftIssue, self.0.it.id)
    }
    pub async fn title(&self) -> String {
        self.0.it.title.clone().unwrap_or_default()
    }
    pub async fn body(&self) -> String {
        self.0.it.body.clone().unwrap_or_default()
    }
    #[graphql(name = "bodyHTML")]
    pub async fn body_html(&self, ctx: &Context<'_>) -> HTML {
        let ctxr = bgh_core::markdown::RenderContext::new(&gql(ctx).state.config.base_url);
        HTML(bgh_core::markdown::render(
            self.0.it.body.as_deref().unwrap_or(""),
            &ctxr,
        ))
    }
    pub async fn body_text(&self) -> String {
        self.0.it.body.clone().unwrap_or_default()
    }
    pub async fn assignees(
        &self,
        ctx: &Context<'_>,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
    ) -> GResult<UserConnection> {
        let l = ctx.data_unchecked::<Loaders>();
        let ids = &self.0.it.assignee_ids;
        let users = many(&l.users, ids.iter().copied()).await?;
        let items = ids
            .iter()
            .filter_map(|id| users.get(id).cloned().map(User))
            .collect();
        Ok(Page::from_vec(items, &ConnArgs::new(first, last, after, before))?.into())
    }
    pub async fn creator(&self, ctx: &Context<'_>) -> GResult<Option<Actor>> {
        self.0.creator_actor(ctx).await
    }
    pub async fn created_at(&self) -> DateTime {
        dt(self.0.it.created_at)
    }
    pub async fn updated_at(&self) -> DateTime {
        dt(self.0.it.updated_at)
    }
    pub async fn project_v2_items(
        &self,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
    ) -> GResult<ProjectV2ItemConnection> {
        Ok(Page::from_vec(
            vec![self.0.clone()],
            &ConnArgs::new(first, last, after, before),
        )?
        .into())
    }
    #[graphql(name = "projectsV2")]
    pub async fn projects_v2(
        &self,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
    ) -> GResult<ProjectV2Connection> {
        Ok(Page::from_vec(
            vec![self.0.project.clone()],
            &ConnArgs::new(first, last, after, before),
        )?
        .into())
    }
}

// ---------------------------------------------------------------------------
// Field values
// ---------------------------------------------------------------------------

/// What every field value knows: its field and item.
#[derive(Clone)]
pub struct ValueBase {
    pub field: ProjectV2FieldConfiguration,
    pub item: ProjectV2Item,
    pub field_id: i64,
}

impl ValueBase {
    fn node_id(&self) -> ID {
        ID(node_id::encode_str(
            NodeType::ProjectV2Item,
            &format!("{}:{}", self.item.it.id, self.field_id),
        ))
    }
}

macro_rules! value_type {
    ($ty:ident, $name:literal, { $($field:ident : $fty:ty),* }, { $($extra:tt)* }) => {
        #[derive(Clone)]
        pub struct $ty {
            pub base: ValueBase,
            $(pub $field: $fty,)*
        }

        #[Object(name = $name)]
        impl $ty {
            pub async fn field(&self) -> &ProjectV2FieldConfiguration {
                &self.base.field
            }
            pub async fn item(&self) -> &ProjectV2Item {
                &self.base.item
            }
            $($extra)*
        }
    };
}

macro_rules! value_meta {
    () => {
        pub async fn id(&self) -> ID {
            self.base.node_id()
        }
        pub async fn database_id(&self) -> Option<i64> {
            None
        }
        pub async fn created_at(&self) -> DateTime {
            dt(self.base.item.it.created_at)
        }
        pub async fn updated_at(&self) -> DateTime {
            dt(self.base.item.it.updated_at)
        }
        pub async fn creator(&self, ctx: &Context<'_>) -> GResult<Option<Actor>> {
            self.base.item.creator_actor(ctx).await
        }
    };
}

value_type!(ProjectV2ItemFieldTextValue, "ProjectV2ItemFieldTextValue", { text: Option<String> }, {
    value_meta!();
    pub async fn text(&self) -> Option<&str> {
        self.text.as_deref()
    }
});
value_type!(ProjectV2ItemFieldNumberValue, "ProjectV2ItemFieldNumberValue", { number: Option<f64> }, {
    value_meta!();
    pub async fn number(&self) -> Option<f64> {
        self.number
    }
});
value_type!(ProjectV2ItemFieldDateValue, "ProjectV2ItemFieldDateValue", { date: Option<Date> }, {
    value_meta!();
    pub async fn date(&self) -> Option<&Date> {
        self.date.as_ref()
    }
});
value_type!(
    ProjectV2ItemFieldSingleSelectValue,
    "ProjectV2ItemFieldSingleSelectValue",
    { option: ProjectV2SingleSelectFieldOption },
    {
        value_meta!();
        pub async fn option_id(&self) -> Option<String> {
            Some(self.option.s("id"))
        }
        pub async fn name(&self) -> Option<String> {
            Some(self.option.s("name"))
        }
        #[graphql(name = "nameHTML")]
        pub async fn name_html(&self) -> Option<HTML> {
            Some(HTML(bgh_core::mail::escape_html(&self.option.s("name"))))
        }
        pub async fn color(&self) -> ProjectV2SingleSelectFieldOptionColor {
            ProjectV2SingleSelectFieldOptionColor::parse(&self.option.s("color"))
        }
        pub async fn description(&self) -> Option<String> {
            Some(self.option.s("description"))
        }
        #[graphql(name = "descriptionHTML")]
        pub async fn description_html(&self) -> Option<HTML> {
            Some(HTML(bgh_core::mail::escape_html(&self.option.s("description"))))
        }
    }
);
value_type!(
    ProjectV2ItemFieldIterationValue,
    "ProjectV2ItemFieldIterationValue",
    { iteration: ProjectV2IterationFieldIteration },
    {
        value_meta!();
        pub async fn iteration_id(&self) -> String {
            self.iteration.s("id")
        }
        pub async fn title(&self) -> String {
            self.iteration.s("title")
        }
        #[graphql(name = "titleHTML")]
        pub async fn title_html(&self) -> String {
            bgh_core::mail::escape_html(&self.iteration.s("title"))
        }
        pub async fn start_date(&self) -> Date {
            Date(self.iteration.s("startDate"))
        }
        pub async fn duration(&self) -> i32 {
            self.iteration.duration_days() as i32
        }
    }
);
value_type!(ProjectV2ItemFieldLabelValue, "ProjectV2ItemFieldLabelValue", { labels: Vec<Label> }, {
    pub async fn labels(
        &self,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
    ) -> GResult<LabelConnection> {
        Ok(Page::from_vec(self.labels.clone(), &ConnArgs::new(first, last, after, before))?.into())
    }
});
value_type!(
    ProjectV2ItemFieldMilestoneValue,
    "ProjectV2ItemFieldMilestoneValue",
    { milestone: Option<Milestone> },
    {
        pub async fn milestone(&self) -> Option<&Milestone> {
            self.milestone.as_ref()
        }
    }
);
value_type!(
    ProjectV2ItemFieldRepositoryValue,
    "ProjectV2ItemFieldRepositoryValue",
    { repository: Option<super::Repository> },
    {
        pub async fn repository(&self) -> Option<&super::Repository> {
            self.repository.as_ref()
        }
    }
);
value_type!(ProjectV2ItemFieldUserValue, "ProjectV2ItemFieldUserValue", { users: Vec<User> }, {
    pub async fn users(
        &self,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
    ) -> GResult<UserConnection> {
        Ok(Page::from_vec(self.users.clone(), &ConnArgs::new(first, last, after, before))?.into())
    }
});
value_type!(
    ProjectV2ItemFieldPullRequestValue,
    "ProjectV2ItemFieldPullRequestValue",
    {},
    {
        /// Linked pull requests (not tracked: always empty).
        pub async fn pull_requests(
            &self,
            first: Option<i32>,
            last: Option<i32>,
            after: Option<String>,
            before: Option<String>,
        ) -> GResult<PullRequestConnection> {
            Ok(Page::from_vec(vec![], &ConnArgs::new(first, last, after, before))?.into())
        }
    }
);
value_type!(
    ProjectV2ItemFieldReviewerValue,
    "ProjectV2ItemFieldReviewerValue",
    {},
    {
        /// Reviewers (no Reviewers field yet: always empty).
        pub async fn reviewers(
            &self,
            first: Option<i32>,
            last: Option<i32>,
            after: Option<String>,
            before: Option<String>,
        ) -> GResult<RequestedReviewerConnection> {
            Ok(Page::from_vec(vec![], &ConnArgs::new(first, last, after, before))?.into())
        }
    }
);

connection!(
    RequestedReviewerConnection,
    RequestedReviewerEdge,
    RequestedReviewer
);

/// A field value of an item.
#[derive(Union, Clone)]
#[graphql(name = "ProjectV2ItemFieldValue")]
pub enum ProjectV2ItemFieldValue {
    Date(ProjectV2ItemFieldDateValue),
    Iteration(ProjectV2ItemFieldIterationValue),
    Label(ProjectV2ItemFieldLabelValue),
    Milestone(ProjectV2ItemFieldMilestoneValue),
    Number(ProjectV2ItemFieldNumberValue),
    PullRequest(ProjectV2ItemFieldPullRequestValue),
    Repository(ProjectV2ItemFieldRepositoryValue),
    Reviewer(ProjectV2ItemFieldReviewerValue),
    SingleSelect(ProjectV2ItemFieldSingleSelectValue),
    Text(ProjectV2ItemFieldTextValue),
    User(ProjectV2ItemFieldUserValue),
}

impl ProjectV2ItemFieldValue {
    pub fn field_id(&self) -> i64 {
        match self {
            Self::Date(v) => v.base.field_id,
            Self::Iteration(v) => v.base.field_id,
            Self::Label(v) => v.base.field_id,
            Self::Milestone(v) => v.base.field_id,
            Self::Number(v) => v.base.field_id,
            Self::PullRequest(v) => v.base.field_id,
            Self::Repository(v) => v.base.field_id,
            Self::Reviewer(v) => v.base.field_id,
            Self::SingleSelect(v) => v.base.field_id,
            Self::Text(v) => v.base.field_id,
            Self::User(v) => v.base.field_id,
        }
    }
}

connection!(
    ProjectV2ItemFieldValueConnection,
    ProjectV2ItemFieldValueEdge,
    ProjectV2ItemFieldValue
);

// ---------------------------------------------------------------------------
// resource(url:)
// ---------------------------------------------------------------------------

/// Resolve a web URL of this server (any host: clients may reach it through
/// a proxy) to a node: `/{owner}/{repo}/issues|pull/{n}`, `/{owner}/{repo}`,
/// `/orgs|users/{login}/projects/{n}`, `/{login}`.
pub async fn resource(ctx: &Context<'_>, url: &str) -> GResult<Option<Node>> {
    let path = match url.split_once("://") {
        Some((_, rest)) => rest.find('/').map_or("", |i| &rest[i..]),
        None => url,
    };
    let path = path.split(['?', '#']).next().unwrap_or("");
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let db = &gql(ctx).state.db;
    let node = |ty: NodeType, id: i64| ID(node_id::encode(ty, id));
    let id = match segs.as_slice() {
        [kind @ ("orgs" | "users"), login, "projects", n, ..] => {
            let Ok(n) = n.parse::<i32>() else {
                return Ok(None);
            };
            let Some(owner) = db::User::find_by_login(db, login).await.gql()? else {
                return Ok(None);
            };
            if owner.is_org() != (*kind == "orgs") {
                return Ok(None);
            }
            return Ok(owner_project(ctx, &owner, n)
                .await
                .ok()
                .flatten()
                .map(Node::ProjectV2));
        }
        [owner, repo, "issues" | "pull" | "pulls", n, ..] => {
            let Ok(n) = n.parse::<i64>() else {
                return Ok(None);
            };
            let id: Option<(i64, bool)> = sqlx::query_as(
                "SELECT i.id, i.is_pull_request FROM issues i
                   JOIN repositories r ON r.id = i.repo_id JOIN users u ON u.id = r.owner_id
                  WHERE lower(u.login) = lower($1) AND lower(r.name) = lower($2) AND i.number = $3",
            )
            .bind(owner)
            .bind(repo.trim_end_matches(".git"))
            .bind(n)
            .fetch_optional(db)
            .await
            .gql()?;
            match id {
                Some((id, pr)) => node(
                    if pr {
                        NodeType::PullRequest
                    } else {
                        NodeType::Issue
                    },
                    id,
                ),
                None => return Ok(None),
            }
        }
        [owner, repo] | [owner, repo, ..] => {
            let id: Option<i64> = sqlx::query_scalar(
                "SELECT r.id FROM repositories r JOIN users u ON u.id = r.owner_id
                  WHERE lower(u.login) = lower($1) AND lower(r.name) = lower($2)",
            )
            .bind(owner)
            .bind(repo.trim_end_matches(".git"))
            .fetch_optional(db)
            .await
            .gql()?;
            match id {
                Some(id) => node(NodeType::Repository, id),
                None => return Ok(None),
            }
        }
        [login] => match db::User::find_by_login(db, login).await.gql()? {
            Some(u) => node(NodeType::for_user_kind(&u.kind), u.id),
            None => return Ok(None),
        },
        _ => return Ok(None),
    };
    crate::query::resolve_node(ctx, &id).await
}
