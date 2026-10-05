//! GitHub REST shapes for issues, comments, events and reactions, plus the
//! batch loaders that render them without N+1 queries, and the compact
//! client shapes recorded in `sync_actions`.

use std::collections::{HashMap, HashSet};

use axum::extract::FromRequestParts;
use axum::http::header;
use axum::http::request::Parts;
use bgh_core::markdown::{self, RenderContext};
use bgh_core::models::api::{
    AuthorAssociation, Label, MinimalRepository, ReactionRollup, SimpleUser,
};
use bgh_core::node_id::{self, NodeType};
use bgh_core::prelude::*;
use bgh_core::time::ts;
use bgh_core::views;
use serde::Serialize;
use serde_json::{Map, Value, json};

// ---------------------------------------------------------------------------
// Media types
// ---------------------------------------------------------------------------

/// Which body representations to return, from the `Accept` header
/// (`application/vnd.github.{raw,text,html,full}+json`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BodyFormat {
    /// `body` only (default and `.raw`).
    #[default]
    Raw,
    /// `body_text` only.
    Text,
    /// `body_html` only.
    Html,
    /// `body`, `body_text` and `body_html`.
    Full,
}

impl BodyFormat {
    pub fn from_accept(accept: &str) -> Self {
        for part in accept.split(',') {
            let mt = part.split(';').next().unwrap_or("").trim();
            let Some(rest) = mt.strip_prefix("application/vnd.github") else {
                continue;
            };
            let rest = rest.strip_prefix(".v3").unwrap_or(rest);
            let param = rest.strip_suffix("+json").unwrap_or(rest);
            match param {
                ".text" => return Self::Text,
                ".html" => return Self::Html,
                ".full" => return Self::Full,
                ".raw" => return Self::Raw,
                _ => {}
            }
        }
        Self::Raw
    }

    fn wants_body(self) -> bool {
        matches!(self, Self::Raw | Self::Full)
    }
    fn wants_text(self) -> bool {
        matches!(self, Self::Text | Self::Full)
    }
    fn wants_html(self) -> bool {
        matches!(self, Self::Html | Self::Full)
    }
}

impl<S: Send + Sync> FromRequestParts<S> for BodyFormat {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        Ok(parts
            .headers
            .get(header::ACCEPT)
            .and_then(|v| v.to_str().ok())
            .map(Self::from_accept)
            .unwrap_or_default())
    }
}

/// `body` / `body_text` / `body_html`, present per [`BodyFormat`].
#[derive(Debug, Clone, Default, Serialize)]
pub struct Bodies {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<Option<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_text: Option<Option<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_html: Option<Option<String>>,
}

impl Bodies {
    pub fn new(
        state: &AppState,
        fmt: BodyFormat,
        owner: &str,
        repo: &str,
        body: Option<&str>,
    ) -> Self {
        let html = || {
            body.map(|b| {
                markdown::render(
                    b,
                    &RenderContext::new(&state.config.base_url).with_repo(owner, repo),
                )
            })
        };
        let html_v = if fmt.wants_html() || fmt.wants_text() {
            html()
        } else {
            None
        };
        Self {
            body: fmt.wants_body().then(|| body.map(str::to_string)),
            body_text: fmt
                .wants_text()
                .then(|| html_v.as_deref().map(html_to_text)),
            body_html: fmt.wants_html().then_some(html_v),
        }
    }
}

/// Plain-text rendering of sanitized HTML (tags stripped, entities decoded,
/// block elements separated by newlines).
pub fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut chars = html.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if c == '<' {
            let end = html[i..].find('>').map(|e| i + e).unwrap_or(html.len() - 1);
            let tag = html[i + 1..end]
                .trim_start_matches('/')
                .split(|c: char| c.is_whitespace() || c == '>' || c == '/')
                .next()
                .unwrap_or("")
                .to_ascii_lowercase();
            if matches!(
                tag.as_str(),
                "p" | "br"
                    | "li"
                    | "div"
                    | "h1"
                    | "h2"
                    | "h3"
                    | "h4"
                    | "h5"
                    | "h6"
                    | "pre"
                    | "tr"
                    | "blockquote"
                    | "ul"
                    | "ol"
                    | "hr"
            ) && !out.ends_with('\n')
                && !out.is_empty()
            {
                out.push('\n');
            }
            while let Some((j, _)) = chars.peek() {
                if *j > end {
                    break;
                }
                chars.next();
            }
        } else if c == '&' {
            let rest = &html[i..];
            let (text, len) = [
                ("&amp;", "&"),
                ("&lt;", "<"),
                ("&gt;", ">"),
                ("&quot;", "\""),
                ("&#39;", "'"),
                ("&nbsp;", " "),
            ]
            .iter()
            .find(|(e, _)| rest.starts_with(e))
            .map(|(e, t)| (*t, e.len()))
            .unwrap_or(("&", 1));
            out.push_str(text);
            for _ in 1..len {
                chars.next();
            }
        } else {
            out.push(c);
        }
    }
    out.trim().to_string()
}

