//! Personal access token restrictions (P47): fine-grained tokens, the
//! organization token policy and narrow classic scopes.
//!
//! **Fine-grained tokens** (`bgh_pat_…`, GitHub's `github_pat_`) are
//! `access_tokens` rows of kind `fine_grained` with one resource owner (the
//! user or an organization), a repository selection (`all` repositories of
//! the owner, `selected` ones, or `public` = read-only access to public
//! repositories) and a permission map in three groups (repository,
//! organization, account). Like installation tokens they carry everything
//! enforcement needs in their scopes ([`token_scopes`]), so authentication
//! needs no extra query:
//!
//! * `fgpat:owner:{id}` marks the token and names the resource owner;
//! * `fgpat:selection:all` | `fgpat:selection:public` | `fgpat:repo:{id}`…;
//! * `fgpat:pending` while the organization hasn't approved it (or denied or
//!   revoked it): the token then reaches nothing private of the owner;
//! * repository permissions that are `token_permissions` categories as
//!   `actions:permission:{category}:{access}` (so
//!   [`TokenPermissions::of`](crate::token_permissions::TokenPermissions::of),
//!   the git transport and the GraphQL mutation guard apply unchanged), the
//!   others as `fgpat:perm:{group}:{name}:{access}`.
//!
//! Enforcement: [`effective_cap`] caps repository roles in
//! `perms::effective` (the user's own role still applies), [`guard`] (run
//! by `token_permissions::middleware`) classifies REST calls with the
//! P8/P17 route-category table and the account/organization permissions,
//! [`has_scope`] answers classic `require_scope` checks.
//!
//! **Organization policy** (`org_pat_policies`): fine-grained tokens may be
//! forbidden, need approval, or have a maximum lifetime; classic tokens may
//! be forbidden or have a maximum lifetime. Authentication computes the
//! organizations whose policy blocks the token and adds one
//! `pat:blocked_org:{id}` scope per organization ([`BLOCKED_ORG_PREFIX`]):
//! their private repositories are then out of reach and `/orgs/{org}/…`
//! answers 403.
//!
//! **Narrow classic scopes**: a classic token without `repo` but with
//! `repo:status` / `repo_deployment` reaches private repositories only for
//! the commit status / deployment endpoints (and metadata): the middleware
//! notes the request's category ([`with_request_need`]) and
//! `perms::effective` asks [`narrow_cap`].

use std::collections::BTreeMap;

use axum::extract::Request;
use axum::http::Method;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use crate::auth::AuthContext;
use crate::error::{ApiError, ApiResult};
use crate::models::db;
use crate::perms::Permission;
use crate::state::AppState;
use crate::token_permissions::{
    Access, Category, Need, PERMISSION_SCOPE_PREFIX, TokenPermissions, classify,
};

/// Scope marking a fine-grained token and naming its resource owner.
pub const OWNER_SCOPE_PREFIX: &str = "fgpat:owner:";
/// Scope of a fine-grained token covering one selected repository.
pub const REPO_SCOPE_PREFIX: &str = "fgpat:repo:";
/// Scope of a fine-grained token covering every repository of its owner.
pub const ALL_SCOPE: &str = "fgpat:selection:all";
/// Scope of a fine-grained token limited to public repositories (read-only).
pub const PUBLIC_SCOPE: &str = "fgpat:selection:public";
/// Scope of a fine-grained token its organization hasn't approved.
pub const PENDING_SCOPE: &str = "fgpat:pending";
/// Scope prefix of non-category permissions: `fgpat:perm:{group}:{name}:{access}`.
pub const PERM_SCOPE_PREFIX: &str = "fgpat:perm:";
/// Scope added at authentication for each organization whose token policy
/// blocks the token: `pat:blocked_org:{org_id}`.
pub const BLOCKED_ORG_PREFIX: &str = "pat:blocked_org:";

/// GitHub's message for calls a personal access token's permissions don't
/// cover.
pub const NOT_ACCESSIBLE: &str = "Resource not accessible by personal access token";

/// Default and longest lifetime of a fine-grained token, in days.
pub const DEFAULT_MAX_LIFETIME_DAYS: i64 = 366;

// ---------------------------------------------------------------------------
// Permission catalog
// ---------------------------------------------------------------------------

/// A permission group of fine-grained tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Group {
    Repository,
    Organization,
    Account,
}

impl Group {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Repository => "repository",
            Self::Organization => "organization",
            Self::Account => "account",
        }
    }
}

/// One permission a fine-grained token can request.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct PermissionDef {
    pub name: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    /// Access levels that may be granted.
    pub access: &'static [&'static str],
}

const RW: &[&str] = &["read", "write"];

const fn def(
    name: &'static str,
    label: &'static str,
    description: &'static str,
    access: &'static [&'static str],
) -> PermissionDef {
    PermissionDef {
        name,
        label,
        description,
        access,
    }
}

