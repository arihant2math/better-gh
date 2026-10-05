//! Fine-grained token permissions (GitHub App style): a map from permission
//! category (`contents`, `issues`, ...) to `none | read | write`, and the
//! route-category table that decides which category a request needs.
//!
//! Used for Actions job tokens (`GITHUB_TOKEN`, whose map comes from the
//! workflow's `permissions:`); installation tokens (GitHub Apps) reuse it.
//!
//! * The map is stored on the token row (`access_tokens.permissions`) and
//!   mirrored into its scopes as `actions:permission:<category>:<access>`
//!   ([`PERMISSION_SCOPE_PREFIX`]), so enforcement needs no extra query.
//! * [`middleware`] (mounted by bgh-server for every route) answers 403
//!   "Resource not accessible by integration" when a job token calls a REST
//!   route whose category it lacks; `/_bgh` writes are refused outright.
//! * Git transport and GraphQL mutations don't go through the REST table:
//!   bgh-repos checks `contents` for pushes, bgh-graphql calls
//!   [`TokenPermissions::check_graphql_mutation`].

use std::collections::BTreeMap;

use axum::extract::{Request, State};
use axum::http::{Method, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use crate::auth::AuthContext;
use crate::error::{ApiError, ApiResult};
use crate::perms::job_token_repo;
use crate::state::AppState;

/// Scope prefix carrying one category of a token's permission map:
/// `actions:permission:contents:write`.
pub const PERMISSION_SCOPE_PREFIX: &str = "actions:permission:";

/// GitHub's message for calls a token's permissions don't cover.
pub const NOT_ACCESSIBLE: &str = "Resource not accessible by integration";

/// A permission category (GitHub's names, `snake_case`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Actions,
    Attestations,
    Checks,
    Contents,
    Deployments,
    Discussions,
    IdToken,
    Issues,
    Metadata,
    Models,
    Packages,
    Pages,
    PullRequests,
    RepositoryProjects,
    SecurityEvents,
    Statuses,
}

impl Category {
    pub const ALL: &[Category] = &[
        Self::Actions,
        Self::Attestations,
        Self::Checks,
        Self::Contents,
        Self::Deployments,
        Self::Discussions,
        Self::IdToken,
        Self::Issues,
        Self::Metadata,
        Self::Models,
        Self::Packages,
        Self::Pages,
        Self::PullRequests,
        Self::RepositoryProjects,
        Self::SecurityEvents,
        Self::Statuses,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Actions => "actions",
            Self::Attestations => "attestations",
            Self::Checks => "checks",
            Self::Contents => "contents",
            Self::Deployments => "deployments",
            Self::Discussions => "discussions",
            Self::IdToken => "id_token",
            Self::Issues => "issues",
            Self::Metadata => "metadata",
            Self::Models => "models",
            Self::Packages => "packages",
            Self::Pages => "pages",
            Self::PullRequests => "pull_requests",
            Self::RepositoryProjects => "repository_projects",
            Self::SecurityEvents => "security_events",
            Self::Statuses => "statuses",
        }
    }

    /// Parse a category; accepts the workflow spelling (`pull-requests`).
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().to_ascii_lowercase().replace('-', "_");
        Self::ALL.iter().copied().find(|c| c.as_str() == s)
    }
}

/// Access level of one category, ordered.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Access {
    #[default]
    None,
    Read,
    Write,
}

impl Access {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "none" => Self::None,
            "read" => Self::Read,
            "write" => Self::Write,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Read => "read",
            Self::Write => "write",
        }
    }
}

/// A token's permission map. Categories not listed are `none`, except
/// `metadata`, which is always at least `read`. Serializes like GitHub's
/// installation-token `permissions` object (`{"contents": "read"}`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TokenPermissions(BTreeMap<Category, Access>);

impl TokenPermissions {
    /// Every category `none` (`permissions: {}`), metadata read.
    pub fn none() -> Self {
        Self::default().normalized()
    }

    /// `permissions: read-all`.
    pub fn read_all() -> Self {
        Self::uniform(Access::Read)
    }

