//! Issue, issue comment, label and milestone objects.

use std::collections::HashMap;

use bgh_core::AppState;
use bgh_core::models::{api, db};
use bgh_core::node_id::{self, NodeType};
use bgh_core::time::{Timestamp, ts};
use bgh_core::urls::Urls;
use serde_json::{Value, json};

use super::common::{self, RepoCtx, assoc_of, user_or_ghost};

/// Load an issue row of `ctx`'s repository.
pub async fn issue_row(
    state: &AppState,
    ctx: &RepoCtx,
    issue_id: i64,
) -> anyhow::Result<Option<db::Issue>> {
    Ok(sqlx::query_as::<_, db::Issue>(&format!(
        "SELECT {} FROM issues WHERE id = $1 AND repo_id = $2",
        db::Issue::COLUMNS
    ))
    .bind(issue_id)
    .bind(ctx.repo.id)
    .fetch_optional(&state.db)
    .await?)
}

/// Labels, assignees and milestone of an issue (one query each).
pub(crate) struct IssueParts {
    pub labels: Vec<db::Label>,
    pub assignee_ids: Vec<i64>,
    pub milestone: Option<db::Milestone>,
}

impl IssueParts {
    pub async fn load(state: &AppState, issue: &db::Issue) -> anyhow::Result<Self> {
        let labels: Vec<db::Label> = sqlx::query_as(&format!(
            "SELECT {} FROM labels l JOIN issue_labels il ON il.label_id = l.id
             WHERE il.issue_id = $1 ORDER BY lower(l.name), l.id",
            db::prefixed("l", db::Label::COLUMNS)
        ))
        .bind(issue.id)
        .fetch_all(&state.db)
        .await?;
        let assignee_ids: Vec<i64> = sqlx::query_scalar(
            "SELECT user_id FROM issue_assignees WHERE issue_id = $1 ORDER BY created_at, user_id",
        )
        .bind(issue.id)
        .fetch_all(&state.db)
        .await?;
        let milestone = match issue.milestone_id {
            Some(id) => milestone_row(state, id).await?,
            None => None,
        };
        Ok(Self {
            labels,
            assignee_ids,
            milestone,
        })
    }

    /// User ids referenced by these parts (for batch loading).
    pub fn user_ids(&self) -> impl Iterator<Item = Option<i64>> + '_ {
        self.assignee_ids
            .iter()
            .map(|id| Some(*id))
            .chain(std::iter::once(
                self.milestone.as_ref().and_then(|m| m.creator_id),
            ))
    }

    pub fn labels_json(&self, urls: &Urls, ctx: &RepoCtx) -> Value {
        Value::Array(
            self.labels
                .iter()
                .map(|l| label_json(urls, ctx, l))
                .collect(),
        )
    }

    /// `(assignee, assignees)`
    pub fn assignees_json(&self, urls: &Urls, users: &HashMap<i64, db::User>) -> (Value, Value) {
        let list: Vec<Value> = self
            .assignee_ids
            .iter()
            .filter_map(|id| users.get(id))
            .map(|u| common::user_json(urls, u))
            .collect();
        (
            list.first().cloned().unwrap_or(Value::Null),
            Value::Array(list),
        )
    }

    pub fn milestone_json(
        &self,
        urls: &Urls,
        ctx: &RepoCtx,
        users: &HashMap<i64, db::User>,
    ) -> Value {
        self.milestone
            .as_ref()
            .map(|m| milestone_json(urls, ctx, m, m.creator_id.and_then(|id| users.get(&id))))
            .unwrap_or(Value::Null)
    }
}

pub(crate) async fn milestone_row(
    state: &AppState,
    milestone_id: i64,
) -> anyhow::Result<Option<db::Milestone>> {
    Ok(sqlx::query_as::<_, db::Milestone>(&format!(
        "SELECT {} FROM milestones WHERE id = $1",
        db::Milestone::COLUMNS
    ))
    .bind(milestone_id)
    .fetch_optional(&state.db)
    .await?)
}

pub(crate) async fn label_row(
    state: &AppState,
    label_id: i64,
) -> anyhow::Result<Option<db::Label>> {
    Ok(sqlx::query_as::<_, db::Label>(&format!(
        "SELECT {} FROM labels WHERE id = $1",
        db::Label::COLUMNS
    ))
    .bind(label_id)
    .fetch_optional(&state.db)
    .await?)
}

