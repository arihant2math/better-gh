//! GitHub GraphQL enums and ordering / filter input objects.

use async_graphql::{Enum, InputObject};

use crate::scalars::DateTime;

#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug, Hash)]
pub enum IssueState {
    Open,
    Closed,
}

#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug, Hash)]
pub enum IssueStateReason {
    Reopened,
    NotPlanned,
    Completed,
    Duplicate,
}

impl IssueStateReason {
    pub fn from_db(s: &str) -> Option<Self> {
        Some(match s {
            "reopened" => Self::Reopened,
            "not_planned" => Self::NotPlanned,
            "completed" => Self::Completed,
            "duplicate" => Self::Duplicate,
            _ => return None,
        })
    }
}

#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug, Hash)]
pub enum IssueClosedStateReason {
    Completed,
    NotPlanned,
    Duplicate,
}

impl IssueClosedStateReason {
    pub fn rest(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::NotPlanned => "not_planned",
            Self::Duplicate => "duplicate",
        }
    }
}

#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug, Hash)]
pub enum PullRequestState {
    Open,
    Closed,
    Merged,
}

#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug, Hash)]
pub enum PullRequestUpdateState {
    Open,
    Closed,
}

#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug)]
pub enum MergeableState {
    Mergeable,
    Conflicting,
    Unknown,
}

#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug)]
pub enum MergeStateStatus {
    Behind,
    Blocked,
    Clean,
    Dirty,
    Draft,
    HasHooks,
    Unknown,
    Unstable,
}

impl MergeStateStatus {
    pub fn from_db(s: &str) -> Self {
        match s {
            "behind" => Self::Behind,
            "blocked" => Self::Blocked,
            "clean" => Self::Clean,
            "dirty" => Self::Dirty,
            "draft" => Self::Draft,
            "has_hooks" => Self::HasHooks,
            "unstable" => Self::Unstable,
            _ => Self::Unknown,
        }
    }
}

#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug)]
pub enum PullRequestReviewDecision {
    ChangesRequested,
    Approved,
    ReviewRequired,
}

#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug, Hash)]
pub enum PullRequestReviewState {
    Pending,
    Commented,
    Approved,
    ChangesRequested,
    Dismissed,
}

impl PullRequestReviewState {
    pub fn from_db(s: &str) -> Self {
        match s {
            "COMMENTED" => Self::Commented,
            "APPROVED" => Self::Approved,
            "CHANGES_REQUESTED" => Self::ChangesRequested,
            "DISMISSED" => Self::Dismissed,
            _ => Self::Pending,
        }
    }
}

#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug)]
pub enum PullRequestReviewEvent {
    Comment,
    Approve,
    RequestChanges,
    Dismiss,
}

impl PullRequestReviewEvent {
    pub fn rest(self) -> &'static str {
        match self {
            Self::Comment => "COMMENT",
            Self::Approve => "APPROVE",
            Self::RequestChanges => "REQUEST_CHANGES",
            Self::Dismiss => "DISMISS",
        }
    }
}

#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug)]
pub enum PullRequestMergeMethod {
    Merge,
    Squash,
    Rebase,
}

impl PullRequestMergeMethod {
    pub fn rest(self) -> &'static str {
        match self {
            Self::Merge => "merge",
            Self::Squash => "squash",
            Self::Rebase => "rebase",
        }
    }

    pub fn from_rest(s: &str) -> Self {
        match s {
            "squash" => Self::Squash,
            "rebase" => Self::Rebase,
            _ => Self::Merge,
        }
    }
}

#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug)]
pub enum CommentAuthorAssociation {
    Member,
    Owner,
    Mannequin,
    Collaborator,
    Contributor,
    FirstTimeContributor,
    FirstTimer,
    None,
}

#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug, Hash)]
pub enum ReactionContent {
    ThumbsUp,
    ThumbsDown,
    Laugh,
    Hooray,
    Confused,
    Heart,
    Rocket,
    Eyes,
}

impl ReactionContent {
    pub const ALL: [Self; 8] = [
        Self::ThumbsUp,
        Self::ThumbsDown,
        Self::Laugh,
        Self::Hooray,
        Self::Confused,
        Self::Heart,
        Self::Rocket,
        Self::Eyes,
    ];

