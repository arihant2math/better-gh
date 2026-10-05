//! Pull request, review and review comment objects.

use std::collections::HashMap;

use bgh_core::AppState;
use bgh_core::models::{api, db};
use bgh_core::node_id::{self, NodeType};
use bgh_core::time::{Timestamp, ts};
use bgh_core::urls::Urls;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use super::common::{self, RepoCtx, assoc_of, href, user_or_ghost, user_or_null};
use super::issues::{IssueParts, issue_row};

/// Load the `pull_requests` row of a PR in `ctx`'s repository.
pub async fn pull_row(
    state: &AppState,
    ctx: &RepoCtx,
    pull_id: i64,
) -> anyhow::Result<Option<db::PullRequest>> {
    Ok(sqlx::query_as::<_, db::PullRequest>(&format!(
        "SELECT {} FROM pull_requests WHERE issue_id = $1 AND repo_id = $2",
        db::PullRequest::COLUMNS
    ))
    .bind(pull_id)
    .bind(ctx.repo.id)
    .fetch_optional(&state.db)
    .await?)
}

/// Webhook `pull_request` object by id (the PR's issue id).
pub async fn pull_request(
    state: &AppState,
    ctx: &RepoCtx,
    pull_id: i64,
) -> anyhow::Result<Option<Value>> {
    let Some(issue) = issue_row(state, ctx, pull_id).await? else {
        return Ok(None);
    };
    let Some(pr) = pull_row(state, ctx, pull_id).await? else {
        return Ok(None);
    };
    Ok(Some(pull_json(state, ctx, &issue, &pr).await?))
}

/// `{api}/repos/{o}/{r}/pulls/comments/{id}`
pub fn review_comment_url(urls: &Urls, ctx: &RepoCtx, id: i64) -> String {
    format!("{}/pulls/comments/{id}", ctx.api_url(urls))
}

/// `{base}/{o}/{r}/pull/{n}#discussion_r{id}`
pub fn review_comment_html_url(urls: &Urls, ctx: &RepoCtx, number: i64, id: i64) -> String {
    format!(
        "{}#discussion_r{id}",
        urls.pull_html(ctx.owner_login(), ctx.name(), number)
    )
}

/// `{base}/{o}/{r}/pull/{n}#pullrequestreview-{id}`
pub fn review_html_url(urls: &Urls, ctx: &RepoCtx, number: i64, id: i64) -> String {
    format!(
        "{}#pullrequestreview-{id}",
        urls.pull_html(ctx.owner_login(), ctx.name(), number)
    )
}

/// Webhook review `state`: lowercase (`approved`, `changes_requested`, ...).
pub fn webhook_review_state(state: &str) -> String {
    state.to_ascii_lowercase()
}

