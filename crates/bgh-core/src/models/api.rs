//! GitHub REST v3 JSON shapes shared across domains.
//!
//! Field names, nesting and null-vs-missing follow GitHub's REST docs
//! (`simple-user`, `public-user`, `private-user`, `organization-simple`,
//! `organization-full`, `minimal-repository`, `full-repository`, `label`,
//! `milestone`, `reaction-rollup`, `team-simple`, `team`).
//!
//! Construct them with the `build`/`new` helpers from database rows so URL
//! and node-id generation stays consistent.

use serde::{Deserialize, Serialize};

use crate::models::db;
use crate::node_id::{self, NodeType};
use crate::perms::Permission;
use crate::time::{Timestamp, ts};
use crate::urls::Urls;

// ---------------------------------------------------------------------------
// Users
// ---------------------------------------------------------------------------

/// `simple-user`: embedded wherever a user is referenced (owner, author, ...).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SimpleUser {
    pub login: String,
    pub id: i64,
    pub node_id: String,
    pub avatar_url: String,
    pub gravatar_id: String,
    pub url: String,
    pub html_url: String,
    pub followers_url: String,
    pub following_url: String,
    pub gists_url: String,
    pub starred_url: String,
    pub subscriptions_url: String,
    pub organizations_url: String,
    pub repos_url: String,
    pub events_url: String,
    pub received_events_url: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub site_admin: bool,
    pub user_view_type: String,
}

/// Login used for deleted users (`author_id IS NULL`).
pub const GHOST_LOGIN: &str = "ghost";

impl SimpleUser {
    pub fn new(urls: &Urls, u: &db::User) -> Self {
        Self::from_parts(
            urls,
            u.id,
            &u.login,
            &u.kind,
            u.site_admin,
            u.avatar_url.as_deref(),
        )
    }

    pub fn from_parts(
        urls: &Urls,
        id: i64,
        login: &str,
        kind: &str,
        site_admin: bool,
        avatar_url: Option<&str>,
    ) -> Self {
        let api = urls.user(login);
        Self {
            login: login.to_string(),
            id,
            node_id: node_id::encode(NodeType::for_user_kind(kind), id),
            avatar_url: urls.avatar(id, avatar_url),
            gravatar_id: String::new(),
            html_url: urls.user_html(login),
            followers_url: format!("{api}/followers"),
            following_url: format!("{api}/following{{/other_user}}"),
            gists_url: format!("{api}/gists{{/gist_id}}"),
            starred_url: format!("{api}/starred{{/owner}}{{/repo}}"),
            subscriptions_url: format!("{api}/subscriptions"),
            organizations_url: format!("{api}/orgs"),
            repos_url: format!("{api}/repos"),
            events_url: format!("{api}/events{{/privacy}}"),
            received_events_url: format!("{api}/received_events"),
            url: api,
            kind: kind.to_string(),
            site_admin,
            user_view_type: "public".into(),
        }
    }

    /// The placeholder for deleted accounts.
    pub fn ghost(urls: &Urls) -> Self {
        Self::from_parts(urls, 0, GHOST_LOGIN, "User", false, None)
    }

    /// Render an optional user, falling back to the ghost user.
    pub fn or_ghost(urls: &Urls, u: Option<&db::User>) -> Self {
        u.map(|u| Self::new(urls, u))
            .unwrap_or_else(|| Self::ghost(urls))
    }
}

/// Aggregate counts shown on user profiles.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, sqlx::FromRow)]
pub struct UserStats {
    pub public_repos: i64,
    pub public_gists: i64,
    pub followers: i64,
    pub following: i64,
}

