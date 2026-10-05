//! Compact client shapes of the synced models (docs/SYNC_PROTOCOL.md §3).
//!
//! This is the **single place** that turns database rows into the camelCase
//! rows the web client stores. Bootstrap, partial sync and every domain
//! crate's sync payloads use it, so a delta always has the same shape as the
//! bootstrap row:
//!
//! ```ignore
//! let mut tx = Tx::begin(&state).await?;
//! sqlx::query("UPDATE issues SET title = $2, updated_at = now() WHERE id = $1")
//!     .bind(issue_id).bind(&title).execute(&mut *tx).await?;
//! tx.sync_issue(issue_id, SyncAction::Update, false).await?; // loads + records
//! tx.commit().await?;
//! ```
//!
//! Rows are built in SQL (`json_build_object`) with set-based subqueries, so
//! loading 10k issues is one statement and no per-row Rust work. Each query
//! yields `(scope, id, j)` where `j` is the row's JSON text.

use std::collections::{BTreeSet, HashMap};

use serde::Serialize;
use serde_json::Value;
use sqlx::PgConnection;

use crate::db::Tx;
use crate::error::ApiResult;
use crate::perms::{self, Permission};
use crate::sync::{SyncAction, org_scope, user_scope};

/// A synced model type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Model {
    User,
    Org,
    Membership,
    Team,
    Repo,
    ViewerRepo,
    Label,
    Milestone,
    Issue,
    Comment,
    Review,
    IssueEvent,
    Notification,
}

impl Model {
    pub const ALL: [Model; 13] = [
        Model::User,
        Model::Org,
        Model::Membership,
        Model::Team,
        Model::Repo,
        Model::ViewerRepo,
        Model::Label,
        Model::Milestone,
        Model::Issue,
        Model::Comment,
        Model::Review,
        Model::IssueEvent,
        Model::Notification,
    ];

    /// Wire name (`"issueEvent"`), also stored in `sync_actions.model`.
    pub fn name(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Org => "org",
            Self::Membership => "membership",
            Self::Team => "team",
            Self::Repo => "repo",
            Self::ViewerRepo => "viewerRepo",
            Self::Label => "label",
            Self::Milestone => "milestone",
            Self::Issue => "issue",
            Self::Comment => "comment",
            Self::Review => "review",
            Self::IssueEvent => "issueEvent",
            Self::Notification => "notification",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.name() == name)
    }

    /// Lazy models are not part of the bootstrap (loaded by partial sync).
    pub fn is_lazy(self) -> bool {
        matches!(self, Self::Comment | Self::Review | Self::IssueEvent)
    }
}

