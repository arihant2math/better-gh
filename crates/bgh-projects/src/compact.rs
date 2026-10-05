//! Compact sync shapes (SYNC_PROTOCOL.md §3) of core models referenced by
//! project snapshots: users, repositories, issues, labels, milestones.

use std::collections::{HashMap, HashSet};

use bgh_core::perms::{self, Permission};
use bgh_core::prelude::*;
use bgh_core::time::ts;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};

pub fn user_json(state: &AppState, u: &db::User) -> Value {
    json!({
        "id": u.id,
        "login": u.login,
        "name": u.name,
        "avatarUrl": state.urls.avatar(u.id, u.avatar_url.as_deref()),
        "type": if u.kind == "Bot" { "Bot" } else { "User" },
    })
}

/// Users by id as compact rows (organizations are skipped).
pub async fn users_json(
    state: &AppState,
    ids: impl IntoIterator<Item = Option<i64>>,
) -> ApiResult<Vec<Value>> {
    let map = bgh_core::views::users_by_id(state, ids).await?;
    let mut users: Vec<&db::User> = map.values().filter(|u| !u.is_org()).collect();
    users.sort_by_key(|u| u.id);
    Ok(users.into_iter().map(|u| user_json(state, u)).collect())
}

pub fn repo_json(r: &db::Repository, owner_login: &str, open_pulls: i64) -> Value {
    json!({
        "id": r.id,
        "ownerId": r.owner_id,
        "owner": owner_login,
        "name": r.name,
        "description": r.description,
        "private": r.is_private(),
        "fork": r.fork,
        "archived": r.archived,
        "defaultBranch": r.default_branch,
        "language": r.language,
        "topics": r.topics,
        "stars": r.stargazers_count,
        "forks": r.forks_count,
        "watchers": r.watchers_count,
        "openIssues": r.open_issues_count,
        "openPulls": open_pulls,
        "hasIssues": r.has_issues,
        "hasProjects": r.has_projects,
        "hasWiki": r.has_wiki,
        "pushedAt": ts(r.pushed_at),
        "createdAt": Timestamp(r.created_at),
        "updatedAt": Timestamp(r.updated_at),
    })
}

pub fn label_json(l: &db::Label) -> Value {
    json!({
        "id": l.id,
        "repoId": l.repo_id,
        "name": l.name,
        "color": l.color,
        "description": l.description,
    })
}

pub fn milestone_json(m: &db::Milestone) -> Value {
    json!({
        "id": m.id,
        "repoId": m.repo_id,
        "number": m.number,
        "title": m.title,
        "description": m.description,
        "state": m.state,
        "dueOn": ts(m.due_on),
        "openIssues": m.open_issues,
        "closedIssues": m.closed_issues,
        "createdAt": Timestamp(m.created_at),
        "updatedAt": Timestamp(m.updated_at),
        "closedAt": ts(m.closed_at),
    })
}

/// An issue with its assignees, labels and PR state, as the compact `Issue`
/// row (without `body`).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct IssueRow {
    pub id: i64,
    pub repo_id: i64,
    pub number: i64,
    pub title: String,
    pub state: String,
    pub state_reason: Option<String>,
    pub author_id: Option<i64>,
    pub is_pull_request: bool,
    pub milestone_id: Option<i64>,
    pub locked: bool,
    pub comments_count: i64,
    pub closed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub assignee_ids: Vec<i64>,
    pub label_ids: Vec<i64>,
    pub draft: Option<bool>,
    pub merged: Option<bool>,
    pub merged_at: Option<DateTime<Utc>>,
    pub merged_by_id: Option<i64>,
    pub head_ref: Option<String>,
    pub base_ref: Option<String>,
}

impl IssueRow {
    pub const SELECT: &'static str = "SELECT i.id, i.repo_id, i.number, i.title, i.state, \
        i.state_reason, i.author_id, i.is_pull_request, i.milestone_id, i.locked, \
        i.comments_count, i.closed_at, i.created_at, i.updated_at, \
        ARRAY(SELECT a.user_id FROM issue_assignees a WHERE a.issue_id = i.id ORDER BY a.user_id) AS assignee_ids, \
        ARRAY(SELECT l.label_id FROM issue_labels l WHERE l.issue_id = i.id ORDER BY l.label_id) AS label_ids, \
        pr.draft, pr.merged, pr.merged_at, pr.merged_by_id, pr.head_ref, pr.base_ref \
        FROM issues i LEFT JOIN pull_requests pr ON pr.issue_id = i.id";