/// `public-user` (`GET /users/{username}`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PublicUser {
    #[serde(flatten)]
    pub user: SimpleUser,
    pub name: Option<String>,
    pub company: Option<String>,
    pub blog: Option<String>,
    pub location: Option<String>,
    pub email: Option<String>,
    pub hireable: Option<bool>,
    pub bio: Option<String>,
    pub twitter_username: Option<String>,
    pub public_repos: i64,
    pub public_gists: i64,
    pub followers: i64,
    pub following: i64,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl PublicUser {
    pub fn new(urls: &Urls, u: &db::User, stats: UserStats) -> Self {
        Self {
            user: SimpleUser::new(urls, u),
            name: u.name.clone(),
            company: u.company.clone(),
            blog: Some(u.blog.clone().unwrap_or_default()),
            location: u.location.clone(),
            email: u.email.clone(),
            hireable: u.hireable,
            bio: u.bio.clone(),
            twitter_username: u.twitter_username.clone(),
            public_repos: stats.public_repos,
            public_gists: stats.public_gists,
            followers: stats.followers,
            following: stats.following,
            created_at: u.created_at.into(),
            updated_at: u.updated_at.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    pub name: String,
    pub space: i64,
    pub private_repos: i64,
    pub collaborators: i64,
}

impl Default for Plan {
    fn default() -> Self {
        Self {
            name: "enterprise".into(),
            space: 976_562_499,
            private_repos: 9_999_999,
            collaborators: 0,
        }
    }
}

/// Private counters for the authenticated user.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, sqlx::FromRow)]
pub struct PrivateUserStats {
    pub total_private_repos: i64,
    pub owned_private_repos: i64,
    pub disk_usage: i64,
    pub collaborators: i64,
}

/// `private-user` (`GET /user`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrivateUser {
    #[serde(flatten)]
    pub user: PublicUser,
    pub private_gists: i64,
    pub total_private_repos: i64,
    pub owned_private_repos: i64,
    pub disk_usage: i64,
    pub collaborators: i64,
    pub two_factor_authentication: bool,
    pub plan: Plan,
}

impl PrivateUser {
    pub fn new(
        urls: &Urls,
        u: &db::User,
        stats: UserStats,
        private: PrivateUserStats,
        two_factor: bool,
    ) -> Self {
        Self {
            user: PublicUser::new(urls, u, stats),
            private_gists: 0,
            total_private_repos: private.total_private_repos,
            owned_private_repos: private.owned_private_repos,
            disk_usage: private.disk_usage,
            collaborators: private.collaborators,
            two_factor_authentication: two_factor,
            plan: Plan::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Organizations
// ---------------------------------------------------------------------------

/// `organization-simple`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrganizationSimple {
    pub login: String,
    pub id: i64,
    pub node_id: String,
    pub url: String,
    pub repos_url: String,
    pub events_url: String,
    pub hooks_url: String,
    pub issues_url: String,
    pub members_url: String,
    pub public_members_url: String,
    pub avatar_url: String,
    pub description: Option<String>,
}

impl OrganizationSimple {
    pub fn new(urls: &Urls, org: &db::User, description: Option<&str>) -> Self {
        let api = urls.org(&org.login);
        Self {
            login: org.login.clone(),
            id: org.id,
            node_id: node_id::encode(NodeType::Organization, org.id),
            repos_url: format!("{api}/repos"),
            events_url: format!("{api}/events"),
            hooks_url: format!("{api}/hooks"),
            issues_url: format!("{api}/issues"),
            members_url: format!("{api}/members{{/member}}"),
            public_members_url: format!("{api}/public_members{{/member}}"),
            avatar_url: urls.avatar(org.id, org.avatar_url.as_deref()),
            description: description.map(str::to_string),
            url: api,
        }
    }
}

/// `organization-full` (`GET /orgs/{org}`). Member-only fields are `None`
/// (omitted) for non-members.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrganizationFull {
    #[serde(flatten)]
    pub org: OrganizationSimple,
    pub name: Option<String>,
    pub company: Option<String>,
    pub blog: Option<String>,
    pub location: Option<String>,
    pub email: Option<String>,
    pub twitter_username: Option<String>,
    pub is_verified: bool,
    pub has_organization_projects: bool,
    pub has_repository_projects: bool,
    pub public_repos: i64,
    pub public_gists: i64,
    pub followers: i64,
    pub following: i64,
    pub html_url: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub archived_at: Option<Timestamp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_private_repos: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owned_private_repos: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub billing_email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_repository_permission: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub members_can_create_repositories: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub two_factor_requirement_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub members_can_create_public_repositories: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub members_can_create_private_repositories: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub members_can_fork_private_repositories: Option<bool>,
    /// `all` | `private` | `none` (legacy summary of the three flags above).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub members_allowed_repository_creation_type: Option<String>,
}

impl OrganizationFull {
    /// `member_view`: include member-only settings (caller is an org member).
    pub fn new(
        urls: &Urls,
        org: &db::User,
        settings: &db::OrgSettings,
        stats: UserStats,
        private_repos: Option<i64>,
        member_view: bool,
    ) -> Self {
        let mv = |b: bool| member_view.then_some(b);
        Self {
            org: OrganizationSimple::new(urls, org, settings.description.as_deref()),
            name: org.name.clone(),
            company: org.company.clone(),
            blog: org.blog.clone(),
            location: org.location.clone(),
            email: org.email.clone(),
            twitter_username: org.twitter_username.clone(),
            is_verified: settings.is_verified,
            has_organization_projects: settings.has_organization_projects,
            has_repository_projects: settings.has_repository_projects,
            public_repos: stats.public_repos,
            public_gists: stats.public_gists,
            followers: stats.followers,
            following: stats.following,
            html_url: urls.user_html(&org.login),
            kind: "Organization".into(),
            created_at: org.created_at.into(),
            updated_at: org.updated_at.into(),
            archived_at: ts(settings.archived_at),
            total_private_repos: private_repos.filter(|_| member_view),
            owned_private_repos: private_repos.filter(|_| member_view),
            billing_email: if member_view {
                settings.billing_email.clone()
            } else {
                None
            },
            default_repository_permission: member_view
                .then(|| settings.default_repository_permission.clone()),
            members_can_create_repositories: mv(settings.members_can_create_repositories),
            two_factor_requirement_enabled: mv(settings.two_factor_requirement_enabled),
            members_can_create_public_repositories: mv(
                settings.members_can_create_public_repositories
            ),
            members_can_create_private_repositories: mv(
                settings.members_can_create_private_repositories
            ),
            members_can_fork_private_repositories: mv(
                settings.members_can_fork_private_repositories
            ),
            members_allowed_repository_creation_type: member_view.then(|| {
                match (
                    settings.members_can_create_repositories,
                    settings.members_can_create_public_repositories,
                ) {
                    (false, _) => "none",
                    (true, true) => "all",
                    (true, false) => "private",
                }
                .to_string()
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// Teams
// ---------------------------------------------------------------------------

/// `team-simple`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TeamSimple {
    pub id: i64,
    pub node_id: String,
    pub url: String,
    pub members_url: String,
    pub name: String,
    pub description: Option<String>,
    /// Legacy permission name: `pull` | `triage` | `push` | `maintain` | `admin`.
    pub permission: String,
    pub privacy: String,
    pub notification_setting: String,
    pub html_url: String,
    pub repositories_url: String,
    pub slug: String,
}

impl TeamSimple {
    pub fn new(urls: &Urls, org_login: &str, t: &db::Team) -> Self {
        let api = urls.team(t.org_id, t.id);
        Self {
            id: t.id,
            node_id: node_id::encode(NodeType::Team, t.id),
            members_url: format!("{api}/members{{/member}}"),
            repositories_url: format!("{api}/repos"),
            url: api,
            name: t.name.clone(),
            description: t.description.clone(),
            permission: Permission::parse(&t.permission)
                .unwrap_or(Permission::Read)
                .legacy_name()
                .to_string(),
            privacy: t.privacy.clone(),
            notification_setting: t.notification_setting.clone(),
            html_url: urls.team_html(org_login, &t.slug),
            slug: t.slug.clone(),
        }
    }
}

/// `team` (team-simple + nullable parent).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Team {
    #[serde(flatten)]
    pub team: TeamSimple,
    pub parent: Option<TeamSimple>,
}

// ---------------------------------------------------------------------------
// Repositories
// ---------------------------------------------------------------------------

/// `permissions` object on repositories, for the authenticated user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoPermissions {
    pub admin: bool,
    pub maintain: bool,
    pub push: bool,
    pub triage: bool,
    pub pull: bool,
}

impl From<Permission> for RepoPermissions {
    fn from(p: Permission) -> Self {
        Self {
            admin: p >= Permission::Admin,
            maintain: p >= Permission::Maintain,
            push: p >= Permission::Write,
            triage: p >= Permission::Triage,
            pull: p >= Permission::Read,
        }
    }
}

/// All the hypermedia `*_url` fields of a repository.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RepoLinks {
    pub archive_url: String,
    pub assignees_url: String,
    pub blobs_url: String,
    pub branches_url: String,
    pub collaborators_url: String,
    pub comments_url: String,
    pub commits_url: String,
    pub compare_url: String,
    pub contents_url: String,
    pub contributors_url: String,
    pub deployments_url: String,
    pub downloads_url: String,
    pub events_url: String,
    pub forks_url: String,
    pub git_commits_url: String,
    pub git_refs_url: String,
    pub git_tags_url: String,
    pub git_url: String,
    pub issue_comment_url: String,
    pub issue_events_url: String,
    pub issues_url: String,
    pub keys_url: String,
    pub labels_url: String,
    pub languages_url: String,
    pub merges_url: String,
    pub milestones_url: String,
    pub notifications_url: String,
    pub pulls_url: String,
    pub releases_url: String,
    pub ssh_url: String,
    pub stargazers_url: String,
    pub statuses_url: String,
    pub subscribers_url: String,
    pub subscription_url: String,
    pub tags_url: String,
    pub teams_url: String,
    pub trees_url: String,
    pub clone_url: String,
    pub mirror_url: Option<String>,
    pub hooks_url: String,
    pub svn_url: String,
}

impl RepoLinks {
    pub fn new(urls: &Urls, owner: &str, name: &str) -> Self {
        let r = urls.repo(owner, name);
        Self {
            archive_url: format!("{r}/{{archive_format}}{{/ref}}"),
            assignees_url: format!("{r}/assignees{{/user}}"),
            blobs_url: format!("{r}/git/blobs{{/sha}}"),
            branches_url: format!("{r}/branches{{/branch}}"),
            collaborators_url: format!("{r}/collaborators{{/collaborator}}"),
            comments_url: format!("{r}/comments{{/number}}"),
            commits_url: format!("{r}/commits{{/sha}}"),
            compare_url: format!("{r}/compare/{{base}}...{{head}}"),
            contents_url: format!("{r}/contents/{{+path}}"),
            contributors_url: format!("{r}/contributors"),
            deployments_url: format!("{r}/deployments"),
            downloads_url: format!("{r}/downloads"),
            events_url: format!("{r}/events"),
            forks_url: format!("{r}/forks"),
            git_commits_url: format!("{r}/git/commits{{/sha}}"),
            git_refs_url: format!("{r}/git/refs{{/sha}}"),
            git_tags_url: format!("{r}/git/tags{{/sha}}"),
            git_url: urls.git_url(owner, name),
            issue_comment_url: format!("{r}/issues/comments{{/number}}"),
            issue_events_url: format!("{r}/issues/events{{/number}}"),
            issues_url: format!("{r}/issues{{/number}}"),
            keys_url: format!("{r}/keys{{/key_id}}"),
            labels_url: format!("{r}/labels{{/name}}"),
            languages_url: format!("{r}/languages"),
            merges_url: format!("{r}/merges"),
            milestones_url: format!("{r}/milestones{{/number}}"),
            notifications_url: format!("{r}/notifications{{?since,all,participating}}"),
            pulls_url: format!("{r}/pulls{{/number}}"),
            releases_url: format!("{r}/releases{{/id}}"),
            ssh_url: urls.ssh_url(owner, name),
            stargazers_url: format!("{r}/stargazers"),
            statuses_url: format!("{r}/statuses/{{sha}}"),
            subscribers_url: format!("{r}/subscribers"),
            subscription_url: format!("{r}/subscription"),
            tags_url: format!("{r}/tags"),
            teams_url: format!("{r}/teams"),
            trees_url: format!("{r}/git/trees{{/sha}}"),
            clone_url: urls.clone_url(owner, name),
            mirror_url: None,
            hooks_url: format!("{r}/hooks"),
            svn_url: urls.repo_html(owner, name),
        }
    }
}

/// `license-simple` (repositories carry `license: null` until detected).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LicenseSimple {
    pub key: String,
    pub name: String,
    pub spdx_id: String,
    pub url: Option<String>,
    pub node_id: String,
}

/// Repository as embedded in lists and events (GitHub `repository` /
/// `minimal-repository`, superset). `permissions` is present only for
/// authenticated callers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MinimalRepository {
    pub id: i64,
    pub node_id: String,
    pub name: String,
    pub full_name: String,
    pub owner: SimpleUser,
    pub private: bool,
    pub html_url: String,
    pub description: Option<String>,
    pub fork: bool,
    pub url: String,
    #[serde(flatten)]
    pub links: RepoLinks,
    pub homepage: Option<String>,
    pub language: Option<String>,
    pub forks_count: i64,
    pub stargazers_count: i64,
    pub watchers_count: i64,
    pub size: i64,
    pub default_branch: String,
    pub open_issues_count: i64,
    pub is_template: bool,
    pub topics: Vec<String>,
    pub has_issues: bool,
    pub has_projects: bool,
    pub has_wiki: bool,
    pub has_pages: bool,
    pub has_downloads: bool,
    pub has_discussions: bool,
    pub archived: bool,
    pub disabled: bool,
    pub visibility: String,
    pub pushed_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permissions: Option<RepoPermissions>,
    pub allow_rebase_merge: bool,
    pub allow_squash_merge: bool,
    pub allow_auto_merge: bool,
    pub delete_branch_on_merge: bool,
    pub allow_merge_commit: bool,
    pub allow_update_branch: bool,
    pub use_squash_pr_title_as_default: bool,
    pub squash_merge_commit_title: String,
    pub squash_merge_commit_message: String,
    pub merge_commit_title: String,
    pub merge_commit_message: String,
    pub allow_forking: bool,
    pub web_commit_signoff_required: bool,
    pub license: Option<LicenseSimple>,
    pub forks: i64,
    pub open_issues: i64,
    pub watchers: i64,
}

impl MinimalRepository {
    /// `owner` must be the row referenced by `repo.owner_id`.
    /// `permission` is the caller's permission (None for anonymous callers).
    pub fn new(
        urls: &Urls,
        repo: &db::Repository,
        owner: &db::User,
        permission: Option<Permission>,
    ) -> Self {
        let html_url = urls.repo_html(&owner.login, &repo.name);
        Self {
            id: repo.id,
            node_id: node_id::encode(NodeType::Repository, repo.id),
            name: repo.name.clone(),
            full_name: format!("{}/{}", owner.login, repo.name),
            owner: SimpleUser::new(urls, owner),
            private: repo.is_private(),
            html_url,
            description: repo.description.clone(),
            fork: repo.fork,
            url: urls.repo(&owner.login, &repo.name),
            links: RepoLinks {
                mirror_url: repo.mirror_url.clone(),
                ..RepoLinks::new(urls, &owner.login, &repo.name)
            },
            homepage: repo.homepage.clone(),
            language: repo.language.clone(),
            forks_count: repo.forks_count,
            stargazers_count: repo.stargazers_count,
            watchers_count: repo.stargazers_count,
            size: repo.size,
            default_branch: repo.default_branch.clone(),
            open_issues_count: repo.open_issues_count,
            is_template: repo.is_template,
            topics: repo.topics.clone(),
            has_issues: repo.has_issues,
            has_projects: repo.has_projects,
            has_wiki: repo.has_wiki,
            has_pages: repo.has_pages,
            has_downloads: true,
            has_discussions: repo.has_discussions,
            archived: repo.archived,
            disabled: repo.disabled,
            visibility: repo.visibility.clone(),
            pushed_at: ts(repo.pushed_at),
            created_at: repo.created_at.into(),
            updated_at: repo.updated_at.into(),
            permissions: permission.map(RepoPermissions::from),
            allow_rebase_merge: repo.allow_rebase_merge,
            allow_squash_merge: repo.allow_squash_merge,
            allow_auto_merge: repo.allow_auto_merge,
            delete_branch_on_merge: repo.delete_branch_on_merge,
            allow_merge_commit: repo.allow_merge_commit,
            allow_update_branch: repo.allow_update_branch,
            use_squash_pr_title_as_default: repo.use_squash_pr_title_as_default,
            squash_merge_commit_title: repo.squash_merge_commit_title.clone(),
            squash_merge_commit_message: repo.squash_merge_commit_message.clone(),
            merge_commit_title: repo.merge_commit_title.clone(),
            merge_commit_message: repo.merge_commit_message.clone(),
            allow_forking: repo.allow_forking,
            web_commit_signoff_required: repo.web_commit_signoff_required,
            license: None,
            forks: repo.forks_count,
            open_issues: repo.open_issues_count,
            watchers: repo.stargazers_count,
        }
    }
}

/// `full-repository` (`GET /repos/{owner}/{repo}`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Repository {
    #[serde(flatten)]
    pub repo: MinimalRepository,
    pub subscribers_count: i64,
    pub network_count: i64,
    pub temp_clone_token: Option<String>,
    pub template_repository: Option<Box<MinimalRepository>>,
    /// Present when the owner is an organization (rendered as simple-user).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub organization: Option<SimpleUser>,
    /// Present for forks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<Box<MinimalRepository>>,
    /// Present for forks: root of the fork network.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<Box<MinimalRepository>>,
}

