//! Opaque GraphQL global node ids.
//!
//! Uses GitHub's legacy format: base64 of `"{len:02}:{Type}{id}"`, e.g.
//! `MDQ6VXNlcjE=` = base64("04:User1"). Existing GitHub clients treat ids as
//! opaque strings, and the GraphQL layer decodes them with [`decode`].

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

/// Node types with a stable name. Add variants as new node kinds appear.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum NodeType {
    User,
    Organization,
    Bot,
    Team,
    Repository,
    Issue,
    PullRequest,
    IssueComment,
    Label,
    Milestone,
    PullRequestReview,
    PullRequestReviewComment,
    Release,
    ReleaseAsset,
    Commit,
    Ref,
    Tree,
    Blob,
    Status,
    CheckSuite,
    CheckRun,
    Reaction,
    Hook,
    PublicKey,
    GpgKey,
    DeployKey,
    Notification,
    IssueEvent,
    OrganizationInvitation,
    PullRequestReviewThread,
    Workflow,
    WorkflowRun,
    Artifact,
    Environment,
    Deployment,
    DeploymentStatus,
    ProjectV2,
    ProjectV2Item,
    ProjectV2Field,
    ProjectV2View,
    DraftIssue,
    CommitComment,
    /// GitHub App (`Integration`).
    Integration,
    DeploymentBranchPolicy,
    /// An environment protection rule (GitHub's `Gate`).
    EnvironmentProtectionRule,
}

impl NodeType {
    pub const ALL: &'static [NodeType] = &[
        Self::User,
        Self::Organization,
        Self::Bot,
        Self::Team,
        Self::Repository,
        Self::Issue,
        Self::PullRequest,
        Self::IssueComment,
        Self::Label,
        Self::Milestone,
        Self::PullRequestReview,
        Self::PullRequestReviewComment,
        Self::Release,
        Self::ReleaseAsset,
        Self::Commit,
        Self::Ref,
        Self::Tree,
        Self::Blob,
        Self::Status,
        Self::CheckSuite,
        Self::CheckRun,
        Self::Reaction,
        Self::Hook,
        Self::PublicKey,
        Self::GpgKey,
        Self::DeployKey,
        Self::Notification,
        Self::IssueEvent,
        Self::OrganizationInvitation,
        Self::PullRequestReviewThread,
        Self::Workflow,
        Self::WorkflowRun,
        Self::Artifact,
        Self::Environment,
        Self::Deployment,
        Self::DeploymentStatus,
        Self::ProjectV2,
        Self::ProjectV2Item,
        Self::ProjectV2Field,
        Self::ProjectV2View,
        Self::DraftIssue,
        Self::CommitComment,
        Self::Integration,
        Self::DeploymentBranchPolicy,
        Self::EnvironmentProtectionRule,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "User",
            Self::Organization => "Organization",
            Self::Bot => "Bot",
            Self::Team => "Team",
            Self::Repository => "Repository",
            Self::Issue => "Issue",
            Self::PullRequest => "PullRequest",
            Self::IssueComment => "IssueComment",
            Self::Label => "Label",
            Self::Milestone => "Milestone",
            Self::PullRequestReview => "PullRequestReview",
            Self::PullRequestReviewComment => "PullRequestReviewComment",
            Self::Release => "Release",
            Self::ReleaseAsset => "ReleaseAsset",
            Self::Commit => "Commit",
            Self::Ref => "Ref",
            Self::Tree => "Tree",
            Self::Blob => "Blob",
            Self::Status => "Status",
            Self::CheckSuite => "CheckSuite",
            Self::CheckRun => "CheckRun",
            Self::Reaction => "Reaction",
            Self::Hook => "Hook",
            Self::PublicKey => "PublicKey",
            Self::GpgKey => "GpgKey",
            Self::DeployKey => "DeployKey",
            Self::Notification => "Notification",
            Self::IssueEvent => "IssueEvent",
            Self::OrganizationInvitation => "OrganizationInvitation",
            Self::PullRequestReviewThread => "PullRequestReviewThread",
            Self::Workflow => "Workflow",
            Self::WorkflowRun => "WorkflowRun",
            Self::Artifact => "Artifact",
            Self::Environment => "Environment",
            Self::Deployment => "Deployment",
            Self::DeploymentStatus => "DeploymentStatus",
            Self::ProjectV2 => "ProjectV2",
            Self::ProjectV2Item => "ProjectV2Item",
            Self::ProjectV2Field => "ProjectV2Field",
            Self::ProjectV2View => "ProjectV2View",
            Self::DraftIssue => "DraftIssue",
            Self::CommitComment => "CommitComment",
            Self::Integration => "Integration",
            Self::DeploymentBranchPolicy => "DeploymentBranchPolicy",
            Self::EnvironmentProtectionRule => "Gate",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|t| t.as_str() == s)
    }

    /// Node type for a `users.type` value.
    pub fn for_user_kind(kind: &str) -> Self {
        match kind {
            "Organization" => Self::Organization,
            "Bot" => Self::Bot,
            _ => Self::User,
        }
    }
}

/// Encode a node id for a numeric database id.
///
/// Draft issues get GitHub's `DI_` prefix: `gh project item-edit` checks
/// for it before treating an id as draft-issue content.
pub fn encode(ty: NodeType, id: i64) -> String {
    let name = ty.as_str();
    let id = STANDARD.encode(format!("{:02}:{name}{id}", name.len()));
    if ty == NodeType::DraftIssue {
        format!("{DRAFT_ISSUE_PREFIX}{id}")
    } else {
        id
    }
}

const DRAFT_ISSUE_PREFIX: &str = "DI_";

/// Encode a node id for a string key (e.g. commit SHAs: `"{repo_id}:{sha}"`).
pub fn encode_str(ty: NodeType, key: &str) -> String {
    let name = ty.as_str();
    STANDARD.encode(format!("{:02}:{name}{key}", name.len()))
}

/// Decode a node id into its type and raw key.
pub fn decode_raw(node_id: &str) -> Option<(NodeType, String)> {
    if let Some(rest) = node_id.strip_prefix(DRAFT_ISSUE_PREFIX) {
        return decode_raw(rest).filter(|(ty, _)| *ty == NodeType::DraftIssue);
    }
    let bytes = STANDARD.decode(node_id).ok()?;
    let s = String::from_utf8(bytes).ok()?;
    let (len, rest) = s.split_once(':')?;
    let len: usize = len.parse().ok()?;
    if rest.len() < len || !rest.is_char_boundary(len) {
        return None;
    }
    let (name, key) = rest.split_at(len);
    Some((NodeType::parse(name)?, key.to_string()))
}

/// Decode a node id with a numeric key.
pub fn decode(node_id: &str) -> Option<(NodeType, i64)> {
    let (ty, key) = decode_raw(node_id)?;
    Some((ty, key.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_github_legacy_format() {
        assert_eq!(encode(NodeType::User, 1), "MDQ6VXNlcjE=");
        assert_eq!(decode("MDQ6VXNlcjE="), Some((NodeType::User, 1)));
        let id = encode(NodeType::PullRequestReviewComment, 42);
        assert_eq!(decode(&id), Some((NodeType::PullRequestReviewComment, 42)));
        assert_eq!(decode("garbage"), None);
        let c = encode_str(NodeType::Commit, "7:abc");
        assert_eq!(decode_raw(&c), Some((NodeType::Commit, "7:abc".into())));
        let d = encode(NodeType::DraftIssue, 9);
        assert!(d.starts_with("DI_"));
        assert_eq!(decode(&d), Some((NodeType::DraftIssue, 9)));
    }
}