    /// `permissions: write-all`.
    pub fn write_all() -> Self {
        Self::uniform(Access::Write)
    }

    fn uniform(access: Access) -> Self {
        Self(Category::ALL.iter().map(|&c| (c, access)).collect()).normalized()
    }

    /// GitHub's restricted default for `GITHUB_TOKEN` (`default_workflow_permissions = read`):
    /// contents and packages read.
    pub fn restricted_default() -> Self {
        Self::from_pairs([
            (Category::Contents, Access::Read),
            (Category::Packages, Access::Read),
        ])
    }

    /// GitHub's permissive default (`default_workflow_permissions = write`):
    /// write to everything but `id-token` (and metadata, read only).
    pub fn permissive_default() -> Self {
        let mut p = Self::write_all();
        p.0.remove(&Category::IdToken);
        p.normalized()
    }

    /// The site/repo default named by `default_workflow_permissions`
    /// (`read` | `write`).
    pub fn default_for(setting: &str) -> Self {
        if setting.eq_ignore_ascii_case("write") {
            Self::permissive_default()
        } else {
            Self::restricted_default()
        }
    }

    pub fn from_pairs(pairs: impl IntoIterator<Item = (Category, Access)>) -> Self {
        Self(pairs.into_iter().collect()).normalized()
    }

    fn normalized(mut self) -> Self {
        self.0.retain(|_, a| *a != Access::None);
        // metadata: read is implied and can't be raised.
        self.0.insert(Category::Metadata, Access::Read);
        self
    }

    /// Access granted for `category`.
    pub fn get(&self, category: Category) -> Access {
        self.0.get(&category).copied().unwrap_or_default()
    }

    pub fn set(&mut self, category: Category, access: Access) {
        self.0.insert(category, access);
        *self = std::mem::take(self).normalized();
    }

    /// Whether any category grants write.
    pub fn has_write(&self) -> bool {
        self.0.values().any(|a| *a == Access::Write)
    }

    /// Downgrade every write to read (pull requests from forks).
    pub fn read_only(mut self) -> Self {
        for a in self.0.values_mut() {
            *a = (*a).min(Access::Read);
        }
        self
    }

    /// Non-`none` entries, in category order.
    pub fn iter(&self) -> impl Iterator<Item = (Category, Access)> + '_ {
        self.0.iter().map(|(c, a)| (*c, *a))
    }

    /// The scopes mirroring this map (see [`PERMISSION_SCOPE_PREFIX`]).
    pub fn to_scopes(&self) -> Vec<String> {
        self.iter()
            .map(|(c, a)| format!("{PERMISSION_SCOPE_PREFIX}{}:{}", c.as_str(), a.as_str()))
            .collect()
    }

    /// The map carried by `scopes`, if any permission scope is present.
    pub fn from_scopes(scopes: &[String]) -> Option<Self> {
        let mut found = false;
        let mut map = BTreeMap::new();
        for s in scopes {
            let Some(rest) = s.strip_prefix(PERMISSION_SCOPE_PREFIX) else {
                continue;
            };
            found = true;
            if let Some((c, a)) = rest.split_once(':')
                && let (Some(c), Some(a)) = (Category::parse(c), Access::parse(a))
            {
                map.insert(c, a);
            }
        }
        found.then(|| Self(map).normalized())
    }

    /// The permission map restricting `auth`: job tokens carry one (tokens
    /// minted before permission scopes existed get [`Self::write_all`], or
    /// read-all when read-only). `None` for every other credential.
    pub fn of(auth: &AuthContext) -> Option<Self> {
        // GitHub App installation tokens carry their map the same way.
        if crate::apps::installation_id(auth).is_some() {
            let scopes = auth.scopes.as_deref().unwrap_or_default();
            return Some(Self::from_scopes(scopes).unwrap_or_else(Self::none));
        }
        job_token_repo(auth)?;
        let scopes = auth.scopes.as_deref().unwrap_or_default();
        Some(Self::from_scopes(scopes).unwrap_or_else(|| {
            if scopes
                .iter()
                .any(|s| s == crate::perms::JOB_TOKEN_READ_ONLY_SCOPE)
            {
                Self::read_all()
            } else {
                Self::write_all()
            }
        }))
    }

    /// Whether the map satisfies `need`.
    pub fn allows(&self, need: &Need) -> bool {
        match need {
            Need::Forbidden => false,
            Need::Any(cats, access) => cats.iter().any(|c| self.get(*c) >= *access),
        }
    }

    /// 403 unless the GraphQL mutation `field` (e.g. `addComment`) is
    /// covered (see [`graphql_mutation_need`]).
    pub fn check_graphql_mutation(&self, field: &str) -> ApiResult<()> {
        if self.allows(&graphql_mutation_need(field)) {
            Ok(())
        } else {
            Err(ApiError::forbidden(NOT_ACCESSIBLE))
        }
    }
}

