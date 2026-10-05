//! Small value types: licenses, languages, topics, templates, projects
//! (always empty), reaction groups, rate limit.

use async_graphql::{Context, ID, Object, SimpleObject, Union};

use super::enums::ReactionContent;
use crate::conn::{ConnArgs, Page, PageInfo, connection, encode_cursor};
use crate::ctx::GResult;
use crate::scalars::{DateTime, URI};

/// A repository's open source license.
#[derive(SimpleObject, Clone)]
pub struct License {
    pub key: String,
    pub name: String,
    pub nickname: Option<String>,
    pub spdx_id: Option<String>,
    pub url: Option<URI>,
    pub id: ID,
}

impl License {
    pub fn from_spdx(spdx: &str) -> Self {
        let (name, nickname) = match spdx {
            "MIT" => ("MIT License", None),
            "Apache-2.0" => ("Apache License 2.0", None),
            "GPL-2.0" => ("GNU General Public License v2.0", Some("GNU GPLv2")),
            "GPL-3.0" => ("GNU General Public License v3.0", Some("GNU GPLv3")),
            "AGPL-3.0" => ("GNU Affero General Public License v3.0", Some("GNU AGPLv3")),
            "LGPL-2.1" => (
                "GNU Lesser General Public License v2.1",
                Some("GNU LGPLv2.1"),
            ),
            "LGPL-3.0" => ("GNU Lesser General Public License v3.0", Some("GNU LGPLv3")),
            "BSD-2-Clause" => ("BSD 2-Clause \"Simplified\" License", None),
            "BSD-3-Clause" => ("BSD 3-Clause \"New\" or \"Revised\" License", None),
            "MPL-2.0" => ("Mozilla Public License 2.0", None),
            "Unlicense" => ("The Unlicense", None),
            other => (other, None),
        };
        Self {
            key: spdx.to_lowercase(),
            name: name.to_string(),
            nickname: nickname.map(str::to_string),
            spdx_id: Some(spdx.to_string()),
            url: Some(URI(format!(
                "http://choosealicense.com/licenses/{}/",
                spdx.to_lowercase()
            ))),
            id: ID(bgh_core::node_id::encode_str(
                bgh_core::node_id::NodeType::Blob,
                &format!("license:{}", spdx.to_lowercase()),
            )),
        }
    }
}

/// The Code of Conduct for a repository (never set).
#[derive(SimpleObject, Clone)]
pub struct CodeOfConduct {
    pub key: String,
    pub name: String,
    pub url: Option<URI>,
    pub body: Option<String>,
}

#[derive(SimpleObject, Clone)]
pub struct FundingLink {
    pub platform: String,
    pub url: URI,
}

#[derive(SimpleObject, Clone)]
pub struct RepositoryContactLink {
    pub about: String,
    pub name: String,
    pub url: URI,
}

/// A repository issue template.
#[derive(SimpleObject, Clone)]
pub struct IssueTemplate {
    pub name: String,
    pub title: Option<String>,
    pub body: Option<String>,
    pub about: Option<String>,
    pub filename: String,
}

/// A repository pull request template.
#[derive(SimpleObject, Clone)]
pub struct PullRequestTemplate {
    pub body: Option<String>,
    pub filename: Option<String>,
}

/// Represents a given language found in repositories.
#[derive(Clone)]
pub struct Language {
    pub name: String,
}

impl Language {
    pub fn new(name: String) -> Self {
        Self { name }
    }
}

#[Object]
impl Language {
    pub async fn id(&self) -> ID {
        ID(bgh_core::node_id::encode_str(
            bgh_core::node_id::NodeType::Blob,
            &format!("language:{}", self.name),
        ))
    }
    pub async fn name(&self) -> &str {
        &self.name
    }
    pub async fn color(&self) -> Option<String> {
        let c = match self.name.as_str() {
            "Rust" => "#dea584",
            "Go" => "#00ADD8",
            "JavaScript" => "#f1e05a",
            "TypeScript" => "#3178c6",
            "Python" => "#3572A5",
            "Ruby" => "#701516",
            "Java" => "#b07219",
            "C" => "#555555",
            "C++" => "#f34b7d",
            "Shell" => "#89e051",
            "HTML" => "#e34c26",
            "CSS" => "#563d7c",
            _ => return None,
        };
        Some(c.to_string())
    }
}