/// Optional related data for [`Repository::new`].
#[derive(Debug, Clone, Default)]
pub struct RepositoryExtras {
    pub subscribers_count: i64,
    pub network_count: i64,
    pub parent: Option<MinimalRepository>,
    pub source: Option<MinimalRepository>,
    pub template_repository: Option<MinimalRepository>,
}

impl Repository {
    pub fn new(
        urls: &Urls,
        repo: &db::Repository,
        owner: &db::User,
        permission: Option<Permission>,
        extras: RepositoryExtras,
    ) -> Self {
        Self {
            repo: MinimalRepository::new(urls, repo, owner, permission),
            subscribers_count: extras.subscribers_count,
            network_count: extras.network_count,
            temp_clone_token: None,
            template_repository: extras.template_repository.map(Box::new),
            organization: owner.is_org().then(|| SimpleUser::new(urls, owner)),
            parent: extras.parent.map(Box::new),
            source: extras.source.map(Box::new),
        }
    }
}

// ---------------------------------------------------------------------------
// Labels, milestones, reactions
// ---------------------------------------------------------------------------

/// `label`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Label {
    pub id: i64,
    pub node_id: String,
    pub url: String,
    pub name: String,
    pub color: String,
    pub default: bool,
    pub description: Option<String>,
}

impl Label {
    pub fn new(urls: &Urls, owner: &str, repo: &str, l: &db::Label) -> Self {
        Self {
            id: l.id,
            node_id: node_id::encode(NodeType::Label, l.id),
            url: urls.label(owner, repo, &l.name),
            name: l.name.clone(),
            color: l.color.clone(),
            default: l.is_default,
            description: l.description.clone(),
        }
    }
}