impl std::fmt::Display for Model {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// Which rows to load: every variant is a set of ids matched against one
/// column of the model's table.
#[derive(Debug, Clone, Copy)]
pub enum Filter<'a> {
    /// Model ids.
    Ids(&'a [i64]),
    /// Rows of these repositories (repo-scoped models).
    Repos(&'a [i64]),
    /// Rows of these issues (comment, review, issueEvent).
    Issues(&'a [i64]),
    /// Rows of these organizations (org, membership, team).
    Orgs(&'a [i64]),
    /// Rows owned by these users (membership by member, notification by recipient).
    Users(&'a [i64]),
}

impl<'a> Filter<'a> {
    fn ids(&self) -> &'a [i64] {
        match *self {
            Self::Ids(v) | Self::Repos(v) | Self::Issues(v) | Self::Orgs(v) | Self::Users(v) => v,
        }
    }
}

/// Loader options.
#[derive(Debug, Clone, Copy, Default)]
pub struct Opts {
    /// Include the lazy `issue.body`.
    pub issue_body: bool,
    /// Viewer, for rows only their owner may see (pending reviews).
    pub viewer: Option<i64>,
}

impl Opts {
    pub fn with_body() -> Self {
        Self {
            issue_body: true,
            viewer: None,
        }
    }
}

/// One loaded row.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub scope: String,
    pub model: Model,
    pub id: i64,
    pub data: Value,
}

// ---------------------------------------------------------------------------
// SQL
// ---------------------------------------------------------------------------

const ISSUE_KEYS: &str = "
    'id', i.id, 'repoId', i.repo_id, 'number', i.number, 'title', i.title,
    'state', i.state,
    'stateReason', CASE i.state_reason WHEN 'duplicate' THEN 'not_planned' ELSE i.state_reason END,
    'authorId', i.author_id,
    'assigneeIds', coalesce((SELECT array_agg(a.user_id ORDER BY a.created_at, a.user_id)
                               FROM issue_assignees a WHERE a.issue_id = i.id), '{}'),
    'labelIds', coalesce((SELECT array_agg(il.label_id ORDER BY il.label_id)
                            FROM issue_labels il WHERE il.issue_id = i.id), '{}'),
    'milestoneId', i.milestone_id, 'comments', i.comments_count, 'locked', i.locked,
    'reactions', coalesce((SELECT json_object_agg(x.content, x.n) FROM
                             (SELECT content, count(*) AS n FROM reactions
                               WHERE subject_type = 'issue' AND subject_id = i.id GROUP BY content) x), '{}'),
    'createdAt', bgh_ts(i.created_at), 'updatedAt', bgh_ts(i.updated_at),
    'closedAt', bgh_ts(i.closed_at), 'isPr', i.is_pull_request";

const PR_KEYS: &str = "
    'draft', p.draft, 'merged', p.merged, 'mergedAt', bgh_ts(p.merged_at),
    'mergedById', p.merged_by_id, 'headRef', p.head_ref, 'headRepoId', p.head_repo_id,
    'headSha', p.head_sha, 'baseRef', p.base_ref, 'baseSha', p.base_sha,
    'mergeable', p.mergeable,
    'mergeableState', CASE p.mergeable_state WHEN 'has_hooks' THEN 'clean'
                                             WHEN 'draft' THEN 'blocked'
                                             ELSE p.mergeable_state END,
    'reviewDecision', coalesce(
        (SELECT CASE WHEN bool_or(d.state = 'CHANGES_REQUESTED') THEN 'changes_requested'
                     WHEN bool_or(d.state = 'APPROVED') THEN 'approved' END
           FROM (SELECT DISTINCT ON (rv.user_id) rv.state FROM pr_reviews rv
                  WHERE rv.pull_id = i.id
                    AND rv.state IN ('APPROVED', 'CHANGES_REQUESTED', 'DISMISSED')
                  ORDER BY rv.user_id, rv.submitted_at DESC NULLS LAST, rv.id DESC) d),
        CASE WHEN EXISTS (SELECT 1 FROM pr_requested_reviewers q WHERE q.pull_id = i.id)
             THEN 'review_required' END),
    'requestedReviewerIds', coalesce((SELECT array_agg(q.user_id ORDER BY q.id) FROM pr_requested_reviewers q
                                        WHERE q.pull_id = i.id AND q.user_id IS NOT NULL), '{}'),
    'requestedTeamIds', coalesce((SELECT array_agg(q.team_id ORDER BY q.id) FROM pr_requested_reviewers q
                                    WHERE q.pull_id = i.id AND q.team_id IS NOT NULL), '{}'),
    'checks', (SELECT CASE WHEN count(*) = 0 THEN NULL
                           WHEN bool_or(c.s NOT IN ('success', 'neutral', 'skipped', 'pending')) THEN 'failure'
                           WHEN bool_or(c.s = 'pending') THEN 'pending'
                           WHEN bool_and(c.s IN ('neutral', 'skipped')) THEN 'neutral'
                           ELSE 'success' END
                 FROM ((SELECT DISTINCT ON (cr.name)
                               CASE WHEN cr.status <> 'completed' THEN 'pending'
                                    ELSE coalesce(cr.conclusion, 'neutral') END AS s
                          FROM check_runs cr
                         WHERE cr.repo_id = i.repo_id AND cr.head_sha = p.head_sha
                         ORDER BY cr.name, cr.id DESC)
                       UNION ALL
                       (SELECT DISTINCT ON (cs.context) cs.state AS s
                          FROM commit_statuses cs
                         WHERE cs.repo_id = i.repo_id AND cs.sha = p.head_sha
                         ORDER BY cs.context, cs.id DESC)) c),
    'additions', p.additions, 'deletions', p.deletions,
    'changedFiles', p.changed_files, 'commits', p.commits";

/// `(scope, id, j)` select for `model`; `$1` is the filter's id array and
/// `$2` the viewer id (may be NULL).
fn select_sql(model: Model, filter: &Filter<'_>, opts: Opts) -> Option<String> {
    use Filter as F;
    let col = |alias: &str, f: &Filter<'_>, map: &[(&str, &str)]| -> Option<String> {
        let kind = match f {
            F::Ids(_) => "ids",
            F::Repos(_) => "repos",
            F::Issues(_) => "issues",
            F::Orgs(_) => "orgs",
            F::Users(_) => "users",
        };
        map.iter()
            .find(|(k, _)| *k == kind)
            .map(|(_, c)| format!("{alias}.{c} = ANY($1)"))
    };
    Some(match model {
        Model::User => format!(
            "SELECT 'user:' || u.id AS scope, u.id, json_build_object(
                 'id', u.id, 'login', u.login, 'name', u.name,
                 'avatarUrl', coalesce(u.avatar_url, ''),
                 'type', CASE WHEN u.type = 'Bot' THEN 'Bot' ELSE 'User' END)::text AS j
               FROM users u WHERE {} AND u.type <> 'Organization'",
            col("u", filter, &[("ids", "id")])?
        ),
        Model::Org => format!(
            "SELECT 'org:' || u.id AS scope, u.id, json_build_object(
                 'id', u.id, 'login', u.login, 'name', u.name,
                 'avatarUrl', coalesce(u.avatar_url, ''), 'description', s.description)::text AS j
               FROM users u LEFT JOIN org_settings s ON s.org_id = u.id
              WHERE {} AND u.type = 'Organization'",
            col("u", filter, &[("ids", "id"), ("orgs", "id")])?
        ),
        Model::Membership => format!(
            "SELECT 'org:' || m.org_id AS scope, m.id, json_build_object(
                 'id', m.id, 'orgId', m.org_id, 'userId', m.user_id, 'role', m.role)::text AS j
               FROM org_members m WHERE {}",
            col(
                "m",
                filter,
                &[("ids", "id"), ("orgs", "org_id"), ("users", "user_id")]
            )?
        ),
        Model::Team => format!(
            "SELECT 'org:' || t.org_id AS scope, t.id, json_build_object(
                 'id', t.id, 'orgId', t.org_id, 'slug', t.slug, 'name', t.name,
                 'description', t.description, 'privacy', t.privacy, 'parentId', t.parent_id,
                 'memberIds', coalesce((SELECT array_agg(tm.user_id ORDER BY tm.user_id)
                                          FROM team_members tm WHERE tm.team_id = t.id), '{{}}'),
                 'repoIds', coalesce((SELECT array_agg(tr.repo_id ORDER BY tr.repo_id)
                                        FROM team_repos tr WHERE tr.team_id = t.id), '{{}}'))::text AS j
               FROM teams t WHERE {}",
            col("t", filter, &[("ids", "id"), ("orgs", "org_id")])?
        ),
        Model::Repo => format!(
            "SELECT 'repo:' || r.id AS scope, r.id, json_build_object(
                 'id', r.id, 'ownerId', r.owner_id, 'owner', o.login, 'name', r.name,
                 'description', r.description, 'private', r.visibility <> 'public',
                 'fork', r.fork, 'archived', r.archived, 'defaultBranch', r.default_branch,
                 'language', r.language, 'topics', r.topics, 'stars', r.stargazers_count,
                 'forks', r.forks_count, 'watchers', r.watchers_count,
                 'openIssues', (SELECT count(*) FROM issues x WHERE x.repo_id = r.id
                                   AND NOT x.is_pull_request AND x.state = 'open'),
                 'openPulls', (SELECT count(*) FROM issues x WHERE x.repo_id = r.id
                                  AND x.is_pull_request AND x.state = 'open'),
                 'hasIssues', r.has_issues, 'hasProjects', r.has_projects, 'hasWiki', r.has_wiki,
                 'pushedAt', bgh_ts(r.pushed_at), 'createdAt', bgh_ts(r.created_at),
                 'updatedAt', bgh_ts(r.updated_at))::text AS j
               FROM repositories r JOIN users o ON o.id = r.owner_id WHERE {}",
            col("r", filter, &[("ids", "id"), ("repos", "id")])?
        ),
        Model::Label => format!(
            "SELECT 'repo:' || l.repo_id AS scope, l.id, json_build_object(
                 'id', l.id, 'repoId', l.repo_id, 'name', l.name, 'color', l.color,
                 'description', l.description)::text AS j
               FROM labels l WHERE {}",
            col("l", filter, &[("ids", "id"), ("repos", "repo_id")])?
        ),
        Model::Milestone => format!(
            "SELECT 'repo:' || m.repo_id AS scope, m.id, json_build_object(
                 'id', m.id, 'repoId', m.repo_id, 'number', m.number, 'title', m.title,
                 'description', m.description, 'state', m.state, 'dueOn', bgh_ts(m.due_on),
                 'openIssues', m.open_issues, 'closedIssues', m.closed_issues,
                 'createdAt', bgh_ts(m.created_at), 'updatedAt', bgh_ts(m.updated_at),
                 'closedAt', bgh_ts(m.closed_at))::text AS j
               FROM milestones m WHERE {}",
            col("m", filter, &[("ids", "id"), ("repos", "repo_id")])?
        ),
        Model::Issue => {
            let body = if opts.issue_body { ", 'body', i.body" } else { "" };
            format!(
                "SELECT 'repo:' || i.repo_id AS scope, i.id, (CASE WHEN p.issue_id IS NULL
                     THEN json_build_object({ISSUE_KEYS}{body})
                     ELSE json_build_object({ISSUE_KEYS}{body}, {PR_KEYS}) END)::text AS j
                   FROM issues i LEFT JOIN pull_requests p ON p.issue_id = i.id
                  WHERE {}",
                col("i", filter, &[("ids", "id"), ("repos", "repo_id")])?
            )
        }
        Model::Comment => format!(
            "SELECT 'repo:' || c.repo_id AS scope, c.id, json_build_object(
                 'id', c.id, 'repoId', c.repo_id, 'issueId', c.issue_id, 'authorId', c.author_id,
                 'body', c.body,
                 'authorAssociation', CASE
                     WHEN c.author_id IS NULL THEN 'NONE'
                     WHEN c.author_id = r.owner_id THEN 'OWNER'
                     WHEN EXISTS (SELECT 1 FROM org_members om
                                   WHERE om.org_id = r.owner_id AND om.user_id = c.author_id) THEN 'MEMBER'
                     WHEN EXISTS (SELECT 1 FROM collaborators k
                                   WHERE k.repo_id = c.repo_id AND k.user_id = c.author_id) THEN 'COLLABORATOR'
                     WHEN EXISTS (SELECT 1 FROM issues x JOIN pull_requests px ON px.issue_id = x.id
                                   WHERE x.repo_id = c.repo_id AND x.author_id = c.author_id
                                     AND px.merged) THEN 'CONTRIBUTOR'
                     ELSE 'NONE' END,
                 'reactions', coalesce((SELECT json_object_agg(x.content, x.n) FROM
                                          (SELECT content, count(*) AS n FROM reactions
                                            WHERE subject_type = 'issue_comment' AND subject_id = c.id
                                            GROUP BY content) x), '{{}}'),
                 'createdAt', bgh_ts(c.created_at), 'updatedAt', bgh_ts(c.updated_at))::text AS j
               FROM comments c JOIN repositories r ON r.id = c.repo_id WHERE {}",
            col(
                "c",
                filter,
                &[("ids", "id"), ("repos", "repo_id"), ("issues", "issue_id")]
            )?
        ),
        Model::Review => format!(
            "SELECT 'repo:' || v.repo_id AS scope, v.id, json_build_object(
                 'id', v.id, 'repoId', v.repo_id, 'issueId', v.pull_id, 'authorId', v.user_id,
                 'state', v.state, 'body', v.body, 'commitId', coalesce(v.commit_id, ''),
                 'submittedAt', bgh_ts(v.submitted_at))::text AS j
               FROM pr_reviews v
              WHERE {} AND (v.state <> 'PENDING' OR v.user_id = $2)",
            col(
                "v",
                filter,
                &[("ids", "id"), ("repos", "repo_id"), ("issues", "pull_id")]
            )?
        ),
        Model::IssueEvent => format!(
            "SELECT 'repo:' || e.repo_id AS scope, e.id, json_build_object(
                 'id', e.id, 'repoId', e.repo_id, 'issueId', e.issue_id, 'actorId', e.actor_id,
                 'event', e.event,
                 'data', json_strip_nulls(json_build_object(
                     'labelId', coalesce(e.data->'label'->'id', e.data->'label_id'),
                     'labelName', coalesce(e.data->'label'->'name', e.data->'label_name'),
                     'labelColor', coalesce(e.data->'label'->'color', e.data->'label_color'),
                     'assigneeId', coalesce(e.data->'assignee_id', e.data->'assignee'->'id'),
                     'reviewerId', coalesce(e.data->'requested_reviewer_id', e.data->'reviewer_id',
                                            e.data->'requested_reviewer'->'id'),
                     'milestoneTitle', coalesce(e.data->'milestone'->'title', e.data->'milestone_title'),
                     'from', coalesce(e.data->'rename'->'from', e.data->'from'),
                     'to', coalesce(e.data->'rename'->'to', e.data->'to'),
                     'stateReason', e.data->'state_reason',
                     'commitId', e.commit_id)),
                 'createdAt', bgh_ts(e.created_at))::text AS j
               FROM issue_events e WHERE {}",
            col(
                "e",
                filter,
                &[("ids", "id"), ("repos", "repo_id"), ("issues", "issue_id")]
            )?
        ),
        Model::Notification => format!(
            "SELECT 'user:' || n.user_id AS scope, n.id, json_build_object(
                 'id', n.id, 'repoId', n.repo_id, 'subjectType', n.subject_type,
                 'subjectId', n.subject_id, 'title', n.subject_title, 'reason', n.reason,
                 'unread', n.unread, 'updatedAt', bgh_ts(n.updated_at),
                 'lastReadAt', bgh_ts(n.last_read_at))::text AS j
               FROM notifications n WHERE {} AND NOT n.done",
            col("n", filter, &[("ids", "id"), ("users", "user_id")])?
        ),
        Model::ViewerRepo => return None,
    })
}