    pub fn rest(self) -> &'static str {
        match self {
            Self::ThumbsUp => "+1",
            Self::ThumbsDown => "-1",
            Self::Laugh => "laugh",
            Self::Hooray => "hooray",
            Self::Confused => "confused",
            Self::Heart => "heart",
            Self::Rocket => "rocket",
            Self::Eyes => "eyes",
        }
    }
}

#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug)]
pub enum RepositoryVisibility {
    Private,
    Public,
    Internal,
}

impl RepositoryVisibility {
    pub fn from_db(s: &str) -> Self {
        match s {
            "private" => Self::Private,
            "internal" => Self::Internal,
            _ => Self::Public,
        }
    }

    pub fn rest(self) -> &'static str {
        match self {
            Self::Private => "private",
            Self::Internal => "internal",
            Self::Public => "public",
        }
    }
}

#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug, Hash)]
pub enum RepositoryPrivacy {
    Public,
    Private,
}

#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug, Hash)]
pub enum RepositoryAffiliation {
    Owner,
    Collaborator,
    OrganizationMember,
}

#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug)]
pub enum RepositoryPermission {
    Admin,
    Maintain,
    Write,
    Triage,
    Read,
}

impl RepositoryPermission {
    pub fn from_perm(p: bgh_core::perms::Permission) -> Option<Self> {
        use bgh_core::perms::Permission as P;
        Some(match p {
            P::Admin => Self::Admin,
            P::Maintain => Self::Maintain,
            P::Write => Self::Write,
            P::Triage => Self::Triage,
            P::Read => Self::Read,
            P::None => return None,
        })
    }
}

#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug)]
pub enum SubscriptionState {
    Unsubscribed,
    Subscribed,
    Ignored,
}

#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug, Hash, Default)]
pub enum OrderDirection {
    #[default]
    Asc,
    Desc,
}

impl OrderDirection {
    pub fn sql(self) -> &'static str {
        match self {
            Self::Asc => "ASC",
            Self::Desc => "DESC",
        }
    }
}

#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug, Hash)]
pub enum RepositoryOrderField {
    CreatedAt,
    UpdatedAt,
    PushedAt,
    Name,
    Stargazers,
}

#[derive(InputObject, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RepositoryOrder {
    pub field: RepositoryOrderField,
    pub direction: OrderDirection,
}

#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug, Hash)]
pub enum IssueOrderField {
    CreatedAt,
    UpdatedAt,
    Comments,
}

#[derive(InputObject, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct IssueOrder {
    pub field: IssueOrderField,
    pub direction: OrderDirection,
}

/// Ways in which to filter lists of issues.
#[derive(InputObject, Clone, Debug, Default)]
pub struct IssueFilters {
    pub assignee: Option<String>,
    pub created_by: Option<String>,
    pub labels: Option<Vec<String>>,
    pub mentioned: Option<String>,
    pub milestone: Option<String>,
    pub milestone_number: Option<String>,
    pub since: Option<DateTime>,
    pub states: Option<Vec<IssueState>>,
    pub viewer_subscribed: Option<bool>,
}

#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug)]
pub enum StatusState {
    Expected,
    Error,
    Failure,
    Pending,
    Success,
}

impl StatusState {
    pub fn from_db(s: &str) -> Self {
        match s {
            "error" | "ERROR" => Self::Error,
            "failure" | "FAILURE" => Self::Failure,
            "pending" | "PENDING" => Self::Pending,
            "EXPECTED" => Self::Expected,
            _ => Self::Success,
        }
    }
}

#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug)]
pub enum CheckStatusState {
    Requested,
    Queued,
    InProgress,
    Completed,
    Waiting,
    Pending,
}

impl CheckStatusState {
    pub fn from_db(s: &str) -> Self {
        match s {
            "requested" => Self::Requested,
            "in_progress" => Self::InProgress,
            "completed" => Self::Completed,
            "waiting" => Self::Waiting,
            "pending" => Self::Pending,
            _ => Self::Queued,
        }
    }
}

#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug)]
pub enum CheckConclusionState {
    ActionRequired,
    TimedOut,
    Cancelled,
    Failure,
    Success,
    Neutral,
    Skipped,
    StartupFailure,
    Stale,
}

impl CheckConclusionState {
    pub fn from_db(s: &str) -> Option<Self> {
        Some(match s {
            "action_required" => Self::ActionRequired,
            "timed_out" => Self::TimedOut,
            "cancelled" => Self::Cancelled,
            "failure" => Self::Failure,
            "success" => Self::Success,
            "neutral" => Self::Neutral,
            "skipped" => Self::Skipped,
            "startup_failure" => Self::StartupFailure,
            "stale" => Self::Stale,
            _ => return None,
        })
    }
}