// ---------------------------------------------------------------------------
// Repositories
// ---------------------------------------------------------------------------

/// A repository with its owner, as needed to build URLs.
#[derive(Debug, Clone)]
pub struct RepoInfo {
    pub repo: db::Repository,
    pub owner: db::User,
}

impl RepoInfo {
    pub fn from_access(access: &RepoAccess) -> Self {
        Self {
            repo: access.repo.clone(),
            owner: access.owner.clone(),
        }
    }
    pub fn owner_login(&self) -> &str {
        &self.owner.login
    }
    pub fn name(&self) -> &str {
        &self.repo.name
    }
}

pub type RepoMap = HashMap<i64, RepoInfo>;

pub fn repo_map(access: &RepoAccess) -> RepoMap {
    HashMap::from([(access.repo.id, RepoInfo::from_access(access))])
}

/// Load repositories (and owners) by id.
pub async fn load_repos(
    state: &AppState,
    ids: impl IntoIterator<Item = i64>,
) -> ApiResult<RepoMap> {
    let mut ids: Vec<i64> = ids.into_iter().collect();
    ids.sort_unstable();
    ids.dedup();
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let repos: Vec<db::Repository> = sqlx::query_as(&format!(
        "SELECT {} FROM repositories WHERE id = ANY($1)",
        db::Repository::COLUMNS
    ))
    .bind(&ids)
    .fetch_all(&state.db)
    .await?;
    let owners = views::users_by_id(state, repos.iter().map(|r| Some(r.owner_id))).await?;
    Ok(repos
        .into_iter()
        .filter_map(|repo| {
            let owner = owners.get(&repo.owner_id)?.clone();
            Some((repo.id, RepoInfo { repo, owner }))
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Author association
// ---------------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct AssocRow {
    repo_id: i64,
    user_id: i64,
    member: bool,
    collab: bool,
    contrib: bool,
}

/// `author_association` for `(repo_id, user_id)` pairs, in one query.
pub async fn associations(
    state: &AppState,
    repos: &RepoMap,
    pairs: impl IntoIterator<Item = (i64, i64)>,
) -> ApiResult<HashMap<(i64, i64), AuthorAssociation>> {
    let mut set: Vec<(i64, i64)> = pairs
        .into_iter()
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    set.sort_unstable();
    let mut out = HashMap::new();
    let mut pending = (Vec::new(), Vec::new());
    for (repo_id, user_id) in set {
        match repos.get(&repo_id) {
            Some(r) if r.repo.owner_id == user_id => {
                out.insert((repo_id, user_id), AuthorAssociation::Owner);
            }
            _ => {
                pending.0.push(repo_id);
                pending.1.push(user_id);
            }
        }
    }
    if pending.0.is_empty() {
        return Ok(out);
    }
    let rows: Vec<AssocRow> = sqlx::query_as(
        r#"
        SELECT x.repo_id, x.user_id,
               EXISTS (SELECT 1 FROM org_members m JOIN repositories r ON r.owner_id = m.org_id
                        WHERE r.id = x.repo_id AND m.user_id = x.user_id) AS member,
               (EXISTS (SELECT 1 FROM collaborators c
                         WHERE c.repo_id = x.repo_id AND c.user_id = x.user_id)
                OR EXISTS (SELECT 1 FROM team_repos tr JOIN team_members tm ON tm.team_id = tr.team_id
                            WHERE tr.repo_id = x.repo_id AND tm.user_id = x.user_id)) AS collab,
               EXISTS (SELECT 1 FROM issues i JOIN pull_requests p ON p.issue_id = i.id
                        WHERE i.repo_id = x.repo_id AND i.author_id = x.user_id AND p.merged) AS contrib
          FROM unnest($1::bigint[], $2::bigint[]) AS x(repo_id, user_id)
        "#,
    )
    .bind(&pending.0)
    .bind(&pending.1)
    .fetch_all(&state.db)
    .await?;
    for r in rows {
        let a = if r.member {
            AuthorAssociation::Member
        } else if r.collab {
            AuthorAssociation::Collaborator
        } else if r.contrib {
            AuthorAssociation::Contributor
        } else {
            AuthorAssociation::None
        };
        out.insert((r.repo_id, r.user_id), a);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Reactions
// ---------------------------------------------------------------------------

/// Reaction counts per subject id for one subject type.
pub async fn reaction_counts(
    state: &AppState,
    subject_type: &str,
    ids: &[i64],
) -> ApiResult<HashMap<i64, Vec<(String, i64)>>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows: Vec<(i64, String, i64)> = sqlx::query_as(
        "SELECT subject_id, content, count(*) FROM reactions
          WHERE subject_type = $1 AND subject_id = ANY($2) GROUP BY 1, 2",
    )
    .bind(subject_type)
    .bind(ids)
    .fetch_all(&state.db)
    .await?;
    let mut out: HashMap<i64, Vec<(String, i64)>> = HashMap::new();
    for (id, content, n) in rows {
        out.entry(id).or_default().push((content, n));
    }
    Ok(out)
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ReactionRow {
    pub id: i64,
    pub subject_type: String,
    pub subject_id: i64,
    pub user_id: i64,
    pub content: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl ReactionRow {
    pub const COLUMNS: &'static str = "id, subject_type, subject_id, user_id, content, created_at";
}

/// `reaction`.
#[derive(Debug, Clone, Serialize)]
pub struct Reaction {
    pub id: i64,
    pub node_id: String,
    pub user: SimpleUser,
    pub content: String,
    pub created_at: Timestamp,
}

pub async fn reactions(state: &AppState, rows: Vec<ReactionRow>) -> ApiResult<Vec<Reaction>> {
    let users = views::users_by_id(state, rows.iter().map(|r| Some(r.user_id))).await?;
    Ok(rows
        .into_iter()
        .map(|r| Reaction {
            id: r.id,
            node_id: node_id::encode(NodeType::Reaction, r.id),
            user: SimpleUser::or_ghost(&state.urls, users.get(&r.user_id)),
            content: r.content,
            created_at: r.created_at.into(),
        })
        .collect())
}

pub fn reaction_sync_json(r: &ReactionRow) -> Value {
    json!({
        "id": r.id,
        "subject_type": r.subject_type,
        "subject_id": r.subject_id,
        "user_id": r.user_id,
        "content": r.content,
        "created_at": Timestamp::from(r.created_at),
    })
}

// ---------------------------------------------------------------------------
// Issues
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct IssuePullRequest {
    pub url: String,
    pub html_url: String,
    pub diff_url: String,
    pub patch_url: String,
    pub merged_at: Option<Timestamp>,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct SubIssuesSummary {
    pub total: i64,
    pub completed: i64,
    pub percent_completed: i64,
}

/// `issue`.
#[derive(Debug, Clone, Serialize)]
pub struct Issue {
    pub url: String,
    pub repository_url: String,
    pub labels_url: String,
    pub comments_url: String,
    pub events_url: String,
    pub html_url: String,
    pub id: i64,
    pub node_id: String,
    pub number: i64,
    pub title: String,
    pub user: SimpleUser,
    pub labels: Vec<Label>,
    pub state: String,
    pub locked: bool,
    pub assignee: Option<SimpleUser>,
    pub assignees: Vec<SimpleUser>,
    pub milestone: Option<api::Milestone>,
    pub comments: i64,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub closed_at: Option<Timestamp>,
    pub author_association: AuthorAssociation,
    pub active_lock_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub draft: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pull_request: Option<IssuePullRequest>,
    pub sub_issues_summary: SubIssuesSummary,
    #[serde(flatten)]
    pub bodies: Bodies,
    /// Present on single-issue responses.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closed_by: Option<Option<SimpleUser>>,
    pub reactions: ReactionRollup,
    pub timeline_url: String,
    pub performed_via_github_app: Option<()>,
    pub state_reason: Option<String>,
    /// Present in cross-repository lists (`/issues`, `/user/issues`, ...).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository: Option<MinimalRepository>,
}

#[derive(sqlx::FromRow)]
struct IssueLabelRow {
    issue_id: i64,
    id: i64,
    repo_id: i64,
    name: String,
    color: String,
    description: Option<String>,
    is_default: bool,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
}

/// Options for [`issues`].
#[derive(Debug, Clone, Copy, Default)]
pub struct IssueOpts {
    /// Include `closed_by` (single-issue responses).
    pub closed_by: bool,
}

/// Labels per issue id (sorted by name).
pub async fn labels_for_issues(
    state: &AppState,
    ids: &[i64],
) -> ApiResult<HashMap<i64, Vec<db::Label>>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows: Vec<IssueLabelRow> = sqlx::query_as(&format!(
        "SELECT il.issue_id, {} FROM issue_labels il JOIN labels l ON l.id = il.label_id
          WHERE il.issue_id = ANY($1) ORDER BY lower(l.name), l.id",
        db::prefixed("l", db::Label::COLUMNS)
    ))
    .bind(ids)
    .fetch_all(&state.db)
    .await?;
    let mut out: HashMap<i64, Vec<db::Label>> = HashMap::new();
    for r in rows {
        out.entry(r.issue_id).or_default().push(db::Label {
            id: r.id,
            repo_id: r.repo_id,
            name: r.name,
            color: r.color,
            description: r.description,
            is_default: r.is_default,
            created_at: r.created_at,
            updated_at: r.updated_at,
        });
    }
    Ok(out)
}

/// Render issues (possibly from several repositories in `repos`), in input
/// order. Rows whose repository is missing from `repos` are skipped.
pub async fn issues(
    state: &AppState,
    fmt: BodyFormat,
    rows: &[db::Issue],
    repos: &RepoMap,
    opts: IssueOpts,
) -> ApiResult<Vec<Issue>> {
    if rows.is_empty() {
        return Ok(vec![]);
    }
    let ids: Vec<i64> = rows.iter().map(|i| i.id).collect();
    let labels = labels_for_issues(state, &ids).await?;
    let assignee_rows: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT issue_id, user_id FROM issue_assignees WHERE issue_id = ANY($1)
          ORDER BY created_at, user_id",
    )
    .bind(&ids)
    .fetch_all(&state.db)
    .await?;
    let milestone_ids: Vec<i64> = rows.iter().filter_map(|i| i.milestone_id).collect();
    let milestones: HashMap<i64, db::Milestone> = if milestone_ids.is_empty() {
        HashMap::new()
    } else {
        sqlx::query_as::<_, db::Milestone>(&format!(
            "SELECT {} FROM milestones WHERE id = ANY($1)",
            db::Milestone::COLUMNS
        ))
        .bind(&milestone_ids)
        .fetch_all(&state.db)
        .await?
        .into_iter()
        .map(|m| (m.id, m))
        .collect()
    };
    let pr_ids: Vec<i64> = rows
        .iter()
        .filter(|i| i.is_pull_request)
        .map(|i| i.id)
        .collect();
    let prs: HashMap<i64, (Option<chrono::DateTime<chrono::Utc>>, bool)> = if pr_ids.is_empty() {
        HashMap::new()
    } else {
        sqlx::query_as::<_, (i64, Option<chrono::DateTime<chrono::Utc>>, bool)>(
            "SELECT issue_id, merged_at, draft FROM pull_requests WHERE issue_id = ANY($1)",
        )
        .bind(&pr_ids)
        .fetch_all(&state.db)
        .await?
        .into_iter()
        .map(|(id, m, d)| (id, (m, d)))
        .collect()
    };
    let summaries: HashMap<i64, (i64, i64)> = sqlx::query_as::<_, (i64, i64, i64)>(
        "SELECT s.parent_id, count(*), count(*) FILTER (WHERE i.state = 'closed')
           FROM sub_issues s JOIN issues i ON i.id = s.child_id
          WHERE s.parent_id = ANY($1) GROUP BY 1",
    )
    .bind(&ids)
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .map(|(id, t, c)| (id, (t, c)))
    .collect();
    let reactions = reaction_counts(state, "issue", &ids).await?;
    let users = views::users_by_id(
        state,
        rows.iter()
            .flat_map(|i| [i.author_id, i.closed_by_id])
            .chain(assignee_rows.iter().map(|(_, u)| Some(*u)))
            .chain(milestones.values().map(|m| m.creator_id)),
    )
    .await?;
    let assoc = associations(
        state,
        repos,
        rows.iter()
            .filter_map(|i| i.author_id.map(|a| (i.repo_id, a))),
    )
    .await?;
    let mut assignees: HashMap<i64, Vec<SimpleUser>> = HashMap::new();
    for (issue_id, user_id) in &assignee_rows {
        if let Some(u) = users.get(user_id) {
            assignees
                .entry(*issue_id)
                .or_default()
                .push(SimpleUser::new(&state.urls, u));
        }
    }
    let urls = &state.urls;
    let mut out = Vec::with_capacity(rows.len());
    for i in rows {
        let Some(info) = repos.get(&i.repo_id) else {
            continue;
        };
        let (o, r) = (info.owner_login(), info.name());
        let url = urls.issue(o, r, i.number);
        let assignees = assignees.remove(&i.id).unwrap_or_default();
        let (total, completed) = summaries.get(&i.id).copied().unwrap_or((0, 0));
        out.push(Issue {
            repository_url: urls.repo(o, r),
            labels_url: format!("{url}/labels{{/name}}"),
            comments_url: format!("{url}/comments"),
            events_url: format!("{url}/events"),
            timeline_url: format!("{url}/timeline"),
            html_url: if i.is_pull_request {
                urls.pull_html(o, r, i.number)
            } else {
                urls.issue_html(o, r, i.number)
            },
            id: i.id,
            node_id: node_id::encode(
                if i.is_pull_request {
                    NodeType::PullRequest
                } else {
                    NodeType::Issue
                },
                i.id,
            ),
            number: i.number,
            title: i.title.clone(),
            user: SimpleUser::or_ghost(urls, i.author_id.and_then(|a| users.get(&a))),
            labels: labels
                .get(&i.id)
                .map(|ls| ls.iter().map(|l| Label::new(urls, o, r, l)).collect())
                .unwrap_or_default(),
            state: i.state.clone(),
            locked: i.locked,
            assignee: assignees.first().cloned(),
            assignees,
            milestone: i.milestone_id.and_then(|m| milestones.get(&m)).map(|m| {
                api::Milestone::new(urls, o, r, m, m.creator_id.and_then(|c| users.get(&c)))
            }),
            comments: i.comments_count,
            created_at: i.created_at.into(),
            updated_at: i.updated_at.into(),
            closed_at: ts(i.closed_at),
            author_association: i
                .author_id
                .and_then(|a| assoc.get(&(i.repo_id, a)).copied())
                .unwrap_or(AuthorAssociation::None),
            active_lock_reason: i.active_lock_reason.clone(),
            draft: i
                .is_pull_request
                .then(|| prs.get(&i.id).map(|p| p.1).unwrap_or(false)),
            pull_request: i.is_pull_request.then(|| {
                let html = urls.pull_html(o, r, i.number);
                IssuePullRequest {
                    url: urls.pull(o, r, i.number),
                    diff_url: format!("{html}.diff"),
                    patch_url: format!("{html}.patch"),
                    html_url: html,
                    merged_at: prs.get(&i.id).and_then(|p| ts(p.0)),
                }
            }),
            sub_issues_summary: SubIssuesSummary {
                total,
                completed,
                percent_completed: if total == 0 {
                    0
                } else {
                    completed * 100 / total
                },
            },
            bodies: Bodies::new(state, fmt, o, r, i.body.as_deref()),
            closed_by: opts.closed_by.then(|| {
                i.closed_by_id
                    .map(|c| SimpleUser::or_ghost(urls, users.get(&c)))
            }),
            reactions: ReactionRollup::from_counts(
                format!("{url}/reactions"),
                reactions.get(&i.id).map(Vec::as_slice).unwrap_or(&[]),
            ),
            url,
            performed_via_github_app: None,
            state_reason: i.state_reason.clone(),
            repository: None,
        });
    }
    Ok(out)
}

/// Fill `repository` for cross-repository lists (drops nothing: callers
/// only pass readable issues).
pub async fn attach_repositories(
    state: &AppState,
    auth: Option<&AuthContext>,
    repos: &RepoMap,
    issues: &mut [Issue],
    rows: &[db::Issue],
) -> ApiResult<()> {
    let list: Vec<db::Repository> = repos.values().map(|r| r.repo.clone()).collect();
    let minimal: HashMap<i64, MinimalRepository> = views::minimal_repos(state, auth, list)
        .await?
        .into_iter()
        .map(|m| (m.id, m))
        .collect();
    let by_id: HashMap<i64, i64> = rows.iter().map(|r| (r.id, r.repo_id)).collect();
    for issue in issues {
        if let Some(repo_id) = by_id.get(&issue.id) {
            issue.repository = minimal.get(repo_id).cloned();
        }
    }
    Ok(())
}

/// Render one issue.
pub async fn issue(
    state: &AppState,
    fmt: BodyFormat,
    row: &db::Issue,
    repos: &RepoMap,
) -> ApiResult<Issue> {
    issues(
        state,
        fmt,
        std::slice::from_ref(row),
        repos,
        IssueOpts { closed_by: true },
    )
    .await?
    .pop()
    .ok_or(ApiError::NotFound)
}

// ---------------------------------------------------------------------------
// Comments
// ---------------------------------------------------------------------------

/// `issue-comment`.
#[derive(Debug, Clone, Serialize)]
pub struct IssueComment {
    pub url: String,
    pub html_url: String,
    pub issue_url: String,
    pub id: i64,
    pub node_id: String,
    pub user: SimpleUser,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub author_association: AuthorAssociation,
    #[serde(flatten)]
    pub bodies: Bodies,
    pub reactions: ReactionRollup,
    pub performed_via_github_app: Option<()>,
}

/// Render comments (any repositories in `repos`), in input order.
pub async fn comments(
    state: &AppState,
    fmt: BodyFormat,
    rows: &[db::Comment],
    repos: &RepoMap,
) -> ApiResult<Vec<IssueComment>> {
    if rows.is_empty() {
        return Ok(vec![]);
    }
    let ids: Vec<i64> = rows.iter().map(|c| c.id).collect();
    let mut issue_ids: Vec<i64> = rows.iter().map(|c| c.issue_id).collect();
    issue_ids.sort_unstable();
    issue_ids.dedup();
    let numbers: HashMap<i64, (i64, bool)> = sqlx::query_as::<_, (i64, i64, bool)>(
        "SELECT id, number, is_pull_request FROM issues WHERE id = ANY($1)",
    )
    .bind(&issue_ids)
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .map(|(id, n, pr)| (id, (n, pr)))
    .collect();
    let users = views::users_by_id(state, rows.iter().map(|c| c.author_id)).await?;
    let reactions = reaction_counts(state, "issue_comment", &ids).await?;
    let assoc = associations(
        state,
        repos,
        rows.iter()
            .filter_map(|c| c.author_id.map(|a| (c.repo_id, a))),
    )
    .await?;
    let urls = &state.urls;
    let mut out = Vec::with_capacity(rows.len());
    for c in rows {
        let Some(info) = repos.get(&c.repo_id) else {
            continue;
        };
        let (o, r) = (info.owner_login(), info.name());
        let (number, is_pr) = numbers.get(&c.issue_id).copied().unwrap_or((0, false));
        let url = urls.issue_comment(o, r, c.id);
        out.push(IssueComment {
            html_url: if is_pr {
                format!("{}#issuecomment-{}", urls.pull_html(o, r, number), c.id)
            } else {
                urls.issue_comment_html(o, r, number, c.id)
            },
            issue_url: urls.issue(o, r, number),
            id: c.id,
            node_id: node_id::encode(NodeType::IssueComment, c.id),
            user: SimpleUser::or_ghost(urls, c.author_id.and_then(|a| users.get(&a))),
            created_at: c.created_at.into(),
            updated_at: c.updated_at.into(),
            author_association: c
                .author_id
                .and_then(|a| assoc.get(&(c.repo_id, a)).copied())
                .unwrap_or(AuthorAssociation::None),
            bodies: Bodies::new(state, fmt, o, r, Some(&c.body)),
            reactions: ReactionRollup::from_counts(
                format!("{url}/reactions"),
                reactions.get(&c.id).map(Vec::as_slice).unwrap_or(&[]),
            ),
            url,
            performed_via_github_app: None,
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Issue events
// ---------------------------------------------------------------------------

/// `issue_events` row.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct EventRow {
    pub id: i64,
    pub issue_id: i64,
    pub repo_id: i64,
    pub actor_id: Option<i64>,
    pub event: String,
    pub commit_id: Option<String>,
    pub data: Value,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl EventRow {
    pub const COLUMNS: &'static str =
        "id, issue_id, repo_id, actor_id, event, commit_id, data, created_at";
}

pub fn event_sync_json(e: &EventRow) -> Value {
    json!({
        "id": e.id,
        "issue_id": e.issue_id,
        "actor_id": e.actor_id,
        "event": e.event,
        "commit_id": e.commit_id,
        "data": e.data,
        "created_at": Timestamp::from(e.created_at),
    })
}

fn data_user_ids(e: &EventRow) -> impl Iterator<Item = Option<i64>> + '_ {
    [
        "assignee_id",
        "assigner_id",
        "requested_reviewer_id",
        "review_requester_id",
    ]
    .into_iter()
    .map(|k| e.data.get(k).and_then(Value::as_i64))
}

/// Event-specific fields shared by the events and timeline APIs.
fn event_object(
    state: &AppState,
    info: &RepoInfo,
    e: &EventRow,
    users: &HashMap<i64, db::User>,
) -> Map<String, Value> {
    let urls = &state.urls;
    let (o, r) = (info.owner_login(), info.name());
    let user = |id: Option<i64>| {
        serde_json::to_value(SimpleUser::or_ghost(urls, id.and_then(|i| users.get(&i))))
            .unwrap_or(Value::Null)
    };
    let mut m = Map::new();
    m.insert("id".into(), json!(e.id));
    m.insert(
        "node_id".into(),
        json!(node_id::encode(NodeType::IssueEvent, e.id)),
    );
    m.insert(
        "url".into(),
        json!(format!("{}/issues/events/{}", urls.repo(o, r), e.id)),
    );
    m.insert("actor".into(), user(e.actor_id));
    m.insert("event".into(), json!(e.event));
    m.insert("commit_id".into(), json!(e.commit_id));
    m.insert(
        "commit_url".into(),
        json!(e.commit_id.as_deref().map(|sha| {
            match e.data.get("commit_repository").and_then(Value::as_str) {
                Some(full) => urls.api(&format!("/repos/{full}/commits/{sha}")),
                None => urls.commit(o, r, sha),
            }
        })),
    );
    m.insert("created_at".into(), json!(Timestamp::from(e.created_at)));
    m.insert("performed_via_github_app".into(), Value::Null);
    let d = &e.data;
    match e.event.as_str() {
        "labeled" | "unlabeled" => {
            m.insert(
                "label".into(),
                d.get("label").cloned().unwrap_or(Value::Null),
            );
        }
        "assigned" | "unassigned" => {
            let assignee = user(d.get("assignee_id").and_then(Value::as_i64));
            m.insert("assignee".into(), assignee);
            m.insert(
                "assigner".into(),
                user(d.get("assigner_id").and_then(Value::as_i64).or(e.actor_id)),
            );
        }
        "milestoned" | "demilestoned" => {
            m.insert(
                "milestone".into(),
                d.get("milestone").cloned().unwrap_or(Value::Null),
            );
        }
        "renamed" => {
            m.insert(
                "rename".into(),
                d.get("rename").cloned().unwrap_or(Value::Null),
            );
        }
        "locked" => {
            m.insert(
                "lock_reason".into(),
                d.get("lock_reason").cloned().unwrap_or(Value::Null),
            );
        }
        "closed" | "reopened" => {
            m.insert(
                "state_reason".into(),
                d.get("state_reason").cloned().unwrap_or(Value::Null),
            );
        }
        "review_requested" | "review_request_removed" => {
            m.insert(
                "requested_reviewer".into(),
                user(d.get("requested_reviewer_id").and_then(Value::as_i64)),
            );
            m.insert(
                "review_requester".into(),
                user(d.get("review_requester_id").and_then(Value::as_i64)),
            );
        }
        _ => {
            // Other events carry any extra fields verbatim (e.g. transferred,
            // sub_issue_added: {"sub_issue": {...}}).
            if let Value::Object(extra) = d {
                for (k, v) in extra {
                    if !k.ends_with("_id") && k != "commit_repository" {
                        m.insert(k.clone(), v.clone());
                    }
                }
            }
        }
    }
    m
}

/// Render events (`issue-event`); with `with_issue`, each embeds its issue
/// (repository-level and single-event responses).
pub async fn events(
    state: &AppState,
    rows: &[EventRow],
    repos: &RepoMap,
    with_issue: bool,
) -> ApiResult<Vec<Value>> {
    let users = views::users_by_id(
        state,
        rows.iter()
            .flat_map(|e| std::iter::once(e.actor_id).chain(data_user_ids(e))),
    )
    .await?;
    let mut issue_json: HashMap<i64, Value> = HashMap::new();
    if with_issue {
        let mut ids: Vec<i64> = rows.iter().map(|e| e.issue_id).collect();
        ids.sort_unstable();
        ids.dedup();
        let issue_rows: Vec<db::Issue> = sqlx::query_as(&format!(
            "SELECT {} FROM issues WHERE id = ANY($1)",
            db::Issue::COLUMNS
        ))
        .bind(&ids)
        .fetch_all(&state.db)
        .await?;
        for i in issues(
            state,
            BodyFormat::Raw,
            &issue_rows,
            repos,
            IssueOpts::default(),
        )
        .await?
        {
            issue_json.insert(i.id, serde_json::to_value(i)?);
        }
    }
    let mut out = Vec::with_capacity(rows.len());
    for e in rows {
        let Some(info) = repos.get(&e.repo_id) else {
            continue;
        };
        let mut m = event_object(state, info, e, &users);
        if with_issue {
            m.insert(
                "issue".into(),
                issue_json.get(&e.issue_id).cloned().unwrap_or(Value::Null),
            );
        }
        out.push(Value::Object(m));
    }
    Ok(out)
}

/// Timeline rendering of a plain event (no embedded issue).
pub fn timeline_event(
    state: &AppState,
    info: &RepoInfo,
    e: &EventRow,
    users: &HashMap<i64, db::User>,
) -> Value {
    Value::Object(event_object(state, info, e, users))
}

pub fn event_user_ids(rows: &[EventRow]) -> Vec<Option<i64>> {
    rows.iter()
        .flat_map(|e| std::iter::once(e.actor_id).chain(data_user_ids(e)))
        .collect()
}

// ---------------------------------------------------------------------------
// Sync shapes
// ---------------------------------------------------------------------------

pub fn label_sync_json(l: &db::Label) -> Value {
    json!({
        "id": l.id,
        "repo_id": l.repo_id,
        "name": l.name,
        "color": l.color,
        "description": l.description,
        "default": l.is_default,
        "updated_at": Timestamp::from(l.updated_at),
    })
}

pub fn milestone_sync_json(m: &db::Milestone) -> Value {
    json!({
        "id": m.id,
        "repo_id": m.repo_id,
        "number": m.number,
        "title": m.title,
        "description": m.description,
        "state": m.state,
        "creator_id": m.creator_id,
        "open_issues": m.open_issues,
        "closed_issues": m.closed_issues,
        "due_on": ts(m.due_on),
        "closed_at": ts(m.closed_at),
        "created_at": Timestamp::from(m.created_at),
        "updated_at": Timestamp::from(m.updated_at),
    })
}

pub fn comment_sync_json(c: &db::Comment) -> Value {
    json!({
        "id": c.id,
        "issue_id": c.issue_id,
        "repo_id": c.repo_id,
        "author_id": c.author_id,
        "body": c.body,
        "created_at": Timestamp::from(c.created_at),
        "updated_at": Timestamp::from(c.updated_at),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_accept() {
        use BodyFormat::*;
        assert_eq!(BodyFormat::from_accept("application/vnd.github+json"), Raw);
        assert_eq!(
            BodyFormat::from_accept("application/vnd.github.html+json"),
            Html
        );
        assert_eq!(
            BodyFormat::from_accept("application/vnd.github.v3.text+json"),
            Text
        );
        assert_eq!(
            BodyFormat::from_accept("application/json, application/vnd.github.full+json"),
            Full
        );
    }

    #[test]
    fn strips_html() {
        assert_eq!(
            html_to_text("<p>Hello <strong>you</strong> &amp; me</p>\n<p>Next</p>"),
            "Hello you & me\n\nNext"
        );
    }
}