/// Repository permissions (category names of `token_permissions`, plus
/// `workflows`).
pub const REPOSITORY_PERMISSIONS: &[PermissionDef] = &[
    def(
        "actions",
        "Actions",
        "Workflows, workflow runs and artifacts.",
        RW,
    ),
    def("checks", "Checks", "Checks on code.", RW),
    def(
        "contents",
        "Contents",
        "Repository contents, commits, branches, downloads, releases, and merges.",
        RW,
    ),
    def(
        "deployments",
        "Deployments",
        "Deployments and deployment statuses.",
        RW,
    ),
    def(
        "discussions",
        "Discussions",
        "Discussions and related comments.",
        RW,
    ),
    def(
        "issues",
        "Issues",
        "Issues and related comments, assignees, labels, and milestones.",
        RW,
    ),
    def(
        "metadata",
        "Metadata",
        "Search repositories, list collaborators, and access repository metadata.",
        &["read"],
    ),
    def(
        "packages",
        "Packages",
        "Packages published to the registry.",
        RW,
    ),
    def(
        "pages",
        "Pages",
        "Retrieve Pages statuses, configuration, and builds.",
        RW,
    ),
    def(
        "pull_requests",
        "Pull requests",
        "Pull requests and related comments, assignees, labels, milestones, and merges.",
        RW,
    ),
    def(
        "repository_projects",
        "Projects",
        "Manage repository projects, columns, and cards.",
        RW,
    ),
    def(
        "security_events",
        "Code scanning alerts",
        "View and manage security events like code scanning alerts.",
        RW,
    ),
    def("statuses", "Commit statuses", "Commit statuses.", RW),
    def(
        "workflows",
        "Workflows",
        "Update GitHub Action workflow files.",
        &["write"],
    ),
];

/// Organization permissions (only for organization resource owners).
pub const ORGANIZATION_PERMISSIONS: &[PermissionDef] = &[
    def(
        "administration",
        "Administration",
        "Manage access to an organization.",
        RW,
    ),
    def("members", "Members", "Organization members and teams.", RW),
    def(
        "projects",
        "Projects",
        "Manage projects for an organization.",
        RW,
    ),
];

/// Account permissions (the token owner's own account).
pub const ACCOUNT_PERMISSIONS: &[PermissionDef] = &[
    def(
        "email_addresses",
        "Email addresses",
        "Manage a user's email addresses.",
        RW,
    ),
    def("followers", "Followers", "A user's followers.", RW),
    def(
        "git_ssh_keys",
        "Git SSH keys",
        "Git SSH keys of the account.",
        RW,
    ),
    def(
        "gpg_keys",
        "GPG keys",
        "View and manage a user's GPG keys.",
        RW,
    ),
    def(
        "gists",
        "Gists",
        "Create and modify a user's gists.",
        &["write"],
    ),
    def(
        "profile",
        "Profile",
        "Manage a user's profile settings.",
        &["write"],
    ),
    def(
        "starring",
        "Starring",
        "List and manage repositories a user is starring.",
        RW,
    ),
];

/// The catalog of a group.
pub fn catalog(group: Group) -> &'static [PermissionDef] {
    match group {
        Group::Repository => REPOSITORY_PERMISSIONS,
        Group::Organization => ORGANIZATION_PERMISSIONS,
        Group::Account => ACCOUNT_PERMISSIONS,
    }
}

/// A fine-grained token's permission map (`access_tokens.permissions`),
/// `{"repository": {"contents": "read"}, "organization": {}, "account": {}}`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FineGrainedPermissions {
    #[serde(default)]
    pub repository: BTreeMap<String, String>,
    #[serde(default)]
    pub organization: BTreeMap<String, String>,
    #[serde(default)]
    pub account: BTreeMap<String, String>,
}

impl FineGrainedPermissions {
    fn group(&self, group: Group) -> &BTreeMap<String, String> {
        match group {
            Group::Repository => &self.repository,
            Group::Organization => &self.organization,
            Group::Account => &self.account,
        }
    }

    /// Validate against the catalog: drops `none` entries, adds
    /// `metadata: read`. Organization permissions need an organization
    /// resource owner. `Err` names the offending `group.name`.
    pub fn validated(self, owner_is_org: bool) -> Result<Self, String> {
        let mut out = Self::default();
        for group in [Group::Repository, Group::Organization, Group::Account] {
            for (name, access) in self.group(group) {
                if access == "none" {
                    continue;
                }
                let def = catalog(group)
                    .iter()
                    .find(|d| d.name == name)
                    .ok_or_else(|| format!("{}.{name}", group.as_str()))?;
                if !def.access.contains(&access.as_str())
                    || (group == Group::Organization && !owner_is_org)
                {
                    return Err(format!("{}.{name}", group.as_str()));
                }
                let target = match group {
                    Group::Repository => &mut out.repository,
                    Group::Organization => &mut out.organization,
                    Group::Account => &mut out.account,
                };
                target.insert(name.clone(), access.clone());
            }
        }
        out.repository.insert("metadata".into(), "read".into());
        Ok(out)
    }