/// `milestone`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Milestone {
    pub url: String,
    pub html_url: String,
    pub labels_url: String,
    pub id: i64,
    pub node_id: String,
    pub number: i64,
    pub state: String,
    pub title: String,
    pub description: Option<String>,
    pub creator: Option<SimpleUser>,
    pub open_issues: i64,
    pub closed_issues: i64,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub closed_at: Option<Timestamp>,
    pub due_on: Option<Timestamp>,
}

impl Milestone {
    pub fn new(
        urls: &Urls,
        owner: &str,
        repo: &str,
        m: &db::Milestone,
        creator: Option<&db::User>,
    ) -> Self {
        let url = urls.milestone(owner, repo, m.number);
        Self {
            html_url: urls.milestone_html(owner, repo, m.number),
            labels_url: format!("{url}/labels"),
            url,
            id: m.id,
            node_id: node_id::encode(NodeType::Milestone, m.id),
            number: m.number,
            state: m.state.clone(),
            title: m.title.clone(),
            description: m.description.clone(),
            creator: creator.map(|u| SimpleUser::new(urls, u)),
            open_issues: m.open_issues,
            closed_issues: m.closed_issues,
            created_at: m.created_at.into(),
            updated_at: m.updated_at.into(),
            closed_at: ts(m.closed_at),
            due_on: ts(m.due_on),
        }
    }
}

