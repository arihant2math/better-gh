//! Typed domain events and the in-process event bus.
//!
//! Events are emitted **after commit** (use [`crate::db::Tx::emit`]) and
//! delivered to listeners registered via
//! [`crate::registry::Registry::on_event`]. Delivery is in-process and
//! best-effort; listeners that need durability should enqueue a job.
//!
//! Events carry ids, not full objects: listeners load what they need.
//! Add variants freely (the enum is `#[non_exhaustive]`).

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

/// A ref change from a push or API write. `old`/`new` are hex SHAs; the zero
/// SHA (`000…0`) denotes creation / deletion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefUpdate {
    pub old: String,
    pub new: String,
    /// Full ref name, e.g. `refs/heads/main`.
    pub refname: String,
}

pub const ZERO_SHA: &str = "0000000000000000000000000000000000000000";

impl RefUpdate {
    pub fn is_create(&self) -> bool {
        self.old.bytes().all(|b| b == b'0')
    }

    pub fn is_delete(&self) -> bool {
        self.new.bytes().all(|b| b == b'0')
    }

    /// Branch name for `refs/heads/*`.
    pub fn branch(&self) -> Option<&str> {
        self.refname.strip_prefix("refs/heads/")
    }

    /// Tag name for `refs/tags/*`.
    pub fn tag(&self) -> Option<&str> {
        self.refname.strip_prefix("refs/tags/")
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PushEvent {
    pub repo_id: i64,
    pub pusher_id: Option<i64>,
    pub updates: Vec<RefUpdate>,
}

/// Domain events. Serialized as `{"type": "issue_opened", ...}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Event {
    Push(PushEvent),
    RepositoryCreated {
        repo_id: i64,
        actor_id: i64,
    },
    RepositoryDeleted {
        repo_id: i64,
        owner_id: i64,
        full_name: String,
        actor_id: i64,
    },
    RepositoryUpdated {
        repo_id: i64,
        actor_id: i64,
    },
    IssueOpened {
        repo_id: i64,
        issue_id: i64,
        actor_id: i64,
    },
    IssueEdited {
        repo_id: i64,
        issue_id: i64,
        actor_id: i64,
        /// GitHub-style `changes` object (`{"title": {"from": "..."}}`).
        changes: serde_json::Value,
    },
    IssueClosed {
        repo_id: i64,
        issue_id: i64,
        actor_id: i64,
    },
    IssueReopened {
        repo_id: i64,
        issue_id: i64,
        actor_id: i64,
    },
    IssueCommentCreated {
        repo_id: i64,
        issue_id: i64,
        comment_id: i64,
        actor_id: i64,
    },
    IssueCommentEdited {
        repo_id: i64,
        issue_id: i64,
        comment_id: i64,
        actor_id: i64,
    },
    IssueCommentDeleted {
        repo_id: i64,
        issue_id: i64,
        comment_id: i64,
        actor_id: i64,
    },
    /// `pull_id` is the issue id of the pull request.
    PullRequestOpened {
        repo_id: i64,
        pull_id: i64,
        actor_id: i64,
    },
    PullRequestSynchronized {
        repo_id: i64,
        pull_id: i64,
        actor_id: Option<i64>,
        before: String,
        after: String,
    },
    PullRequestClosed {
        repo_id: i64,
        pull_id: i64,
        actor_id: i64,
    },
    PullRequestReopened {
        repo_id: i64,
        pull_id: i64,
        actor_id: i64,
    },
    PullRequestMerged {
        repo_id: i64,
        pull_id: i64,
        actor_id: i64,
        merge_commit_sha: String,
    },
    PullRequestReviewSubmitted {
        repo_id: i64,
        pull_id: i64,
        review_id: i64,
        actor_id: i64,
    },
    // ----- pulls (B4) -----
    PullRequestEdited {
        repo_id: i64,
        pull_id: i64,
        actor_id: i64,
        /// GitHub-style `changes` object (`{"title": {"from": "..."}}`).
        changes: serde_json::Value,
    },
    PullRequestReadyForReview {
        repo_id: i64,
        pull_id: i64,
        actor_id: i64,
    },
    PullRequestConvertedToDraft {
        repo_id: i64,
        pull_id: i64,
        actor_id: i64,
    },
    PullRequestReviewRequested {
        repo_id: i64,
        pull_id: i64,
        actor_id: i64,
        reviewer_id: Option<i64>,
        team_id: Option<i64>,
    },
    PullRequestReviewRequestRemoved {
        repo_id: i64,
        pull_id: i64,
        actor_id: i64,
        reviewer_id: Option<i64>,
        team_id: Option<i64>,
    },
    PullRequestReviewEdited {
        repo_id: i64,
        pull_id: i64,
        review_id: i64,
        actor_id: i64,
    },
    PullRequestReviewDismissed {
        repo_id: i64,
        pull_id: i64,
        review_id: i64,
        actor_id: Option<i64>,
    },
    PullRequestReviewCommentCreated {
        repo_id: i64,
        pull_id: i64,
        comment_id: i64,
        actor_id: i64,
    },
    PullRequestReviewCommentEdited {
        repo_id: i64,
        pull_id: i64,
        comment_id: i64,
        actor_id: i64,
    },
    PullRequestReviewCommentDeleted {
        repo_id: i64,
        pull_id: i64,
        comment_id: i64,
        actor_id: i64,
    },
    /// `comment_id` is the thread's root review comment.
    PullRequestReviewThreadResolved {
        repo_id: i64,
        pull_id: i64,
        comment_id: i64,
        actor_id: i64,
    },
    PullRequestReviewThreadUnresolved {
        repo_id: i64,
        pull_id: i64,
        comment_id: i64,
        actor_id: i64,
    },
    PullRequestAutoMergeEnabled {
        repo_id: i64,
        pull_id: i64,
        actor_id: i64,
    },
    PullRequestAutoMergeDisabled {
        repo_id: i64,
        pull_id: i64,
        actor_id: Option<i64>,
    },
    CommitStatusCreated {
        repo_id: i64,
        status_id: i64,
        sha: String,
        actor_id: i64,
    },
    CheckRunCreated {
        repo_id: i64,
        check_run_id: i64,
        actor_id: Option<i64>,
    },
    CheckRunCompleted {
        repo_id: i64,
        check_run_id: i64,
        actor_id: Option<i64>,
    },
    CheckRunRerequested {
        repo_id: i64,
        check_run_id: i64,
        actor_id: i64,
    },
    /// A check suite was created (`POST /check-suites`) and wants runs.
    CheckSuiteRequested {
        repo_id: i64,
        check_suite_id: i64,
        actor_id: Option<i64>,
    },
    CheckSuiteRerequested {
        repo_id: i64,
        check_suite_id: i64,
        actor_id: i64,
    },
    CheckSuiteCompleted {
        repo_id: i64,
        check_suite_id: i64,
    },
    ReleasePublished {
        repo_id: i64,
        release_id: i64,
        actor_id: i64,
    },
    IssueLabeled {
        repo_id: i64,
        issue_id: i64,
        label_id: i64,
        actor_id: i64,
    },
    IssueUnlabeled {
        repo_id: i64,
        issue_id: i64,
        label_id: i64,
        actor_id: i64,
    },
    IssueAssigned {
        repo_id: i64,
        issue_id: i64,
        assignee_id: i64,
        actor_id: i64,
    },
    IssueUnassigned {
        repo_id: i64,
        issue_id: i64,
        assignee_id: i64,
        actor_id: i64,
    },
    IssueMilestoned {
        repo_id: i64,
        issue_id: i64,
        milestone_id: i64,
        actor_id: i64,
    },
    IssueDemilestoned {
        repo_id: i64,
        issue_id: i64,
        milestone_id: i64,
        actor_id: i64,
    },
    IssueLocked {
        repo_id: i64,
        issue_id: i64,
        actor_id: i64,
    },
    IssueUnlocked {
        repo_id: i64,
        issue_id: i64,
        actor_id: i64,
    },
    IssuePinned {
        repo_id: i64,
        issue_id: i64,
        actor_id: i64,
    },
    IssueUnpinned {
        repo_id: i64,
        issue_id: i64,
        actor_id: i64,
    },
    IssueTransferred {
        /// New repository.
        repo_id: i64,
        issue_id: i64,
        old_repo_id: i64,
        old_number: i64,
        actor_id: i64,
    },
    IssueMentioned {
        repo_id: i64,
        issue_id: i64,
        /// Set when the mention is in a comment (else the issue body).
        comment_id: Option<i64>,
        /// The mentioned user.
        user_id: i64,
        actor_id: i64,
    },
    IssueCrossReferenced {
        /// Repository of the referenced issue.
        repo_id: i64,
        /// The referenced issue.
        issue_id: i64,
        /// Issue (or PR) whose body/comment mentions it.
        source_issue_id: i64,
        source_comment_id: Option<i64>,
        actor_id: i64,
    },
    IssueReferenced {
        repo_id: i64,
        issue_id: i64,
        commit_id: String,
        actor_id: Option<i64>,
    },
    SubIssueAdded {
        /// Repository of the parent issue.
        repo_id: i64,
        parent_id: i64,
        sub_issue_id: i64,
        actor_id: i64,
    },
    SubIssueRemoved {
        repo_id: i64,
        parent_id: i64,
        sub_issue_id: i64,
        actor_id: i64,
    },
    LabelCreated {
        repo_id: i64,
        label_id: i64,
        actor_id: i64,
    },
    LabelEdited {
        repo_id: i64,
        label_id: i64,
        actor_id: i64,
        /// GitHub-style `changes` object.
        changes: serde_json::Value,
    },
    LabelDeleted {
        repo_id: i64,
        label_id: i64,
        name: String,
        actor_id: i64,
    },
    MilestoneCreated {
        repo_id: i64,
        milestone_id: i64,
        actor_id: i64,
    },
    MilestoneEdited {
        repo_id: i64,
        milestone_id: i64,
        actor_id: i64,
        changes: serde_json::Value,
    },
    MilestoneClosed {
        repo_id: i64,
        milestone_id: i64,
        actor_id: i64,
    },
    MilestoneOpened {
        repo_id: i64,
        milestone_id: i64,
        actor_id: i64,
    },
    MilestoneDeleted {
        repo_id: i64,
        milestone_id: i64,
        number: i64,
        title: String,
        actor_id: i64,
    },
    ReactionCreated {
        repo_id: i64,
        /// `issue` | `issue_comment` | ...
        subject_type: String,
        subject_id: i64,
        reaction_id: i64,
        actor_id: i64,
    },
    ReactionDeleted {
        repo_id: i64,
        subject_type: String,
        subject_id: i64,
        reaction_id: i64,
        actor_id: i64,
    },
    OrgMemberAdded {
        org_id: i64,
        user_id: i64,
        actor_id: i64,
    },
    /// Someone's read access may have changed (collaborator/team/membership
    /// removed, visibility changed, transfer). bgh-sync rechecks the
    /// affected live subscriptions and revokes lost scopes. Set whichever
    /// ids are known; all `None` rechecks every subscription.
    AccessChanged {
        repo_id: Option<i64>,
        org_id: Option<i64>,
        user_id: Option<i64>,
    },
    OrgMemberRemoved {
        org_id: i64,
        user_id: i64,
        actor_id: i64,
    },
    OrgMemberInvited {
        org_id: i64,
        invitation_id: i64,
        actor_id: i64,
    },
    TeamCreated {
        org_id: i64,
        team_id: i64,
        actor_id: i64,
    },
    TeamEdited {
        org_id: i64,
        team_id: i64,
        actor_id: i64,
        /// GitHub-style `changes` object.
        changes: serde_json::Value,
    },
    TeamDeleted {
        org_id: i64,
        team_id: i64,
        slug: String,
        actor_id: i64,
    },
    TeamMemberAdded {
        org_id: i64,
        team_id: i64,
        user_id: i64,
        actor_id: i64,
    },
    TeamMemberRemoved {
        org_id: i64,
        team_id: i64,
        user_id: i64,
        actor_id: i64,
    },
    TeamRepoAdded {
        org_id: i64,
        team_id: i64,
        repo_id: i64,
        actor_id: i64,
    },
    TeamRepoRemoved {
        org_id: i64,
        team_id: i64,
        repo_id: i64,
        actor_id: i64,
    },
    UserFollowed {
        actor_id: i64,
        target_id: i64,
    },
    /// A user starred (`starred: true`) or unstarred a repository.
    RepositoryStarred {
        repo_id: i64,
        actor_id: i64,
        starred: bool,
    },
    /// `fork_id` was created as a fork of `repo_id`.
    RepositoryForked {
        repo_id: i64,
        fork_id: i64,
        actor_id: i64,
    },
    RepositoryRenamed {
        repo_id: i64,
        actor_id: i64,
        old_name: String,
    },
    RepositoryTransferred {
        repo_id: i64,
        actor_id: i64,
        old_owner_id: i64,
    },
    /// A collaborator was added (invitation accepted or direct add).
    CollaboratorAdded {
        repo_id: i64,
        user_id: i64,
        actor_id: i64,
    },
}

impl Event {
    /// Stable snake_case name (`"issue_opened"`), same as the serde tag.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Push(_) => "push",
            Self::RepositoryCreated { .. } => "repository_created",
            Self::RepositoryDeleted { .. } => "repository_deleted",
            Self::RepositoryUpdated { .. } => "repository_updated",
            Self::IssueOpened { .. } => "issue_opened",
            Self::IssueEdited { .. } => "issue_edited",
            Self::IssueClosed { .. } => "issue_closed",
            Self::IssueReopened { .. } => "issue_reopened",
            Self::IssueCommentCreated { .. } => "issue_comment_created",
            Self::IssueCommentEdited { .. } => "issue_comment_edited",
            Self::IssueCommentDeleted { .. } => "issue_comment_deleted",
            Self::PullRequestOpened { .. } => "pull_request_opened",
            Self::PullRequestSynchronized { .. } => "pull_request_synchronized",
            Self::PullRequestClosed { .. } => "pull_request_closed",
            Self::PullRequestReopened { .. } => "pull_request_reopened",
            Self::PullRequestMerged { .. } => "pull_request_merged",
            Self::PullRequestReviewSubmitted { .. } => "pull_request_review_submitted",
            Self::PullRequestEdited { .. } => "pull_request_edited",
            Self::PullRequestReadyForReview { .. } => "pull_request_ready_for_review",
            Self::PullRequestConvertedToDraft { .. } => "pull_request_converted_to_draft",
            Self::PullRequestReviewRequested { .. } => "pull_request_review_requested",
            Self::PullRequestReviewRequestRemoved { .. } => "pull_request_review_request_removed",
            Self::PullRequestReviewEdited { .. } => "pull_request_review_edited",
            Self::PullRequestReviewDismissed { .. } => "pull_request_review_dismissed",
            Self::PullRequestReviewCommentCreated { .. } => "pull_request_review_comment_created",
            Self::PullRequestReviewCommentEdited { .. } => "pull_request_review_comment_edited",
            Self::PullRequestReviewCommentDeleted { .. } => "pull_request_review_comment_deleted",
            Self::PullRequestReviewThreadResolved { .. } => "pull_request_review_thread_resolved",
            Self::PullRequestReviewThreadUnresolved { .. } => {
                "pull_request_review_thread_unresolved"
            }
            Self::PullRequestAutoMergeEnabled { .. } => "pull_request_auto_merge_enabled",
            Self::PullRequestAutoMergeDisabled { .. } => "pull_request_auto_merge_disabled",
            Self::CommitStatusCreated { .. } => "commit_status_created",
            Self::CheckRunCreated { .. } => "check_run_created",
            Self::CheckRunCompleted { .. } => "check_run_completed",
            Self::CheckRunRerequested { .. } => "check_run_rerequested",
            Self::CheckSuiteRequested { .. } => "check_suite_requested",
            Self::CheckSuiteRerequested { .. } => "check_suite_rerequested",
            Self::CheckSuiteCompleted { .. } => "check_suite_completed",
            Self::ReleasePublished { .. } => "release_published",
            Self::IssueLabeled { .. } => "issue_labeled",
            Self::IssueUnlabeled { .. } => "issue_unlabeled",
            Self::IssueAssigned { .. } => "issue_assigned",
            Self::IssueUnassigned { .. } => "issue_unassigned",
            Self::IssueMilestoned { .. } => "issue_milestoned",
            Self::IssueDemilestoned { .. } => "issue_demilestoned",
            Self::IssueLocked { .. } => "issue_locked",
            Self::IssueUnlocked { .. } => "issue_unlocked",
            Self::IssuePinned { .. } => "issue_pinned",
            Self::IssueUnpinned { .. } => "issue_unpinned",
            Self::IssueTransferred { .. } => "issue_transferred",
            Self::IssueMentioned { .. } => "issue_mentioned",
            Self::IssueCrossReferenced { .. } => "issue_cross_referenced",
            Self::IssueReferenced { .. } => "issue_referenced",
            Self::SubIssueAdded { .. } => "sub_issue_added",
            Self::SubIssueRemoved { .. } => "sub_issue_removed",
            Self::LabelCreated { .. } => "label_created",
            Self::LabelEdited { .. } => "label_edited",
            Self::LabelDeleted { .. } => "label_deleted",
            Self::MilestoneCreated { .. } => "milestone_created",
            Self::MilestoneEdited { .. } => "milestone_edited",
            Self::MilestoneClosed { .. } => "milestone_closed",
            Self::MilestoneOpened { .. } => "milestone_opened",
            Self::MilestoneDeleted { .. } => "milestone_deleted",
            Self::ReactionCreated { .. } => "reaction_created",
            Self::ReactionDeleted { .. } => "reaction_deleted",
            Self::OrgMemberAdded { .. } => "org_member_added",
            Self::AccessChanged { .. } => "access_changed",
            Self::OrgMemberRemoved { .. } => "org_member_removed",
            Self::OrgMemberInvited { .. } => "org_member_invited",
            Self::TeamCreated { .. } => "team_created",
            Self::TeamEdited { .. } => "team_edited",
            Self::TeamDeleted { .. } => "team_deleted",
            Self::TeamMemberAdded { .. } => "team_member_added",
            Self::TeamMemberRemoved { .. } => "team_member_removed",
            Self::TeamRepoAdded { .. } => "team_repo_added",
            Self::TeamRepoRemoved { .. } => "team_repo_removed",
            Self::UserFollowed { .. } => "user_followed",
            Self::RepositoryStarred { .. } => "repository_starred",
            Self::RepositoryForked { .. } => "repository_forked",
            Self::RepositoryRenamed { .. } => "repository_renamed",
            Self::RepositoryTransferred { .. } => "repository_transferred",
            Self::CollaboratorAdded { .. } => "collaborator_added",
        }
    }

    /// Repository the event belongs to, if any.
    pub fn repo_id(&self) -> Option<i64> {
        match self {
            Self::Push(p) => Some(p.repo_id),
            Self::RepositoryCreated { repo_id, .. }
            | Self::RepositoryDeleted { repo_id, .. }
            | Self::RepositoryUpdated { repo_id, .. }
            | Self::IssueOpened { repo_id, .. }
            | Self::IssueEdited { repo_id, .. }
            | Self::IssueClosed { repo_id, .. }
            | Self::IssueReopened { repo_id, .. }
            | Self::IssueCommentCreated { repo_id, .. }
            | Self::IssueCommentEdited { repo_id, .. }
            | Self::IssueCommentDeleted { repo_id, .. }
            | Self::PullRequestOpened { repo_id, .. }
            | Self::PullRequestSynchronized { repo_id, .. }
            | Self::PullRequestClosed { repo_id, .. }
            | Self::PullRequestReopened { repo_id, .. }
            | Self::PullRequestMerged { repo_id, .. }
            | Self::PullRequestReviewSubmitted { repo_id, .. }
            | Self::IssueLabeled { repo_id, .. }
            | Self::IssueUnlabeled { repo_id, .. }
            | Self::IssueAssigned { repo_id, .. }
            | Self::IssueUnassigned { repo_id, .. }
            | Self::IssueMilestoned { repo_id, .. }
            | Self::IssueDemilestoned { repo_id, .. }
            | Self::IssueLocked { repo_id, .. }
            | Self::IssueUnlocked { repo_id, .. }
            | Self::IssuePinned { repo_id, .. }
            | Self::IssueUnpinned { repo_id, .. }
            | Self::IssueTransferred { repo_id, .. }
            | Self::IssueMentioned { repo_id, .. }
            | Self::IssueCrossReferenced { repo_id, .. }
            | Self::IssueReferenced { repo_id, .. }
            | Self::SubIssueAdded { repo_id, .. }
            | Self::SubIssueRemoved { repo_id, .. }
            | Self::LabelCreated { repo_id, .. }
            | Self::LabelEdited { repo_id, .. }
            | Self::LabelDeleted { repo_id, .. }
            | Self::MilestoneCreated { repo_id, .. }
            | Self::MilestoneEdited { repo_id, .. }
            | Self::MilestoneClosed { repo_id, .. }
            | Self::MilestoneOpened { repo_id, .. }
            | Self::MilestoneDeleted { repo_id, .. }
            | Self::ReactionCreated { repo_id, .. }
            | Self::ReactionDeleted { repo_id, .. }
            | Self::ReleasePublished { repo_id, .. }
            | Self::TeamRepoAdded { repo_id, .. }
            | Self::TeamRepoRemoved { repo_id, .. }
            | Self::RepositoryStarred { repo_id, .. }
            | Self::RepositoryForked { repo_id, .. }
            | Self::RepositoryRenamed { repo_id, .. }
            | Self::RepositoryTransferred { repo_id, .. }
            | Self::CollaboratorAdded { repo_id, .. }
            | Self::PullRequestEdited { repo_id, .. }
            | Self::PullRequestReadyForReview { repo_id, .. }
            | Self::PullRequestConvertedToDraft { repo_id, .. }
            | Self::PullRequestReviewRequested { repo_id, .. }
            | Self::PullRequestReviewRequestRemoved { repo_id, .. }
            | Self::PullRequestReviewEdited { repo_id, .. }
            | Self::PullRequestReviewDismissed { repo_id, .. }
            | Self::PullRequestReviewCommentCreated { repo_id, .. }
            | Self::PullRequestReviewCommentEdited { repo_id, .. }
            | Self::PullRequestReviewCommentDeleted { repo_id, .. }
            | Self::PullRequestReviewThreadResolved { repo_id, .. }
            | Self::PullRequestReviewThreadUnresolved { repo_id, .. }
            | Self::PullRequestAutoMergeEnabled { repo_id, .. }
            | Self::PullRequestAutoMergeDisabled { repo_id, .. }
            | Self::CommitStatusCreated { repo_id, .. }
            | Self::CheckRunCreated { repo_id, .. }
            | Self::CheckRunCompleted { repo_id, .. }
            | Self::CheckRunRerequested { repo_id, .. }
            | Self::CheckSuiteRequested { repo_id, .. }
            | Self::CheckSuiteRerequested { repo_id, .. }
            | Self::CheckSuiteCompleted { repo_id, .. } => Some(*repo_id),
            Self::OrgMemberAdded { .. }
            | Self::OrgMemberRemoved { .. }
            | Self::OrgMemberInvited { .. }
            | Self::TeamCreated { .. }
            | Self::TeamEdited { .. }
            | Self::TeamDeleted { .. }
            | Self::TeamMemberAdded { .. }
            | Self::TeamMemberRemoved { .. }
            | Self::UserFollowed { .. } => None,
            Self::AccessChanged { repo_id, .. } => *repo_id,
        }
    }

    /// The user who caused the event, if known.
    pub fn actor_id(&self) -> Option<i64> {
        match self {
            Self::Push(p) => p.pusher_id,
            Self::PullRequestSynchronized { actor_id, .. } => *actor_id,
            Self::IssueReferenced { actor_id, .. }
            | Self::PullRequestReviewDismissed { actor_id, .. }
            | Self::PullRequestAutoMergeDisabled { actor_id, .. }
            | Self::CheckRunCreated { actor_id, .. }
            | Self::CheckRunCompleted { actor_id, .. }
            | Self::CheckSuiteRequested { actor_id, .. } => *actor_id,
            Self::CheckSuiteCompleted { .. } => None,
            Self::RepositoryCreated { actor_id, .. }
            | Self::RepositoryDeleted { actor_id, .. }
            | Self::RepositoryUpdated { actor_id, .. }
            | Self::IssueOpened { actor_id, .. }
            | Self::IssueEdited { actor_id, .. }
            | Self::IssueClosed { actor_id, .. }
            | Self::IssueReopened { actor_id, .. }
            | Self::IssueCommentCreated { actor_id, .. }
            | Self::IssueCommentEdited { actor_id, .. }
            | Self::IssueCommentDeleted { actor_id, .. }
            | Self::PullRequestOpened { actor_id, .. }
            | Self::PullRequestClosed { actor_id, .. }
            | Self::PullRequestReopened { actor_id, .. }
            | Self::PullRequestMerged { actor_id, .. }
            | Self::PullRequestReviewSubmitted { actor_id, .. }
            | Self::PullRequestEdited { actor_id, .. }
            | Self::PullRequestReadyForReview { actor_id, .. }
            | Self::PullRequestConvertedToDraft { actor_id, .. }
            | Self::PullRequestReviewRequested { actor_id, .. }
            | Self::PullRequestReviewRequestRemoved { actor_id, .. }
            | Self::PullRequestReviewEdited { actor_id, .. }
            | Self::PullRequestReviewCommentCreated { actor_id, .. }
            | Self::PullRequestReviewCommentEdited { actor_id, .. }
            | Self::PullRequestReviewCommentDeleted { actor_id, .. }
            | Self::PullRequestReviewThreadResolved { actor_id, .. }
            | Self::PullRequestReviewThreadUnresolved { actor_id, .. }
            | Self::PullRequestAutoMergeEnabled { actor_id, .. }
            | Self::CommitStatusCreated { actor_id, .. }
            | Self::CheckRunRerequested { actor_id, .. }
            | Self::CheckSuiteRerequested { actor_id, .. }
            | Self::ReleasePublished { actor_id, .. }
            | Self::IssueLabeled { actor_id, .. }
            | Self::IssueUnlabeled { actor_id, .. }
            | Self::IssueAssigned { actor_id, .. }
            | Self::IssueUnassigned { actor_id, .. }
            | Self::IssueMilestoned { actor_id, .. }
            | Self::IssueDemilestoned { actor_id, .. }
            | Self::IssueLocked { actor_id, .. }
            | Self::IssueUnlocked { actor_id, .. }
            | Self::IssuePinned { actor_id, .. }
            | Self::IssueUnpinned { actor_id, .. }
            | Self::IssueTransferred { actor_id, .. }
            | Self::IssueMentioned { actor_id, .. }
            | Self::IssueCrossReferenced { actor_id, .. }
            | Self::SubIssueAdded { actor_id, .. }
            | Self::SubIssueRemoved { actor_id, .. }
            | Self::LabelCreated { actor_id, .. }
            | Self::LabelEdited { actor_id, .. }
            | Self::LabelDeleted { actor_id, .. }
            | Self::MilestoneCreated { actor_id, .. }
            | Self::MilestoneEdited { actor_id, .. }
            | Self::MilestoneClosed { actor_id, .. }
            | Self::MilestoneOpened { actor_id, .. }
            | Self::MilestoneDeleted { actor_id, .. }
            | Self::ReactionCreated { actor_id, .. }
            | Self::ReactionDeleted { actor_id, .. }
            | Self::OrgMemberAdded { actor_id, .. }
            | Self::OrgMemberRemoved { actor_id, .. }
            | Self::OrgMemberInvited { actor_id, .. }
            | Self::TeamCreated { actor_id, .. }
            | Self::TeamEdited { actor_id, .. }
            | Self::TeamDeleted { actor_id, .. }
            | Self::TeamMemberAdded { actor_id, .. }
            | Self::TeamMemberRemoved { actor_id, .. }
            | Self::TeamRepoAdded { actor_id, .. }
            | Self::TeamRepoRemoved { actor_id, .. }
            | Self::UserFollowed { actor_id, .. }
            | Self::RepositoryStarred { actor_id, .. }
            | Self::RepositoryForked { actor_id, .. }
            | Self::RepositoryRenamed { actor_id, .. }
            | Self::RepositoryTransferred { actor_id, .. }
            | Self::CollaboratorAdded { actor_id, .. } => Some(*actor_id),
            Self::AccessChanged { .. } => None,
        }
    }
}

/// In-process broadcast bus. Cheap to clone.
#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<Arc<Event>>,
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

impl EventBus {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(4096);
        Self { tx }
    }

    /// Publish an event. Only call after the producing transaction committed.
    pub fn emit(&self, event: Event) {
        tracing::debug!(event = event.name(), "event");
        // No receivers is fine.
        let _ = self.tx.send(Arc::new(event));
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Arc<Event>> {
        self.tx.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_with_tag() {
        let e = Event::IssueOpened {
            repo_id: 1,
            issue_id: 2,
            actor_id: 3,
        };
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["type"], "issue_opened");
        assert_eq!(v["type"], e.name());
        assert_eq!(e.repo_id(), Some(1));
        let p = Event::Push(PushEvent {
            repo_id: 1,
            pusher_id: None,
            updates: vec![],
        });
        assert_eq!(serde_json::to_value(&p).unwrap()["type"], p.name());
    }

    #[test]
    fn ref_update_helpers() {
        let u = RefUpdate {
            old: ZERO_SHA.into(),
            new: "a".repeat(40),
            refname: "refs/heads/main".into(),
        };
        assert!(u.is_create() && !u.is_delete());
        assert_eq!(u.branch(), Some("main"));
    }
}