    /// The scopes mirroring this map (see the module docs).
    pub fn to_scopes(&self) -> Vec<String> {
        let mut cats = Vec::new();
        let mut out = Vec::new();
        for (name, access) in &self.repository {
            match (Category::parse(name), Access::parse(access)) {
                (Some(c), Some(a)) => cats.push((c, a)),
                _ => out.push(format!("{PERM_SCOPE_PREFIX}repository:{name}:{access}")),
            }
        }
        let mut scopes = TokenPermissions::from_pairs(cats).to_scopes();
        scopes.append(&mut out);
        for (group, map) in [
            (Group::Organization, &self.organization),
            (Group::Account, &self.account),
        ] {
            for (name, access) in map {
                scopes.push(format!(
                    "{PERM_SCOPE_PREFIX}{}:{name}:{access}",
                    group.as_str()
                ));
            }
        }
        scopes
    }
}

/// The scopes of a fine-grained token (see the module docs).
pub fn token_scopes(
    owner_id: i64,
    selection: &str,
    repo_ids: &[i64],
    approved: bool,
    permissions: &FineGrainedPermissions,
) -> Vec<String> {
    let mut scopes = vec![format!("{OWNER_SCOPE_PREFIX}{owner_id}")];
    match selection {
        "all" => scopes.push(ALL_SCOPE.into()),
        "selected" => scopes.extend(repo_ids.iter().map(|id| format!("{REPO_SCOPE_PREFIX}{id}"))),
        _ => scopes.push(PUBLIC_SCOPE.into()),
    }
    if !approved {
        scopes.push(PENDING_SCOPE.into());
    }
    scopes.extend(permissions.to_scopes());
    scopes
}

// ---------------------------------------------------------------------------
// Recognizing credentials
// ---------------------------------------------------------------------------

fn scopes(auth: &AuthContext) -> &[String] {
    auth.scopes.as_deref().unwrap_or_default()
}

/// Resource owner of a fine-grained token (`None` for other credentials).
pub fn resource_owner(auth: &AuthContext) -> Option<i64> {
    scopes(auth)
        .iter()
        .find_map(|s| s.strip_prefix(OWNER_SCOPE_PREFIX)?.parse().ok())
}

/// Whether `auth` is a fine-grained personal access token.
pub fn is_fine_grained(auth: &AuthContext) -> bool {
    resource_owner(auth).is_some()
}

/// Whether the token policy of organization `org_id` blocks `auth`.
pub fn is_blocked(auth: &AuthContext, org_id: i64) -> bool {
    let want = format!("{BLOCKED_ORG_PREFIX}{org_id}");
    scopes(auth).contains(&want)
}

fn has_blocks(auth: &AuthContext) -> bool {
    scopes(auth)
        .iter()
        .any(|s| s.starts_with(BLOCKED_ORG_PREFIX))
}

/// Whether a scope is internal bookkeeping (not shown in `X-OAuth-Scopes`).
pub fn is_internal_scope(scope: &str) -> bool {
    scope.starts_with(BLOCKED_ORG_PREFIX) || scope.starts_with("fgpat:")
}

/// Whether a fine-grained token may reach private resources of its owner
/// (approved and not blocked by the owner's policy).
fn active(auth: &AuthContext, owner: i64) -> bool {
    !scopes(auth).iter().any(|s| s == PENDING_SCOPE) && !is_blocked(auth, owner)
}

/// Whether a fine-grained token covers `repo` (`None` for other
/// credentials).
pub fn covers(auth: &AuthContext, repo: &db::Repository) -> Option<bool> {
    let owner = resource_owner(auth)?;
    Some(covers_ids(auth, owner, repo.id, repo.owner_id))
}

fn covers_ids(auth: &AuthContext, owner: i64, repo_id: i64, repo_owner: i64) -> bool {
    if repo_owner != owner || !active(auth, owner) {
        return false;
    }
    let one = format!("{REPO_SCOPE_PREFIX}{repo_id}");
    scopes(auth).iter().any(|s| *s == ALL_SCOPE || *s == one)
}

/// Access a fine-grained token grants for `group`/`name` (repository
/// categories come from the permission scopes).
pub fn access(auth: &AuthContext, group: Group, name: &str) -> Access {
    if group == Group::Repository
        && let Some(c) = Category::parse(name)
    {
        return TokenPermissions::from_scopes(scopes(auth))
            .map(|p| p.get(c))
            .unwrap_or_default();
    }
    let prefix = format!("{PERM_SCOPE_PREFIX}{}:{name}:", group.as_str());
    scopes(auth)
        .iter()
        .filter_map(|s| Access::parse(s.strip_prefix(&prefix)?))
        .max()
        .unwrap_or_default()
}