/// Reaction contents accepted by GitHub.
pub const REACTION_CONTENTS: [&str; 8] = [
    "+1", "-1", "laugh", "confused", "heart", "hooray", "rocket", "eyes",
];

/// `reaction-rollup`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReactionRollup {
    pub url: String,
    pub total_count: i64,
    #[serde(rename = "+1")]
    pub plus_one: i64,
    #[serde(rename = "-1")]
    pub minus_one: i64,
    pub laugh: i64,
    pub confused: i64,
    pub heart: i64,
    pub hooray: i64,
    pub eyes: i64,
    pub rocket: i64,
}

impl ReactionRollup {
    /// Build from `(content, count)` pairs, e.g. the rows of
    /// `SELECT content, count(*) FROM reactions WHERE ... GROUP BY content`.
    pub fn from_counts(url: String, counts: &[(String, i64)]) -> Self {
        let mut r = Self {
            url,
            ..Self::default()
        };
        for (content, n) in counts {
            match content.as_str() {
                "+1" => r.plus_one += n,
                "-1" => r.minus_one += n,
                "laugh" => r.laugh += n,
                "confused" => r.confused += n,
                "heart" => r.heart += n,
                "hooray" => r.hooray += n,
                "eyes" => r.eyes += n,
                "rocket" => r.rocket += n,
                _ => continue,
            }
            r.total_count += n;
        }
        r
    }
}