#[derive(sqlx::FromRow)]
struct RawRow {
    scope: String,
    id: i64,
    j: String,
}

fn unsupported(model: Model, filter: &Filter<'_>) -> sqlx::Error {
    sqlx::Error::Protocol(format!(
        "sync shapes: model {model} cannot be loaded by {filter:?}"
    ))
}

/// Load compact rows of `model` matching `filter`.
pub async fn load(
    conn: &mut PgConnection,
    model: Model,
    filter: Filter<'_>,
    opts: Opts,
) -> Result<Vec<Row>, sqlx::Error> {
    let sql = select_sql(model, &filter, opts).ok_or_else(|| unsupported(model, &filter))?;
    let rows: Vec<RawRow> = sqlx::query_as(&format!("{sql} ORDER BY 2"))
        .bind(filter.ids())
        .bind(opts.viewer)
        .fetch_all(conn)
        .await?;
    rows.into_iter()
        .map(|r| {
            Ok(Row {
                scope: r.scope,
                model,
                id: r.id,
                data: serde_json::from_str(&r.j).map_err(|e| sqlx::Error::Decode(Box::new(e)))?,
            })
        })
        .collect()
}

/// Load one row by id.
pub async fn load_one(
    conn: &mut PgConnection,
    model: Model,
    id: i64,
    opts: Opts,
) -> Result<Option<Row>, sqlx::Error> {
    Ok(load(conn, model, Filter::Ids(&[id]), opts)
        .await?
        .into_iter()
        .next())
}