#[derive(SimpleObject, Clone)]
pub struct LanguageEdge {
    pub cursor: String,
    pub node: Language,
    pub size: i32,
}

#[derive(SimpleObject)]
pub struct LanguageConnection {
    pub edges: Vec<LanguageEdge>,
    pub nodes: Vec<Language>,
    pub page_info: PageInfo,
    pub total_count: i32,
    pub total_size: i32,
}

impl LanguageConnection {
    pub fn new(langs: Vec<(String, i64)>, args: &ConnArgs) -> GResult<Self> {
        let total_size: i64 = langs.iter().map(|l| l.1).sum();
        let page = Page::from_vec(langs, args)?;
        let page_info = page.page_info();
        let edges = page
            .items
            .iter()
            .enumerate()
            .map(|(i, (name, size))| LanguageEdge {
                cursor: encode_cursor(page.offset + i as i64 + 1),
                node: Language::new(name.clone()),
                size: i32::try_from(*size).unwrap_or(i32::MAX),
            })
            .collect();
        Ok(Self {
            edges,
            nodes: page
                .items
                .into_iter()
                .map(|(n, _)| Language::new(n))
                .collect(),
            page_info,
            total_count: page.total as i32,
            total_size: i32::try_from(total_size).unwrap_or(i32::MAX),
        })
    }
}

/// A topic aggregates entities that are related to a subject.
#[derive(Clone)]
pub struct Topic {
    pub name: String,
}

#[Object]
impl Topic {
    pub async fn id(&self) -> ID {
        ID(bgh_core::node_id::encode_str(
            bgh_core::node_id::NodeType::Blob,
            &format!("topic:{}", self.name),
        ))
    }
    pub async fn name(&self) -> &str {
        &self.name
    }
}

/// A repository-topic connects a repository to a topic.
#[derive(Clone)]
pub struct RepositoryTopic {
    pub name: String,
}

#[Object]
impl RepositoryTopic {
    pub async fn id(&self) -> ID {
        ID(bgh_core::node_id::encode_str(
            bgh_core::node_id::NodeType::Blob,
            &format!("repository-topic:{}", self.name),
        ))
    }
    pub async fn topic(&self) -> Topic {
        Topic {
            name: self.name.clone(),
        }
    }
    pub async fn url(&self, ctx: &Context<'_>) -> URI {
        let g = crate::ctx::gql(ctx);
        URI(g.state.urls.html(&format!("/topics/{}", self.name)))
    }
    pub async fn resource_path(&self) -> URI {
        URI(format!("/topics/{}", self.name))
    }
}

connection!(
    RepositoryTopicConnection,
    RepositoryTopicEdge,
    RepositoryTopic
);

impl RepositoryTopicConnection {
    pub fn new(topics: Vec<String>, args: &ConnArgs) -> GResult<Self> {
        let items = topics
            .into_iter()
            .map(|name| RepositoryTopic { name })
            .collect();
        Ok(Page::from_vec(items, args)?.into())
    }
}

// ---------------------------------------------------------------------------
// Projects (classic and v2): always empty here; bgh-projects owns them.
// ---------------------------------------------------------------------------

#[derive(SimpleObject, Clone)]
pub struct Project {
    pub id: ID,
    pub name: String,
    pub number: i32,
    pub body: Option<String>,
    pub resource_path: URI,
    pub url: URI,
    pub closed: bool,
}

#[derive(SimpleObject, Clone)]
pub struct ProjectColumn {
    pub id: ID,
    pub name: String,
}

#[derive(SimpleObject, Clone)]
pub struct ProjectCard {
    pub id: ID,
    pub project: Project,
    pub column: Option<ProjectColumn>,
}

#[derive(SimpleObject, Clone)]
#[graphql(name = "ProjectV2")]
pub struct ProjectV2 {
    pub id: ID,
    pub number: i32,
    pub title: String,
    pub resource_path: URI,
    pub url: URI,
    pub closed: bool,
}

#[derive(SimpleObject, Clone)]
#[graphql(name = "ProjectV2ItemFieldSingleSelectValue")]
pub struct ProjectV2ItemFieldSingleSelectValue {
    pub option_id: Option<String>,
    pub name: Option<String>,
}

