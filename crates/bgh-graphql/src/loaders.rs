//! Per-request DataLoaders: every relation is batch-loaded (one query per
//! relation per batch, never per parent row).

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Arc;

use async_graphql::dataloader::{DataLoader, Loader};
use bgh_core::auth::AuthContext;
use bgh_core::perms::{self, Permission};
use bgh_core::prelude::*;
use sqlx::FromRow;

use crate::ctx::{GResult, api_err};

type LResult<K, V> = Result<HashMap<K, V>, Arc<ApiError>>;

fn db_err(e: sqlx::Error) -> Arc<ApiError> {
    Arc::new(e.into())
}

/// Load one key from a loader, mapping errors to GraphQL errors.
pub async fn one<L, K>(loader: &DataLoader<L>, key: K) -> GResult<Option<L::Value>>
where
    L: Loader<K, Error = Arc<ApiError>>,
    K: Send + Sync + Hash + Eq + Clone + 'static,
{
    loader.load_one(key).await.map_err(|e| arc_err(&e))
}

/// Load many keys (result keeps only found keys).
pub async fn many<L, K>(
    loader: &DataLoader<L>,
    keys: impl IntoIterator<Item = K>,
) -> GResult<HashMap<K, L::Value>>
where
    L: Loader<K, Error = Arc<ApiError>>,
    K: Send + Sync + Hash + Eq + Clone + 'static,
{
    loader.load_many(keys).await.map_err(|e| arc_err(&e))
}

fn arc_err(e: &Arc<ApiError>) -> async_graphql::Error {
    match &**e {
        ApiError::NotFound => api_err(ApiError::NotFound),
        other => {
            tracing::error!(error = %other, "graphql loader error");
            crate::ctx::err(
                "INTERNAL",
                "Something went wrong while executing your query.",
            )
        }
    }
}

/// All loaders of one request.
pub struct Loaders {
    pub users: DataLoader<UserLoader>,
    pub repos: DataLoader<RepoLoader>,
    pub issues: DataLoader<IssueLoader>,
    pub pulls: DataLoader<PullLoader>,
    pub issue_labels: DataLoader<IssueLabelsLoader>,
    pub issue_assignees: DataLoader<IssueAssigneesLoader>,
    pub milestones: DataLoader<MilestoneLoader>,
    pub reactions: DataLoader<ReactionLoader>,
    pub comments: DataLoader<CommentsLoader>,
    pub reviews: DataLoader<ReviewsLoader>,
    pub review_requests: DataLoader<ReviewRequestsLoader>,
    pub teams: DataLoader<TeamLoader>,
    pub rollups: DataLoader<RollupLoader>,
    pub latest_release: DataLoader<LatestReleaseLoader>,
    pub release_assets: DataLoader<ReleaseAssetsLoader>,
    pub review_comments: DataLoader<ReviewCommentsLoader>,
    pub closing_issues: DataLoader<ClosingIssuesLoader>,
    pub associations: DataLoader<AssociationLoader>,
    pub git_refs: DataLoader<crate::model::git::RefLoader>,
    pub commits: DataLoader<crate::model::git::CommitLoader>,
    pub git_empty: DataLoader<crate::model::git::EmptyLoader>,
    pub users_by_email: DataLoader<crate::model::git::UserByEmailLoader>,
}