/// Webhook `pull_request` object for loaded rows.
pub async fn pull_json(
    state: &AppState,
    ctx: &RepoCtx,
    issue: &db::Issue,
    pr: &db::PullRequest,
) -> anyhow::Result<Value> {
    let urls = &state.urls;
    let (o, r) = (ctx.owner_login(), ctx.name());
    let parts = IssueParts::load(state, issue).await?;

    let requests: Vec<(Option<i64>, Option<i64>)> = sqlx::query_as(
        "SELECT user_id, team_id FROM pr_requested_reviewers WHERE pull_id = $1 ORDER BY id",
    )
    .bind(issue.id)
    .fetch_all(&state.db)
    .await?;
    let team_ids: Vec<i64> = requests.iter().filter_map(|(_, t)| *t).collect();
    let teams: Vec<db::Team> = if team_ids.is_empty() {
        Vec::new()
    } else {
        sqlx::query_as(&format!(
            "SELECT {} FROM teams WHERE id = ANY($1)",
            db::Team::COLUMNS
        ))
        .bind(&team_ids)
        .fetch_all(&state.db)
        .await?
    };

    let auto_merge_by = pr
        .auto_merge
        .as_ref()
        .and_then(|a| a.get("enabled_by_id"))
        .and_then(Value::as_i64);
    let users = common::users(
        state,
        parts
            .user_ids()
            .chain([issue.author_id, pr.merged_by_id, auto_merge_by])
            .chain(requests.iter().map(|(u, _)| *u)),
    )
    .await?;
    let assoc = match issue.author_id {
        Some(id) => common::author_associations(state, &ctx.repo, &[id]).await?,
        None => HashMap::new(),
    };

    // Head repository: same repo, a fork, or deleted.
    let head_ctx = match pr.head_repo_id {
        Some(id) if id == ctx.repo.id => Some(ctx.clone()),
        Some(id) => RepoCtx::load(state, id).await?,
        None => None,
    };
    let author = user_or_ghost(urls, &users, issue.author_id);
    let author_login = author["login"].as_str().unwrap_or("ghost").to_string();
    let head = branch_json(
        urls,
        head_ctx.as_ref(),
        &pr.head_ref,
        &pr.head_sha,
        &author,
        &author_login,
    );
    let base = branch_json(
        urls,
        Some(ctx),
        &pr.base_ref,
        &pr.base_sha,
        &author,
        &author_login,
    );

    let api_url = urls.pull(o, r, issue.number);
    let html_url = urls.pull_html(o, r, issue.number);
    let issue_url = urls.issue(o, r, issue.number);
    let statuses_url = format!("{}/statuses/{}", ctx.api_url(urls), pr.head_sha);
    let review_comment_url = format!("{}/pulls/comments{{/number}}", ctx.api_url(urls));
    let (assignee, assignees) = parts.assignees_json(urls, &users);
    let requested_reviewers: Vec<Value> = requests
        .iter()
        .filter_map(|(u, _)| u.and_then(|id| users.get(&id)))
        .map(|u| common::user_json(urls, u))
        .collect();
    let requested_teams: Vec<Value> = team_ids
        .iter()
        .filter_map(|id| teams.iter().find(|t| t.id == *id))
        .map(|t| team_json(urls, ctx, t))
        .collect();
    let auto_merge = pr.auto_merge.as_ref().map(|a| {
        json!({
            "enabled_by": user_or_null(urls, &users, auto_merge_by),
            "merge_method": a.get("merge_method").cloned().unwrap_or(json!("merge")),
            "commit_title": a.get("commit_title").cloned().unwrap_or(Value::Null),
            "commit_message": a.get("commit_message").cloned().unwrap_or(Value::Null),
        })
    });

    // Split in two `json!` calls to stay under the macro recursion limit.
    let mut v = json!({
        "url": api_url,
        "id": issue.id,
        "node_id": node_id::encode(NodeType::PullRequest, issue.id),
        "html_url": html_url,
        "diff_url": format!("{html_url}.diff"),
        "patch_url": format!("{html_url}.patch"),
        "issue_url": issue_url,
        "number": issue.number,
        "state": issue.state,
        "locked": issue.locked,
        "title": issue.title,
        "user": author,
        "body": issue.body,
        "created_at": Timestamp(issue.created_at),
        "updated_at": Timestamp(issue.updated_at),
        "closed_at": ts(issue.closed_at),
        "merged_at": ts(pr.merged_at),
        "merge_commit_sha": pr.merge_commit_sha,
        "assignee": assignee,
        "assignees": assignees,
        "requested_reviewers": requested_reviewers,
        "requested_teams": requested_teams,
        "labels": parts.labels_json(urls, ctx),
        "milestone": parts.milestone_json(urls, ctx, &users),
        "draft": pr.draft,
    });
    let rest = json!({
        "commits_url": format!("{api_url}/commits"),
        "review_comments_url": format!("{api_url}/comments"),
        "review_comment_url": review_comment_url,
        "comments_url": format!("{issue_url}/comments"),
        "statuses_url": statuses_url,
        "head": head,
        "base": base,
        "_links": {
            "self": href(&api_url),
            "html": href(&html_url),
            "issue": href(&issue_url),
            "comments": href(format!("{issue_url}/comments")),
            "review_comments": href(format!("{api_url}/comments")),
            "review_comment": href(review_comment_url),
            "commits": href(format!("{api_url}/commits")),
            "statuses": href(statuses_url),
        },
        "author_association": assoc_of(&assoc, issue.author_id),
        "auto_merge": auto_merge,
        "active_lock_reason": issue.active_lock_reason,
        "merged": pr.merged,
        "mergeable": pr.mergeable,
        "rebaseable": pr.rebaseable,
        "mergeable_state": pr.mergeable_state,
        "merged_by": user_or_null(urls, &users, pr.merged_by_id),
        "comments": issue.comments_count,
        "review_comments": pr.review_comments_count,
        "maintainer_can_modify": pr.maintainer_can_modify,
        "commits": pr.commits,
        "additions": pr.additions,
        "deletions": pr.deletions,
        "changed_files": pr.changed_files,
    });
    if let (Some(m), Value::Object(rest)) = (v.as_object_mut(), rest) {
        m.extend(rest);
    }
    Ok(v)
}