#[derive(SimpleObject, Clone)]
#[graphql(name = "ProjectV2ItemFieldTextValue")]
pub struct ProjectV2ItemFieldTextValue {
    pub text: Option<String>,
}

#[derive(Union, Clone)]
#[graphql(name = "ProjectV2ItemFieldValue")]
pub enum ProjectV2ItemFieldValue {
    SingleSelect(ProjectV2ItemFieldSingleSelectValue),
    Text(ProjectV2ItemFieldTextValue),
}

#[derive(Clone)]
pub struct ProjectV2Item {
    pub id: ID,
    pub project: ProjectV2,
}

#[Object(name = "ProjectV2Item")]
impl ProjectV2Item {
    pub async fn id(&self) -> &ID {
        &self.id
    }
    pub async fn project(&self) -> &ProjectV2 {
        &self.project
    }
    pub async fn field_value_by_name(&self, name: String) -> Option<ProjectV2ItemFieldValue> {
        let _ = name;
        None
    }
}

macro_rules! empty_connection {
    ($conn:ident, $node:ty, $name:literal) => {
        #[derive(Default)]
        pub struct $conn;

        #[Object(name = $name)]
        impl $conn {
            async fn nodes(&self) -> Vec<$node> {
                vec![]
            }
            async fn total_count(&self) -> i32 {
                0
            }
            async fn page_info(&self) -> PageInfo {
                Page::<()>::empty().page_info()
            }
        }
    };
}

empty_connection!(ProjectConnection, Project, "ProjectConnection");
empty_connection!(ProjectV2Connection, ProjectV2, "ProjectV2Connection");
empty_connection!(ProjectCardConnection, ProjectCard, "ProjectCardConnection");
empty_connection!(
    ProjectV2ItemConnection,
    ProjectV2Item,
    "ProjectV2ItemConnection"
);

// ---------------------------------------------------------------------------
// Reactions
// ---------------------------------------------------------------------------

/// A group of emoji reactions to a particular piece of content.
#[derive(Clone)]
pub struct ReactionGroup {
    pub content: ReactionContent,
    pub count: i64,
    pub viewer_has_reacted: bool,
}

#[Object]
impl ReactionGroup {
    pub async fn content(&self) -> ReactionContent {
        self.content
    }
    pub async fn viewer_has_reacted(&self) -> bool {
        self.viewer_has_reacted
    }
    pub async fn users(&self) -> ReactorCount {
        ReactorCount(self.count)
    }
    pub async fn reactors(&self) -> ReactorCount {
        ReactorCount(self.count)
    }
    pub async fn created_at(&self) -> Option<DateTime> {
        None
    }
}

pub struct ReactorCount(pub i64);

#[Object(name = "ReactingUserConnection")]
impl ReactorCount {
    pub async fn total_count(&self) -> i32 {
        self.0 as i32
    }
}

/// GitHub always returns all eight groups, in a fixed order.
pub fn reaction_groups(summary: &crate::loaders::ReactionSummary) -> Vec<ReactionGroup> {
    ReactionContent::ALL
        .iter()
        .map(|c| {
            let (count, mine) = summary
                .groups
                .iter()
                .find(|g| g.0 == c.rest())
                .map(|g| (g.1, g.2))
                .unwrap_or((0, false));
            ReactionGroup {
                content: *c,
                count,
                viewer_has_reacted: mine,
            }
        })
        .collect()
}

pub async fn load_reaction_groups(
    ctx: &Context<'_>,
    subject_type: &'static str,
    id: i64,
) -> GResult<Vec<ReactionGroup>> {
    let l = ctx.data_unchecked::<crate::loaders::Loaders>();
    let s = crate::loaders::one(&l.reactions, (subject_type.to_string(), id))
        .await?
        .unwrap_or_default();
    Ok(reaction_groups(&s))
}

// ---------------------------------------------------------------------------
// Rate limit
// ---------------------------------------------------------------------------

/// Represents the client's rate limit.
#[derive(SimpleObject, Clone)]
pub struct RateLimit {
    pub cost: i32,
    pub limit: i32,
    pub node_count: i32,
    pub remaining: i32,
    pub reset_at: DateTime,
    pub used: i32,
}
