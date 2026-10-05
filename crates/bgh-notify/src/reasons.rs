//! GitHub notification reasons.

use serde::{Deserialize, Serialize};

/// Why a user received a notification (GitHub's `reason` values).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    /// A deployment waits for your review (environment required reviewer).
    ApprovalRequested,
    Assign,
    Author,
    Comment,
    CiActivity,
    Invitation,
    Manual,
    Mention,
    ReviewRequested,
    SecurityAlert,
    StateChange,
    Subscribed,
    TeamMention,
}

impl Reason {
    pub const ALL: [Reason; 13] = [
        Self::ApprovalRequested,
        Self::Assign,
        Self::Author,
        Self::Comment,
        Self::CiActivity,
        Self::Invitation,
        Self::Manual,
        Self::Mention,
        Self::ReviewRequested,
        Self::SecurityAlert,
        Self::StateChange,
        Self::Subscribed,
        Self::TeamMention,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::ApprovalRequested => "approval_requested",
            Self::Assign => "assign",
            Self::Author => "author",
            Self::Comment => "comment",
            Self::CiActivity => "ci_activity",
            Self::Invitation => "invitation",
            Self::Manual => "manual",
            Self::Mention => "mention",
            Self::ReviewRequested => "review_requested",
            Self::SecurityAlert => "security_alert",
            Self::StateChange => "state_change",
            Self::Subscribed => "subscribed",
            Self::TeamMention => "team_mention",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|r| r.as_str() == s)
    }

    /// Precedence when several reasons apply to one recipient (higher wins).
    pub fn rank(self) -> u8 {
        match self {
            Self::ReviewRequested | Self::ApprovalRequested => 10,
            Self::Assign => 9,
            Self::Mention => 8,
            Self::TeamMention => 7,
            Self::CiActivity => 6,
            Self::Author => 5,
            Self::Comment => 4,
            Self::StateChange => 3,
            Self::Manual => 2,
            Self::Invitation | Self::SecurityAlert => 1,
            Self::Subscribed => 0,
        }
    }

    /// "Participating" reasons (the `participating=true` filter and the
    /// email "participating" bucket); `subscribed` means watching.
    pub fn is_participating(self) -> bool {
        !matches!(self, Self::Subscribed | Self::SecurityAlert)
    }

    /// Human sentence for email footers: "You are receiving this because ...".
    pub fn explanation(self) -> &'static str {
        match self {
            Self::ApprovalRequested => "your approval was requested for a deployment",
            Self::Assign => "you were assigned",
            Self::Author => "you authored the thread",
            Self::Comment => "you commented",
            Self::CiActivity => "a workflow run you triggered completed",
            Self::Invitation => "you were invited",
            Self::Manual => "you are subscribed to this thread",
            Self::Mention => "you were mentioned",
            Self::ReviewRequested => "your review was requested",
            Self::SecurityAlert => "of a security alert",
            Self::StateChange => "you modified the open/close state",
            Self::Subscribed => "you are watching this repository",
            Self::TeamMention => "you are on a team that was mentioned",
        }
    }
}

impl std::fmt::Display for Reason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_rank() {
        for r in Reason::ALL {
            assert_eq!(Reason::parse(r.as_str()), Some(r));
            assert_eq!(serde_json::to_value(r).unwrap(), r.as_str());
        }
        assert!(Reason::Mention.rank() > Reason::Comment.rank());
        assert!(Reason::Comment.rank() > Reason::Subscribed.rank());
        assert!(!Reason::Subscribed.is_participating());
        assert_eq!(Reason::parse("bogus"), None);
    }
}