fn has_repo_write(auth: &AuthContext) -> bool {
    scopes(auth).iter().any(|s| {
        (s.starts_with(PERMISSION_SCOPE_PREFIX) || s.starts_with(PERM_SCOPE_PREFIX))
            && s.ends_with(":write")
            && !s.starts_with(&format!("{PERM_SCOPE_PREFIX}organization:"))
            && !s.starts_with(&format!("{PERM_SCOPE_PREFIX}account:"))
    })
}

fn public_floor(repo: &db::Repository) -> Permission {
    if repo.is_private() {
        Permission::None
    } else {
        Permission::Read
    }
}

/// Repository role cap of a fine-grained token (`None` for other
/// credentials): covered repositories get Write when any repository
/// permission is `write`, else Read (the route-category table limits the
/// categories); other repositories are seen like an anonymous caller would.
/// `perms::effective` takes the minimum with the user's own role.
pub fn effective_cap(auth: &AuthContext, repo: &db::Repository) -> Option<Permission> {
    if !covers(auth, repo)? {
        return Some(public_floor(repo));
    }
    Some(if has_repo_write(auth) {
        Permission::Write
    } else {
        Permission::Read
    })
}

/// [`crate::perms::ReadableRepos`] of a fine-grained token: the private
/// repositories of its resource owner it covers and the user can read.
pub async fn readable_repos(
    db: impl sqlx::PgExecutor<'_>,
    auth: &AuthContext,
) -> Result<crate::perms::ReadableRepos, sqlx::Error> {
    let none = crate::perms::ReadableRepos::default();
    let Some(owner) = resource_owner(auth) else {
        return Ok(none);
    };
    let all = scopes(auth).iter().any(|s| s == ALL_SCOPE);
    let selected: Vec<i64> = scopes(auth)
        .iter()
        .filter_map(|s| s.strip_prefix(REPO_SCOPE_PREFIX)?.parse().ok())
        .collect();
    if !active(auth, owner) || (!all && selected.is_empty()) {
        return Ok(none);
    }
    let only_ids = (!all).then_some(selected.as_slice());
    let private_ids =
        crate::perms::private_readable_ids(db, auth.user.id, &[], Some(owner), only_ids).await?;
    Ok(crate::perms::ReadableRepos {
        all: false,
        private_ids,
    })
}

/// Classic scope checks (`require_scope`) for fine-grained tokens, mapped
/// to their permissions (`None` for other credentials). `repo` itself is
/// never implied: repository access goes through [`effective_cap`] and the
/// route-category table.
pub fn has_scope(auth: &AuthContext, scope: &str) -> Option<bool> {
    resource_owner(auth)?;
    let a = |g: Group, n: &str| access(auth, g, n);
    use Access::{Read, Write};
    use Group::{Account, Organization, Repository};
    Some(match scope {
        "public_repo" => has_repo_write(auth),
        "repo:status" => a(Repository, "statuses") >= Write,
        "repo_deployment" => a(Repository, "deployments") >= Write,
        "security_events" => a(Repository, "security_events") >= Read,
        "workflow" => a(Repository, "workflows") >= Write,
        "read:packages" => a(Repository, "packages") >= Read,
        "write:packages" | "delete:packages" => a(Repository, "packages") >= Write,
        "read:org" => {
            a(Organization, "members") >= Read || a(Organization, "administration") >= Read
        }
        "write:org" => a(Organization, "members") >= Write,
        "admin:org" => a(Organization, "administration") >= Write,
        "read:project" => a(Organization, "projects") >= Read,
        "project" => a(Organization, "projects") >= Write,
        "read:user" => true,
        "user:email" => a(Account, "email_addresses") >= Read,
        "user:follow" => a(Account, "followers") >= Write,
        "user" => a(Account, "profile") >= Write,
        "read:public_key" => a(Account, "git_ssh_keys") >= Read,
        "write:public_key" | "admin:public_key" => a(Account, "git_ssh_keys") >= Write,
        "read:gpg_key" => a(Account, "gpg_keys") >= Read,
        "write:gpg_key" | "admin:gpg_key" => a(Account, "gpg_keys") >= Write,
        "gist" => a(Account, "gists") >= Write,
        _ => false,
    })
}

/// Whether a git request may proceed for a fine-grained token: pushes need
/// `contents: write`, fetches of private repositories `contents: read`.
/// `Ok` for other credentials.
pub fn check_git(auth: Option<&AuthContext>, repo: &db::Repository, write: bool) -> ApiResult<()> {
    let Some(auth) = auth.filter(|a| is_fine_grained(a)) else {
        return Ok(());
    };
    let contents = access(auth, Group::Repository, "contents");
    let ok = if write {
        contents >= Access::Write
    } else {
        contents >= Access::Read || !repo.is_private()
    };
    if ok {
        Ok(())
    } else {
        Err(ApiError::forbidden(format!(
            "Permission to {} denied to {}.",
            repo.name, auth.user.login
        )))
    }
}