/// What a request needs from a permission map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Need {
    /// At least `access` in any of the categories.
    Any(Vec<Category>, Access),
    /// Never available to these tokens (administration, secrets, user
    /// account endpoints, ...).
    Forbidden,
}

impl Need {
    fn one(c: Category, a: Access) -> Self {
        Self::Any(vec![c], a)
    }
}

use Category as C;

/// The route-category table: `(path pattern relative to /api/v3, rule)`.
/// Patterns use `*` for one segment and a trailing `**` for any rest
/// (including nothing). First match wins, so specific rules come first.
/// The access level comes from the method (GET/HEAD read, else write)
/// unless the rule fixes it.
enum Rule {
    /// Category (or alternatives), access from the method.
    Cats(&'static [Category]),
    /// Reads need the categories, writes are forbidden.
    ReadOnly(&'static [Category]),
    /// Writes need `contents: write` regardless of the matched area
    /// (merging a pull request), reads need the categories.
    WriteContents(&'static [Category]),
    Forbidden,
}

const ISSUES_OR_PULLS: &[Category] = &[C::Issues, C::PullRequests];

const REPO_ROUTES: &[(&str, Rule)] = &[
    // Administration and secrets: never available to integration tokens.
    ("hooks/**", Rule::Forbidden),
    ("collaborators/**", Rule::ReadOnly(&[C::Metadata])),
    ("invitations/**", Rule::Forbidden),
    ("keys/**", Rule::Forbidden),
    ("branches/*/protection/**", Rule::Forbidden),
    ("rulesets/**", Rule::Forbidden),
    ("transfer", Rule::Forbidden),
    ("topics", Rule::ReadOnly(&[C::Metadata])),
    ("forks", Rule::ReadOnly(&[C::Metadata])),
    ("subscription", Rule::Forbidden),
    ("notifications", Rule::Forbidden),
    ("actions/secrets/**", Rule::Forbidden),
    ("actions/organization-secrets/**", Rule::Forbidden),
    ("actions/variables/**", Rule::Forbidden),
    ("actions/organization-variables/**", Rule::Forbidden),
    ("actions/runners/**", Rule::Forbidden),
    ("actions/permissions/**", Rule::Forbidden),
    ("environments/*/secrets/**", Rule::Forbidden),
    ("environments/*/variables/**", Rule::Forbidden),
    // Pull requests: merging and branch updates write contents.
    ("pulls/*/merge", Rule::WriteContents(&[C::PullRequests])),
    (
        "pulls/*/update-branch",
        Rule::WriteContents(&[C::PullRequests]),
    ),
    ("pulls/**", Rule::Cats(&[C::PullRequests])),
    // Issues (and the issue endpoints shared with pull requests).
    ("issues/*/comments/**", Rule::Cats(ISSUES_OR_PULLS)),
    ("issues/*/labels/**", Rule::Cats(ISSUES_OR_PULLS)),
    ("issues/*/assignees/**", Rule::Cats(ISSUES_OR_PULLS)),
    ("issues/*/reactions/**", Rule::Cats(ISSUES_OR_PULLS)),
    ("issues/comments/**", Rule::Cats(ISSUES_OR_PULLS)),
    ("issues/**", Rule::Cats(&[C::Issues])),
    ("labels/**", Rule::Cats(ISSUES_OR_PULLS)),
    ("milestones/**", Rule::Cats(ISSUES_OR_PULLS)),
    ("assignees/**", Rule::ReadOnly(ISSUES_OR_PULLS)),
    ("comments/**", Rule::Cats(&[C::Contents])),
    // Commit statuses and checks.
    ("statuses/**", Rule::Cats(&[C::Statuses])),
    ("commits/*/status", Rule::ReadOnly(&[C::Statuses])),
    ("commits/*/statuses", Rule::ReadOnly(&[C::Statuses])),
    ("commits/*/check-runs", Rule::ReadOnly(&[C::Checks])),
    ("commits/*/check-suites", Rule::ReadOnly(&[C::Checks])),
    ("check-runs/**", Rule::Cats(&[C::Checks])),
    ("check-suites/**", Rule::Cats(&[C::Checks])),
    // Actions, deployments, pages, security, packages, projects.
    ("actions/**", Rule::Cats(&[C::Actions])),
    ("deployments/**", Rule::Cats(&[C::Deployments])),
    ("environments/**", Rule::Cats(&[C::Deployments, C::Actions])),
    ("pages/**", Rule::Cats(&[C::Pages])),
    ("code-scanning/**", Rule::Cats(&[C::SecurityEvents])),
    ("secret-scanning/**", Rule::Cats(&[C::SecurityEvents])),
    ("dependabot/**", Rule::Cats(&[C::SecurityEvents])),
    ("packages/**", Rule::Cats(&[C::Packages])),
    ("projects/**", Rule::Cats(&[C::RepositoryProjects])),
    ("discussions/**", Rule::Cats(&[C::Discussions])),
    ("attestations/**", Rule::Cats(&[C::Attestations])),
    // Contents: files, git data, refs, commits, releases, dispatches.
    ("contents/**", Rule::Cats(&[C::Contents])),
    ("readme/**", Rule::Cats(&[C::Contents])),
    ("git/**", Rule::Cats(&[C::Contents])),
    ("branches/**", Rule::Cats(&[C::Contents])),
    ("merge-upstream", Rule::Cats(&[C::Contents])),
    ("merges", Rule::Cats(&[C::Contents])),
    ("commits/**", Rule::Cats(&[C::Contents])),
    ("compare/**", Rule::Cats(&[C::Contents])),
    ("tags/**", Rule::Cats(&[C::Contents])),
    ("releases/**", Rule::Cats(&[C::Contents])),
    ("tarball/**", Rule::Cats(&[C::Contents])),
    ("zipball/**", Rule::Cats(&[C::Contents])),
    ("dispatches", Rule::Cats(&[C::Contents])),
    ("lfs/**", Rule::Cats(&[C::Contents])),
    // Everything else on a repository (the repo itself, stargazers,
    // languages, ...) is metadata: readable, never writable.
    ("**", Rule::ReadOnly(&[C::Metadata])),
];

fn matches(pattern: &str, segs: &[&str]) -> bool {
    let mut i = 0;
    for p in pattern.split('/') {
        if p == "**" {
            return true;
        }
        match segs.get(i) {
            Some(s) if p == "*" || p == *s => i += 1,
            _ => return false,
        }
    }
    i == segs.len()
}

fn need_for(rule: &Rule, write: bool) -> Need {
    let access = if write { Access::Write } else { Access::Read };
    match rule {
        Rule::Forbidden => Need::Forbidden,
        Rule::Cats(c) => Need::Any(c.to_vec(), access),
        Rule::ReadOnly(c) if !write => Need::Any(c.to_vec(), Access::Read),
        Rule::ReadOnly(_) => Need::Forbidden,
        Rule::WriteContents(c) if !write => Need::Any(c.to_vec(), Access::Read),
        Rule::WriteContents(_) => Need::one(C::Contents, Access::Write),
    }
}

/// The category a REST call needs. `path` is relative to `/api/v3` (a
/// leading `/api/v3` is stripped).
pub fn classify(method: &Method, path: &str) -> Need {
    let path = path.strip_prefix("/api/v3").unwrap_or(path);
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let write = !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS);
    let read = |c: Category| {
        if write {
            Need::Forbidden
        } else {
            Need::one(c, Access::Read)
        }
    };
    match segs.as_slice() {
        ["repos", _owner, _repo, rest @ ..] => {
            // `/repos/{o}/{r}` itself: read metadata; edits/deletes are
            // administration.
            for (pattern, rule) in REPO_ROUTES {
                if matches(pattern, rest) {
                    return need_for(rule, write);
                }
            }
            read(C::Metadata)
        }
        // Rendering markdown writes nothing.
        ["markdown", ..] => Need::one(C::Metadata, Access::Read),
        // Installation token endpoints (`DELETE /installation/token`).
        ["installation", ..] => Need::one(C::Metadata, Access::Read),
        // Packages of users and organizations.
        ["user" | "users" | "orgs", rest @ ..] if rest.contains(&"packages") => Need::one(
            C::Packages,
            if write { Access::Write } else { Access::Read },
        ),
        // The token's own account and its settings: never.
        ["user", ..]
        | ["authorizations", ..]
        | ["applications", ..]
        | ["admin", ..]
        | ["notifications", ..]
        | ["gists", ..] => Need::Forbidden,
        // Public reads (users, orgs, search, meta, rate limit, ...).
        _ => read(C::Metadata),
    }
}

/// What a GraphQL mutation (root field name) needs.
pub fn graphql_mutation_need(field: &str) -> Need {
    let any = |c: &[Category]| Need::Any(c.to_vec(), Access::Write);
    match field {
        "createIssue" | "updateIssue" | "closeIssue" | "reopenIssue" | "pinIssue"
        | "unpinIssue" | "transferIssue" => any(&[C::Issues]),
        "addComment"
        | "updateIssueComment"
        | "deleteIssueComment"
        | "addLabelsToLabelable"
        | "removeLabelsFromLabelable"
        | "addAssigneesToAssignable"
        | "removeAssigneesFromAssignable"
        | "replaceActorsForAssignable"
        | "lockLockable"
        | "unlockLockable"
        | "addReaction"
        | "removeReaction" => any(ISSUES_OR_PULLS),
        "createPullRequest"
        | "updatePullRequest"
        | "closePullRequest"
        | "reopenPullRequest"
        | "markPullRequestReadyForReview"
        | "convertPullRequestToDraft"
        | "addPullRequestReview"
        | "addPullRequestReviewComment"
        | "addPullRequestReviewThread"
        | "submitPullRequestReview"
        | "dismissPullRequestReview"
        | "deletePullRequestReview"
        | "requestReviews"
        | "requestReviewsByLogin"
        | "enablePullRequestAutoMerge"
        | "disablePullRequestAutoMerge"
        | "resolveReviewThread"
        | "unresolveReviewThread" => any(&[C::PullRequests]),
        "mergePullRequest"
        | "updatePullRequestBranch"
        | "createRef"
        | "updateRef"
        | "deleteRef"
        | "createLinkedBranch"
        | "createCommitOnBranch" => any(&[C::Contents]),
        _ => Need::Forbidden,
    }
}

/// Middleware (mounted for every route by bgh-server): requests made with
/// an Actions job token must be covered by the token's permission map.
/// REST routes are classified with [`classify`]; `/_bgh` writes are
/// refused; git transport and GraphQL are checked by their handlers.
pub async fn middleware(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    if !req.headers().contains_key(header::AUTHORIZATION) {
        return next.run(req).await;
    }
    let path = req.uri().path().to_string();
    let rest_api = path == "/api/v3" || path.starts_with("/api/v3/");
    let private_api = path.starts_with("/_bgh/");
    if !rest_api && !private_api {
        return next.run(req).await;
    }
    // Errors (bad credentials) are left to the handler's extractor.
    let Ok(Some(auth)) = crate::auth::resolve_request(&state, &mut req).await else {
        return next.run(req).await;
    };
    let Some(perms) = TokenPermissions::of(&auth) else {
        return next.run(req).await;
    };
    // Auth may have been resolved (and cached) before the request context
    // existed; note the triggering actor for audit entries now.
    if let Some(actor) = crate::perms::job_token_actor(&auth) {
        crate::sync::context::note_actions_actor(actor);
    }
    let need = if rest_api {
        classify(req.method(), &path)
    } else if matches!(*req.method(), Method::GET | Method::HEAD) {
        Need::one(C::Metadata, Access::Read)
    } else {
        Need::Forbidden
    };
    if perms.allows(&need) {
        return next.run(req).await;
    }
    // Reads of other repositories are capped to anonymous access by
    // `perms::effective`, so let them through: only the token's own
    // repository is guarded by its read permissions.
    if let Need::Any(_, Access::Read) = need
        && let Some(own) = job_token_repo(&auth)
        && let Some((owner, repo)) = repo_of(&path)
        && !is_repo(&state, own, owner, repo).await
    {
        return next.run(req).await;
    }
    // Same for installation tokens and repositories they don't cover.
    if let Need::Any(_, Access::Read) = need
        && crate::apps::installation_id(&auth).is_some()
        && let Some((owner, repo)) = repo_of(&path)
        && !crate::apps::covers_repo_named(&state, &auth, owner, repo).await
    {
        return next.run(req).await;
    }
    ApiError::forbidden(NOT_ACCESSIBLE).into_response()
}

fn repo_of(path: &str) -> Option<(&str, &str)> {
    let rest = path.strip_prefix("/api/v3/repos/")?;
    let mut it = rest.split('/');
    Some((it.next()?, it.next()?))
}

async fn is_repo(state: &AppState, id: i64, owner: &str, name: &str) -> bool {
    sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (SELECT 1 FROM repositories r JOIN users u ON u.id = r.owner_id
          WHERE r.id = $1 AND lower(u.login) = lower($2) AND lower(r.name) = lower($3))",
    )
    .bind(id)
    .bind(owner)
    .bind(name.strip_suffix(".git").unwrap_or(name))
    .fetch_one(&state.db)
    .await
    // Fail closed: treat lookup errors as the token's own repository.
    .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn need(m: Method, p: &str) -> Need {
        classify(&m, p)
    }

    #[test]
    fn categories_parse_both_spellings() {
        assert_eq!(Category::parse("pull-requests"), Some(C::PullRequests));
        assert_eq!(Category::parse("id-token"), Some(C::IdToken));
        assert_eq!(Category::parse("security_events"), Some(C::SecurityEvents));
        assert_eq!(Category::parse("admin"), None);
    }

    #[test]
    fn defaults_and_forms() {
        let r = TokenPermissions::restricted_default();
        assert_eq!(r.get(C::Contents), Access::Read);
        assert_eq!(r.get(C::Issues), Access::None);
        assert_eq!(r.get(C::Metadata), Access::Read);
        assert!(!r.has_write());
        let w = TokenPermissions::permissive_default();
        assert_eq!(w.get(C::Contents), Access::Write);
        assert_eq!(w.get(C::IdToken), Access::None);
        assert_eq!(w.get(C::Metadata), Access::Read);
        assert_eq!(TokenPermissions::none().get(C::Contents), Access::None);
        assert_eq!(TokenPermissions::none().get(C::Metadata), Access::Read);
        let ro = TokenPermissions::write_all().read_only();
        assert!(!ro.has_write());
        assert_eq!(ro.get(C::Issues), Access::Read);
        assert_eq!(
            serde_json::to_value(TokenPermissions::from_pairs([(C::Issues, Access::Write)]))
                .unwrap(),
            serde_json::json!({"issues": "write", "metadata": "read"})
        );
    }

    #[test]
    fn scopes_round_trip() {
        let p = TokenPermissions::from_pairs([
            (C::Contents, Access::Read),
            (C::PullRequests, Access::Write),
        ]);
        let scopes = p.to_scopes();
        assert!(scopes.contains(&"actions:permission:pull_requests:write".to_string()));
        assert_eq!(TokenPermissions::from_scopes(&scopes), Some(p));
        assert_eq!(TokenPermissions::from_scopes(&["repo".into()]), None);
    }

    #[test]
    fn route_table() {
        use Access::*;
        assert_eq!(
            need(Method::PUT, "/api/v3/repos/o/r/contents/a/b.txt"),
            Need::one(C::Contents, Write)
        );
        assert_eq!(
            need(Method::GET, "/repos/o/r/git/refs/heads/main"),
            Need::one(C::Contents, Read)
        );
        assert_eq!(
            need(Method::POST, "/repos/o/r/issues/3/comments"),
            Need::Any(vec![C::Issues, C::PullRequests], Write)
        );
        assert_eq!(
            need(Method::PATCH, "/repos/o/r/issues/3"),
            Need::one(C::Issues, Write)
        );
        assert_eq!(
            need(Method::PUT, "/repos/o/r/pulls/3/merge"),
            Need::one(C::Contents, Write)
        );
        assert_eq!(
            need(Method::GET, "/repos/o/r/pulls/3/merge"),
            Need::one(C::PullRequests, Read)
        );
        assert_eq!(
            need(Method::POST, "/repos/o/r/statuses/abc"),
            Need::one(C::Statuses, Write)
        );
        assert_eq!(
            need(Method::POST, "/repos/o/r/check-runs"),
            Need::one(C::Checks, Write)
        );
        assert_eq!(
            need(Method::POST, "/repos/o/r/actions/runs/1/cancel"),
            Need::one(C::Actions, Write)
        );
        assert_eq!(
            need(Method::GET, "/repos/o/r/actions/secrets"),
            Need::Forbidden
        );
        assert_eq!(need(Method::POST, "/repos/o/r/hooks"), Need::Forbidden);
        assert_eq!(need(Method::PATCH, "/repos/o/r"), Need::Forbidden);
        assert_eq!(need(Method::DELETE, "/repos/o/r"), Need::Forbidden);
        assert_eq!(
            need(Method::GET, "/repos/o/r"),
            Need::one(C::Metadata, Read)
        );
        assert_eq!(
            need(Method::GET, "/repos/o/r/stargazers"),
            Need::one(C::Metadata, Read)
        );
        assert_eq!(
            need(Method::POST, "/repos/o/r/dispatches"),
            Need::one(C::Contents, Write)
        );
        assert_eq!(need(Method::GET, "/user"), Need::Forbidden);
        assert_eq!(need(Method::PATCH, "/user"), Need::Forbidden);
        assert_eq!(need(Method::POST, "/user/repos"), Need::Forbidden);
        assert_eq!(
            need(Method::GET, "/users/octocat"),
            Need::one(C::Metadata, Read)
        );
        assert_eq!(need(Method::POST, "/orgs/acme/teams"), Need::Forbidden);
        assert_eq!(
            need(Method::GET, "/orgs/acme/packages"),
            Need::one(C::Packages, Read)
        );
        assert_eq!(
            need(Method::POST, "/markdown"),
            Need::one(C::Metadata, Read)
        );
    }

    #[test]
    fn allows() {
        let p = TokenPermissions::from_pairs([(C::Issues, Access::Write)]);
        assert!(p.allows(&classify(&Method::POST, "/repos/o/r/issues/1/comments")));
        assert!(!p.allows(&classify(&Method::PUT, "/repos/o/r/contents/x")));
        assert!(!p.allows(&classify(&Method::GET, "/repos/o/r/contents/x")));
        assert!(p.allows(&classify(&Method::GET, "/repos/o/r")));
        assert!(p.check_graphql_mutation("addComment").is_ok());
        assert!(p.check_graphql_mutation("createRef").is_err());
        assert!(p.check_graphql_mutation("createRepository").is_err());
    }
}