pub fn label_json(urls: &Urls, ctx: &RepoCtx, l: &db::Label) -> Value {
    serde_json::to_value(api::Label::new(urls, ctx.owner_login(), ctx.name(), l))
        .unwrap_or(Value::Null)
}

pub fn milestone_json(
    urls: &Urls,
    ctx: &RepoCtx,
    m: &db::Milestone,
    creator: Option<&db::User>,
) -> Value {
    serde_json::to_value(api::Milestone::new(
        urls,
        ctx.owner_login(),
        ctx.name(),
        m,
        creator,
    ))
    .unwrap_or(Value::Null)
}

/// `label` object by id (`None` if deleted or in another repository).
pub async fn label(
    state: &AppState,
    ctx: &RepoCtx,
    label_id: i64,
) -> anyhow::Result<Option<Value>> {
    Ok(label_row(state, label_id)
        .await?
        .filter(|l| l.repo_id == ctx.repo.id)
        .map(|l| label_json(&state.urls, ctx, &l)))
}

/// `milestone` object by id (`None` if deleted or in another repository).
pub async fn milestone(
    state: &AppState,
    ctx: &RepoCtx,
    milestone_id: i64,
) -> anyhow::Result<Option<Value>> {
    let Some(m) = milestone_row(state, milestone_id)
        .await?
        .filter(|m| m.repo_id == ctx.repo.id)
    else {
        return Ok(None);
    };
    let creator = match m.creator_id {
        Some(id) => db::User::find(&state.db, id).await?,
        None => None,
    };
    Ok(Some(milestone_json(&state.urls, ctx, &m, creator.as_ref())))
}

/// Webhook `issue` object by id (with `pull_request` for PRs).
pub async fn issue(
    state: &AppState,
    ctx: &RepoCtx,
    issue_id: i64,
) -> anyhow::Result<Option<Value>> {
    match issue_row(state, ctx, issue_id).await? {
        Some(row) => Ok(Some(issue_json(state, ctx, &row).await?)),
        None => Ok(None),
    }
}

/// Webhook `issue` object for a loaded row.
pub async fn issue_json(
    state: &AppState,
    ctx: &RepoCtx,
    issue: &db::Issue,
) -> anyhow::Result<Value> {
    let urls = &state.urls;
    let parts = IssueParts::load(state, issue).await?;
    // (merged_at, draft) of the pull request half, for PRs only.
    let pull: Option<(Option<chrono::DateTime<chrono::Utc>>, bool)> = if issue.is_pull_request {
        Some(
            sqlx::query_as("SELECT merged_at, draft FROM pull_requests WHERE issue_id = $1")
                .bind(issue.id)
                .fetch_optional(&state.db)
                .await?
                .unwrap_or((None, false)),
        )
    } else {
        None
    };
    let api_url = urls.issue(ctx.owner_login(), ctx.name(), issue.number);
    let reactions =
        common::reactions(state, "issue", issue.id, format!("{api_url}/reactions")).await?;
    let users = common::users(
        state,
        parts.user_ids().chain(std::iter::once(issue.author_id)),
    )
    .await?;
    let assoc = match issue.author_id {
        Some(id) => common::author_associations(state, &ctx.repo, &[id]).await?,
        None => HashMap::new(),
    };
    let (assignee, assignees) = parts.assignees_json(urls, &users);
    let html_url = if issue.is_pull_request {
        urls.pull_html(ctx.owner_login(), ctx.name(), issue.number)
    } else {
        urls.issue_html(ctx.owner_login(), ctx.name(), issue.number)
    };
    let mut v = json!({
        "url": api_url,
        "repository_url": ctx.api_url(urls),
        "labels_url": format!("{api_url}/labels{{/name}}"),
        "comments_url": format!("{api_url}/comments"),
        "events_url": format!("{api_url}/events"),
        "html_url": html_url,
        "id": issue.id,
        "node_id": node_id::encode(NodeType::Issue, issue.id),
        "number": issue.number,
        "title": issue.title,
        "user": user_or_ghost(urls, &users, issue.author_id),
        "labels": parts.labels_json(urls, ctx),
        "state": issue.state,
        "locked": issue.locked,
        "assignee": assignee,
        "assignees": assignees,
        "milestone": parts.milestone_json(urls, ctx, &users),
        "comments": issue.comments_count,
        "created_at": Timestamp(issue.created_at),
        "updated_at": Timestamp(issue.updated_at),
        "closed_at": ts(issue.closed_at),
        "author_association": assoc_of(&assoc, issue.author_id),
        "active_lock_reason": issue.active_lock_reason,
        "body": issue.body,
        "reactions": reactions,
        "timeline_url": format!("{api_url}/timeline"),
        "performed_via_github_app": null,
        "state_reason": issue.state_reason,
    });
    if let Some((merged_at, draft)) = pull {
        let pr_html = urls.pull_html(ctx.owner_login(), ctx.name(), issue.number);
        v["draft"] = json!(draft);
        v["pull_request"] = json!({
            "url": urls.pull(ctx.owner_login(), ctx.name(), issue.number),
            "html_url": pr_html,
            "diff_url": format!("{pr_html}.diff"),
            "patch_url": format!("{pr_html}.patch"),
            "merged_at": ts(merged_at),
        });
    }
    Ok(v)
}