/// `author_association` values on issues, PRs and comments.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AuthorAssociation {
    Collaborator,
    Contributor,
    FirstTimer,
    FirstTimeContributor,
    Mannequin,
    Member,
    None,
    Owner,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use chrono::Utc;

    fn user(kind: &str) -> db::User {
        db::User {
            id: 7,
            login: "octo".into(),
            kind: kind.into(),
            name: Some("Octo Cat".into()),
            email: None,
            bio: None,
            company: None,
            location: None,
            blog: None,
            twitter_username: None,
            hireable: None,
            avatar_url: None,
            site_admin: false,
            suspended_at: None,
            password_hash: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn simple_user_shape() {
        let urls = Urls::new(&Config::default());
        let v = serde_json::to_value(SimpleUser::new(&urls, &user("User"))).unwrap();
        assert_eq!(v["login"], "octo");
        assert_eq!(v["type"], "User");
        assert_eq!(v["url"], "http://localhost:3000/api/v3/users/octo");
        assert_eq!(
            v["following_url"],
            "http://localhost:3000/api/v3/users/octo/following{/other_user}"
        );
        assert_eq!(v["gravatar_id"], "");
    }

    #[test]
    fn public_user_flattens() {
        let urls = Urls::new(&Config::default());
        let v = serde_json::to_value(PublicUser::new(&urls, &user("User"), UserStats::default()))
            .unwrap();
        assert_eq!(v["login"], "octo");
        assert_eq!(v["name"], "Octo Cat");
        assert_eq!(v["blog"], "");
        assert!(v["email"].is_null());
        assert!(v["created_at"].as_str().unwrap().ends_with('Z'));
    }

    #[test]
    fn reaction_rollup_counts() {
        let r = ReactionRollup::from_counts(
            "u".into(),
            &[("+1".into(), 2), ("eyes".into(), 1), ("bogus".into(), 5)],
        );
        assert_eq!(r.total_count, 3);
        let v = serde_json::to_value(r).unwrap();
        assert_eq!(v["+1"], 2);
        assert_eq!(v["-1"], 0);
    }

    #[test]
    fn permissions_object() {
        let p = RepoPermissions::from(Permission::Write);
        assert!(p.push && p.triage && p.pull && !p.maintain && !p.admin);
    }
}
