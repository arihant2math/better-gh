//! GraphQL object model (GitHub v4 type and field names).

// The interface derives repeat `ty = ...` in field/arg attributes, which
// clippy mistakes for duplicated attributes.
#![allow(clippy::duplicated_attributes)]

pub mod actor;
pub mod enums;
pub mod git;
pub mod issue;
pub mod issue_type;
pub mod merge_queue;
pub mod misc;
pub mod moderation;
pub mod project;
pub mod pull;
pub mod release;
pub mod repo;
pub mod ruleset;

use async_graphql::{ID, Interface};
use bgh_core::node_id::{self, NodeType};

pub use actor::{Bot, Mannequin, Organization, Team, User};
pub use git::Commit;
pub use issue::{Issue, IssueComment, Label, Milestone};
pub use pull::{PullRequest, PullRequestReview};
pub use release::Release;
pub use repo::{Ref, Repository};

use crate::scalars::URI;

/// `id` of a node.
pub fn nid(ty: NodeType, id: i64) -> ID {
    ID(node_id::encode(ty, id))
}

/// An object with an ID.
#[derive(Interface, Clone)]
#[graphql(field(name = "id", ty = "ID"))]
pub enum Node {
    User(User),
    Organization(Organization),
    Bot(Bot),
    Team(Team),
    Repository(Repository),
    Issue(Issue),
    PullRequest(PullRequest),
    IssueComment(IssueComment),
    Label(Label),
    Milestone(Milestone),
    PullRequestReview(PullRequestReview),
    Release(Release),
    Ref(Ref),
    Commit(Commit),
    ProjectV2(project::ProjectV2),
    ProjectV2Item(project::ProjectV2Item),
    DraftIssue(project::DraftIssue),
    ProjectV2Field(project::ProjectV2Field),
    ProjectV2SingleSelectField(project::ProjectV2SingleSelectField),
    ProjectV2IterationField(project::ProjectV2IterationField),
    ProjectV2View(project::ProjectV2View),
    MergeQueue(merge_queue::MergeQueue),
    MergeQueueEntry(merge_queue::MergeQueueEntry),
}

/// Represents an object which can take actions on GitHub.
#[derive(Interface, Clone)]
#[graphql(
    field(name = "login", ty = "String"),
    field(
        name = "avatar_url",
        ty = "URI",
        arg(name = "size", ty = "Option<i32>")
    ),
    field(name = "url", ty = "URI"),
    field(name = "resource_path", ty = "URI")
)]
pub enum Actor {
    User(User),
    Organization(Organization),
    Bot(Bot),
    Mannequin(Mannequin),
}

/// Represents an owner of a Repository.
#[derive(Interface, Clone)]
#[graphql(
    field(name = "id", ty = "ID"),
    field(name = "login", ty = "String"),
    field(
        name = "avatar_url",
        ty = "URI",
        arg(name = "size", ty = "Option<i32>")
    ),
    field(name = "url", ty = "URI"),
    field(name = "resource_path", ty = "URI"),
    field(
        name = "repository",
        ty = "Option<Repository>",
        arg(name = "name", ty = "String"),
        arg(name = "follow_renames", ty = "bool", default)
    ),
    field(
        name = "repositories",
        ty = "repo::RepositoryConnection",
        arg(name = "first", ty = "Option<i32>"),
        arg(name = "last", ty = "Option<i32>"),
        arg(name = "after", ty = "Option<String>"),
        arg(name = "before", ty = "Option<String>"),
        arg(name = "privacy", ty = "Option<enums::RepositoryPrivacy>"),
        arg(name = "is_fork", ty = "Option<bool>"),
        arg(name = "is_archived", ty = "Option<bool>"),
        arg(name = "is_locked", ty = "Option<bool>"),
        arg(
            name = "owner_affiliations",
            ty = "Option<Vec<Option<enums::RepositoryAffiliation>>>"
        ),
        arg(
            name = "affiliations",
            ty = "Option<Vec<Option<enums::RepositoryAffiliation>>>"
        ),
        arg(name = "order_by", ty = "Option<enums::RepositoryOrder>")
    )
)]
pub enum RepositoryOwner {
    User(User),
    Organization(Organization),
}

impl RepositoryOwner {
    pub fn from_user(u: std::sync::Arc<bgh_core::models::db::User>) -> Self {
        if u.is_org() {
            Self::Organization(Organization(u))
        } else {
            Self::User(User(u))
        }
    }
}

impl Actor {
    pub fn from_user(u: std::sync::Arc<bgh_core::models::db::User>) -> Self {
        match u.kind.as_str() {
            "Organization" => Self::Organization(Organization(u)),
            "Bot" => Self::Bot(Bot(u)),
            _ => Self::User(User(u)),
        }
    }
}