/// `head` / `base` object.
fn branch_json(
    urls: &Urls,
    ctx: Option<&RepoCtx>,
    git_ref: &str,
    sha: &str,
    fallback_user: &Value,
    fallback_login: &str,
) -> Value {
    match ctx {
        Some(c) => json!({
            "label": format!("{}:{git_ref}", c.owner_login()),
            "ref": git_ref,
            "sha": sha,
            "user": common::user_json(urls, &c.owner),
            "repo": c.repository(urls),
        }),
        None => json!({
            "label": format!("{fallback_login}:{git_ref}"),
            "ref": git_ref,
            "sha": sha,
            "user": fallback_user,
            "repo": null,
        }),
    }
}

/// team-simple JSON (teams belong to the repository's owning org).
pub fn team_json(urls: &Urls, ctx: &RepoCtx, t: &db::Team) -> Value {
    serde_json::to_value(api::TeamSimple::new(urls, ctx.owner_login(), t)).unwrap_or(Value::Null)
}

/// team-simple JSON by id.
pub async fn team(state: &AppState, ctx: &RepoCtx, team_id: i64) -> anyhow::Result<Option<Value>> {
    let t: Option<db::Team> = sqlx::query_as(&format!(
        "SELECT {} FROM teams WHERE id = $1",
        db::Team::COLUMNS
    ))
    .bind(team_id)
    .fetch_optional(&state.db)
    .await?;
    Ok(t.map(|t| team_json(&state.urls, ctx, &t)))
}

// ---------------------------------------------------------------------------
// Reviews
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct ReviewRow {
    pub id: i64,
    pub pull_id: i64,
    pub user_id: Option<i64>,
    pub body: String,
    pub state: String,
    pub commit_id: Option<String>,
    pub submitted_at: Option<DateTime<Utc>>,
}

/// Webhook `review` object by id.
pub async fn review(
    state: &AppState,
    ctx: &RepoCtx,
    review_id: i64,
) -> anyhow::Result<Option<Value>> {
    let row: Option<ReviewRow> = sqlx::query_as(
        "SELECT id, pull_id, user_id, body, state, commit_id, submitted_at
         FROM pr_reviews WHERE id = $1 AND repo_id = $2",
    )
    .bind(review_id)
    .bind(ctx.repo.id)
    .fetch_optional(&state.db)
    .await?;
    let Some(row) = row else { return Ok(None) };
    let Some(issue) = issue_row(state, ctx, row.pull_id).await? else {
        return Ok(None);
    };
    let urls = &state.urls;
    let users = common::users(state, [row.user_id]).await?;
    let assoc = match row.user_id {
        Some(id) => common::author_associations(state, &ctx.repo, &[id]).await?,
        None => HashMap::new(),
    };
    let html_url = review_html_url(urls, ctx, issue.number, row.id);
    let pull_url = urls.pull(ctx.owner_login(), ctx.name(), issue.number);
    Ok(Some(json!({
        "id": row.id,
        "node_id": node_id::encode(NodeType::PullRequestReview, row.id),
        "user": user_or_ghost(urls, &users, row.user_id),
        "body": (!row.body.is_empty()).then_some(&row.body),
        "commit_id": row.commit_id,
        "submitted_at": ts(row.submitted_at),
        "state": webhook_review_state(&row.state),
        "html_url": html_url,
        "pull_request_url": pull_url,
        "author_association": assoc_of(&assoc, row.user_id),
        "_links": {
            "html": href(&html_url),
            "pull_request": href(&pull_url),
        },
    })))
}

