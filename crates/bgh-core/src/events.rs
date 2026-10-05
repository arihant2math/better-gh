//! Typed domain events and the event bus.
//!
//! Events are queued with [`crate::db::Tx::emit`], written to the
//! transactional outbox (`event_outbox`) in the same transaction, and
//! delivered after commit to listeners registered via
//! [`crate::registry::Registry::on_event`]. Delivery is durable and
//! **at-least-once** (see [`crate::outbox`]): each listener has a cursor,
//! survives restarts and lag, and must be idempotent ([`effect_key`]).
//!
//! Events carry ids, not full objects: listeners load what they need.
//! Add variants freely (the enum is `#[non_exhaustive]`).

use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, OnceLock};

use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tokio::sync::{broadcast, mpsc, oneshot, watch};

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
    /// Where the refs came from when not a client push:
    /// [`PushEvent::ORIGIN_MIRROR`] or [`PushEvent::ORIGIN_IMPORT`]. Search
    /// indexing, activity and webhooks treat these like pushes; Actions
    /// doesn't start workflows for them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
}

impl PushEvent {
    pub const ORIGIN_MIRROR: &'static str = "mirror";
    pub const ORIGIN_IMPORT: &'static str = "import";
    /// Git step of a metadata import (P18): like an import, and also no
    /// webhooks, notifications, activity or commit-keyword closing.
    pub const ORIGIN_METADATA_IMPORT: &'static str = "metadata-import";

    /// Refs fetched from a remote (mirror sync or import), not pushed.
    pub fn is_fetched(&self) -> bool {
        self.origin.is_some()
    }