/// Rows of `model` as comma-separated JSON objects (no brackets) and the row
/// count, aggregated in Postgres — the bootstrap fast path.
pub async fn load_joined(
    conn: &mut PgConnection,
    model: Model,
    filter: Filter<'_>,
    opts: Opts,
) -> Result<(String, i64), sqlx::Error> {
    if filter.ids().is_empty() {
        return Ok((String::new(), 0));
    }
    let sql = select_sql(model, &filter, opts).ok_or_else(|| unsupported(model, &filter))?;
    sqlx::query_as(&format!(
        "SELECT coalesce(string_agg(s.j, ',' ORDER BY s.id), ''), count(*) FROM ({sql}) s"
    ))
    .bind(filter.ids())
    .bind(opts.viewer)
    .fetch_one(conn)
    .await
}

// ---------------------------------------------------------------------------
// viewerRepo
// ---------------------------------------------------------------------------

/// `ViewerRepo` row (scope `user:{viewer}`, id = repo id).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewerRepo {
    pub id: i64,
    pub permission: Permission,
    pub starred: bool,
    /// `subscribed` | `ignored` | `participating`
    pub watching: &'static str,
}

#[derive(sqlx::FromRow)]
struct ViewerRow {
    id: i64,
    starred: bool,
    subscribed: Option<bool>,
    ignored: Option<bool>,
}