// ---------------------------------------------------------------------------
// Review comments
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct ReviewCommentRow {
    pub id: i64,
    pub pull_id: i64,
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
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Webhook pull request review `comment` object by id.
pub async fn review_comment(
    state: &AppState,
    ctx: &RepoCtx,
    comment_id: i64,
) -> anyhow::Result<Option<Value>> {
    let row: Option<ReviewCommentRow> = sqlx::query_as(
        "SELECT id, pull_id, review_id, in_reply_to_id, user_id, body, path, commit_id,
                original_commit_id, diff_hunk, subject_type, side, start_side, line,
                original_line, start_line, original_start_line, position, original_position,
                created_at, updated_at
         FROM pr_review_comments WHERE id = $1 AND repo_id = $2",
    )
    .bind(comment_id)
    .bind(ctx.repo.id)
    .fetch_optional(&state.db)
    .await?;
    let Some(c) = row else { return Ok(None) };
    let Some(issue) = issue_row(state, ctx, c.pull_id).await? else {
        return Ok(None);
    };
    let urls = &state.urls;
    let users = common::users(state, [c.user_id]).await?;
    let assoc = match c.user_id {
        Some(id) => common::author_associations(state, &ctx.repo, &[id]).await?,
        None => HashMap::new(),
    };
    let url = review_comment_url(urls, ctx, c.id);
    let html_url = review_comment_html_url(urls, ctx, issue.number, c.id);
    let pull_url = urls.pull(ctx.owner_login(), ctx.name(), issue.number);
    let reactions = common::reactions(
        state,
        "pull_request_review_comment",
        c.id,
        format!("{url}/reactions"),
    )
    .await?;
    let mut v = json!({
        "url": url,
        "pull_request_review_id": c.review_id,
        "id": c.id,
        "node_id": node_id::encode(NodeType::PullRequestReviewComment, c.id),
        "diff_hunk": c.diff_hunk,
        "path": c.path,
        "position": c.position,
        "original_position": c.original_position,
        "commit_id": c.commit_id,
        "original_commit_id": c.original_commit_id,
        "user": user_or_ghost(urls, &users, c.user_id),
        "body": c.body,
        "created_at": Timestamp(c.created_at),
        "updated_at": Timestamp(c.updated_at),
        "html_url": html_url,
        "pull_request_url": pull_url,
        "author_association": assoc_of(&assoc, c.user_id),
        "_links": {
            "self": href(&url),
            "html": href(&html_url),
            "pull_request": href(&pull_url),
        },
        "reactions": reactions,
        "start_line": c.start_line,
        "original_start_line": c.original_start_line,
        "start_side": c.start_side,
        "line": c.line,
        "original_line": c.original_line,
        "side": c.side,
        "subject_type": c.subject_type,
        "performed_via_github_app": null,
    });
    if let Some(reply) = c.in_reply_to_id {
        v["in_reply_to_id"] = json!(reply);
    }
    Ok(Some(v))
}

/// Minimal review comment object for a deleted comment (the row is gone).
pub fn deleted_review_comment_json(urls: &Urls, ctx: &RepoCtx, number: i64, id: i64) -> Value {
    json!({
        "id": id,
        "node_id": node_id::encode(NodeType::PullRequestReviewComment, id),
        "url": review_comment_url(urls, ctx, id),
        "html_url": review_comment_html_url(urls, ctx, number, id),
        "pull_request_url": urls.pull(ctx.owner_login(), ctx.name(), number),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_states_are_lowercase() {
        assert_eq!(webhook_review_state("APPROVED"), "approved");
        assert_eq!(
            webhook_review_state("CHANGES_REQUESTED"),
            "changes_requested"
        );
        assert_eq!(webhook_review_state("COMMENTED"), "commented");
        assert_eq!(webhook_review_state("DISMISSED"), "dismissed");
    }
}