    /// Refs of a metadata import: only indexing reacts to them.
    pub fn is_quiet(&self) -> bool {
        self.origin.as_deref() == Some(Self::ORIGIN_METADATA_IMPORT)
    }
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
        /// GitHub-style `changes` (`{"body": {"from": old}}`).
        #[serde(default)]
        changes: serde_json::Value,
    },
    IssueCommentDeleted {
        repo_id: i64,
        issue_id: i64,
        comment_id: i64,
        actor_id: i64,
        /// Webhook JSON of the comment taken before the delete (`null`
        /// from older producers).
        #[serde(default)]
        comment: serde_json::Value,
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
        /// GitHub-style `changes` (`{"body": {"from": old}}`).
        #[serde(default)]
        changes: serde_json::Value,
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
        /// GitHub-style `changes` (`{"body": {"from": old}}`).
        #[serde(default)]
        changes: serde_json::Value,
    },
    PullRequestReviewCommentDeleted {
        repo_id: i64,
        pull_id: i64,
        comment_id: i64,
        actor_id: i64,
        /// Webhook JSON of the comment taken before the delete (`null`
        /// from older producers).
        #[serde(default)]
        comment: serde_json::Value,
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
        actor_id: Option<i64>,
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
    /// An issue was linked to a pull request that closes it (closing
    /// keyword in the PR body or a manual link). `repo_id` / `issue_id`
    /// are the issue's; `pull_id` is the issue id of the pull request.
    IssueConnected {
        repo_id: i64,
        issue_id: i64,
        pull_id: i64,
        actor_id: i64,
    },
    /// The link of [`Event::IssueConnected`] was removed.
    IssueDisconnected {
        repo_id: i64,
        issue_id: i64,
        pull_id: i64,
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
        label: serde_json::Value,
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
        milestone: serde_json::Value,
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
        /// Webhook `team` JSON taken before the delete (`null` from older
        /// producers).
        #[serde(default)]
        team: serde_json::Value,
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
        permission: String,
    },
    /// The issue row is gone; `issue` is its GitHub REST JSON at deletion.
    IssueDeleted {
        repo_id: i64,
        issue_id: i64,
        actor_id: i64,
        issue: serde_json::Value,
    },
    RepositoryArchived {
        repo_id: i64,
        actor_id: i64,
    },
    RepositoryUnarchived {
        repo_id: i64,
        actor_id: i64,
    },
    /// Visibility changed to public.
    RepositoryPublicized {
        repo_id: i64,
        actor_id: i64,
    },
    /// Visibility changed to private.
    RepositoryPrivatized {
        repo_id: i64,
        actor_id: i64,
    },
    StarCreated {
        repo_id: i64,
        actor_id: i64,
    },
    StarDeleted {
        repo_id: i64,
        actor_id: i64,
    },
    CollaboratorEdited {
        repo_id: i64,
        user_id: i64,
        actor_id: i64,
        /// Previous role name.
        old_permission: String,
        permission: String,
    },
    CollaboratorRemoved {
        repo_id: i64,
        user_id: i64,
        actor_id: i64,
    },
    ReleaseCreated {
        repo_id: i64,
        release_id: i64,
        actor_id: i64,
    },
    ReleaseEdited {
        repo_id: i64,
        release_id: i64,
        actor_id: i64,
        changes: serde_json::Value,
    },
    ReleaseDeleted {
        repo_id: i64,
        release_id: i64,
        actor_id: i64,
        release: serde_json::Value,
        tag_name: String,
    },
    /// `action`: `created` | `completed` | `rerequested` | `requested_action`.
    CheckRunUpdated {
        repo_id: i64,
        check_run_id: i64,
        action: String,
        actor_id: Option<i64>,
    },
    /// `action`: `requested` | `rerequested` | `completed`. On `completed`
    /// the actor (usually the pusher) gets a `ci_activity` notification.
    CheckSuiteUpdated {
        repo_id: i64,
        check_suite_id: i64,
        action: String,
        actor_id: Option<i64>,
    },
    /// Actions workflow run lifecycle. `action`: `requested` |
    /// `in_progress` | `completed`; `workflow_run` is the GitHub REST JSON
    /// of the run (built by bgh-actions).
    WorkflowRunUpdated {
        repo_id: i64,
        run_id: i64,
        action: String,
        actor_id: Option<i64>,
        workflow_run: serde_json::Value,
        /// GitHub REST JSON of the workflow (`workflow` key), if available.
        workflow: Option<serde_json::Value>,
    },
    ReleaseUpdated {
        repo_id: i64,
        release_id: i64,
        actor_id: i64,
    },
    /// A new container package version was pushed to a package linked to
    /// `repo_id` (webhook `package` / `published`).
    PackagePublished {
        repo_id: i64,
        package_id: i64,
        version_id: i64,
        actor_id: i64,
        /// Tag the version was pushed with, if any.
        #[serde(default)]
        tag: Option<String>,
    },
    /// An existing package version was re-tagged (webhook `package` /
    /// `updated`).
    PackageUpdated {
        repo_id: i64,
        package_id: i64,
        version_id: i64,
        actor_id: i64,
        #[serde(default)]
        tag: Option<String>,
    },
    /// Site-level user account change (GHES global webhook `user` event).
    /// `action`: `created` | `deleted` | `renamed` | `suspended` |
    /// `unsuspended` | `promoted` | `demoted`. `login` is the current login
    /// (the old one for `deleted`); `data` holds extras (`{"from": old}`).
    UserAccountChanged {
        user_id: i64,
        login: String,
        action: String,
        actor_id: i64,
        data: serde_json::Value,
    },
    /// Site-level organization change (GHES global webhook `organization`
    /// event). `action`: `created` | `deleted` | `renamed`.
    OrganizationChanged {
        org_id: i64,
        login: String,
        action: String,
        actor_id: i64,
        data: serde_json::Value,
    },
    /// `POST /admin/hooks/{id}/pings`: deliver a `ping` to a global webhook.
    GlobalHookPing {
        hook_id: i64,
        actor_id: i64,
    },
    /// Actions job lifecycle; `action` is `queued` | `in_progress` |
    /// `completed` | `waiting` (GitHub's `workflow_job` webhook).
    WorkflowJobUpdated {
        repo_id: i64,
        run_id: i64,
        job_id: i64,
        action: String,
        /// GitHub REST JSON of the job (`GET /actions/jobs/{id}`), built by
        /// bgh-actions; `null` from older producers.
        #[serde(default)]
        workflow_job: serde_json::Value,
    },
    /// A deployment was created (`POST /repos/{o}/{r}/deployments`, or an
    /// Actions job with `environment:`). Webhook `deployment` created.
    DeploymentCreated {
        repo_id: i64,
        deployment_id: i64,
        actor_id: Option<i64>,
    },
    /// A deployment status was created (by the API, or `inactive` by
    /// `auto_inactive`). `state` is the new status' state. Webhook
    /// `deployment_status` created; `success` adds `deployed` events to the
    /// pull requests whose head is the deployed commit.
    DeploymentStatusCreated {
        repo_id: i64,
        deployment_id: i64,
        status_id: i64,
        state: String,
        actor_id: Option<i64>,
    },
    /// A deployment review was requested (a job reached an environment with
    /// required reviewers) or submitted (`POST
    /// /actions/runs/{id}/pending_deployments`). `action`: `requested` |
    /// `approved` | `rejected`. `payload` holds the pre-rendered webhook
    /// fields besides `action`, `repository` and `sender` (`environment`,
    /// `reviewers`, `workflow_run`, `workflow_job_run(s)`, `requestor` /
    /// `approver`, `comment`, `since`); `reviewer_ids` are the users to
    /// notify (team reviewers expanded) on `requested`. Webhook
    /// `deployment_review`.
    DeploymentReview {
        repo_id: i64,
        run_id: i64,
        action: String,
        actor_id: Option<i64>,
        #[serde(default)]
        reviewer_ids: Vec<i64>,
        #[serde(default)]
        payload: serde_json::Value,
    },
    /// A commit comment was created (`POST /repos/{o}/{r}/commits/{sha}/comments`).
    /// Webhook `commit_comment` created, activity `CommitCommentEvent`,
    /// notifications to the commit author (resolved by email when the
    /// comment was created) and mentioned users.
    CommitCommentCreated {
        repo_id: i64,
        comment_id: i64,
        actor_id: i64,
        #[serde(default)]
        commit_author_id: Option<i64>,
    },
    /// A browser session ended (logout or revocation): sync sockets of
    /// that session (or of every session of the user when `session_id` is
    /// `None`) must close with code 4001.
    SessionEnded {
        user_id: i64,
        session_id: Option<i64>,
    },
    /// Repository settings changed (`PATCH /repos/..`, topics): GitHub's
    /// `repository` `edited` with a `changes` object
    /// (`{"description": {"from": ..}}`). Renames, visibility and archive
    /// changes have their own variants.
    RepositoryEdited {
        repo_id: i64,
        actor_id: i64,
        changes: serde_json::Value,
    },
    /// A release changed state on edit: `action` is `released` (became a
    /// full release), `prereleased` (became a pre-release) or `unpublished`
    /// (published release turned back into a draft).
    ReleaseStateChanged {
        repo_id: i64,
        release_id: i64,
        actor_id: i64,
        action: String,
    },
    /// `key` is the deploy key's REST JSON (rendered by bgh-repos).
    DeployKeyCreated {
        repo_id: i64,
        key_id: i64,
        actor_id: i64,
        key: serde_json::Value,
    },
    /// The key row is gone; `key` is its REST JSON at deletion.
    DeployKeyDeleted {
        repo_id: i64,
        key_id: i64,
        actor_id: i64,
        key: serde_json::Value,
    },
    /// Classic branch protection changed. `action`: `created` | `edited` |
    /// `deleted`; `rule` is GitHub's `branch_protection_rule` webhook
    /// object, `changes` the edited fields (`{"<field>": {"from": ..}}`).
    BranchProtectionRuleChanged {
        repo_id: i64,
        actor_id: i64,
        action: String,
        rule: serde_json::Value,
        #[serde(default)]
        changes: serde_json::Value,
    },
    /// Repository ruleset changed. `action`: `created` | `edited` |
    /// `deleted`; `ruleset` is the REST ruleset JSON.
    RepositoryRulesetChanged {
        repo_id: i64,
        actor_id: i64,
        action: String,
        ruleset: serde_json::Value,
        #[serde(default)]
        changes: serde_json::Value,
    },
    /// A user clicked one of a check run's `actions` buttons (GitHub's
    /// `check_run` `requested_action`); `identifier` is the action's id.
    CheckRunActionRequested {
        repo_id: i64,
        check_run_id: i64,
        actor_id: i64,
        identifier: String,
    },
    /// Wiki pages written (GitHub `gollum`). `pages` is the webhook
    /// `pages` array: `[{"page_name", "title", "summary", "action":
    /// "created"|"edited"|"deleted", "sha", "html_url"}]`.
    WikiPagesUpdated {
        repo_id: i64,
        actor_id: i64,
        pages: serde_json::Value,
    },
    /// `POST /repos/{o}/{r}/dispatches` (the `repository_dispatch` webhook
    /// and workflow trigger).
    RepositoryDispatch {
        repo_id: i64,
        actor_id: i64,
        event_type: String,
        #[serde(default)]
        client_payload: serde_json::Value,
        branch: String,
    },
    /// A secret scanning alert changed (P65, `bgh-security`). `action`:
    /// `created` | `resolved` | `reopened`. Webhook `secret_scanning_alert`.
    SecretScanningAlert {
        repo_id: i64,
        alert_id: i64,
        action: String,
        #[serde(default)]
        actor_id: Option<i64>,
    },
    /// A secret scanning alert got a new location (webhook
    /// `secret_scanning_alert_location` `created`).
    SecretScanningAlertLocationCreated {
        repo_id: i64,
        alert_id: i64,
        location_id: i64,
    },
}