// ---------------------------------------------------------------------------
// Narrow classic scopes
// ---------------------------------------------------------------------------

/// Categories a classic token without `repo` reaches on private
/// repositories through its narrow scopes (empty for other credentials).
fn narrow_categories(auth: &AuthContext) -> Vec<Category> {
    if is_fine_grained(auth)
        || crate::apps::is_integration(auth)
        || crate::perms::job_token_repo(auth).is_some()
    {
        return Vec::new();
    }
    let Some(granted) = auth.scopes.as_deref() else {
        return Vec::new();
    };
    if granted.iter().any(|s| s == "repo") {
        return Vec::new();
    }
    let mut out = Vec::new();
    if granted.iter().any(|s| s == "repo:status") {
        out.push(Category::Statuses);
    }
    if granted.iter().any(|s| s == "repo_deployment") {
        out.push(Category::Deployments);
    }
    out
}

/// Whether `auth` is a classic token with narrow repository scopes
/// (`repo:status`, `repo_deployment`) and without `repo`.
pub fn is_narrow_classic(auth: &AuthContext) -> bool {
    !narrow_categories(auth).is_empty()
}

tokio::task_local! {
    /// What the current REST request needs (set by [`guard`] for tokens
    /// with narrow classic scopes).
    static REQUEST_NEED: Need;
}

/// Run `f` with `need` as the current request's category (see
/// [`narrow_cap`]).
pub async fn with_request_need<F: std::future::Future>(need: Need, f: F) -> F::Output {
    REQUEST_NEED.scope(need, f).await
}

/// Role on a private repository of a classic token with narrow scopes for
/// the current request: Write (Read for reads) when the request's category
/// is one of its narrow scopes, Read for metadata reads, else `None`.
/// Outside a classified REST request (git, GraphQL) private repositories
/// stay out of reach.
pub fn narrow_cap(auth: &AuthContext) -> Option<Permission> {
    let granted = narrow_categories(auth);
    if granted.is_empty() {
        return None;
    }
    REQUEST_NEED
        .try_with(|need| match need {
            Need::Any(cats, access) => {
                let narrow = cats.iter().any(|c| granted.contains(c));
                let metadata = *access == Access::Read && cats.contains(&Category::Metadata);
                if narrow && *access == Access::Write {
                    Some(Permission::Write)
                } else if narrow || metadata {
                    Some(Permission::Read)
                } else {
                    None
                }
            }
            Need::Forbidden => None,
        })
        .ok()
        .flatten()
}

// ---------------------------------------------------------------------------
// Middleware
// ---------------------------------------------------------------------------

/// Whether [`guard`] must look at requests made with `auth`.
pub fn applies(auth: &AuthContext) -> bool {
    is_fine_grained(auth) || is_narrow_classic(auth) || has_blocks(auth)
}

/// What a REST call needs from a fine-grained token.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Decision {
    /// Nothing beyond the handler's own checks.
    Pass,
    /// A repository call (`/repos/{o}/{r}/…`) or a category outside one.
    Repo(Need),
    /// An account permission of the token.
    Account(&'static str, Access),
    /// An organization permission (writes to `/orgs/{org}/…`).
    Org(&'static str, Access),
    Forbidden,
}

fn fine_grained_decision(method: &Method, path: &str) -> Decision {
    let rest = path.strip_prefix("/api/v3").unwrap_or(path);
    let segs: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
    let write = !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS);
    let access = if write { Access::Write } else { Access::Read };
    let account = |name: &'static str| Decision::Account(name, access);
    match segs.as_slice() {
        ["repos", _, _, ..] => Decision::Repo(classify(method, path)),
        [_, rest @ ..]
            if rest.contains(&"packages") && matches!(segs[0], "user" | "users" | "orgs") =>
        {
            Decision::Repo(Need::Any(vec![Category::Packages], access))
        }
        ["user"] if write => account("profile"),
        ["user", "emails" | "public_emails" | "email", ..] => account("email_addresses"),
        ["user", "keys", ..] => account("git_ssh_keys"),
        ["user", "gpg_keys", ..] => account("gpg_keys"),
        ["user", "followers" | "following", ..] => account("followers"),
        ["user", "starred", ..] => account("starring"),
        ["gists", ..] if write => account("gists"),
        ["notifications", ..]
        | ["authorizations", ..]
        | ["applications", ..]
        | ["admin", ..]
        | ["user", "installations", ..]
        | ["user", "codespaces", ..] => Decision::Forbidden,
        [
            "orgs",
            _,
            "members"
            | "public_members"
            | "memberships"
            | "teams"
            | "invitations"
            | "outside_collaborators",
            ..,
        ] if write => Decision::Org("members", Access::Write),
        ["orgs", _, "projectsV2" | "projects", ..] if write => {
            Decision::Org("projects", Access::Write)
        }
        [
            "orgs",
            _,
            "personal-access-token-requests" | "personal-access-tokens",
            ..,
        ] => Decision::Org("administration", access),
        ["orgs", _, "repos"] if write => Decision::Forbidden,
        ["orgs", _, ..] if write => Decision::Org("administration", Access::Write),
        ["markdown", ..] => Decision::Pass,
        _ if write => Decision::Forbidden,
        _ => Decision::Pass,
    }
}