/// `viewerRepo` rows for `user_id` given already computed permissions
/// (repos with `Permission::None` are skipped).
pub async fn viewer_repos(
    conn: &mut PgConnection,
    user_id: i64,
    permissions: &HashMap<i64, Permission>,
) -> Result<Vec<ViewerRepo>, sqlx::Error> {
    let ids: Vec<i64> = permissions
        .iter()
        .filter(|(_, p)| **p >= Permission::Read)
        .map(|(id, _)| *id)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows: Vec<ViewerRow> = sqlx::query_as(
        "SELECT r.id,
                EXISTS (SELECT 1 FROM stars s WHERE s.user_id = $1 AND s.repo_id = r.id) AS starred,
                w.subscribed, w.ignored
           FROM unnest($2::bigint[]) AS r(id)
           LEFT JOIN watches w ON w.user_id = $1 AND w.repo_id = r.id
          ORDER BY r.id",
    )
    .bind(user_id)
    .bind(&ids)
    .fetch_all(conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| ViewerRepo {
            id: r.id,
            permission: permissions[&r.id],
            starred: r.starred,
            watching: match (r.ignored, r.subscribed) {
                (Some(true), _) => "ignored",
                (_, Some(true)) => "subscribed",
                _ => "participating",
            },
        })
        .collect())
}