impl Event {
    /// A metadata import's git push ([`PushEvent::is_quiet`]): listeners
    /// with user-visible effects (webhooks, notifications, activity,
    /// commit-keyword closing) skip it.
    pub fn is_quiet(&self) -> bool {
        matches!(self, Event::Push(p) if p.is_quiet())
    }

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
            Self::IssueConnected { .. } => "issue_connected",
            Self::IssueDisconnected { .. } => "issue_disconnected",
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
            Self::IssueDeleted { .. } => "issue_deleted",
            Self::RepositoryArchived { .. } => "repository_archived",
            Self::RepositoryUnarchived { .. } => "repository_unarchived",
            Self::RepositoryPublicized { .. } => "repository_publicized",
            Self::RepositoryPrivatized { .. } => "repository_privatized",
            Self::StarCreated { .. } => "star_created",
            Self::StarDeleted { .. } => "star_deleted",
            Self::CollaboratorEdited { .. } => "collaborator_edited",
            Self::CollaboratorRemoved { .. } => "collaborator_removed",
            Self::ReleaseCreated { .. } => "release_created",
            Self::ReleaseEdited { .. } => "release_edited",
            Self::ReleaseDeleted { .. } => "release_deleted",
            Self::CheckRunUpdated { .. } => "check_run_updated",
            Self::CheckSuiteUpdated { .. } => "check_suite_updated",
            Self::WorkflowRunUpdated { .. } => "workflow_run_updated",
            Self::ReleaseUpdated { .. } => "release_updated",
            Self::PackagePublished { .. } => "package_published",
            Self::PackageUpdated { .. } => "package_updated",
            Self::UserAccountChanged { .. } => "user_account_changed",
            Self::OrganizationChanged { .. } => "organization_changed",
            Self::GlobalHookPing { .. } => "global_hook_ping",
            Self::WorkflowJobUpdated { .. } => "workflow_job_updated",
            Self::SessionEnded { .. } => "session_ended",
            Self::DeploymentCreated { .. } => "deployment_created",
            Self::DeploymentStatusCreated { .. } => "deployment_status_created",
            Self::DeploymentReview { .. } => "deployment_review",
            Self::CommitCommentCreated { .. } => "commit_comment_created",
            Self::RepositoryEdited { .. } => "repository_edited",
            Self::ReleaseStateChanged { .. } => "release_state_changed",
            Self::DeployKeyCreated { .. } => "deploy_key_created",
            Self::DeployKeyDeleted { .. } => "deploy_key_deleted",
            Self::BranchProtectionRuleChanged { .. } => "branch_protection_rule_changed",
            Self::RepositoryRulesetChanged { .. } => "repository_ruleset_changed",
            Self::WikiPagesUpdated { .. } => "wiki_pages_updated",
            Self::RepositoryDispatch { .. } => "repository_dispatch",
            Self::CheckRunActionRequested { .. } => "check_run_action_requested",
            Self::SecretScanningAlert { .. } => "secret_scanning_alert",
            Self::SecretScanningAlertLocationCreated { .. } => {
                "secret_scanning_alert_location_created"
            }
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
            | Self::IssueConnected { repo_id, .. }
            | Self::IssueDisconnected { repo_id, .. }
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
            | Self::CheckSuiteCompleted { repo_id, .. }
            | Self::IssueDeleted { repo_id, .. }
            | Self::RepositoryArchived { repo_id, .. }
            | Self::RepositoryUnarchived { repo_id, .. }
            | Self::RepositoryPublicized { repo_id, .. }
            | Self::RepositoryPrivatized { repo_id, .. }
            | Self::StarCreated { repo_id, .. }
            | Self::StarDeleted { repo_id, .. }
            | Self::CollaboratorEdited { repo_id, .. }
            | Self::CollaboratorRemoved { repo_id, .. }
            | Self::ReleaseCreated { repo_id, .. }
            | Self::ReleaseEdited { repo_id, .. }
            | Self::ReleaseDeleted { repo_id, .. }
            | Self::CheckRunUpdated { repo_id, .. }
            | Self::CheckSuiteUpdated { repo_id, .. }
            | Self::WorkflowRunUpdated { repo_id, .. }
            | Self::ReleaseUpdated { repo_id, .. }
            | Self::PackagePublished { repo_id, .. }
            | Self::PackageUpdated { repo_id, .. }
            | Self::WorkflowJobUpdated { repo_id, .. }
            | Self::RepositoryEdited { repo_id, .. }
            | Self::ReleaseStateChanged { repo_id, .. }
            | Self::DeployKeyCreated { repo_id, .. }
            | Self::DeployKeyDeleted { repo_id, .. }
            | Self::BranchProtectionRuleChanged { repo_id, .. }
            | Self::RepositoryRulesetChanged { repo_id, .. }
            | Self::WikiPagesUpdated { repo_id, .. }
            | Self::RepositoryDispatch { repo_id, .. }
            | Self::CheckRunActionRequested { repo_id, .. }
            | Self::DeploymentCreated { repo_id, .. }
            | Self::DeploymentStatusCreated { repo_id, .. }
            | Self::DeploymentReview { repo_id, .. }
            | Self::CommitCommentCreated { repo_id, .. }
            | Self::SecretScanningAlert { repo_id, .. }
            | Self::SecretScanningAlertLocationCreated { repo_id, .. } => Some(*repo_id),
            Self::OrgMemberAdded { .. }
            | Self::OrgMemberRemoved { .. }
            | Self::OrgMemberInvited { .. }
            | Self::TeamCreated { .. }
            | Self::TeamEdited { .. }
            | Self::TeamDeleted { .. }
            | Self::TeamMemberAdded { .. }
            | Self::TeamMemberRemoved { .. }
            | Self::UserFollowed { .. }
            | Self::UserAccountChanged { .. }
            | Self::OrganizationChanged { .. }
            | Self::GlobalHookPing { .. }
            | Self::SessionEnded { .. } => None,
            Self::AccessChanged { repo_id, .. } => *repo_id,
        }
    }

    /// Whether an Actions job token (`GITHUB_TOKEN`) caused the event: its
    /// writes are attributed to `github-actions[bot]` (see
    /// [`crate::bots`]). Workflow triggers ignore such events (except
    /// dispatches), like GitHub, so workflows can't trigger themselves.
    pub fn via_actions_token(&self) -> bool {
        crate::bots::is_actions_bot(self.actor_id())
    }

    /// The user who caused the event, if known.
    pub fn actor_id(&self) -> Option<i64> {
        match self {
            Self::Push(p) => p.pusher_id,
            Self::PullRequestSynchronized { actor_id, .. }
            | Self::IssueReferenced { actor_id, .. }
            | Self::PullRequestReviewDismissed { actor_id, .. }
            | Self::PullRequestAutoMergeDisabled { actor_id, .. }
            | Self::CheckRunCreated { actor_id, .. }
            | Self::CheckRunCompleted { actor_id, .. }
            | Self::CheckSuiteRequested { actor_id, .. }
            | Self::CheckRunUpdated { actor_id, .. }
            | Self::CheckSuiteUpdated { actor_id, .. }
            | Self::WorkflowRunUpdated { actor_id, .. }
            | Self::CommitStatusCreated { actor_id, .. }
            | Self::DeploymentCreated { actor_id, .. }
            | Self::DeploymentStatusCreated { actor_id, .. }
            | Self::DeploymentReview { actor_id, .. }
            | Self::SecretScanningAlert { actor_id, .. } => *actor_id,
            Self::SecretScanningAlertLocationCreated { .. } => None,
            Self::CheckSuiteCompleted { .. }
            | Self::AccessChanged { .. }
            | Self::WorkflowJobUpdated { .. } => None,
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
            | Self::CommitCommentCreated { actor_id, .. }
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
            | Self::IssueConnected { actor_id, .. }
            | Self::IssueDisconnected { actor_id, .. }
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
            | Self::CollaboratorAdded { actor_id, .. }
            | Self::IssueDeleted { actor_id, .. }
            | Self::RepositoryArchived { actor_id, .. }
            | Self::RepositoryUnarchived { actor_id, .. }
            | Self::RepositoryPublicized { actor_id, .. }
            | Self::RepositoryPrivatized { actor_id, .. }
            | Self::StarCreated { actor_id, .. }
            | Self::StarDeleted { actor_id, .. }
            | Self::CollaboratorEdited { actor_id, .. }
            | Self::CollaboratorRemoved { actor_id, .. }
            | Self::ReleaseCreated { actor_id, .. }
            | Self::ReleaseEdited { actor_id, .. }
            | Self::ReleaseDeleted { actor_id, .. }
            | Self::ReleaseUpdated { actor_id, .. }
            | Self::PackagePublished { actor_id, .. }
            | Self::PackageUpdated { actor_id, .. }
            | Self::UserAccountChanged { actor_id, .. }
            | Self::OrganizationChanged { actor_id, .. }
            | Self::GlobalHookPing { actor_id, .. }
            | Self::RepositoryEdited { actor_id, .. }
            | Self::ReleaseStateChanged { actor_id, .. }
            | Self::DeployKeyCreated { actor_id, .. }
            | Self::DeployKeyDeleted { actor_id, .. }
            | Self::BranchProtectionRuleChanged { actor_id, .. }
            | Self::RepositoryRulesetChanged { actor_id, .. }
            | Self::WikiPagesUpdated { actor_id, .. }
            | Self::RepositoryDispatch { actor_id, .. }
            | Self::CheckRunActionRequested { actor_id, .. } => Some(*actor_id),
            Self::SessionEnded { user_id, .. } => Some(*user_id),
        }
    }
}