impl Loaders {
    pub fn new(state: &AppState, auth: Option<&AuthContext>) -> Self {
        let s = || state.clone();
        let viewer = auth.map(|a| a.user.id);
        Self {
            users: DataLoader::new(UserLoader(s()), tokio::spawn),
            repos: DataLoader::new(
                RepoLoader {
                    state: s(),
                    auth: auth.cloned(),
                },
                tokio::spawn,
            ),
            issues: DataLoader::new(IssueLoader(s()), tokio::spawn),
            pulls: DataLoader::new(PullLoader(s()), tokio::spawn),
            issue_labels: DataLoader::new(IssueLabelsLoader(s()), tokio::spawn),
            issue_assignees: DataLoader::new(IssueAssigneesLoader(s()), tokio::spawn),
            milestones: DataLoader::new(MilestoneLoader(s()), tokio::spawn),
            reactions: DataLoader::new(ReactionLoader { state: s(), viewer }, tokio::spawn),
            comments: DataLoader::new(CommentsLoader(s()), tokio::spawn),
            reviews: DataLoader::new(ReviewsLoader { state: s(), viewer }, tokio::spawn),
            review_requests: DataLoader::new(ReviewRequestsLoader(s()), tokio::spawn),
            teams: DataLoader::new(TeamLoader(s()), tokio::spawn),
            rollups: DataLoader::new(RollupLoader(s()), tokio::spawn),
            latest_release: DataLoader::new(LatestReleaseLoader(s()), tokio::spawn),
            release_assets: DataLoader::new(ReleaseAssetsLoader(s()), tokio::spawn),
            review_comments: DataLoader::new(ReviewCommentsLoader(s()), tokio::spawn),
            closing_issues: DataLoader::new(ClosingIssuesLoader(s()), tokio::spawn),
            associations: DataLoader::new(AssociationLoader(s()), tokio::spawn),
            git_refs: DataLoader::new(crate::model::git::RefLoader(s()), tokio::spawn),
            commits: DataLoader::new(crate::model::git::CommitLoader(s()), tokio::spawn),
            git_empty: DataLoader::new(crate::model::git::EmptyLoader(s()), tokio::spawn),
            users_by_email: DataLoader::new(
                crate::model::git::UserByEmailLoader(s()),
                tokio::spawn,
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// Users, teams
// ---------------------------------------------------------------------------

pub struct UserLoader(AppState);

impl Loader<i64> for UserLoader {
    type Value = Arc<db::User>;
    type Error = Arc<ApiError>;

    async fn load(&self, keys: &[i64]) -> LResult<i64, Self::Value> {
        let rows = db::User::find_many(&self.0.db, keys)
            .await
            .map_err(db_err)?;
        Ok(rows.into_iter().map(|u| (u.id, Arc::new(u))).collect())
    }
}

pub struct TeamLoader(AppState);

#[derive(Debug, Clone, FromRow)]
pub struct TeamRow {
    #[sqlx(flatten)]
    pub team: db::Team,
    pub org_login: String,
}

impl Loader<i64> for TeamLoader {
    type Value = Arc<TeamRow>;
    type Error = Arc<ApiError>;

    async fn load(&self, keys: &[i64]) -> LResult<i64, Self::Value> {
        let rows: Vec<TeamRow> = sqlx::query_as(&format!(
            "SELECT {}, o.login AS org_login FROM teams t JOIN users o ON o.id = t.org_id
              WHERE t.id = ANY($1)",
            db::prefixed("t", db::Team::COLUMNS)
        ))
        .bind(keys)
        .fetch_all(&self.0.db)
        .await
        .map_err(db_err)?;
        Ok(rows.into_iter().map(|t| (t.team.id, Arc::new(t))).collect())
    }
}

// ---------------------------------------------------------------------------
// Repositories (row + owner + the viewer's effective permission)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct RepoRow {
    pub repo: db::Repository,
    pub owner: db::User,
    /// Effective permission of the viewer (token scopes applied).
    pub perm: Permission,
}

impl RepoRow {
    pub fn readable(&self) -> bool {
        self.perm >= Permission::Read
    }

    pub fn full_name(&self) -> String {
        format!("{}/{}", self.owner.login, self.repo.name)
    }
}

pub struct RepoLoader {
    state: AppState,
    auth: Option<AuthContext>,
}

impl RepoLoader {
    pub async fn rows(
        state: &AppState,
        auth: Option<&AuthContext>,
        repos: Vec<db::Repository>,
    ) -> Result<Vec<RepoRow>, ApiError> {
        let owners =
            bgh_core::views::users_by_id(state, repos.iter().map(|r| Some(r.owner_id))).await?;
        let raw = perms::repo_permissions(&state.db, auth.map(|a| a.user.id), &repos).await?;
        Ok(repos
            .into_iter()
            .filter_map(|repo| {
                let owner = owners.get(&repo.owner_id)?.clone();
                let p = raw.get(&repo.id).copied().unwrap_or(Permission::None);
                let perm = perms::effective(auth, &repo, p);
                Some(RepoRow { repo, owner, perm })
            })
            .collect())
    }
}

impl Loader<i64> for RepoLoader {
    type Value = Arc<RepoRow>;
    type Error = Arc<ApiError>;

    async fn load(&self, keys: &[i64]) -> LResult<i64, Self::Value> {
        let repos: Vec<db::Repository> = sqlx::query_as(&format!(
            "SELECT {} FROM repositories WHERE id = ANY($1)",
            db::Repository::COLUMNS
        ))
        .bind(keys)
        .fetch_all(&self.state.db)
        .await
        .map_err(db_err)?;
        let rows = Self::rows(&self.state, self.auth.as_ref(), repos)
            .await
            .map_err(Arc::new)?;
        Ok(rows.into_iter().map(|r| (r.repo.id, Arc::new(r))).collect())
    }
}

// ---------------------------------------------------------------------------
// Issues and pull requests
// ---------------------------------------------------------------------------

pub struct IssueLoader(AppState);

impl Loader<i64> for IssueLoader {
    type Value = Arc<db::Issue>;
    type Error = Arc<ApiError>;

    async fn load(&self, keys: &[i64]) -> LResult<i64, Self::Value> {
        let rows: Vec<db::Issue> = sqlx::query_as(&format!(
            "SELECT {} FROM issues WHERE id = ANY($1)",
            db::Issue::COLUMNS
        ))
        .bind(keys)
        .fetch_all(&self.0.db)
        .await
        .map_err(db_err)?;
        Ok(rows.into_iter().map(|i| (i.id, Arc::new(i))).collect())
    }
}

pub struct PullLoader(AppState);

impl Loader<i64> for PullLoader {
    type Value = Arc<db::PullRequest>;
    type Error = Arc<ApiError>;

    async fn load(&self, keys: &[i64]) -> LResult<i64, Self::Value> {
        let rows: Vec<db::PullRequest> = sqlx::query_as(&format!(
            "SELECT {} FROM pull_requests WHERE issue_id = ANY($1)",
            db::PullRequest::COLUMNS
        ))
        .bind(keys)
        .fetch_all(&self.0.db)
        .await
        .map_err(db_err)?;
        Ok(rows
            .into_iter()
            .map(|p| (p.issue_id, Arc::new(p)))
            .collect())
    }
}

pub struct IssueLabelsLoader(AppState);

#[derive(FromRow)]
struct IssueLabelRow {
    issue_id: i64,
    #[sqlx(flatten)]
    label: db::Label,
}

impl Loader<i64> for IssueLabelsLoader {
    type Value = Arc<Vec<db::Label>>;
    type Error = Arc<ApiError>;

    async fn load(&self, keys: &[i64]) -> LResult<i64, Self::Value> {
        let rows: Vec<IssueLabelRow> = sqlx::query_as(&format!(
            "SELECT il.issue_id, {} FROM issue_labels il JOIN labels l ON l.id = il.label_id
              WHERE il.issue_id = ANY($1) ORDER BY lower(l.name), l.id",
            db::prefixed("l", db::Label::COLUMNS)
        ))
        .bind(keys)
        .fetch_all(&self.0.db)
        .await
        .map_err(db_err)?;
        Ok(group(keys, rows, |r| (r.issue_id, r.label)))
    }
}

pub struct IssueAssigneesLoader(AppState);

impl Loader<i64> for IssueAssigneesLoader {
    type Value = Arc<Vec<i64>>;
    type Error = Arc<ApiError>;

    async fn load(&self, keys: &[i64]) -> LResult<i64, Self::Value> {
        let rows: Vec<(i64, i64)> = sqlx::query_as(
            "SELECT issue_id, user_id FROM issue_assignees WHERE issue_id = ANY($1)
              ORDER BY created_at, user_id",
        )
        .bind(keys)
        .fetch_all(&self.0.db)
        .await
        .map_err(db_err)?;
        Ok(group(keys, rows, |r| r))
    }
}

pub struct MilestoneLoader(AppState);

impl Loader<i64> for MilestoneLoader {
    type Value = Arc<db::Milestone>;
    type Error = Arc<ApiError>;

    async fn load(&self, keys: &[i64]) -> LResult<i64, Self::Value> {
        let rows: Vec<db::Milestone> = sqlx::query_as(&format!(
            "SELECT {} FROM milestones WHERE id = ANY($1)",
            db::Milestone::COLUMNS
        ))
        .bind(keys)
        .fetch_all(&self.0.db)
        .await
        .map_err(db_err)?;
        Ok(rows.into_iter().map(|m| (m.id, Arc::new(m))).collect())
    }
}

/// Issues a pull request closes (closing keywords in its body).
pub struct ClosingIssuesLoader(AppState);

impl Loader<i64> for ClosingIssuesLoader {
    type Value = Arc<Vec<db::Issue>>;
    type Error = Arc<ApiError>;

    async fn load(&self, keys: &[i64]) -> LResult<i64, Self::Value> {
        // Parse closing keywords (`fixes #12`) from PR bodies; same-repo only.
        let prs: Vec<(i64, i64, Option<String>)> =
            sqlx::query_as("SELECT id, repo_id, body FROM issues WHERE id = ANY($1)")
                .bind(keys)
                .fetch_all(&self.0.db)
                .await
                .map_err(db_err)?;
        let mut wanted: Vec<(i64, i64, i64)> = vec![];
        for (id, repo_id, body) in &prs {
            for n in closing_numbers(body.as_deref().unwrap_or("")) {
                wanted.push((*id, *repo_id, n));
            }
        }
        let mut out: HashMap<i64, Vec<db::Issue>> = keys.iter().map(|k| (*k, vec![])).collect();
        if !wanted.is_empty() {
            let repo_ids: Vec<i64> = wanted.iter().map(|w| w.1).collect();
            let numbers: Vec<i64> = wanted.iter().map(|w| w.2).collect();
            let rows: Vec<db::Issue> = sqlx::query_as(&format!(
                "SELECT {} FROM issues i
                  WHERE NOT i.is_pull_request
                    AND (i.repo_id, i.number) IN (SELECT * FROM unnest($1::bigint[], $2::bigint[]))",
                db::prefixed("i", db::Issue::COLUMNS)
            ))
            .bind(&repo_ids)
            .bind(&numbers)
            .fetch_all(&self.0.db)
            .await
            .map_err(db_err)?;
            for (pr, repo_id, n) in wanted {
                if let Some(i) = rows.iter().find(|i| i.repo_id == repo_id && i.number == n)
                    && let Some(v) = out.get_mut(&pr)
                    && !v.iter().any(|x| x.id == i.id)
                {
                    v.push(i.clone());
                }
            }
        }
        Ok(out.into_iter().map(|(k, v)| (k, Arc::new(v))).collect())
    }
}

/// `#n` references preceded by a GitHub closing keyword.
pub fn closing_numbers(body: &str) -> Vec<i64> {
    const KEYWORDS: &[&str] = &[
        "close", "closes", "closed", "fix", "fixes", "fixed", "resolve", "resolves", "resolved",
    ];
    let words: Vec<&str> = body.split_whitespace().collect();
    let mut out = vec![];
    for pair in words.windows(2) {
        let kw = pair[0].trim_end_matches(':').to_ascii_lowercase();
        if KEYWORDS.contains(&kw.as_str())
            && let Some(n) = pair[1]
                .trim_end_matches(|c: char| !c.is_ascii_digit())
                .strip_prefix('#')
                .and_then(|n| n.parse::<i64>().ok())
            && !out.contains(&n)
        {
            out.push(n);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Reactions
// ---------------------------------------------------------------------------

/// Reaction counts per content for one subject, plus whether the viewer
/// reacted.
#[derive(Debug, Clone, Default)]
pub struct ReactionSummary {
    /// (content, count, viewer_has_reacted)
    pub groups: Vec<(String, i64, bool)>,
}

pub struct ReactionLoader {
    state: AppState,
    viewer: Option<i64>,
}

impl Loader<(String, i64)> for ReactionLoader {
    type Value = Arc<ReactionSummary>;
    type Error = Arc<ApiError>;

    async fn load(&self, keys: &[(String, i64)]) -> LResult<(String, i64), Self::Value> {
        let types: Vec<&str> = keys.iter().map(|k| k.0.as_str()).collect();
        let ids: Vec<i64> = keys.iter().map(|k| k.1).collect();
        let rows: Vec<(String, i64, String, i64, bool)> = sqlx::query_as(
            "SELECT r.subject_type, r.subject_id, r.content, count(*),
                    coalesce(bool_or(r.user_id = $3), false)
               FROM reactions r
              WHERE (r.subject_type, r.subject_id) IN (SELECT * FROM unnest($1::text[], $2::bigint[]))
              GROUP BY 1, 2, 3",
        )
        .bind(&types)
        .bind(&ids)
        .bind(self.viewer.unwrap_or(0))
        .fetch_all(&self.state.db)
        .await
        .map_err(db_err)?;
        let mut out: HashMap<(String, i64), ReactionSummary> = keys
            .iter()
            .map(|k| (k.clone(), ReactionSummary::default()))
            .collect();
        for (ty, id, content, n, mine) in rows {
            if let Some(s) = out.get_mut(&(ty, id)) {
                s.groups.push((content, n, mine));
            }
        }
        Ok(out.into_iter().map(|(k, v)| (k, Arc::new(v))).collect())
    }
}

// ---------------------------------------------------------------------------
// Issue comments (windowed per issue)
// ---------------------------------------------------------------------------

pub struct CommentsLoader(AppState);

/// Comments of an issue in a window (`offset`, `limit` rows, by creation).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CommentsKey {
    pub issue_id: i64,
    pub offset: i64,
    pub limit: i64,
}

#[derive(FromRow)]
struct CommentWin {
    #[sqlx(flatten)]
    c: db::Comment,
}

impl Loader<CommentsKey> for CommentsLoader {
    type Value = Arc<Vec<db::Comment>>;
    type Error = Arc<ApiError>;

    async fn load(&self, keys: &[CommentsKey]) -> LResult<CommentsKey, Self::Value> {
        let mut by_window: HashMap<(i64, i64), Vec<i64>> = HashMap::new();
        for k in keys {
            by_window
                .entry((k.offset, k.limit))
                .or_default()
                .push(k.issue_id);
        }
        let mut out = HashMap::new();
        for ((offset, limit), ids) in by_window {
            let rows: Vec<CommentWin> = sqlx::query_as(&format!(
                "SELECT {cols} FROM (
                   SELECT c.*, row_number() OVER (PARTITION BY c.issue_id ORDER BY c.created_at, c.id) AS rn
                     FROM comments c WHERE c.issue_id = ANY($1)
                 ) c WHERE c.rn > $2 AND c.rn <= $2 + $3 ORDER BY c.issue_id, c.rn",
                cols = db::prefixed("c", db::Comment::COLUMNS)
            ))
            .bind(&ids)
            .bind(offset)
            .bind(limit)
            .fetch_all(&self.0.db)
            .await
            .map_err(db_err)?;
            let mut grouped: HashMap<i64, Vec<db::Comment>> =
                ids.iter().map(|i| (*i, vec![])).collect();
            for r in rows {
                grouped.entry(r.c.issue_id).or_default().push(r.c);
            }
            for (issue_id, v) in grouped {
                out.insert(
                    CommentsKey {
                        issue_id,
                        offset,
                        limit,
                    },
                    Arc::new(v),
                );
            }
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// Reviews, review requests, review comments
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, FromRow)]
pub struct ReviewRow {
    pub id: i64,
    pub pull_id: i64,
    pub repo_id: i64,
    pub user_id: Option<i64>,
    pub body: String,
    pub state: String,
    pub commit_id: Option<String>,
    pub submitted_at: Option<chrono::DateTime<chrono::Utc>>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl ReviewRow {
    pub const COLUMNS: &'static str = "id, pull_id, repo_id, user_id, body, state, commit_id, submitted_at, created_at, updated_at";
}

/// All reviews of a pull request visible to the viewer (submitted ones plus
/// the viewer's own pending review), oldest first.
pub struct ReviewsLoader {
    state: AppState,
    viewer: Option<i64>,
}

impl Loader<i64> for ReviewsLoader {
    type Value = Arc<Vec<ReviewRow>>;
    type Error = Arc<ApiError>;

    async fn load(&self, keys: &[i64]) -> LResult<i64, Self::Value> {
        let rows: Vec<ReviewRow> = sqlx::query_as(&format!(
            "SELECT {} FROM pr_reviews WHERE pull_id = ANY($1)
                AND (state <> 'PENDING' OR user_id = $2) ORDER BY pull_id, id",
            ReviewRow::COLUMNS
        ))
        .bind(keys)
        .bind(self.viewer.unwrap_or(0))
        .fetch_all(&self.state.db)
        .await
        .map_err(db_err)?;
        Ok(group(keys, rows, |r| (r.pull_id, r)))
    }
}

#[derive(Debug, Clone, FromRow)]
pub struct ReviewRequestRow {
    pub id: i64,
    pub pull_id: i64,
    pub user_id: Option<i64>,
    pub team_id: Option<i64>,
}

pub struct ReviewRequestsLoader(AppState);

impl Loader<i64> for ReviewRequestsLoader {
    type Value = Arc<Vec<ReviewRequestRow>>;
    type Error = Arc<ApiError>;

    async fn load(&self, keys: &[i64]) -> LResult<i64, Self::Value> {
        let rows: Vec<ReviewRequestRow> = sqlx::query_as(
            "SELECT id, pull_id, user_id, team_id FROM pr_requested_reviewers
              WHERE pull_id = ANY($1) ORDER BY pull_id, id",
        )
        .bind(keys)
        .fetch_all(&self.0.db)
        .await
        .map_err(db_err)?;
        Ok(group(keys, rows, |r| (r.pull_id, r)))
    }
}

#[derive(Debug, Clone, FromRow)]
pub struct ReviewCommentRow {
    pub id: i64,
    pub pull_id: i64,
    pub repo_id: i64,
    pub review_id: Option<i64>,
    pub in_reply_to_id: Option<i64>,
    pub user_id: Option<i64>,
    pub body: String,
    pub path: String,
    pub commit_id: String,
    pub original_commit_id: String,
    pub diff_hunk: String,
    pub subject_type: String,
    pub side: Option<String>,
    pub start_side: Option<String>,
    pub line: Option<i32>,
    pub original_line: Option<i32>,
    pub start_line: Option<i32>,
    pub original_start_line: Option<i32>,
    pub position: Option<i32>,
    pub original_position: Option<i32>,
    pub resolved_at: Option<chrono::DateTime<chrono::Utc>>,
    pub resolved_by_id: Option<i64>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl ReviewCommentRow {
    pub const COLUMNS: &'static str = "id, pull_id, repo_id, review_id, in_reply_to_id, user_id, \
        body, path, commit_id, original_commit_id, diff_hunk, subject_type, side, start_side, \
        line, original_line, start_line, original_start_line, position, original_position, \
        resolved_at, resolved_by_id, created_at, updated_at";
}

/// All review comments of a pull request, oldest first.
pub struct ReviewCommentsLoader(AppState);

impl Loader<i64> for ReviewCommentsLoader {
    type Value = Arc<Vec<ReviewCommentRow>>;
    type Error = Arc<ApiError>;

    async fn load(&self, keys: &[i64]) -> LResult<i64, Self::Value> {
        let rows: Vec<ReviewCommentRow> = sqlx::query_as(&format!(
            "SELECT {} FROM pr_review_comments WHERE pull_id = ANY($1) ORDER BY pull_id, id",
            ReviewCommentRow::COLUMNS
        ))
        .bind(keys)
        .fetch_all(&self.0.db)
        .await
        .map_err(db_err)?;
        Ok(group(keys, rows, |r| (r.pull_id, r)))
    }
}

// ---------------------------------------------------------------------------
// Status check rollup (latest status per context + latest check run per name)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, FromRow)]
pub struct StatusRow {
    pub id: i64,
    pub repo_id: i64,
    pub sha: String,
    pub state: String,
    pub context: String,
    pub description: Option<String>,
    pub target_url: Option<String>,
    pub avatar_url: Option<String>,
    pub creator_id: Option<i64>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, FromRow)]
pub struct CheckRunRow {
    pub id: i64,
    pub check_suite_id: i64,
    pub repo_id: i64,
    pub head_sha: String,
    pub name: String,
    pub status: String,
    pub conclusion: Option<String>,
    pub details_url: Option<String>,
    pub output: serde_json::Value,
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub app_slug: String,
    pub workflow_name: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Rollup {
    pub statuses: Vec<StatusRow>,
    pub checks: Vec<CheckRunRow>,
}

impl Rollup {
    pub fn is_empty(&self) -> bool {
        self.statuses.is_empty() && self.checks.is_empty()
    }

    /// GitHub's rollup state: ERROR, FAILURE, PENDING, EXPECTED, SUCCESS.
    pub fn state(&self) -> &'static str {
        let mut any_fail = false;
        let mut any_error = false;
        let mut any_pending = false;
        for s in &self.statuses {
            match s.state.as_str() {
                "failure" => any_fail = true,
                "error" => any_error = true,
                "pending" => any_pending = true,
                _ => {}
            }
        }
        for c in &self.checks {
            if c.status != "completed" {
                any_pending = true;
                continue;
            }
            if let Some(
                "failure" | "timed_out" | "cancelled" | "action_required" | "startup_failure",
            ) = c.conclusion.as_deref()
            {
                any_fail = true;
            }
        }
        if any_error {
            "ERROR"
        } else if any_fail {
            "FAILURE"
        } else if any_pending {
            "PENDING"
        } else {
            "SUCCESS"
        }
    }
}

pub struct RollupLoader(AppState);

impl Loader<(i64, String)> for RollupLoader {
    type Value = Arc<Rollup>;
    type Error = Arc<ApiError>;

    async fn load(&self, keys: &[(i64, String)]) -> LResult<(i64, String), Self::Value> {
        let repos: Vec<i64> = keys.iter().map(|k| k.0).collect();
        let shas: Vec<String> = keys.iter().map(|k| k.1.clone()).collect();
        let statuses: Vec<StatusRow> = sqlx::query_as(
            "SELECT DISTINCT ON (s.repo_id, s.sha, s.context)
                    s.id, s.repo_id, s.sha, s.state, s.context, s.description, s.target_url,
                    s.avatar_url, s.creator_id, s.created_at
               FROM commit_statuses s
              WHERE (s.repo_id, s.sha) IN (SELECT * FROM unnest($1::bigint[], $2::text[]))
              ORDER BY s.repo_id, s.sha, s.context, s.id DESC",
        )
        .bind(&repos)
        .bind(&shas)
        .fetch_all(&self.0.db)
        .await
        .map_err(db_err)?;
        let checks: Vec<CheckRunRow> = sqlx::query_as(
            "SELECT DISTINCT ON (r.repo_id, r.head_sha, r.name)
                    r.id, r.check_suite_id, r.repo_id, r.head_sha, r.name, r.status, r.conclusion,
                    r.details_url, r.output, r.started_at, r.completed_at, s.app_slug,
                    NULL::text AS workflow_name
               FROM check_runs r JOIN check_suites s ON s.id = r.check_suite_id
              WHERE (r.repo_id, r.head_sha) IN (SELECT * FROM unnest($1::bigint[], $2::text[]))
              ORDER BY r.repo_id, r.head_sha, r.name, r.id DESC",
        )
        .bind(&repos)
        .bind(&shas)
        .fetch_all(&self.0.db)
        .await
        .map_err(db_err)?;
        let mut out: HashMap<(i64, String), Rollup> = keys
            .iter()
            .map(|k| (k.clone(), Rollup::default()))
            .collect();
        for s in statuses {
            if let Some(r) = out.get_mut(&(s.repo_id, s.sha.clone())) {
                r.statuses.push(s);
            }
        }
        for c in checks {
            if let Some(r) = out.get_mut(&(c.repo_id, c.head_sha.clone())) {
                r.checks.push(c);
            }
        }
        Ok(out.into_iter().map(|(k, v)| (k, Arc::new(v))).collect())
    }
}

// ---------------------------------------------------------------------------
// Releases
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, FromRow)]
pub struct ReleaseRow {
    pub id: i64,
    pub repo_id: i64,
    pub tag_name: String,
    pub target_commitish: String,
    pub name: Option<String>,
    pub body: Option<String>,
    pub draft: bool,
    pub prerelease: bool,
    pub author_id: Option<i64>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub published_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl ReleaseRow {
    pub const COLUMNS: &'static str = "id, repo_id, tag_name, target_commitish, name, body, draft, \
        prerelease, author_id, created_at, published_at";
}

/// Latest published, non-prerelease release per repository.
pub struct LatestReleaseLoader(AppState);

impl Loader<i64> for LatestReleaseLoader {
    type Value = Arc<ReleaseRow>;
    type Error = Arc<ApiError>;

    async fn load(&self, keys: &[i64]) -> LResult<i64, Self::Value> {
        let rows: Vec<ReleaseRow> = sqlx::query_as(&format!(
            "SELECT DISTINCT ON (repo_id) {} FROM releases
              WHERE repo_id = ANY($1) AND NOT draft AND NOT prerelease
              ORDER BY repo_id, published_at DESC NULLS LAST, id DESC",
            ReleaseRow::COLUMNS
        ))
        .bind(keys)
        .fetch_all(&self.0.db)
        .await
        .map_err(db_err)?;
        Ok(rows.into_iter().map(|r| (r.repo_id, Arc::new(r))).collect())
    }
}

#[derive(Debug, Clone, FromRow)]
pub struct AssetRow {
    pub id: i64,
    pub release_id: i64,
    pub name: String,
    pub content_type: String,
    pub size: i64,
    pub download_count: i64,
    pub uploader_id: Option<i64>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

pub struct ReleaseAssetsLoader(AppState);

impl Loader<i64> for ReleaseAssetsLoader {
    type Value = Arc<Vec<AssetRow>>;
    type Error = Arc<ApiError>;

    async fn load(&self, keys: &[i64]) -> LResult<i64, Self::Value> {
        let rows: Vec<AssetRow> = sqlx::query_as(
            "SELECT id, release_id, name, content_type, size, download_count, uploader_id,
                    created_at, updated_at
               FROM release_assets WHERE release_id = ANY($1) ORDER BY release_id, id",
        )
        .bind(keys)
        .fetch_all(&self.0.db)
        .await
        .map_err(db_err)?;
        Ok(group(keys, rows, |r| (r.release_id, r)))
    }
}

/// Group rows by parent key; every requested key gets an entry.
fn group<R, V>(keys: &[i64], rows: Vec<R>, f: impl Fn(R) -> (i64, V)) -> HashMap<i64, Arc<Vec<V>>> {
    let mut out: HashMap<i64, Vec<V>> = keys.iter().map(|k| (*k, vec![])).collect();
    for r in rows {
        let (k, v) = f(r);
        out.entry(k).or_default().push(v);
    }
    out.into_iter().map(|(k, v)| (k, Arc::new(v))).collect()
}

// ---------------------------------------------------------------------------
// Author association per (repo, user)
// ---------------------------------------------------------------------------

pub struct AssociationLoader(pub AppState);

impl Loader<(i64, i64)> for AssociationLoader {
    /// `OWNER` | `MEMBER` | `COLLABORATOR` | `CONTRIBUTOR` | `NONE`
    type Value = &'static str;
    type Error = Arc<ApiError>;

    async fn load(&self, keys: &[(i64, i64)]) -> LResult<(i64, i64), Self::Value> {
        let repos: Vec<i64> = keys.iter().map(|k| k.0).collect();
        let users: Vec<i64> = keys.iter().map(|k| k.1).collect();
        let rows: Vec<(i64, i64, bool, bool, bool, bool)> = sqlx::query_as(
            r#"
            SELECT x.repo_id, x.user_id,
                   EXISTS (SELECT 1 FROM repositories r WHERE r.id = x.repo_id AND r.owner_id = x.user_id),
                   EXISTS (SELECT 1 FROM org_members m JOIN repositories r ON r.owner_id = m.org_id
                            WHERE r.id = x.repo_id AND m.user_id = x.user_id),
                   (EXISTS (SELECT 1 FROM collaborators c
                             WHERE c.repo_id = x.repo_id AND c.user_id = x.user_id)
                    OR EXISTS (SELECT 1 FROM team_repos tr JOIN team_members tm ON tm.team_id = tr.team_id
                                WHERE tr.repo_id = x.repo_id AND tm.user_id = x.user_id)),
                   EXISTS (SELECT 1 FROM issues i JOIN pull_requests p ON p.issue_id = i.id
                            WHERE i.repo_id = x.repo_id AND i.author_id = x.user_id AND p.merged)
              FROM unnest($1::bigint[], $2::bigint[]) AS x(repo_id, user_id)
            "#,
        )
        .bind(&repos)
        .bind(&users)
        .fetch_all(&self.0.db)
        .await
        .map_err(db_err)?;
        Ok(rows
            .into_iter()
            .map(|(r, u, owner, member, collab, contrib)| {
                let a = if owner {
                    "OWNER"
                } else if member {
                    "MEMBER"
                } else if collab {
                    "COLLABORATOR"
                } else if contrib {
                    "CONTRIBUTOR"
                } else {
                    "NONE"
                };
                ((r, u), a)
            })
            .collect())
    }
}