// ---------------------------------------------------------------------------
// referenced users
// ---------------------------------------------------------------------------

/// User ids referenced by a compact row (authors, assignees, members,
/// reviewers, actors), for `refs.user` / `models.user`.
pub fn referenced_users(model: &str, data: &Value, out: &mut BTreeSet<i64>) {
    if model == Model::User.name() {
        return;
    }
    for key in ["authorId", "userId", "actorId", "mergedById"] {
        if let Some(id) = data.get(key).and_then(Value::as_i64) {
            out.insert(id);
        }
    }
    for key in ["assigneeIds", "requestedReviewerIds", "memberIds"] {
        if let Some(ids) = data.get(key).and_then(Value::as_array) {
            out.extend(ids.iter().filter_map(Value::as_i64));
        }
    }
    if model == Model::IssueEvent.name()
        && let Some(d) = data.get("data")
    {
        for key in ["assigneeId", "reviewerId"] {
            if let Some(id) = d.get(key).and_then(Value::as_i64) {
                out.insert(id);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tx helpers for domain crates
// ---------------------------------------------------------------------------

impl Tx {
    /// Load the compact row of `model` `id` (as seen inside this
    /// transaction) and record it with `action` in the row's scope.
    /// Returns `false` (recording nothing) if the row doesn't exist.
    ///
    /// For `issue` this omits the lazy `body`; use [`Tx::sync_issue`] when
    /// the body changed. Not for `viewerRepo` (use [`Tx::sync_viewer_repo`])
    /// or deletes (use [`Tx::sync_delete`]).
    pub async fn sync_model(
        &mut self,
        model: Model,
        id: i64,
        action: SyncAction,
    ) -> ApiResult<bool> {
        Ok(self.sync_models(model, &[id], action).await? > 0)
    }

    /// Batch form of [`Tx::sync_model`] (one query for all ids). Returns how
    /// many rows were recorded.
    pub async fn sync_models(
        &mut self,
        model: Model,
        ids: &[i64],
        action: SyncAction,
    ) -> ApiResult<usize> {
        self.sync_loaded(model, ids, action, Opts::default()).await
    }

    /// Record an issue / pull request. Pass `body_changed = true` when the
    /// body was set or edited (inserts too) so the lazy `body` is included.
    pub async fn sync_issue(
        &mut self,
        issue_id: i64,
        action: SyncAction,
        body_changed: bool,
    ) -> ApiResult<bool> {
        let opts = Opts {
            issue_body: body_changed,
            viewer: None,
        };
        Ok(self
            .sync_loaded(Model::Issue, &[issue_id], action, opts)
            .await?
            > 0)
    }

    async fn sync_loaded(
        &mut self,
        model: Model,
        ids: &[i64],
        action: SyncAction,
        opts: Opts,
    ) -> ApiResult<usize> {
        if ids.is_empty() {
            return Ok(0);
        }
        let rows = load(self, model, Filter::Ids(ids), opts).await?;
        for row in &rows {
            self.sync(&row.scope, model.name(), row.id, action, &row.data)
                .await?;
        }
        Ok(rows.len())
    }

    /// Record a delete. Call it in the transaction that deletes the row
    /// (the scope can't be derived from a deleted row). Deleting an `issue`
    /// also deletes its comments/reviews/events on the client.
    pub async fn sync_delete(&mut self, scope: &str, model: Model, id: i64) -> ApiResult<()> {
        self.sync(scope, model.name(), id, SyncAction::Delete, &Value::Null)
            .await
    }

    /// Record a user profile change (login/name/avatar) in `user:{id}` and
    /// in every `org:{id}` scope the user is a member of (protocol §3.1).
    pub async fn sync_user(&mut self, user_id: i64) -> ApiResult<()> {
        let Some(row) = load_one(self, Model::User, user_id, Opts::default()).await? else {
            return Ok(());
        };
        let orgs: Vec<i64> =
            sqlx::query_scalar("SELECT org_id FROM org_members WHERE user_id = $1 ORDER BY org_id")
                .bind(user_id)
                .fetch_all(&mut **self)
                .await?;
        self.sync(
            &user_scope(user_id),
            "user",
            user_id,
            SyncAction::Update,
            &row.data,
        )
        .await?;
        for org in orgs {
            self.sync(
                &org_scope(org),
                "user",
                user_id,
                SyncAction::Update,
                &row.data,
            )
            .await?;
        }
        Ok(())
    }

    /// Record the `viewerRepo` row of `user_id` for `repo_id` (permission,
    /// starred, watching), or a delete when the user can no longer read the
    /// repository. Call after changing stars, watches or the user's access.
    pub async fn sync_viewer_repo(&mut self, user_id: i64, repo_id: i64) -> ApiResult<()> {
        let repo = sqlx::query_as::<_, crate::models::db::Repository>(&format!(
            "SELECT {} FROM repositories WHERE id = $1",
            crate::models::db::Repository::COLUMNS
        ))
        .bind(repo_id)
        .fetch_optional(&mut **self)
        .await?;
        let scope = user_scope(user_id);
        let permission = match &repo {
            Some(repo) => perms::repo_permission(&mut **self, Some(user_id), repo).await?,
            None => Permission::None,
        };
        if permission < Permission::Read {
            return self.sync_delete(&scope, Model::ViewerRepo, repo_id).await;
        }
        let map = HashMap::from([(repo_id, permission)]);
        let rows = viewer_repos(self, user_id, &map).await?;
        for row in rows {
            self.sync(
                &scope,
                Model::ViewerRepo.name(),
                row.id,
                SyncAction::Update,
                &row,
            )
            .await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn model_names_roundtrip() {
        for m in Model::ALL {
            assert_eq!(Model::parse(m.name()), Some(m));
        }
        assert_eq!(Model::parse("repository"), None);
        assert!(Model::Comment.is_lazy() && !Model::Issue.is_lazy());
    }

    #[test]
    fn every_model_but_viewer_repo_has_sql() {
        for m in Model::ALL {
            let sql = select_sql(m, &Filter::Ids(&[1]), Opts::default());
            assert_eq!(sql.is_none(), m == Model::ViewerRepo, "{m}");
        }
        assert!(select_sql(Model::Label, &Filter::Orgs(&[1]), Opts::default()).is_none());
    }

    #[test]
    fn collects_referenced_users() {
        let mut out = BTreeSet::new();
        referenced_users(
            "issue",
            &json!({"authorId": 1, "assigneeIds": [2, 3], "mergedById": null, "requestedReviewerIds": [4]}),
            &mut out,
        );
        referenced_users(
            "issueEvent",
            &json!({"actorId": 5, "data": {"assigneeId": 6}}),
            &mut out,
        );
        referenced_users("user", &json!({"id": 9}), &mut out);
        assert_eq!(out.into_iter().collect::<Vec<_>>(), vec![1, 2, 3, 4, 5, 6]);
    }
}