/// The event bus. Cheap to clone.
///
/// Durable delivery goes through the outbox ([`crate::outbox`]): events
/// committed by [`crate::db::Tx`] are already in `event_outbox`; events
/// passed to [`EventBus::emit`] directly are appended by a background
/// writer (in order). Committed events are also broadcast in-process to
/// ephemeral [`EventBus::subscribe`]rs (tests, live views), which may lag
/// and drop; durable listeners never see the broadcast.
#[derive(Clone)]
pub struct EventBus {
    inner: Arc<BusInner>,
}

struct BusInner {
    tx: broadcast::Sender<Arc<Event>>,
    /// Bumped whenever new outbox rows may be visible (wakes consumers).
    wake: watch::Sender<u64>,
    db: Option<PgPool>,
    writer: OnceLock<mpsc::UnboundedSender<WriterMsg>>,
}

enum WriterMsg {
    Event(Event),
    Flush(oneshot::Sender<()>),
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

impl EventBus {
    /// A bus without a database: [`Self::emit`] only broadcasts in-process.
    pub fn new() -> Self {
        Self::build(None)
    }

    /// A bus whose direct [`Self::emit`]s are appended to the outbox in `db`.
    pub fn durable(db: PgPool) -> Self {
        Self::build(Some(db))
    }