fn forbidden() -> Response {
    ApiError::forbidden(NOT_ACCESSIBLE).into_response()
}

fn path_segment(path: &str, prefix: &str, index: usize) -> Option<String> {
    path.strip_prefix(prefix)?
        .split('/')
        .filter(|s| !s.is_empty())
        .nth(index)
        .map(str::to_string)
}

async fn account_id(state: &AppState, login: &str) -> Option<i64> {
    sqlx::query_scalar("SELECT id FROM users WHERE lower(login) = lower($1)")
        .bind(login)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten()
}

/// Whether a fine-grained token covers the repository `owner/name` (fails
/// closed: lookup errors count as covered, so category checks apply).
async fn covers_repo_named(state: &AppState, auth: &AuthContext, owner: &str, name: &str) -> bool {
    let Some(token_owner) = resource_owner(auth) else {
        return false;
    };
    let row: Result<Option<(i64, i64)>, _> = sqlx::query_as(
        "SELECT r.id, r.owner_id FROM repositories r JOIN users u ON u.id = r.owner_id
          WHERE lower(u.login) = lower($1) AND lower(r.name) = lower($2)",
    )
    .bind(owner)
    .bind(name.strip_suffix(".git").unwrap_or(name))
    .fetch_optional(&state.db)
    .await;
    match row {
        Ok(Some((id, repo_owner))) => covers_ids(auth, token_owner, id, repo_owner),
        Ok(None) => false,
        Err(_) => true,
    }
}

/// Middleware step (run by `token_permissions::middleware` when
/// [`applies`]): enforce fine-grained permissions, organization token
/// policies and narrow classic scopes, then run the request.
pub async fn guard(state: &AppState, auth: &AuthContext, req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    let method = req.method().clone();
    let rest_api = path == "/api/v3" || path.starts_with("/api/v3/");

    // Organizations whose policy blocks this token.
    if rest_api
        && has_blocks(auth)
        && let Some(org) = path_segment(&path, "/api/v3/orgs/", 0)
        && let Some(org_id) = account_id(state, &org).await
        && is_blocked(auth, org_id)
    {
        let kind = if is_fine_grained(auth) {
            "fine-grained personal access tokens"
        } else {
            "personal access tokens (classic)"
        };
        return ApiError::forbidden(format!(
            "`{org}` forbids access via {kind} under its token policy. Please use a GitHub \
             App, OAuth App, or a personal access token allowed by the organization."
        ))
        .into_response();
    }

    if let Some(owner) = resource_owner(auth) {
        if !rest_api {
            // `/_bgh` (web JSON): reads only, like integration tokens.
            return if matches!(method, Method::GET | Method::HEAD) {
                next.run(req).await
            } else {
                forbidden()
            };
        }
        match fine_grained_decision(&method, &path) {
            Decision::Pass => {}
            Decision::Forbidden => return forbidden(),
            Decision::Account(name, needed) => {
                if access(auth, Group::Account, name) < needed {
                    return forbidden();
                }
            }
            Decision::Org(name, needed) => {
                let org = path_segment(&path, "/api/v3/orgs/", 0).unwrap_or_default();
                let ok = account_id(state, &org).await == Some(owner)
                    && active(auth, owner)
                    && access(auth, Group::Organization, name) >= needed;
                if !ok {
                    return forbidden();
                }
            }
            Decision::Repo(need) => {
                let perms = TokenPermissions::of(auth).unwrap_or_else(TokenPermissions::none);
                let read = matches!(need, Need::Any(_, Access::Read));
                let repo = path_segment(&path, "/api/v3/repos/", 0).zip(path_segment(
                    &path,
                    "/api/v3/repos/",
                    1,
                ));
                if !perms.allows(&need) {
                    // Reads of repositories the token doesn't cover are
                    // capped to anonymous access by `perms::effective`.
                    let covered = match &repo {
                        Some((o, r)) => covers_repo_named(state, auth, o, r).await,
                        None => true,
                    };
                    if !read || covered {
                        return forbidden();
                    }
                } else if !read
                    && let Some((o, r)) = &repo
                    && !covers_repo_named(state, auth, o, r).await
                {
                    // No writes outside the token's repositories, even
                    // where any reader may write (issues on public
                    // repositories).
                    return forbidden();
                }
            }
        }
        return next.run(req).await;
    }

    if rest_api && is_narrow_classic(auth) && path.starts_with("/api/v3/repos/") {
        let need = classify(&method, &path);
        return with_request_need(need, next.run(req)).await;
    }
    next.run(req).await
}

// ---------------------------------------------------------------------------
// Policies
// ---------------------------------------------------------------------------

