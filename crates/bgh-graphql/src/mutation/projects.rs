//! Projects (v2) mutations (bgh-projects service functions).

use std::collections::HashMap;

use async_graphql::{Context, ID, InputObject, Object, SimpleObject};
use bgh_core::node_id::NodeType;
use bgh_projects::fields::{IterationInput, IterationsInput, OptionInput};
use bgh_projects::items::{CreateBody, DraftInput, UpdateBody};
use serde_json::{Value, json};

use super::{decode, guard, issue_by_node, repo_by_node};
use crate::conn::{ConnArgs, Page};
use crate::ctx::{GResult, OrGql, err, gql, not_found};
use crate::loaders::{Loaders, one};
use crate::model::Repository;
use crate::model::Team;
use crate::model::project::{
    DraftIssue, ProjectV2, ProjectV2CustomFieldType, ProjectV2FieldConfiguration, ProjectV2Item,
    ProjectV2ItemConnection, ProjectV2SingleSelectFieldOptionColor, field_config,
};
use crate::scalars::Date;

// ---------------------------------------------------------------------------
// Inputs and payloads
// ---------------------------------------------------------------------------

#[derive(InputObject)]
#[graphql(name = "CreateProjectV2Input")]
pub struct CreateProjectV2Input {
    pub owner_id: ID,
    pub title: String,
    pub repository_id: Option<ID>,
    pub team_id: Option<ID>,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
#[graphql(name = "CreateProjectV2Payload")]
pub struct CreateProjectV2Payload {
    pub project_v2: Option<ProjectV2>,
    pub client_mutation_id: Option<String>,
}

#[derive(InputObject)]
#[graphql(name = "UpdateProjectV2Input")]
pub struct UpdateProjectV2Input {
    pub project_id: ID,
    pub title: Option<String>,
    pub short_description: Option<String>,
    pub readme: Option<String>,
    pub closed: Option<bool>,
    pub public: Option<bool>,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
#[graphql(name = "UpdateProjectV2Payload")]
pub struct UpdateProjectV2Payload {
    pub project_v2: Option<ProjectV2>,
    pub client_mutation_id: Option<String>,
}

#[derive(InputObject)]
#[graphql(name = "DeleteProjectV2Input")]
pub struct DeleteProjectV2Input {
    pub project_id: ID,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
#[graphql(name = "DeleteProjectV2Payload")]
pub struct DeleteProjectV2Payload {
    pub project_v2: Option<ProjectV2>,
    pub client_mutation_id: Option<String>,
}

#[derive(InputObject)]
#[graphql(name = "AddProjectV2ItemByIdInput")]
pub struct AddProjectV2ItemByIdInput {
    pub project_id: ID,
    pub content_id: ID,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
#[graphql(name = "AddProjectV2ItemByIdPayload")]
pub struct AddProjectV2ItemByIdPayload {
    pub item: Option<ProjectV2Item>,
    pub client_mutation_id: Option<String>,
}

#[derive(InputObject)]
#[graphql(name = "AddProjectV2DraftIssueInput")]
pub struct AddProjectV2DraftIssueInput {
    pub project_id: ID,
    pub title: String,
    pub body: Option<String>,
    pub assignee_ids: Option<Vec<ID>>,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
#[graphql(name = "AddProjectV2DraftIssuePayload")]
pub struct AddProjectV2DraftIssuePayload {
    pub project_item: Option<ProjectV2Item>,
    pub client_mutation_id: Option<String>,
}

#[derive(InputObject)]
#[graphql(name = "UpdateProjectV2DraftIssueInput")]
pub struct UpdateProjectV2DraftIssueInput {
    pub draft_issue_id: ID,
    pub title: Option<String>,
    pub body: Option<String>,
    pub assignee_ids: Option<Vec<ID>>,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
#[graphql(name = "UpdateProjectV2DraftIssuePayload")]
pub struct UpdateProjectV2DraftIssuePayload {
    pub draft_issue: Option<DraftIssue>,
    pub client_mutation_id: Option<String>,
}

/// The value to set (exactly one member).
#[derive(InputObject)]
#[graphql(name = "ProjectV2FieldValue")]
pub struct ProjectV2FieldValue {
    pub text: Option<String>,
    pub number: Option<f64>,
    pub date: Option<Date>,
    pub single_select_option_id: Option<String>,
    pub iteration_id: Option<String>,
}

#[derive(InputObject)]
#[graphql(name = "UpdateProjectV2ItemFieldValueInput")]
pub struct UpdateProjectV2ItemFieldValueInput {
    pub project_id: ID,
    pub item_id: ID,
    pub field_id: ID,
    pub value: ProjectV2FieldValue,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
#[graphql(name = "UpdateProjectV2ItemFieldValuePayload")]
pub struct UpdateProjectV2ItemFieldValuePayload {
    pub project_v2_item: Option<ProjectV2Item>,
    pub client_mutation_id: Option<String>,
}

#[derive(InputObject)]
#[graphql(name = "ClearProjectV2ItemFieldValueInput")]
pub struct ClearProjectV2ItemFieldValueInput {
    pub project_id: ID,
    pub item_id: ID,
    pub field_id: ID,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
#[graphql(name = "ClearProjectV2ItemFieldValuePayload")]
pub struct ClearProjectV2ItemFieldValuePayload {
    pub project_v2_item: Option<ProjectV2Item>,
    pub client_mutation_id: Option<String>,
}

#[derive(InputObject)]
#[graphql(name = "DeleteProjectV2ItemInput")]
pub struct DeleteProjectV2ItemInput {
    pub project_id: ID,
    pub item_id: ID,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
#[graphql(name = "DeleteProjectV2ItemPayload")]
pub struct DeleteProjectV2ItemPayload {
    pub deleted_item_id: Option<ID>,
    pub client_mutation_id: Option<String>,
}

#[derive(InputObject)]
#[graphql(name = "ArchiveProjectV2ItemInput")]
pub struct ArchiveProjectV2ItemInput {
    pub project_id: ID,
    pub item_id: ID,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
#[graphql(name = "ArchiveProjectV2ItemPayload")]
pub struct ArchiveProjectV2ItemPayload {
    pub item: Option<ProjectV2Item>,
    pub client_mutation_id: Option<String>,
}

#[derive(InputObject)]
#[graphql(name = "UnarchiveProjectV2ItemInput")]
pub struct UnarchiveProjectV2ItemInput {
    pub project_id: ID,
    pub item_id: ID,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
#[graphql(name = "UnarchiveProjectV2ItemPayload")]
pub struct UnarchiveProjectV2ItemPayload {
    pub item: Option<ProjectV2Item>,
    pub client_mutation_id: Option<String>,
}

#[derive(InputObject)]
#[graphql(name = "UpdateProjectV2ItemPositionInput")]
pub struct UpdateProjectV2ItemPositionInput {
    pub project_id: ID,
    pub item_id: ID,
    pub after_id: Option<ID>,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
#[graphql(name = "UpdateProjectV2ItemPositionPayload")]
pub struct UpdateProjectV2ItemPositionPayload {
    pub items: Option<ProjectV2ItemConnection>,
    pub client_mutation_id: Option<String>,
}

#[derive(InputObject)]
#[graphql(name = "ProjectV2SingleSelectFieldOptionInput")]
pub struct ProjectV2SingleSelectFieldOptionInput {
    pub name: String,
    pub color: ProjectV2SingleSelectFieldOptionColor,
    pub description: Option<String>,
}

#[derive(InputObject)]
#[graphql(name = "ProjectV2Iteration")]
pub struct ProjectV2IterationInput {
    pub title: Option<String>,
    pub start_date: Date,
    pub duration: i32,
}

#[derive(InputObject)]
#[graphql(name = "ProjectV2IterationFieldConfigurationInput")]
pub struct ProjectV2IterationFieldConfigurationInput {
    pub start_date: Date,
    pub duration: i32,
    pub iterations: Option<Vec<ProjectV2IterationInput>>,
}

#[derive(InputObject)]
#[graphql(name = "CreateProjectV2FieldInput")]
pub struct CreateProjectV2FieldInput {
    pub project_id: ID,
    pub data_type: ProjectV2CustomFieldType,
    pub name: String,
    pub single_select_options: Option<Vec<ProjectV2SingleSelectFieldOptionInput>>,
    pub iteration_configuration: Option<ProjectV2IterationFieldConfigurationInput>,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
#[graphql(name = "CreateProjectV2FieldPayload")]
pub struct CreateProjectV2FieldPayload {
    pub project_v2_field: Option<ProjectV2FieldConfiguration>,
    pub client_mutation_id: Option<String>,
}

#[derive(InputObject)]
#[graphql(name = "DeleteProjectV2FieldInput")]
pub struct DeleteProjectV2FieldInput {
    pub field_id: ID,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
#[graphql(name = "DeleteProjectV2FieldPayload")]
pub struct DeleteProjectV2FieldPayload {
    pub project_v2_field: Option<ProjectV2FieldConfiguration>,
    pub client_mutation_id: Option<String>,
}

#[derive(InputObject)]
#[graphql(name = "LinkProjectV2ToRepositoryInput")]
pub struct LinkProjectV2ToRepositoryInput {
    pub project_id: ID,
    pub repository_id: ID,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
#[graphql(name = "LinkProjectV2ToRepositoryPayload")]
pub struct LinkProjectV2ToRepositoryPayload {
    pub repository: Option<Repository>,
    pub client_mutation_id: Option<String>,
}

#[derive(InputObject)]
#[graphql(name = "UnlinkProjectV2FromRepositoryInput")]
pub struct UnlinkProjectV2FromRepositoryInput {
    pub project_id: ID,
    pub repository_id: ID,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
#[graphql(name = "UnlinkProjectV2FromRepositoryPayload")]
pub struct UnlinkProjectV2FromRepositoryPayload {
    pub repository: Option<Repository>,
    pub client_mutation_id: Option<String>,
}

#[derive(InputObject)]
#[graphql(name = "LinkProjectV2ToTeamInput")]
pub struct LinkProjectV2ToTeamInput {
    pub project_id: ID,
    pub team_id: ID,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
#[graphql(name = "LinkProjectV2ToTeamPayload")]
pub struct LinkProjectV2ToTeamPayload {
    pub team: Option<Team>,
    pub client_mutation_id: Option<String>,
}

#[derive(InputObject)]
#[graphql(name = "UnlinkProjectV2FromTeamInput")]
pub struct UnlinkProjectV2FromTeamInput {
    pub project_id: ID,
    pub team_id: ID,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
#[graphql(name = "UnlinkProjectV2FromTeamPayload")]
pub struct UnlinkProjectV2FromTeamPayload {
    pub team: Option<Team>,
    pub client_mutation_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// A project item by node id, checked against `project` (when given).
async fn item_in(ctx: &Context<'_>, project: &ProjectV2, id: &ID) -> GResult<ProjectV2Item> {
    let iid = decode(id, &[NodeType::ProjectV2Item], "a ProjectV2Item")?;
    project.item(ctx, iid).await?.ok_or_else(|| {
        not_found(format!(
            "Could not resolve to a ProjectV2Item with the global id of '{}'.",
            id.0
        ))
    })
}

async fn reload_item(ctx: &Context<'_>, project: &ProjectV2, id: i64) -> GResult<ProjectV2Item> {
    project
        .item(ctx, id)
        .await?
        .ok_or_else(|| not_found("item not found"))
}

/// Reload a project after a write (role may matter for the payload).
async fn reload_project(ctx: &Context<'_>, id: i64) -> GResult<ProjectV2> {
    ProjectV2::load(ctx, id)
        .await?
        .ok_or_else(|| not_found("project not found"))
}

/// User ids for user node ids.
fn user_ids(ids: &[ID]) -> GResult<Vec<i64>> {
    ids.iter()
        .map(|id| decode(id, &[NodeType::User, NodeType::Bot], "a User"))
        .collect()
}

/// Add an issue / pull request (by node id) to a project; used by
/// `addProjectV2ItemById` and `createIssue(projectV2Ids:)`.
pub async fn add_content(
    ctx: &Context<'_>,
    project: &ProjectV2,
    content_id: &ID,
) -> GResult<ProjectV2Item> {
    let a = guard(ctx)?;
    let (issue, _) = issue_by_node(ctx, content_id).await?;
    let g = gql(ctx);
    let (item, _) = bgh_projects::items::add_item(
        &g.state,
        a,
        project.pid(),
        CreateBody {
            issue_id: Some(issue.id),
            ..Default::default()
        },
    )
    .await
    .gql()?;
    reload_item(ctx, project, item.id).await
}

/// `projectV2Ids` of `createIssue` / `createPullRequest`: add the new
/// issue to each project (all ids are resolved before the first write).
pub async fn add_to_projects(ctx: &Context<'_>, issue_id: i64, projects: &[ID]) -> GResult<()> {
    let mut resolved = Vec::with_capacity(projects.len());
    for id in projects {
        resolved.push(ProjectV2::by_node(ctx, id).await?);
    }
    let a = guard(ctx)?;
    for project in resolved {
        bgh_projects::items::add_item(
            &gql(ctx).state,
            a,
            project.pid(),
            CreateBody {
                issue_id: Some(issue_id),
                ..Default::default()
            },
        )
        .await
        .gql()?;
    }
    Ok(())
}

async fn set_archived(
    ctx: &Context<'_>,
    project_id: &ID,
    item_id: &ID,
    archived: bool,
) -> GResult<ProjectV2Item> {
    let a = guard(ctx)?;
    let project = ProjectV2::by_node(ctx, project_id).await?;
    let item = item_in(ctx, &project, item_id).await?;
    bgh_projects::items::update_item(
        &gql(ctx).state,
        a,
        project.pid(),
        item.it.id,
        UpdateBody {
            archived: Some(archived),
            ..Default::default()
        },
    )
    .await
    .gql()?;
    reload_item(ctx, &project, item.it.id).await
}

async fn set_value(
    ctx: &Context<'_>,
    project_id: &ID,
    item_id: &ID,
    field_id: &ID,
    value: Value,
) -> GResult<ProjectV2Item> {
    let a = guard(ctx)?;
    let project = ProjectV2::by_node(ctx, project_id).await?;
    let item = item_in(ctx, &project, item_id).await?;
    let fid = decode(field_id, &[NodeType::ProjectV2Field], "a ProjectV2Field")?;
    if !item.fields.iter().any(|f| f.id == fid) {
        return Err(not_found(format!(
            "Could not resolve to a ProjectV2Field with the global id of '{}'.",
            field_id.0
        )));
    }
    bgh_projects::items::update_item(
        &gql(ctx).state,
        a,
        project.pid(),
        item.it.id,
        UpdateBody {
            values: Some(HashMap::from([(fid.to_string(), value)])),
            ..Default::default()
        },
    )
    .await
    .gql()?;
    reload_item(ctx, &project, item.it.id).await
}

async fn link_repo(
    ctx: &Context<'_>,
    project_id: &ID,
    repository_id: &ID,
    link: bool,
) -> GResult<Repository> {
    let a = guard(ctx)?;
    let project = ProjectV2::by_node(ctx, project_id).await?;
    let repo = repo_by_node(ctx, repository_id).await?;
    bgh_projects::projects::link_repo_row(&gql(ctx).state, a, project.pid(), repo.repo.id, link)
        .await
        .gql()?;
    Ok(Repository(repo))
}

async fn link_team(ctx: &Context<'_>, project_id: &ID, team_id: &ID, link: bool) -> GResult<Team> {
    let a = guard(ctx)?;
    let project = ProjectV2::by_node(ctx, project_id).await?;
    let tid = decode(team_id, &[NodeType::Team], "a Team")?;
    let l = ctx.data_unchecked::<Loaders>();
    let team = one(&l.teams, tid)
        .await?
        .ok_or_else(|| not_found("Could not resolve to a Team with the given id."))?;
    bgh_projects::projects::link_team_row(&gql(ctx).state, a, project.pid(), tid, link)
        .await
        .gql()?;
    Ok(Team(team))
}

// ---------------------------------------------------------------------------
// Mutations
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct ProjectMutations;

#[Object]
impl ProjectMutations {
    /// Creates a new project.
    #[graphql(name = "createProjectV2")]
    pub async fn create_project_v2(
        &self,
        ctx: &Context<'_>,
        input: CreateProjectV2Input,
    ) -> GResult<CreateProjectV2Payload> {
        let a = guard(ctx)?;
        let g = gql(ctx);
        let oid = decode(
            &input.owner_id,
            &[NodeType::User, NodeType::Organization],
            "an Owner",
        )?;
        let l = ctx.data_unchecked::<Loaders>();
        let owner = one(&l.users, oid).await?.ok_or_else(|| {
            not_found(format!(
                "Could not resolve to a node with the global id of '{}'.",
                input.owner_id.0
            ))
        })?;
        let row = bgh_projects::projects::create_project(
            &g.state,
            a,
            bgh_projects::projects::CreateBody {
                owner: Some(owner.login.clone()),
                title: Some(input.title),
                ..Default::default()
            },
        )
        .await
        .gql()?;
        if let Some(repo_id) = &input.repository_id {
            let repo = repo_by_node(ctx, repo_id).await?;
            bgh_projects::projects::link_repo_row(&g.state, a, row.id, repo.repo.id, true)
                .await
                .gql()?;
        }
        if let Some(team_id) = &input.team_id {
            let tid = decode(team_id, &[NodeType::Team], "a Team")?;
            bgh_projects::projects::link_team_row(&g.state, a, row.id, tid, true)
                .await
                .gql()?;
        }
        Ok(CreateProjectV2Payload {
            project_v2: Some(reload_project(ctx, row.id).await?),
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Updates an existing project.
    #[graphql(name = "updateProjectV2")]
    pub async fn update_project_v2(
        &self,
        ctx: &Context<'_>,
        input: UpdateProjectV2Input,
    ) -> GResult<UpdateProjectV2Payload> {
        let a = guard(ctx)?;
        let project = ProjectV2::by_node(ctx, &input.project_id).await?;
        bgh_projects::projects::update_project(
            &gql(ctx).state,
            a,
            project.pid(),
            bgh_projects::projects::UpdateBody {
                title: input.title,
                short_description: input.short_description.map(Some),
                readme: input.readme.map(Some),
                public: input.public,
                closed: input.closed,
            },
        )
        .await
        .gql()?;
        Ok(UpdateProjectV2Payload {
            project_v2: Some(reload_project(ctx, project.pid()).await?),
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Deletes a project.
    #[graphql(name = "deleteProjectV2")]
    pub async fn delete_project_v2(
        &self,
        ctx: &Context<'_>,
        input: DeleteProjectV2Input,
    ) -> GResult<DeleteProjectV2Payload> {
        let a = guard(ctx)?;
        let project = ProjectV2::by_node(ctx, &input.project_id).await?;
        bgh_projects::projects::delete_project(&gql(ctx).state, a, project.pid())
            .await
            .gql()?;
        Ok(DeleteProjectV2Payload {
            project_v2: Some(project),
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Links a project to a repository.
    #[graphql(name = "linkProjectV2ToRepository")]
    pub async fn link_project_v2_to_repository(
        &self,
        ctx: &Context<'_>,
        input: LinkProjectV2ToRepositoryInput,
    ) -> GResult<LinkProjectV2ToRepositoryPayload> {
        let repo = link_repo(ctx, &input.project_id, &input.repository_id, true).await?;
        Ok(LinkProjectV2ToRepositoryPayload {
            repository: Some(repo),
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Unlinks a project from a repository.
    #[graphql(name = "unlinkProjectV2FromRepository")]
    pub async fn unlink_project_v2_from_repository(
        &self,
        ctx: &Context<'_>,
        input: UnlinkProjectV2FromRepositoryInput,
    ) -> GResult<UnlinkProjectV2FromRepositoryPayload> {
        let repo = link_repo(ctx, &input.project_id, &input.repository_id, false).await?;
        Ok(UnlinkProjectV2FromRepositoryPayload {
            repository: Some(repo),
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Links a project to a team of its organization.
    #[graphql(name = "linkProjectV2ToTeam")]
    pub async fn link_project_v2_to_team(
        &self,
        ctx: &Context<'_>,
        input: LinkProjectV2ToTeamInput,
    ) -> GResult<LinkProjectV2ToTeamPayload> {
        let team = link_team(ctx, &input.project_id, &input.team_id, true).await?;
        Ok(LinkProjectV2ToTeamPayload {
            team: Some(team),
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Unlinks a project from a team.
    #[graphql(name = "unlinkProjectV2FromTeam")]
    pub async fn unlink_project_v2_from_team(
        &self,
        ctx: &Context<'_>,
        input: UnlinkProjectV2FromTeamInput,
    ) -> GResult<UnlinkProjectV2FromTeamPayload> {
        let team = link_team(ctx, &input.project_id, &input.team_id, false).await?;
        Ok(UnlinkProjectV2FromTeamPayload {
            team: Some(team),
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Adds an existing issue or pull request to a project.
    #[graphql(name = "addProjectV2ItemById")]
    pub async fn add_project_v2_item_by_id(
        &self,
        ctx: &Context<'_>,
        input: AddProjectV2ItemByIdInput,
    ) -> GResult<AddProjectV2ItemByIdPayload> {
        guard(ctx)?;
        let project = ProjectV2::by_node(ctx, &input.project_id).await?;
        let item = add_content(ctx, &project, &input.content_id).await?;
        Ok(AddProjectV2ItemByIdPayload {
            item: Some(item),
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Creates a draft issue in a project.
    #[graphql(name = "addProjectV2DraftIssue")]
    pub async fn add_project_v2_draft_issue(
        &self,
        ctx: &Context<'_>,
        input: AddProjectV2DraftIssueInput,
    ) -> GResult<AddProjectV2DraftIssuePayload> {
        let a = guard(ctx)?;
        let project = ProjectV2::by_node(ctx, &input.project_id).await?;
        let g = gql(ctx);
        let (item, _) = bgh_projects::items::add_item(
            &g.state,
            a,
            project.pid(),
            CreateBody {
                draft: Some(DraftInput {
                    title: Some(input.title),
                    body: input.body,
                }),
                ..Default::default()
            },
        )
        .await
        .gql()?;
        if let Some(ids) = &input.assignee_ids {
            bgh_projects::items::update_item(
                &g.state,
                a,
                project.pid(),
                item.id,
                UpdateBody {
                    assignee_ids: Some(user_ids(ids)?),
                    ..Default::default()
                },
            )
            .await
            .gql()?;
        }
        Ok(AddProjectV2DraftIssuePayload {
            project_item: Some(reload_item(ctx, &project, item.id).await?),
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Updates a draft issue's title, body or assignees.
    #[graphql(name = "updateProjectV2DraftIssue")]
    pub async fn update_project_v2_draft_issue(
        &self,
        ctx: &Context<'_>,
        input: UpdateProjectV2DraftIssueInput,
    ) -> GResult<UpdateProjectV2DraftIssuePayload> {
        let a = guard(ctx)?;
        let iid = decode(
            &input.draft_issue_id,
            &[NodeType::DraftIssue],
            "a DraftIssue",
        )?;
        let pid: Option<i64> = sqlx::query_scalar(
            "SELECT project_id FROM project_items WHERE id = $1 AND content_type = 'DraftIssue'",
        )
        .bind(iid)
        .fetch_optional(&gql(ctx).state.db)
        .await
        .gql()?;
        let missing = || {
            not_found(format!(
                "Could not resolve to a DraftIssue with the global id of '{}'.",
                input.draft_issue_id.0
            ))
        };
        let project = match pid {
            Some(pid) => ProjectV2::load(ctx, pid).await?.ok_or_else(missing)?,
            None => return Err(missing()),
        };
        bgh_projects::items::update_item(
            &gql(ctx).state,
            a,
            project.pid(),
            iid,
            UpdateBody {
                title: input.title.clone(),
                body: input.body.clone().map(Some),
                assignee_ids: match &input.assignee_ids {
                    Some(ids) => Some(user_ids(ids)?),
                    None => None,
                },
                ..Default::default()
            },
        )
        .await
        .gql()?;
        let item = reload_item(ctx, &project, iid).await?;
        Ok(UpdateProjectV2DraftIssuePayload {
            draft_issue: Some(DraftIssue(item)),
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Sets the value of a field on an item.
    #[graphql(name = "updateProjectV2ItemFieldValue")]
    pub async fn update_project_v2_item_field_value(
        &self,
        ctx: &Context<'_>,
        input: UpdateProjectV2ItemFieldValueInput,
    ) -> GResult<UpdateProjectV2ItemFieldValuePayload> {
        let v = input.value;
        let set = [
            v.text.map(Value::String),
            v.number.map(|n| json!(n)),
            v.date.map(|d| Value::String(d.0)),
            v.single_select_option_id.map(Value::String),
            v.iteration_id.map(Value::String),
        ];
        let mut set = set.into_iter().flatten();
        let (Some(value), None) = (set.next(), set.next()) else {
            return Err(err(
                "UNPROCESSABLE",
                "Exactly one of text, number, date, singleSelectOptionId or iterationId must be set.",
            ));
        };
        let item = set_value(
            ctx,
            &input.project_id,
            &input.item_id,
            &input.field_id,
            value,
        )
        .await?;
        Ok(UpdateProjectV2ItemFieldValuePayload {
            project_v2_item: Some(item),
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Clears the value of a field on an item.
    #[graphql(name = "clearProjectV2ItemFieldValue")]
    pub async fn clear_project_v2_item_field_value(
        &self,
        ctx: &Context<'_>,
        input: ClearProjectV2ItemFieldValueInput,
    ) -> GResult<ClearProjectV2ItemFieldValuePayload> {
        let item = set_value(
            ctx,
            &input.project_id,
            &input.item_id,
            &input.field_id,
            Value::Null,
        )
        .await?;
        Ok(ClearProjectV2ItemFieldValuePayload {
            project_v2_item: Some(item),
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Removes an item from a project.
    #[graphql(name = "deleteProjectV2Item")]
    pub async fn delete_project_v2_item(
        &self,
        ctx: &Context<'_>,
        input: DeleteProjectV2ItemInput,
    ) -> GResult<DeleteProjectV2ItemPayload> {
        let a = guard(ctx)?;
        let project = ProjectV2::by_node(ctx, &input.project_id).await?;
        let item = item_in(ctx, &project, &input.item_id).await?;
        bgh_projects::items::delete_item(&gql(ctx).state, a, project.pid(), item.it.id)
            .await
            .gql()?;
        Ok(DeleteProjectV2ItemPayload {
            deleted_item_id: Some(input.item_id),
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Archives an item.
    #[graphql(name = "archiveProjectV2Item")]
    pub async fn archive_project_v2_item(
        &self,
        ctx: &Context<'_>,
        input: ArchiveProjectV2ItemInput,
    ) -> GResult<ArchiveProjectV2ItemPayload> {
        let item = set_archived(ctx, &input.project_id, &input.item_id, true).await?;
        Ok(ArchiveProjectV2ItemPayload {
            item: Some(item),
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Unarchives an item.
    #[graphql(name = "unarchiveProjectV2Item")]
    pub async fn unarchive_project_v2_item(
        &self,
        ctx: &Context<'_>,
        input: UnarchiveProjectV2ItemInput,
    ) -> GResult<UnarchiveProjectV2ItemPayload> {
        let item = set_archived(ctx, &input.project_id, &input.item_id, false).await?;
        Ok(UnarchiveProjectV2ItemPayload {
            item: Some(item),
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Moves an item after another one (`afterId` null: to the top).
    #[graphql(name = "updateProjectV2ItemPosition")]
    pub async fn update_project_v2_item_position(
        &self,
        ctx: &Context<'_>,
        input: UpdateProjectV2ItemPositionInput,
    ) -> GResult<UpdateProjectV2ItemPositionPayload> {
        let a = guard(ctx)?;
        let project = ProjectV2::by_node(ctx, &input.project_id).await?;
        let item = item_in(ctx, &project, &input.item_id).await?;
        let after = match &input.after_id {
            Some(id) => Some(item_in(ctx, &project, id).await?.it.id),
            None => None,
        };
        let g = gql(ctx);
        bgh_projects::items::move_item(&g.state, a, project.pid(), item.it.id, after)
            .await
            .gql()?;
        let rows =
            bgh_projects::rest::filter_items(&g.state, g.auth.as_ref(), project.pid(), None, true)
                .await
                .gql()?;
        let items = project.wrap_items(ctx, rows).await?;
        Ok(UpdateProjectV2ItemPositionPayload {
            items: Some(Page::from_vec(items, &ConnArgs::default())?.into()),
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Creates a custom field.
    #[graphql(name = "createProjectV2Field")]
    pub async fn create_project_v2_field(
        &self,
        ctx: &Context<'_>,
        input: CreateProjectV2FieldInput,
    ) -> GResult<CreateProjectV2FieldPayload> {
        let a = guard(ctx)?;
        let project = ProjectV2::by_node(ctx, &input.project_id).await?;
        let options = input.single_select_options.map(|opts| {
            opts.into_iter()
                .map(|o| OptionInput {
                    id: None,
                    name: Some(o.name),
                    color: Some(o.color.as_str().to_string()),
                    description: o.description,
                })
                .collect()
        });
        let iterations = input.iteration_configuration.map(|c| IterationsInput {
            start_date: Some(c.start_date.0),
            duration: Some(i64::from(c.duration)),
            count: None,
            iterations: c.iterations.map(|its| {
                its.into_iter()
                    .map(|i| IterationInput {
                        id: None,
                        title: i.title,
                        start_date: Some(i.start_date.0),
                        duration: Some(i64::from(i.duration)),
                    })
                    .collect()
            }),
        });
        let field = bgh_projects::fields::create_field(
            &gql(ctx).state,
            a,
            project.pid(),
            bgh_projects::fields::CreateBody {
                name: Some(input.name),
                data_type: Some(input.data_type.data_type().to_string()),
                options,
                iterations,
            },
        )
        .await
        .gql()?;
        Ok(CreateProjectV2FieldPayload {
            project_v2_field: Some(field_config(std::sync::Arc::new(field), project)),
            client_mutation_id: input.client_mutation_id,
        })
    }

    /// Deletes a custom field.
    #[graphql(name = "deleteProjectV2Field")]
    pub async fn delete_project_v2_field(
        &self,
        ctx: &Context<'_>,
        input: DeleteProjectV2FieldInput,
    ) -> GResult<DeleteProjectV2FieldPayload> {
        let a = guard(ctx)?;
        let fid = decode(
            &input.field_id,
            &[NodeType::ProjectV2Field],
            "a ProjectV2Field",
        )?;
        let pid: Option<i64> =
            sqlx::query_scalar("SELECT project_id FROM project_fields WHERE id = $1")
                .bind(fid)
                .fetch_optional(&gql(ctx).state.db)
                .await
                .gql()?;
        let missing = || {
            not_found(format!(
                "Could not resolve to a ProjectV2Field with the global id of '{}'.",
                input.field_id.0
            ))
        };
        let project = match pid {
            Some(pid) => ProjectV2::load(ctx, pid).await?.ok_or_else(missing)?,
            None => return Err(missing()),
        };
        let field = bgh_projects::fields::delete_field(&gql(ctx).state, a, project.pid(), fid)
            .await
            .gql()?;
        Ok(DeleteProjectV2FieldPayload {
            project_v2_field: Some(field_config(std::sync::Arc::new(field), project)),
            client_mutation_id: input.client_mutation_id,
        })
    }
}