    fn build(db: Option<PgPool>) -> Self {
        let (tx, _) = broadcast::channel(4096);
        let (wake, _) = watch::channel(0);
        Self {
            inner: Arc::new(BusInner {
                tx,
                wake,
                db,
                writer: OnceLock::new(),
            }),
        }
    }

    /// Publish an event outside a [`crate::db::Tx`]. Prefer `tx.emit`, which
    /// writes the outbox row atomically with the change; this appends it
    /// asynchronously (in call order; [`Self::flush`] waits for it).
    pub fn emit(&self, event: Event) {
        tracing::debug!(event = event.name(), "event");
        // No receivers is fine.
        let _ = self.inner.tx.send(Arc::new(event.clone()));
        if let Some(writer) = self.writer() {
            let _ = writer.send(WriterMsg::Event(event));
        }
    }

    /// Wait until every event passed to [`Self::emit`] so far is in the
    /// outbox (or was given up on after a database error).
    pub async fn flush(&self) {
        let Some(writer) = self.inner.writer.get() else {
            return;
        };
        let (done, wait) = oneshot::channel();
        if writer.send(WriterMsg::Flush(done)).is_ok() {
            let _ = wait.await;
        }
    }

    /// Called by [`crate::db::Tx::commit`] once events written to the
    /// outbox in that transaction are committed: broadcast them in-process
    /// and wake the consumers.
    pub(crate) fn committed(&self, events: Vec<Event>) {
        if events.is_empty() {
            return;
        }
        for event in events {
            tracing::debug!(event = event.name(), "event");
            let _ = self.inner.tx.send(Arc::new(event));
        }
        self.wake();
    }

