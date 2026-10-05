//! GHES `/enterprise/stats/{type}` and `/enterprise/settings/license`.

use std::sync::Arc;

use axum::extract::State;
use bgh_core::prelude::*;
use serde::Serialize;
use serde_json::{Value, json};
use sqlx::FromRow;

/// Key of the push counter in `site_counters`.
pub const PUSHES: &str = "total_pushes";

#[derive(Debug, Serialize, FromRow)]
pub struct RepoStats {
    pub total_repos: i64,
    pub root_repos: i64,
    pub fork_repos: i64,
    pub org_repos: i64,
    pub total_pushes: i64,
    pub total_wikis: i64,
}

#[derive(Debug, Serialize, FromRow)]
pub struct HookStats {
    pub total_hooks: i64,
    pub active_hooks: i64,
    pub inactive_hooks: i64,
}

#[derive(Debug, Serialize, FromRow)]
pub struct PageStats {
    pub total_pages: i64,
}

#[derive(Debug, Serialize, FromRow)]
pub struct OrgStats {
    pub total_orgs: i64,
    pub disabled_orgs: i64,
    pub total_teams: i64,
    pub total_team_members: i64,
}

#[derive(Debug, Serialize, FromRow)]
pub struct UserStats {
    pub total_users: i64,
    pub admin_users: i64,
    pub suspended_users: i64,
}

#[derive(Debug, Serialize, FromRow)]
pub struct PullStats {
    pub total_pulls: i64,
    pub merged_pulls: i64,
    pub mergeable_pulls: i64,
    pub unmergeable_pulls: i64,
}

#[derive(Debug, Serialize, FromRow)]
pub struct IssueStats {
    pub total_issues: i64,
    pub open_issues: i64,
    pub closed_issues: i64,
}

#[derive(Debug, Serialize, FromRow)]
pub struct MilestoneStats {
    pub total_milestones: i64,
    pub open_milestones: i64,
    pub closed_milestones: i64,
}

#[derive(Debug, Serialize)]
pub struct GistStats {
    pub total_gists: i64,
    pub private_gists: i64,
    pub public_gists: i64,
}

#[derive(Debug, Serialize, FromRow)]
pub struct CommentStats {
    pub total_commit_comments: i64,
    pub total_gist_comments: i64,
    pub total_issue_comments: i64,
    pub total_pull_request_comments: i64,
}

async fn repos(state: &AppState) -> ApiResult<RepoStats> {
    Ok(sqlx::query_as(
        "SELECT count(*) AS total_repos,
                count(*) FILTER (WHERE NOT r.fork) AS root_repos,
                count(*) FILTER (WHERE r.fork) AS fork_repos,
                count(*) FILTER (WHERE o.type = 'Organization') AS org_repos,
                coalesce((SELECT value FROM site_counters WHERE key = $1), 0) AS total_pushes,
                count(*) FILTER (WHERE r.has_wiki) AS total_wikis
           FROM repositories r JOIN users o ON o.id = r.owner_id",
    )
    .bind(PUSHES)
    .fetch_one(&state.db)
    .await?)
}

async fn hooks(state: &AppState) -> ApiResult<HookStats> {
    Ok(sqlx::query_as(
        "SELECT count(*) AS total_hooks,
                count(*) FILTER (WHERE active) AS active_hooks,
                count(*) FILTER (WHERE NOT active) AS inactive_hooks
           FROM webhooks",
    )
    .fetch_one(&state.db)
    .await?)
}

async fn pages(state: &AppState) -> ApiResult<PageStats> {
    Ok(
        sqlx::query_as("SELECT count(*) AS total_pages FROM repositories WHERE has_pages")
            .fetch_one(&state.db)
            .await?,
    )
}

async fn orgs(state: &AppState) -> ApiResult<OrgStats> {
    Ok(sqlx::query_as(
        "SELECT (SELECT count(*) FROM users WHERE type = 'Organization') AS total_orgs,
                (SELECT count(*) FROM org_settings WHERE archived_at IS NOT NULL) AS disabled_orgs,
                (SELECT count(*) FROM teams) AS total_teams,
                (SELECT count(*) FROM team_members) AS total_team_members",
    )
    .fetch_one(&state.db)
    .await?)
}

async fn users(state: &AppState) -> ApiResult<UserStats> {
    Ok(sqlx::query_as(
        "SELECT count(*) AS total_users,
                count(*) FILTER (WHERE site_admin) AS admin_users,
                count(*) FILTER (WHERE suspended_at IS NOT NULL) AS suspended_users
           FROM users WHERE type = 'User'",
    )
    .fetch_one(&state.db)
    .await?)
}