/// `org_pat_policies` row (defaults when the organization has none).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
pub struct PatPolicy {
    pub fine_grained_allowed: bool,
    pub fine_grained_require_approval: bool,
    pub fine_grained_max_lifetime_days: Option<i32>,
    pub classic_allowed: bool,
    pub classic_max_lifetime_days: Option<i32>,
}

impl Default for PatPolicy {
    fn default() -> Self {
        Self {
            fine_grained_allowed: true,
            fine_grained_require_approval: false,
            fine_grained_max_lifetime_days: None,
            classic_allowed: true,
            classic_max_lifetime_days: None,
        }
    }
}

impl PatPolicy {
    pub const COLUMNS: &'static str = "fine_grained_allowed, fine_grained_require_approval, \
        fine_grained_max_lifetime_days, classic_allowed, classic_max_lifetime_days";

    /// The policy of `org_id` (defaults for users and unconfigured orgs).
    pub async fn load(db: impl sqlx::PgExecutor<'_>, org_id: i64) -> Result<Self, sqlx::Error> {
        Ok(sqlx::query_as(&format!(
            "SELECT {} FROM org_pat_policies WHERE org_id = $1",
            Self::COLUMNS
        ))
        .bind(org_id)
        .fetch_optional(db)
        .await?
        .unwrap_or_default())
    }

    /// Longest fine-grained token lifetime the policy allows, in days.
    pub fn max_lifetime_days(&self) -> i64 {
        self.fine_grained_max_lifetime_days
            .map_or(DEFAULT_MAX_LIFETIME_DAYS, i64::from)
            .min(DEFAULT_MAX_LIFETIME_DAYS)
    }
}

/// SQL expression (over the token row `t`) listing the organizations whose
/// token policy blocks the token, for the authentication query: classic
/// PATs (not Actions job tokens) when classic tokens are forbidden or the
/// token outlives the maximum lifetime; fine-grained tokens when their
/// organization forbids them or they outlive its maximum lifetime.
pub const BLOCKED_ORGS_SQL: &str = "(SELECT array_agg(p.org_id) FROM org_pat_policies p
      WHERE CASE
        WHEN t.kind = 'pat' THEN
             NOT EXISTS (SELECT 1 FROM unnest(t.scopes) s WHERE s LIKE 'actions:%')
             AND (NOT p.classic_allowed
                  OR (p.classic_max_lifetime_days IS NOT NULL
                      AND (t.expires_at IS NULL
                           OR t.expires_at > t.created_at
                              + make_interval(days => p.classic_max_lifetime_days))))
        WHEN t.kind = 'fine_grained' THEN
             p.org_id = t.resource_owner_id
             AND (NOT p.fine_grained_allowed
                  OR (p.fine_grained_max_lifetime_days IS NOT NULL
                      AND (t.expires_at IS NULL
                           OR t.expires_at > t.created_at
                              + make_interval(days => p.fine_grained_max_lifetime_days)
                              + interval '1 hour')))
        ELSE false END)";