    /// Ephemeral in-process subscription (best effort: a slow receiver
    /// lags and loses events). Durable consumers use `reg.on_event`.
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<Event>> {
        self.inner.tx.subscribe()
    }

    /// Wake durable consumers (new outbox rows may be visible).
    pub fn wake(&self) {
        self.inner.wake.send_modify(|n| *n = n.wrapping_add(1));
    }

    /// A receiver that changes whenever [`Self::wake`] is called.
    pub fn wake_receiver(&self) -> watch::Receiver<u64> {
        self.inner.wake.subscribe()
    }

    /// Whether direct emits reach the outbox.
    pub fn is_durable(&self) -> bool {
        self.inner.db.is_some()
    }

    fn writer(&self) -> Option<&mpsc::UnboundedSender<WriterMsg>> {
        let db = self.inner.db.as_ref()?;
        if let Some(w) = self.inner.writer.get() {
            return Some(w);
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            tracing::warn!("EventBus::emit outside a tokio runtime: event not persisted");
            return None;
        };
        Some(self.inner.writer.get_or_init(|| {
            let (tx, rx) = mpsc::unbounded_channel();
            // The writer holds only a weak reference so the bus (and its
            // pool) can be dropped; it exits once every sender is gone.
            handle.spawn(run_writer(db.clone(), Arc::downgrade(&self.inner), rx));
            tx
        }))
    }
}

/// Appends directly emitted events to the outbox, batching whatever is
/// queued, retrying database errors a few times.
async fn run_writer(
    db: PgPool,
    bus: std::sync::Weak<BusInner>,
    mut rx: mpsc::UnboundedReceiver<WriterMsg>,
) {
    let mut batch: Vec<Event> = Vec::new();
    let mut flushes: Vec<oneshot::Sender<()>> = Vec::new();
    while let Some(msg) = rx.recv().await {
        let mut next = Some(msg);
        while let Some(msg) = next.take() {
            match msg {
                WriterMsg::Event(e) => batch.push(e),
                WriterMsg::Flush(done) => flushes.push(done),
            }
            if batch.len() < 1000 {
                next = rx.try_recv().ok();
            }
        }
        if !batch.is_empty() {
            let mut attempt = 0;
            loop {
                attempt += 1;
                match append_standalone(&db, &batch).await {
                    Ok(()) => break,
                    Err(err) if attempt < 5 => {
                        tracing::warn!(?err, attempt, "event outbox write failed; retrying");
                        tokio::time::sleep(std::time::Duration::from_millis(100 << attempt)).await;
                    }
                    Err(err) => {
                        tracing::error!(?err, lost = batch.len(), "event outbox write failed");
                        break;
                    }
                }
            }
            batch.clear();
            if let Some(bus) = bus.upgrade() {
                bus.wake.send_modify(|n| *n = n.wrapping_add(1));
            }
        }
        for done in flushes.drain(..) {
            let _ = done.send(());
        }
    }
}

async fn append_standalone(db: &PgPool, events: &[Event]) -> Result<(), sqlx::Error> {
    let mut tx = db.begin().await?;
    crate::outbox::append(&mut tx, events).await?;
    tx.commit().await
}

tokio::task_local! {
    static CURRENT: EventContext;
}

/// Identity of the outbox event a durable listener is handling.
struct EventContext {
    listener: &'static str,
    id: i64,
    seq: AtomicI32,
}

/// Outbox id of the event the current listener invocation is handling
/// (`None` outside a durable listener, e.g. a handler called directly).
pub fn current_event_id() -> Option<i64> {
    CURRENT.try_with(|c| c.id).ok()
}

/// Idempotency key for the next side effect of the current listener
/// invocation: `(event id, n)` where `n` counts calls within this
/// invocation (0, 1, ...). Delivery is at-least-once, so a handler that
/// writes rows should store this key under a unique index and skip the
/// write on conflict; a deterministic handler produces the same keys on
/// redelivery.
pub fn effect_key() -> Option<(i64, i32)> {
    CURRENT
        .try_with(|c| (c.id, c.seq.fetch_add(1, Ordering::Relaxed)))
        .ok()
}

/// Claim the next side effect of the current listener invocation (see
/// [`effect_key`]) in `conn`'s transaction: false if this listener already
/// performed it for this event (a redelivery), in which case skip the
/// effect. Always true outside a durable listener.
pub async fn claim_effect(conn: &mut sqlx::PgConnection) -> Result<bool, sqlx::Error> {
    let Ok(listener) = CURRENT.try_with(|c| c.listener) else {
        return Ok(true);
    };
    let Some((event_id, seq)) = effect_key() else {
        return Ok(true);
    };
    let inserted = sqlx::query(
        "INSERT INTO event_receipts (listener, event_id, seq) VALUES ($1, $2, $3)
         ON CONFLICT DO NOTHING",
    )
    .bind(listener)
    .bind(event_id)
    .bind(seq)
    .execute(conn)
    .await?
    .rows_affected();
    Ok(inserted == 1)
}

/// Run `fut` as listener `listener` handling outbox event `id` (what the
/// durable consumer does; also lets tests replay a delivery).
pub async fn with_listener_event<F: std::future::Future>(
    listener: &'static str,
    id: i64,
    fut: F,
) -> F::Output {
    CURRENT
        .scope(
            EventContext {
                listener,
                id,
                seq: AtomicI32::new(0),
            },
            fut,
        )
        .await
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
            origin: None,
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