async fn pulls(state: &AppState) -> ApiResult<PullStats> {
    Ok(sqlx::query_as(
        "SELECT count(*) AS total_pulls,
                count(*) FILTER (WHERE merged) AS merged_pulls,
                count(*) FILTER (WHERE NOT merged AND mergeable IS TRUE) AS mergeable_pulls,
                count(*) FILTER (WHERE NOT merged AND mergeable IS FALSE) AS unmergeable_pulls
           FROM pull_requests",
    )
    .fetch_one(&state.db)
    .await?)
}

async fn issues(state: &AppState) -> ApiResult<IssueStats> {
    Ok(sqlx::query_as(
        "SELECT count(*) AS total_issues,
                count(*) FILTER (WHERE state = 'open') AS open_issues,
                count(*) FILTER (WHERE state = 'closed') AS closed_issues
           FROM issues WHERE NOT is_pull_request",
    )
    .fetch_one(&state.db)
    .await?)
}

async fn milestones(state: &AppState) -> ApiResult<MilestoneStats> {
    Ok(sqlx::query_as(
        "SELECT count(*) AS total_milestones,
                count(*) FILTER (WHERE state = 'open') AS open_milestones,
                count(*) FILTER (WHERE state = 'closed') AS closed_milestones
           FROM milestones",
    )
    .fetch_one(&state.db)
    .await?)
}

fn gists() -> GistStats {
    // Gists are not implemented yet.
    GistStats {
        total_gists: 0,
        private_gists: 0,
        public_gists: 0,
    }
}

async fn comments(state: &AppState) -> ApiResult<CommentStats> {
    Ok(sqlx::query_as(
        "SELECT 0::bigint AS total_commit_comments,
                0::bigint AS total_gist_comments,
                (SELECT count(*) FROM comments c JOIN issues i ON i.id = c.issue_id
                  WHERE NOT i.is_pull_request) AS total_issue_comments,
                (SELECT count(*) FROM comments c JOIN issues i ON i.id = c.issue_id
                  WHERE i.is_pull_request)
                  + (SELECT count(*) FROM pr_review_comments) AS total_pull_request_comments",
    )
    .fetch_one(&state.db)
    .await?)
}

/// `GET /enterprise/stats/{type}`: `all` returns every group keyed by
/// name; the others return the group object itself.
pub async fn get(
    State(state): State<AppState>,
    _auth: RequireSiteAdmin,
    Path(kind): Path<String>,
) -> ApiResult<Json<Value>> {
    let v = match kind.as_str() {
        "all" => {
            let (r, h, p, o, u, pr, i, m, c) = tokio::try_join!(
                repos(&state),
                hooks(&state),
                pages(&state),
                orgs(&state),
                users(&state),
                pulls(&state),
                issues(&state),
                milestones(&state),
                comments(&state),
            )?;
            json!({
                "repos": r, "hooks": h, "pages": p, "orgs": o, "users": u,
                "pulls": pr, "issues": i, "milestones": m, "gists": gists(), "comments": c,
            })
        }
        "repos" => serde_json::to_value(repos(&state).await?)?,
        "hooks" => serde_json::to_value(hooks(&state).await?)?,
        "pages" => serde_json::to_value(pages(&state).await?)?,
        "orgs" => serde_json::to_value(orgs(&state).await?)?,
        "users" => serde_json::to_value(users(&state).await?)?,
        "pulls" => serde_json::to_value(pulls(&state).await?)?,
        "issues" => serde_json::to_value(issues(&state).await?)?,
        "milestones" => serde_json::to_value(milestones(&state).await?)?,
        "gists" => serde_json::to_value(gists())?,
        "comments" => serde_json::to_value(comments(&state).await?)?,
        _ => return Err(ApiError::NotFound),
    };
    Ok(Json(v))
}

/// `GET /enterprise/settings/license`. Self-hosted and open source: seats
/// are unlimited and the license never expires.
pub async fn license(
    State(state): State<AppState>,
    _auth: RequireSiteAdmin,
) -> ApiResult<Json<Value>> {
    let used: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM users WHERE type = 'User' AND suspended_at IS NULL",
    )
    .fetch_one(&state.db)
    .await?;
    Ok(Json(json!({
        "seats": "unlimited",
        "seats_used": used,
        "seats_available": "unlimited",
        "kind": "standard",
        "days_until_expiration": null,
        "expire_at": null,
    })))
}

/// Event listener: count pushes for `total_pushes`.
pub async fn on_event(state: AppState, event: Arc<Event>) -> anyhow::Result<()> {
    if let Event::Push(_) = &*event {
        sqlx::query(
            "INSERT INTO site_counters (key, value) VALUES ($1, 1)
             ON CONFLICT (key) DO UPDATE SET value = site_counters.value + 1, updated_at = now()",
        )
        .bind(PUSHES)
        .execute(&state.db)
        .await?;
    }
    Ok(())
}