/// The scopes added for `blocked` organizations (see [`BLOCKED_ORG_PREFIX`]).
pub fn blocked_scopes(blocked: &[i64]) -> impl Iterator<Item = String> + '_ {
    blocked.iter().map(|id| format!("{BLOCKED_ORG_PREFIX}{id}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::AuthMethod;

    fn ctx(scopes: Vec<String>) -> AuthContext {
        let now = chrono::Utc::now();
        AuthContext {
            user: db::User {
                id: 1,
                login: "alice".into(),
                kind: "User".into(),
                name: None,
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
                created_at: now,
                updated_at: now,
            },
            method: AuthMethod::Token { token_id: 1 },
            scopes: Some(scopes),
        }
    }

    fn fine(selection: &str, repos: &[i64], perms: &[(&str, &str)]) -> AuthContext {
        let p = FineGrainedPermissions {
            repository: perms
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            ..Default::default()
        }
        .validated(false)
        .unwrap();
        ctx(token_scopes(10, selection, repos, true, &p))
    }

    #[test]
    fn validates_permissions() {
        let p = FineGrainedPermissions {
            repository: [("contents".to_string(), "read".to_string())].into(),
            ..Default::default()
        };
        let v = p.clone().validated(false).unwrap();
        assert_eq!(v.repository["metadata"], "read");
        let bad = FineGrainedPermissions {
            repository: [("admin".to_string(), "write".to_string())].into(),
            ..Default::default()
        };
        assert_eq!(bad.validated(false).unwrap_err(), "repository.admin");
        let org = FineGrainedPermissions {
            organization: [("members".to_string(), "read".to_string())].into(),
            ..Default::default()
        };
        assert!(org.clone().validated(false).is_err());
        assert!(org.validated(true).is_ok());
        let wf = FineGrainedPermissions {
            repository: [("workflows".to_string(), "read".to_string())].into(),
            ..Default::default()
        };
        assert!(wf.validated(false).is_err());
    }

    #[test]
    fn scopes_and_access() {
        let a = fine(
            "selected",
            &[5],
            &[("contents", "read"), ("workflows", "write")],
        );
        assert_eq!(resource_owner(&a), Some(10));
        assert_eq!(access(&a, Group::Repository, "contents"), Access::Read);
        assert_eq!(access(&a, Group::Repository, "issues"), Access::None);
        assert_eq!(access(&a, Group::Repository, "workflows"), Access::Write);
        assert_eq!(has_scope(&a, "workflow"), Some(true));
        assert_eq!(has_scope(&a, "repo"), Some(false));
        assert_eq!(has_scope(&a, "read:user"), Some(true));
        assert_eq!(has_scope(&a, "admin:org"), Some(false));
        assert!(covers_ids(&a, 10, 5, 10));
        assert!(!covers_ids(&a, 10, 6, 10));
        assert!(!covers_ids(&a, 10, 5, 11));
        let ro = fine("all", &[], &[("contents", "read")]);
        assert!(!has_repo_write(&ro));
        assert!(covers_ids(&ro, 10, 99, 10));
        let public = fine("public", &[], &[]);
        assert!(!covers_ids(&public, 10, 99, 10));
        let mut pending = fine("all", &[], &[]);
        pending.scopes.as_mut().unwrap().push(PENDING_SCOPE.into());
        assert!(!covers_ids(&pending, 10, 99, 10));
        let mut blocked = fine("all", &[], &[]);
        blocked
            .scopes
            .as_mut()
            .unwrap()
            .push(format!("{BLOCKED_ORG_PREFIX}10"));
        assert!(!covers_ids(&blocked, 10, 99, 10));
        assert_eq!(has_scope(&ctx(vec!["repo".into()]), "repo"), None);
    }

    #[test]
    fn narrow_classic() {
        assert!(is_narrow_classic(&ctx(vec!["repo:status".into()])));
        assert!(!is_narrow_classic(&ctx(vec![
            "repo".into(),
            "repo:status".into()
        ])));
        assert!(!is_narrow_classic(&ctx(vec!["public_repo".into()])));
        let a = ctx(vec!["repo:status".into()]);
        assert_eq!(narrow_cap(&a), None);
    }

    #[tokio::test]
    async fn narrow_cap_follows_request() {
        let a = ctx(vec!["repo:status".into()]);
        let status = classify(&Method::POST, "/api/v3/repos/o/r/statuses/abc");
        let contents = classify(&Method::GET, "/api/v3/repos/o/r/contents/README");
        let meta = classify(&Method::GET, "/api/v3/repos/o/r");
        let deploy = classify(&Method::POST, "/api/v3/repos/o/r/deployments");
        assert_eq!(
            with_request_need(status, async { narrow_cap(&a) }).await,
            Some(Permission::Write)
        );
        assert_eq!(
            with_request_need(contents, async { narrow_cap(&a) }).await,
            None
        );
        assert_eq!(
            with_request_need(meta, async { narrow_cap(&a) }).await,
            Some(Permission::Read)
        );
        assert_eq!(
            with_request_need(deploy, async { narrow_cap(&a) }).await,
            None
        );
    }

    #[test]
    fn decisions() {
        use Decision as D;
        let d = |m: Method, p: &str| fine_grained_decision(&m, p);
        assert_eq!(d(Method::GET, "/api/v3/user"), D::Pass);
        assert_eq!(
            d(Method::PATCH, "/api/v3/user"),
            D::Account("profile", Access::Write)
        );
        assert_eq!(
            d(Method::GET, "/api/v3/user/emails"),
            D::Account("email_addresses", Access::Read)
        );
        assert_eq!(d(Method::POST, "/api/v3/user/repos"), D::Forbidden);
        assert_eq!(d(Method::GET, "/api/v3/user/repos"), D::Pass);
        assert_eq!(d(Method::GET, "/api/v3/notifications"), D::Forbidden);
        assert_eq!(
            d(Method::PUT, "/api/v3/orgs/acme/memberships/bob"),
            D::Org("members", Access::Write)
        );
        assert_eq!(
            d(Method::PATCH, "/api/v3/orgs/acme"),
            D::Org("administration", Access::Write)
        );
        assert_eq!(d(Method::POST, "/api/v3/orgs/acme/repos"), D::Forbidden);
        assert_eq!(d(Method::GET, "/api/v3/orgs/acme/members"), D::Pass);
        assert_eq!(
            d(Method::GET, "/api/v3/repos/o/r/contents/x"),
            D::Repo(Need::Any(vec![Category::Contents], Access::Read))
        );
        assert_eq!(d(Method::POST, "/api/v3/markdown"), D::Pass);
        assert_eq!(
            d(Method::POST, "/api/v3/user/keys"),
            D::Account("git_ssh_keys", Access::Write)
        );
    }
}