/// `comments` row of `ctx`'s repository.
pub(crate) async fn comment_row(
    state: &AppState,
    ctx: &RepoCtx,
    comment_id: i64,
) -> anyhow::Result<Option<db::Comment>> {
    Ok(sqlx::query_as::<_, db::Comment>(&format!(
        "SELECT {} FROM comments WHERE id = $1 AND repo_id = $2",
        db::Comment::COLUMNS
    ))
    .bind(comment_id)
    .bind(ctx.repo.id)
    .fetch_optional(&state.db)
    .await?)
}

/// `html_url` of an issue comment (PR conversation comments live under `/pull/`).
pub(crate) fn comment_html_url(urls: &Urls, ctx: &RepoCtx, issue: &db::Issue, id: i64) -> String {
    if issue.is_pull_request {
        format!(
            "{}#issuecomment-{id}",
            urls.pull_html(ctx.owner_login(), ctx.name(), issue.number)
        )
    } else {
        urls.issue_comment_html(ctx.owner_login(), ctx.name(), issue.number, id)
    }
}

/// Webhook issue `comment` object by id.
pub async fn issue_comment(
    state: &AppState,
    ctx: &RepoCtx,
    comment_id: i64,
) -> anyhow::Result<Option<Value>> {
    let Some(c) = comment_row(state, ctx, comment_id).await? else {
        return Ok(None);
    };
    let Some(issue) = issue_row(state, ctx, c.issue_id).await? else {
        return Ok(None);
    };
    Ok(Some(comment_json(state, ctx, &issue, &c).await?))
}

/// Webhook issue `comment` object for a loaded row.
pub async fn comment_json(
    state: &AppState,
    ctx: &RepoCtx,
    issue: &db::Issue,
    c: &db::Comment,
) -> anyhow::Result<Value> {
    let urls = &state.urls;
    let url = urls.issue_comment(ctx.owner_login(), ctx.name(), c.id);
    let users = common::users(state, [c.author_id]).await?;
    let assoc = match c.author_id {
        Some(id) => common::author_associations(state, &ctx.repo, &[id]).await?,
        None => HashMap::new(),
    };
    let reactions =
        common::reactions(state, "issue_comment", c.id, format!("{url}/reactions")).await?;
    Ok(json!({
        "url": url,
        "html_url": comment_html_url(urls, ctx, issue, c.id),
        "issue_url": urls.issue(ctx.owner_login(), ctx.name(), issue.number),
        "id": c.id,
        "node_id": node_id::encode(NodeType::IssueComment, c.id),
        "user": user_or_ghost(urls, &users, c.author_id),
        "created_at": Timestamp(c.created_at),
        "updated_at": Timestamp(c.updated_at),
        "author_association": assoc_of(&assoc, c.author_id),
        "body": c.body,
        "reactions": reactions,
        "performed_via_github_app": null,
    }))
}

/// Minimal comment object for a deleted comment (the row is gone).
pub fn deleted_comment_json(urls: &Urls, ctx: &RepoCtx, issue: &db::Issue, id: i64) -> Value {
    json!({
        "id": id,
        "node_id": node_id::encode(NodeType::IssueComment, id),
        "url": urls.issue_comment(ctx.owner_login(), ctx.name(), id),
        "html_url": comment_html_url(urls, ctx, issue, id),
        "issue_url": urls.issue(ctx.owner_login(), ctx.name(), issue.number),
    })
}

