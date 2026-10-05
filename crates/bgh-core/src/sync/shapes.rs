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
    // Extension models (docs/SYNC_PROTOCOL.md §3.2): streamed as deltas in
    // `repo:{id}`, not part of the bootstrap.
    ReviewComment,
    CheckRun,
    CheckSuite,
    CommitStatus,
}

impl Model {
    pub const ALL: [Model; 17] = [
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
        Model::ReviewComment,
        Model::CheckRun,
        Model::CheckSuite,
        Model::CommitStatus,
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
            Self::ReviewComment => "reviewComment",
            Self::CheckRun => "checkRun",
            Self::CheckSuite => "checkSuite",
            Self::CommitStatus => "commitStatus",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.name() == name)
    }

    /// Lazy models are not part of the bootstrap (loaded by partial sync).
    pub fn is_lazy(self) -> bool {
        matches!(self, Self::Comment | Self::Review | Self::IssueEvent)
    }

    /// Extension models: deltas only (no bootstrap or partial sync).
    pub fn is_extension(self) -> bool {
        matches!(
            self,
            Self::ReviewComment | Self::CheckRun | Self::CheckSuite | Self::CommitStatus
        )
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

/// Issue columns (aliases are the JSON keys; `row_to_json` is about twice
/// as fast as `json_build_object` for wide rows). `i` = issue row, `aa`,
/// `la`, `ra` = pre-aggregated assignees, labels, reactions; `si` = parent
/// (sub-issues), `pin` = pinned.
const ISSUE_COLS: &str = r#"
    i.id AS "id", i.repo_id AS "repoId", i.number AS "number", i.title AS "title",
    i.state AS "state",
    CASE i.state_reason WHEN 'duplicate' THEN 'not_planned' ELSE i.state_reason END AS "stateReason",
    i.author_id AS "authorId", coalesce(aa.ids, '{}') AS "assigneeIds",
    coalesce(la.ids, '{}') AS "labelIds", i.milestone_id AS "milestoneId",
    i.comments_count AS "comments", i.locked AS "locked",
    i.active_lock_reason AS "activeLockReason", coalesce(ra.r, '{}') AS "reactions",
    si.parent_id AS "parentId", coalesce(sc.ids, '{}') AS "subIssueIds",
    (pin.issue_id IS NOT NULL) AS "pinned",
    bgh_ts(i.created_at) AS "createdAt", bgh_ts(i.updated_at) AS "updatedAt",
    bgh_ts(i.closed_at) AS "closedAt", i.is_pull_request AS "isPr""#;

/// Pull request columns (`p` = pull_requests, `rq` = requested reviewers,
/// `rd` = review decision, `ck` = combined checks). The last five are
/// protocol extensions (§3.2).
const PR_COLS: &str = r#"
    p.draft AS "draft", p.merged AS "merged", bgh_ts(p.merged_at) AS "mergedAt",
    p.merged_by_id AS "mergedById", p.head_ref AS "headRef", p.head_repo_id AS "headRepoId",
    p.head_sha AS "headSha", p.base_ref AS "baseRef", p.base_sha AS "baseSha",
    p.mergeable AS "mergeable",
    CASE p.mergeable_state WHEN 'has_hooks' THEN 'clean' WHEN 'draft' THEN 'blocked'
                           ELSE p.mergeable_state END AS "mergeableState",
    coalesce(rd.decision, CASE WHEN rq.pull_id IS NOT NULL THEN 'review_required' END) AS "reviewDecision",
    coalesce(rq.users, '{}') AS "requestedReviewerIds",
    coalesce(rq.teams, '{}') AS "requestedTeamIds",
    ck.checks AS "checks",
    p.additions AS "additions", p.deletions AS "deletions",
    p.changed_files AS "changedFiles", p.commits AS "commits",
    p.merge_commit_sha AS "mergeCommitSha", p.rebaseable AS "rebaseable",
    p.maintainer_can_modify AS "maintainerCanModify",
    CASE WHEN p.auto_merge IS NOT NULL THEN json_build_object(
        'enabledById', p.auto_merge->'enabled_by_id',
        'mergeMethod', p.auto_merge->'merge_method') END AS "autoMerge",
    p.review_comments_count AS "reviewComments""#;

/// Issue/PR select. `fi` / `fx` are the filter predicate on the issue
/// aliases `i` / `x`. Child rows are aggregated once per set (hash joins),
/// the issue JSON and the PR JSON are built with `row_to_json` and spliced
/// (`{..issue..,..pr..}`) for pull requests.
fn issue_sql(fi: &str, fx: &str, body: bool) -> String {
    let body = if body { r#", i.body AS "body""# } else { "" };
    format!(
        r#"SELECT 'repo:' || i.repo_id AS scope, i.id,
       CASE WHEN p.issue_id IS NULL THEN b.j ELSE left(b.j, -1) || ',' || substr(pj.j, 2) END AS j
  FROM issues i
  LEFT JOIN (SELECT a.issue_id, array_agg(a.user_id ORDER BY a.created_at, a.user_id) AS ids
               FROM issue_assignees a JOIN issues x ON x.id = a.issue_id
              WHERE {fx} GROUP BY a.issue_id) aa ON aa.issue_id = i.id
  LEFT JOIN (SELECT l.issue_id, array_agg(l.label_id ORDER BY l.label_id) AS ids
               FROM issue_labels l JOIN issues x ON x.id = l.issue_id
              WHERE {fx} GROUP BY l.issue_id) la ON la.issue_id = i.id
  LEFT JOIN (SELECT z.subject_id, json_object_agg(z.content, z.n) AS r
               FROM (SELECT e.subject_id, e.content, count(*) AS n
                       FROM reactions e JOIN issues x ON x.id = e.subject_id
                      WHERE e.subject_type = 'issue' AND {fx}
                      GROUP BY e.subject_id, e.content) z
              GROUP BY z.subject_id) ra ON ra.subject_id = i.id
  LEFT JOIN sub_issues si ON si.child_id = i.id
  LEFT JOIN (SELECT s.parent_id, array_agg(s.child_id ORDER BY s.position, s.child_id) AS ids
               FROM sub_issues s JOIN issues x ON x.id = s.parent_id
              WHERE {fx} GROUP BY s.parent_id) sc ON sc.parent_id = i.id
  LEFT JOIN pinned_issues pin ON pin.issue_id = i.id
  LEFT JOIN pull_requests p ON p.issue_id = i.id
  LEFT JOIN (SELECT q.pull_id,
                    array_agg(q.user_id ORDER BY q.id) FILTER (WHERE q.user_id IS NOT NULL) AS users,
                    array_agg(q.team_id ORDER BY q.id) FILTER (WHERE q.team_id IS NOT NULL) AS teams
               FROM pr_requested_reviewers q JOIN issues x ON x.id = q.pull_id
              WHERE {fx} GROUP BY q.pull_id) rq ON rq.pull_id = i.id
  LEFT JOIN (SELECT z.pull_id,
                    CASE WHEN bool_or(z.state = 'CHANGES_REQUESTED') THEN 'changes_requested'
                         WHEN bool_or(z.state = 'APPROVED') THEN 'approved' END AS decision
               FROM (SELECT DISTINCT ON (v.pull_id, v.user_id) v.pull_id, v.state
                       FROM pr_reviews v JOIN issues x ON x.id = v.pull_id
                      WHERE {fx} AND v.state IN ('APPROVED', 'CHANGES_REQUESTED', 'DISMISSED')
                      ORDER BY v.pull_id, v.user_id, v.submitted_at DESC NULLS LAST, v.id DESC) z
              GROUP BY z.pull_id) rd ON rd.pull_id = i.id
  LEFT JOIN (SELECT c.repo_id, c.sha,
                    CASE WHEN bool_or(c.s NOT IN ('success', 'neutral', 'skipped', 'pending')) THEN 'failure'
                         WHEN bool_or(c.s = 'pending') THEN 'pending'
                         WHEN bool_and(c.s IN ('neutral', 'skipped')) THEN 'neutral'
                         ELSE 'success' END AS checks
               FROM ((SELECT DISTINCT ON (cr.repo_id, cr.head_sha, cr.name)
                             cr.repo_id, cr.head_sha AS sha,
                             CASE WHEN cr.status <> 'completed' THEN 'pending'
                                  ELSE coalesce(cr.conclusion, 'neutral') END AS s
                        FROM check_runs cr
                        JOIN pull_requests px ON px.repo_id = cr.repo_id AND px.head_sha = cr.head_sha
                        JOIN issues x ON x.id = px.issue_id
                       WHERE {fx}
                       ORDER BY cr.repo_id, cr.head_sha, cr.name, cr.id DESC)
                     UNION ALL
                     (SELECT DISTINCT ON (cs.repo_id, cs.sha, cs.context) cs.repo_id, cs.sha, cs.state
                        FROM commit_statuses cs
                        JOIN pull_requests px ON px.repo_id = cs.repo_id AND px.head_sha = cs.sha
                        JOIN issues x ON x.id = px.issue_id
                       WHERE {fx}
                       ORDER BY cs.repo_id, cs.sha, cs.context, cs.id DESC)) c
              GROUP BY c.repo_id, c.sha) ck ON ck.repo_id = i.repo_id AND ck.sha = p.head_sha
  CROSS JOIN LATERAL (SELECT row_to_json(ij)::text AS j FROM (SELECT {ISSUE_COLS}{body}) ij) b
  LEFT JOIN LATERAL (SELECT row_to_json(pr)::text AS j FROM (SELECT {PR_COLS}) pr
                      WHERE p.issue_id IS NOT NULL) pj ON true
 WHERE {fi}"#
    )
}

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
                 'mirrorUrl', r.mirror_url,
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
            let map = [("ids", "id"), ("repos", "repo_id")];
            issue_sql(
                &col("i", filter, &map)?,
                &col("x", filter, &map)?,
                opts.issue_body,
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
                     'lockReason', e.data->'lock_reason',
                     'sourceIssueId', e.data->'source_issue_id',
                     'sourceCommentId', e.data->'source_comment_id',
                     'sourceNumber', e.data->'source_number',
                     'sourceRepository', e.data->'source_repository',
                     'sourceIsPr', e.data->'source_is_pull_request',
                     'subIssueId', coalesce(e.data->'sub_issue'->'id', e.data->'sub_issue_id'),
                     'subIssueNumber', e.data->'sub_issue'->'number',
                     'subIssueRepository', e.data->'sub_issue'->'repository',
                     'parentIssueId', coalesce(e.data->'parent_issue'->'id', e.data->'parent_issue_id'),
                     'parentIssueNumber', e.data->'parent_issue'->'number',
                     'parentIssueRepository', e.data->'parent_issue'->'repository',
                     'fromRepository', e.data->'from_repository',
                     'teamId', e.data->'requested_team_id',
                     'before', e.data->'before',
                     'after', e.data->'after',
                     'ref', e.data->'ref',
                     'reviewId', e.data->'dismissed_review'->'review_id',
                     'dismissalMessage', e.data->'dismissed_review'->'dismissal_message',
                     'mergeMethod', e.data->'merge_method',
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
        Model::ReviewComment => format!(
            "SELECT 'repo:' || c.repo_id AS scope, c.id, json_build_object(
                 'id', c.id, 'repoId', c.repo_id, 'issueId', c.pull_id, 'reviewId', c.review_id,
                 'inReplyToId', c.in_reply_to_id, 'authorId', c.user_id, 'body', c.body,
                 'path', c.path, 'commitId', c.commit_id, 'originalCommitId', c.original_commit_id,
                 'subjectType', c.subject_type, 'side', c.side, 'startSide', c.start_side,
                 'line', c.line, 'originalLine', c.original_line, 'startLine', c.start_line,
                 'originalStartLine', c.original_start_line, 'position', c.position,
                 'originalPosition', c.original_position, 'diffHunk', c.diff_hunk,
                 'outdated', (c.subject_type = 'line' AND c.position IS NULL),
                 'resolvedAt', bgh_ts(c.resolved_at), 'resolvedById', c.resolved_by_id,
                 'reactions', coalesce((SELECT json_object_agg(x.content, x.n) FROM
                                          (SELECT content, count(*) AS n FROM reactions
                                            WHERE subject_type = 'pull_request_review_comment'
                                              AND subject_id = c.id
                                            GROUP BY content) x), '{{}}'),
                 'createdAt', bgh_ts(c.created_at), 'updatedAt', bgh_ts(c.updated_at))::text AS j
               FROM pr_review_comments c
              WHERE {} AND NOT EXISTS (SELECT 1 FROM pr_reviews v
                                        WHERE v.id = c.review_id AND v.state = 'PENDING'
                                          AND v.user_id IS DISTINCT FROM $2)",
            col(
                "c",
                filter,
                &[("ids", "id"), ("repos", "repo_id"), ("issues", "pull_id")]
            )?
        ),
        Model::CheckRun => format!(
            "SELECT 'repo:' || r.repo_id AS scope, r.id, json_build_object(
                 'id', r.id, 'repoId', r.repo_id, 'checkSuiteId', r.check_suite_id,
                 'headSha', r.head_sha, 'name', r.name, 'status', r.status,
                 'conclusion', r.conclusion, 'detailsUrl', r.details_url,
                 'title', r.output->'title', 'startedAt', bgh_ts(r.started_at),
                 'completedAt', bgh_ts(r.completed_at))::text AS j
               FROM check_runs r WHERE {}",
            col("r", filter, &[("ids", "id"), ("repos", "repo_id")])?
        ),
        Model::CheckSuite => format!(
            "SELECT 'repo:' || s.repo_id AS scope, s.id, json_build_object(
                 'id', s.id, 'repoId', s.repo_id, 'headSha', s.head_sha,
                 'headBranch', s.head_branch, 'appSlug', s.app_slug, 'status', s.status,
                 'conclusion', s.conclusion,
                 'latestCheckRunsCount', s.latest_check_runs_count)::text AS j
               FROM check_suites s WHERE {}",
            col("s", filter, &[("ids", "id"), ("repos", "repo_id")])?
        ),
        Model::CommitStatus => format!(
            "SELECT 'repo:' || cs.repo_id AS scope, cs.id, json_build_object(
                 'id', cs.id, 'repoId', cs.repo_id, 'sha', cs.sha, 'state', cs.state,
                 'context', cs.context, 'description', cs.description,
                 'targetUrl', cs.target_url, 'creatorId', cs.creator_id,
                 'createdAt', bgh_ts(cs.created_at))::text AS j
               FROM commit_statuses cs WHERE {}",
            col("cs", filter, &[("ids", "id"), ("repos", "repo_id")])?
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
        "SELECT coalesce(string_agg(s.j, ','), ''), count(*) FROM ({sql}) s"
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
    for key in [
        "authorId",
        "userId",
        "actorId",
        "mergedById",
        "creatorId",
        "resolvedById",
    ] {
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
