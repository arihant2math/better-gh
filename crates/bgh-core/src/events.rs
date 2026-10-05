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
    ReleasePublished {
        repo_id: i64,
        release_id: i64,
        actor_id: i64,
    },
    OrgMemberAdded {
        org_id: i64,
        user_id: i64,
        actor_id: i64,
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
    /// A browser session ended (logout or revocation): sync sockets of
    /// that session (or of every session of the user when `session_id` is
    /// `None`) must close with code 4001.
    SessionEnded {
        user_id: i64,
        session_id: Option<i64>,
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
            Self::ReleasePublished { .. } => "release_published",
            Self::OrgMemberAdded { .. } => "org_member_added",
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
            Self::SessionEnded { .. } => "session_ended",
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
            | Self::ReleasePublished { repo_id, .. }
            | Self::TeamRepoAdded { repo_id, .. }
            | Self::TeamRepoRemoved { repo_id, .. } => Some(*repo_id),
            Self::OrgMemberAdded { .. }
            | Self::OrgMemberRemoved { .. }
            | Self::OrgMemberInvited { .. }
            | Self::TeamCreated { .. }
            | Self::TeamEdited { .. }
            | Self::TeamDeleted { .. }
            | Self::TeamMemberAdded { .. }
            | Self::TeamMemberRemoved { .. }
            | Self::UserFollowed { .. }
            | Self::SessionEnded { .. } => None,
        }
    }

    /// The user who caused the event, if known.
    pub fn actor_id(&self) -> Option<i64> {
        match self {
            Self::Push(p) => p.pusher_id,
            Self::PullRequestSynchronized { actor_id, .. } => *actor_id,
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
            | Self::ReleasePublished { actor_id, .. }
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
            | Self::UserFollowed { actor_id, .. } => Some(*actor_id),
            Self::SessionEnded { user_id, .. } => Some(*user_id),
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