/// A repository-scoped delivery built with the standard envelope.
fn delivery(
    state: &AppState,
    ctx: &RepoCtx,
    event: &'static str,
    action: &str,
    entries: Vec<(&str, Value)>,
    sender: Value,
) -> super::HookEvent {
    super::HookEvent {
        event,
        action: Some(action.to_string()),
        repo_id: Some(ctx.repo.id),
        org_id: ctx.org_id(),
        payload: common::envelope(&state.urls, ctx, Some(action), entries, sender),
    }
}

/// `issues` `transferred`, delivered to the old repository: `issue` is the
/// issue as it was there, `changes` has `new_issue` and `new_repository`.
pub(super) async fn transferred(
    state: &AppState,
    repo_id: i64,
    issue_id: i64,
    old_repo_id: i64,
    old_number: i64,
    actor_id: i64,
) -> anyhow::Result<Vec<super::HookEvent>> {
    let (Some(new_ctx), Some(old_ctx)) = (
        RepoCtx::load(state, repo_id).await?,
        RepoCtx::load(state, old_repo_id).await?,
    ) else {
        return Ok(Vec::new());
    };
    let Some(row) = issue_row(state, &new_ctx, issue_id).await? else {
        return Ok(Vec::new());
    };
    let new_issue = issue_json(state, &new_ctx, &row).await?;
    let old_row = db::Issue {
        repo_id: old_repo_id,
        number: old_number,
        ..row
    };
    let old_issue = issue_json(state, &old_ctx, &old_row).await?;
    let sender = common::sender(state, Some(actor_id)).await?;
    Ok(vec![delivery(
        state,
        &old_ctx,
        "issues",
        "transferred",
        vec![
            ("issue", old_issue),
            (
                "changes",
                json!({
                    "new_issue": new_issue,
                    "new_repository": new_ctx.repository(&state.urls),
                }),
            ),
        ],
        sender,
    )])
}

/// `sub_issues`: `sub_issue_added`/`removed` to the parent's repository
/// and `parent_issue_added`/`removed` to the sub-issue's repository.
pub(super) async fn sub_issues(
    state: &AppState,
    parent_id: i64,
    sub_issue_id: i64,
    added: bool,
    actor_id: i64,
) -> anyhow::Result<Vec<super::HookEvent>> {
    let repos: Vec<(i64, i64)> =
        sqlx::query_as("SELECT id, repo_id FROM issues WHERE id = ANY($1)")
            .bind([parent_id, sub_issue_id])
            .fetch_all(&state.db)
            .await?;
    let repo_of = |id: i64| repos.iter().find(|r| r.0 == id).map(|r| r.1);
    let (Some(parent_repo), Some(sub_repo)) = (repo_of(parent_id), repo_of(sub_issue_id)) else {
        return Ok(Vec::new());
    };
    let Some(parent_ctx) = RepoCtx::load(state, parent_repo).await? else {
        return Ok(Vec::new());
    };
    let sub_ctx = if sub_repo == parent_repo {
        parent_ctx.clone()
    } else {
        match RepoCtx::load(state, sub_repo).await? {
            Some(c) => c,
            None => return Ok(Vec::new()),
        }
    };
    let (Some(parent), Some(sub)) = (
        issue_row(state, &parent_ctx, parent_id).await?,
        issue_row(state, &sub_ctx, sub_issue_id).await?,
    ) else {
        return Ok(Vec::new());
    };
    let parent_json = issue_json(state, &parent_ctx, &parent).await?;
    let sub_json = issue_json(state, &sub_ctx, &sub).await?;
    let sender = common::sender(state, Some(actor_id)).await?;
    let suffix = if added { "added" } else { "removed" };
    let urls = &state.urls;
    let to_parent = delivery(
        state,
        &parent_ctx,
        "sub_issues",
        &format!("sub_issue_{suffix}"),
        vec![
            ("sub_issue_id", json!(sub_issue_id)),
            ("sub_issue", sub_json.clone()),
            ("sub_issue_repo", sub_ctx.repository(urls)),
            ("parent_issue_id", json!(parent_id)),
            ("parent_issue", parent_json.clone()),
        ],
        sender.clone(),
    );
    let to_sub = delivery(
        state,
        &sub_ctx,
        "sub_issues",
        &format!("parent_issue_{suffix}"),
        vec![
            ("parent_issue_id", json!(parent_id)),
            ("parent_issue", parent_json),
            ("parent_issue_repo", parent_ctx.repository(urls)),
            ("sub_issue_id", json!(sub_issue_id)),
            ("sub_issue", sub_json),
        ],
        sender,
    );
    Ok(vec![to_parent, to_sub])
}