/// The possible states of a check run in a status rollup.
#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug, Hash)]
pub enum CheckRunState {
    ActionRequired,
    Cancelled,
    Completed,
    Failure,
    InProgress,
    Neutral,
    Pending,
    Queued,
    Skipped,
    Stale,
    StartupFailure,
    Success,
    TimedOut,
    Waiting,
}

#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug)]
pub enum PatchStatus {
    Added,
    Deleted,
    Renamed,
    Copied,
    Modified,
    Changed,
}

impl PatchStatus {
    pub fn from_rest(s: &str) -> Self {
        match s {
            "added" => Self::Added,
            "removed" | "deleted" => Self::Deleted,
            "renamed" => Self::Renamed,
            "copied" => Self::Copied,
            "modified" => Self::Modified,
            _ => Self::Changed,
        }
    }
}

#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug)]
pub enum FileViewedState {
    Dismissed,
    Viewed,
    Unviewed,
}

#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug)]
pub enum SearchType {
    Issue,
    Repository,
    User,
    Discussion,
}

#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug, Hash)]
pub enum MilestoneState {
    Open,
    Closed,
}

#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug, Hash)]
pub enum LockReason {
    OffTopic,
    TooHeated,
    Resolved,
    Spam,
}

impl LockReason {
    pub fn rest(self) -> &'static str {
        match self {
            Self::OffTopic => "off-topic",
            Self::TooHeated => "too heated",
            Self::Resolved => "resolved",
            Self::Spam => "spam",
        }
    }

    pub fn from_db(s: &str) -> Option<Self> {
        Some(match s {
            "off-topic" => Self::OffTopic,
            "too heated" => Self::TooHeated,
            "resolved" => Self::Resolved,
            "spam" => Self::Spam,
            _ => return None,
        })
    }
}

#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug, Hash)]
pub enum RefOrderField {
    TagCommitDate,
    Alphabetical,
}

#[derive(InputObject, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RefOrder {
    pub field: RefOrderField,
    pub direction: OrderDirection,
}

#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug, Hash)]
pub enum ReleaseOrderField {
    CreatedAt,
    Name,
}

#[derive(InputObject, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ReleaseOrder {
    pub field: ReleaseOrderField,
    pub direction: OrderDirection,
}

#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug, Hash)]
pub enum MilestoneOrderField {
    DueDate,
    CreatedAt,
    UpdatedAt,
    Number,
}

#[derive(InputObject, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MilestoneOrder {
    pub field: MilestoneOrderField,
    pub direction: OrderDirection,
}

#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug, Hash)]
pub enum LabelOrderField {
    Name,
    CreatedAt,
}

#[derive(InputObject, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LabelOrder {
    pub field: LabelOrderField,
    pub direction: OrderDirection,
}

#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug)]
pub enum DiffSide {
    Left,
    Right,
}

#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug)]
pub enum PullRequestReviewThreadSubjectType {
    Line,
    File,
}

#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug)]
pub enum RepositoryLockReason {
    Moving,
    Billing,
    Rename,
    Migrating,
    TradeRestriction,
    TransferringOwnership,
}

#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug, Hash)]
pub enum ProjectState {
    Open,
    Closed,
}

#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug, Hash)]
pub enum ProjectOrderField {
    CreatedAt,
    UpdatedAt,
    Name,
}

#[derive(InputObject, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ProjectOrder {
    pub field: ProjectOrderField,
    pub direction: OrderDirection,
}

#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug, Hash)]
#[graphql(name = "ProjectV2OrderField")]
pub enum ProjectV2OrderField {
    Title,
    Number,
    UpdatedAt,
    CreatedAt,
}

#[derive(InputObject, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[graphql(name = "ProjectV2Order")]
pub struct ProjectV2Order {
    pub field: ProjectV2OrderField,
    pub direction: OrderDirection,
}

#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug, Hash)]
pub enum TeamOrderField {
    Name,
}

#[derive(InputObject, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TeamOrder {
    pub field: TeamOrderField,
    pub direction: OrderDirection,
}

#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug, Hash)]
pub enum ComparisonStatus {
    Diverged,
    Ahead,
    Behind,
    Identical,
}
