//! Crate-local GitHub REST shapes (`email`, `key`, `gpg-key`,
//! `org-membership`, `organization-invitation`, `team-full`,
//! `team-membership`, `team-repository`). Shared shapes live in
//! `bgh_core::models::api`.

use bgh_core::models::api::{
    OrganizationFull, OrganizationSimple, RepoPermissions, Repository, SimpleUser, TeamSimple,
};
use bgh_core::node_id::{self, NodeType};
use bgh_core::perms::{OrgRole, Permission};
use bgh_core::prelude::*;
use bgh_core::time::ts;
use bgh_core::urls::Urls;
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::FromRow;

// ---------------------------------------------------------------------------
// Emails
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, FromRow)]
pub struct EmailRow {
    pub id: i64,
    pub user_id: i64,
    pub email: String,
    pub verified: bool,
    pub is_primary: bool,
    pub visibility: Option<String>,
}

impl EmailRow {
    pub const COLUMNS: &'static str = "id, user_id, email, verified, is_primary, visibility";
}

/// `email`
#[derive(Debug, Clone, Serialize)]
pub struct Email {
    pub email: String,
    pub primary: bool,
    pub verified: bool,
    pub visibility: Option<String>,
}

impl From<&EmailRow> for Email {
    fn from(e: &EmailRow) -> Self {
        Self {
            email: e.email.clone(),
            primary: e.is_primary,
            verified: e.verified,
            visibility: if e.is_primary {
                Some(e.visibility.clone().unwrap_or_else(|| "private".into()))
            } else {
                None
            },
        }
    }
}

// ---------------------------------------------------------------------------
// SSH keys
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, FromRow)]
pub struct SshKeyRow {
    pub id: i64,
    pub user_id: i64,
    pub title: String,
    pub key: String,
    pub fingerprint: String,
    pub verified: bool,
    pub read_only: bool,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

impl SshKeyRow {
    pub const COLUMNS: &'static str =
        "id, user_id, title, key, fingerprint, verified, read_only, created_at, last_used_at";
}

/// `key` (authenticated user's keys).
#[derive(Debug, Clone, Serialize)]
pub struct SshKey {
    pub key: String,
    pub id: i64,
    pub url: String,
    pub title: String,
    pub created_at: Timestamp,
    pub verified: bool,
    pub read_only: bool,
    pub last_used: Option<Timestamp>,
}

impl SshKey {
    pub fn new(urls: &Urls, k: &SshKeyRow) -> Self {
        Self {
            key: k.key.clone(),
            id: k.id,
            url: urls.api(&format!("/user/keys/{}", k.id)),
            title: k.title.clone(),
            created_at: k.created_at.into(),
            verified: k.verified,
            read_only: k.read_only,
            last_used: ts(k.last_used_at),
        }
    }
}

/// `key-simple` (`GET /users/{username}/keys`).
#[derive(Debug, Clone, Serialize)]
pub struct SshKeySimple {
    pub id: i64,
    pub key: String,
    pub created_at: Timestamp,
    pub last_used: Option<Timestamp>,
}

// ---------------------------------------------------------------------------
// GPG keys
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, FromRow)]
pub struct GpgKeyRow {
    pub id: i64,
    pub user_id: i64,
    pub name: Option<String>,
    pub key_id: String,
    pub primary_key_id: Option<i64>,
    pub public_key: String,
    pub raw_key: Option<String>,
    pub emails: serde_json::Value,
    pub can_sign: bool,
    pub can_encrypt_comms: bool,
    pub can_encrypt_storage: bool,
    pub can_certify: bool,
    pub expires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

impl GpgKeyRow {
    pub const COLUMNS: &'static str = "id, user_id, name, key_id, primary_key_id, public_key, \
        raw_key, emails, can_sign, can_encrypt_comms, can_encrypt_storage, can_certify, \
        expires_at, created_at";
}

/// `gpg-key`
#[derive(Debug, Clone, Serialize)]
pub struct GpgKey {
    pub id: i64,
    pub name: Option<String>,
    pub primary_key_id: Option<i64>,
    pub key_id: String,
    pub public_key: String,
    pub emails: serde_json::Value,
    pub subkeys: Vec<GpgKey>,
    pub can_sign: bool,
    pub can_encrypt_comms: bool,
    pub can_encrypt_storage: bool,
    pub can_certify: bool,
    pub created_at: Timestamp,
    pub expires_at: Option<Timestamp>,
    pub revoked: bool,
    pub raw_key: Option<String>,
}

impl GpgKey {
    /// `subkeys` are the rows whose `primary_key_id` is `k.id`.
    pub fn new(k: &GpgKeyRow, subkeys: &[&GpgKeyRow]) -> Self {
        Self {
            id: k.id,
            name: k.name.clone(),
            primary_key_id: k.primary_key_id,
            key_id: k.key_id.clone(),
            public_key: k.public_key.clone(),
            emails: if k.primary_key_id.is_some() {
                serde_json::json!([])
            } else {
                k.emails.clone()
            },
            subkeys: subkeys.iter().map(|s| Self::new(s, &[])).collect(),
            can_sign: k.can_sign,
            can_encrypt_comms: k.can_encrypt_comms,
            can_encrypt_storage: k.can_encrypt_storage,
            can_certify: k.can_certify,
            created_at: k.created_at.into(),
            expires_at: ts(k.expires_at),
            revoked: false,
            raw_key: k.raw_key.clone(),
        }
    }