    pub fn json(&self) -> Value {
        let mut v = json!({
            "id": self.id,
            "repoId": self.repo_id,
            "number": self.number,
            "title": self.title,
            "state": self.state,
            "stateReason": self.state_reason.as_deref().filter(|r| *r != "duplicate"),
            "authorId": self.author_id,
            "assigneeIds": self.assignee_ids,
            "labelIds": self.label_ids,
            "milestoneId": self.milestone_id,
            "comments": self.comments_count,
            "locked": self.locked,
            "createdAt": Timestamp(self.created_at),
            "updatedAt": Timestamp(self.updated_at),
            "closedAt": ts(self.closed_at),
            "isPr": self.is_pull_request,
        });
        if self.is_pull_request {
            let o = v.as_object_mut().expect("object");
            o.insert("draft".into(), json!(self.draft.unwrap_or(false)));
            o.insert("merged".into(), json!(self.merged.unwrap_or(false)));
            o.insert("mergedAt".into(), json!(ts(self.merged_at)));
            o.insert("mergedById".into(), json!(self.merged_by_id));
            o.insert("headRef".into(), json!(self.head_ref));
            o.insert("baseRef".into(), json!(self.base_ref));
        }
        v
    }
}

/// Core rows referenced by project items, restricted to repositories the
/// caller can read.
#[derive(Debug, Default)]
pub struct Refs {
    pub issues: Vec<Value>,
    pub repos: Vec<Value>,
    pub labels: Vec<Value>,
    pub milestones: Vec<Value>,
    pub user_ids: Vec<i64>,
}

/// Repositories readable by `auth` among `repo_ids`.
pub async fn readable_repos(
    state: &AppState,
    auth: Option<&AuthContext>,
    repo_ids: &[i64],
) -> ApiResult<Vec<db::Repository>> {
    if repo_ids.is_empty() {
        return Ok(vec![]);
    }
    let repos: Vec<db::Repository> = sqlx::query_as(&format!(
        "SELECT {} FROM repositories WHERE id = ANY($1) ORDER BY id",
        db::Repository::COLUMNS
    ))
    .bind(repo_ids)
    .fetch_all(&state.db)
    .await?;
    let raw = perms::repo_permissions(&state.db, auth.map(|a| a.user.id), &repos).await?;
    Ok(repos
        .into_iter()
        .filter(|r| {
            let p = raw.get(&r.id).copied().unwrap_or(Permission::None);
            perms::effective(auth, r, p) >= Permission::Read
        })
        .collect())
}

pub async fn load_refs(
    state: &AppState,
    auth: Option<&AuthContext>,
    issue_ids: &[i64],
) -> ApiResult<Refs> {
    if issue_ids.is_empty() {
        return Ok(Refs::default());
    }
    let issues: Vec<IssueRow> = sqlx::query_as(&format!(
        "{} WHERE i.id = ANY($1) ORDER BY i.id",
        IssueRow::SELECT
    ))
    .bind(issue_ids)
    .fetch_all(&state.db)
    .await?;
    let mut repo_ids: Vec<i64> = issues.iter().map(|i| i.repo_id).collect();
    repo_ids.sort_unstable();
    repo_ids.dedup();
    let repos = readable_repos(state, auth, &repo_ids).await?;
    let readable: HashSet<i64> = repos.iter().map(|r| r.id).collect();
    let ids: Vec<i64> = readable.iter().copied().collect();

    let owners =
        bgh_core::views::users_by_id(state, repos.iter().map(|r| Some(r.owner_id))).await?;
    let open_pulls: HashMap<i64, i64> = sqlx::query_as::<_, (i64, i64)>(
        "SELECT repo_id, count(*) FROM issues
          WHERE repo_id = ANY($1) AND is_pull_request AND state = 'open' GROUP BY repo_id",
    )
    .bind(&ids)
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .collect();
    let labels: Vec<db::Label> = sqlx::query_as(&format!(
        "SELECT {} FROM labels WHERE repo_id = ANY($1) ORDER BY repo_id, lower(name), id",
        db::Label::COLUMNS
    ))
    .bind(&ids)
    .fetch_all(&state.db)
    .await?;
    let milestones: Vec<db::Milestone> = sqlx::query_as(&format!(
        "SELECT {} FROM milestones WHERE repo_id = ANY($1) ORDER BY repo_id, number",
        db::Milestone::COLUMNS
    ))
    .bind(&ids)
    .fetch_all(&state.db)
    .await?;

    let mut out = Refs::default();
    for i in issues.iter().filter(|i| readable.contains(&i.repo_id)) {
        out.user_ids.extend(i.author_id);
        out.user_ids.extend(&i.assignee_ids);
        out.issues.push(i.json());
    }
    for r in &repos {
        let login = owners
            .get(&r.owner_id)
            .map(|o| o.login.as_str())
            .unwrap_or("");
        out.repos.push(repo_json(
            r,
            login,
            open_pulls.get(&r.id).copied().unwrap_or(0),
        ));
    }
    out.labels = labels.iter().map(label_json).collect();
    out.milestones = milestones.iter().map(milestone_json).collect();
    Ok(out)
}