    /// Group primary keys with their subkeys (input: any order).
    pub fn group(rows: &[GpgKeyRow]) -> Vec<Self> {
        rows.iter()
            .filter(|r| r.primary_key_id.is_none())
            .map(|p| {
                let subs: Vec<&GpgKeyRow> = rows
                    .iter()
                    .filter(|s| s.primary_key_id == Some(p.id))
                    .collect();
                Self::new(p, &subs)
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Organizations
// ---------------------------------------------------------------------------

/// `org-membership`
#[derive(Debug, Clone, Serialize)]
pub struct OrgMembership {
    pub url: String,
    pub state: String,
    pub role: OrgRole,
    pub organization_url: String,
    pub organization: OrganizationSimple,
    pub user: Option<SimpleUser>,
    pub permissions: OrgMembershipPermissions,
}

#[derive(Debug, Clone, Serialize)]
pub struct OrgMembershipPermissions {
    pub can_create_repository: bool,
}

impl OrgMembership {
    pub fn new(
        urls: &Urls,
        org: &db::User,
        description: Option<&str>,
        user: &db::User,
        state: &str,
        role: OrgRole,
        can_create_repository: bool,
    ) -> Self {
        Self {
            url: urls.api(&format!("/orgs/{}/memberships/{}", org.login, user.login)),
            state: state.into(),
            role,
            organization_url: urls.org(&org.login),
            organization: OrganizationSimple::new(urls, org, description),
            user: Some(SimpleUser::new(urls, user)),
            permissions: OrgMembershipPermissions {
                can_create_repository,
            },
        }
    }
}

#[derive(Debug, Clone, FromRow)]
pub struct InvitationRow {
    pub id: i64,
    pub org_id: i64,
    pub invitee_id: Option<i64>,
    pub email: Option<String>,
    pub inviter_id: Option<i64>,
    pub role: String,
    pub team_ids: Vec<i64>,
    pub created_at: DateTime<Utc>,
    pub failed_at: Option<DateTime<Utc>>,
    pub failed_reason: Option<String>,
}

impl InvitationRow {
    pub const COLUMNS: &'static str = "id, org_id, invitee_id, email, inviter_id, role, team_ids, \
        created_at, failed_at, failed_reason";
}

/// `organization-invitation`
#[derive(Debug, Clone, Serialize)]
pub struct OrgInvitation {
    pub id: i64,
    pub login: Option<String>,
    pub email: Option<String>,
    pub role: String,
    pub created_at: Timestamp,
    pub failed_at: Option<Timestamp>,
    pub failed_reason: Option<String>,
    pub inviter: SimpleUser,
    pub team_count: i64,
    pub node_id: String,
    pub invitation_teams_url: String,
    pub invitation_source: String,
}

impl OrgInvitation {
    pub fn new(
        urls: &Urls,
        inv: &InvitationRow,
        invitee: Option<&db::User>,
        inviter: Option<&db::User>,
    ) -> Self {
        Self {
            id: inv.id,
            login: invitee.map(|u| u.login.clone()),
            email: inv
                .email
                .clone()
                .or_else(|| invitee.and_then(|u| u.email.clone())),
            role: inv.role.clone(),
            created_at: inv.created_at.into(),
            failed_at: ts(inv.failed_at),
            failed_reason: inv.failed_reason.clone(),
            inviter: SimpleUser::or_ghost(urls, inviter),
            team_count: inv.team_ids.len() as i64,
            node_id: node_id::encode(NodeType::OrganizationInvitation, inv.id),
            invitation_teams_url: urls.api(&format!(
                "/organizations/{}/invitations/{}/teams",
                inv.org_id, inv.id
            )),
            invitation_source: "member".into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Teams
// ---------------------------------------------------------------------------

/// `team-full`
#[derive(Debug, Clone, Serialize)]
pub struct TeamFull {
    #[serde(flatten)]
    pub team: TeamSimple,
    pub parent: Option<TeamSimple>,
    pub members_count: i64,
    pub repos_count: i64,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub organization: OrganizationFull,
}

/// `team-membership`
#[derive(Debug, Clone, Serialize)]
pub struct TeamMembership {
    pub url: String,
    pub role: String,
    pub state: String,
}

impl TeamMembership {
    pub fn new(urls: &Urls, team: &db::Team, login: &str, role: &str, state: &str) -> Self {
        Self {
            url: format!("{}/memberships/{login}", urls.team(team.org_id, team.id)),
            role: role.into(),
            state: state.into(),
        }
    }
}

/// `team-repository`: full repository + the team's `permissions` and
/// `role_name`.
#[derive(Debug, Clone, Serialize)]
pub struct TeamRepository {
    #[serde(flatten)]
    pub repo: Repository,
    pub role_name: String,
}

impl TeamRepository {
    pub fn new(repo: Repository, team_permission: Permission) -> Self {
        let mut repo = repo;
        repo.repo.permissions = Some(RepoPermissions::from(team_permission));
        Self {
            repo,
            role_name: team_permission.as_str().into(),
        }
    }
}
