//! `GET /repos/{owner}/{repo}/activity` (package P31): branch pushes and
//! ref changes, recorded by post-receive processing ([`record`]) for every
//! branch update (pushes, API ref writes, merges, mirror syncs).
//!
//! Filters: `direction` (`desc`), `ref` (branch or full ref),
//! `actor` (login), `time_period` (`day|week|month|quarter|year`),
//! `activity_type`; page-number pagination with `Link` headers.

use axum::Router;
use axum::extract::State;
use axum::routing::get;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use bgh_core::events::RefUpdate;
use bgh_core::models::api::SimpleUser;
use bgh_core::prelude::*;
use serde::{Deserialize, Serialize};

pub fn routes() -> Router<AppState> {
    Router::new().route("/repos/{owner}/{repo}/activity", get(list))
}

const TYPES: &[&str] = &[
    "push",
    "force_push",
    "branch_creation",
    "branch_deletion",
    "pr_merge",
    "merge_queue_merge",
];

/// Classify and record branch updates (tags are not part of the log).
/// `git` decides fast-forwards; merges are recognized by the PR's merge
/// commit.
pub async fn record(
    conn: &mut sqlx::PgConnection,
    git: Option<&bgh_git::GitCli>,
    repo_id: i64,
    actor_id: Option<i64>,
    updates: &[RefUpdate],
) -> anyhow::Result<()> {
    for u in updates.iter().filter(|u| u.branch().is_some()) {
        let kind = if u.is_create() {
            "branch_creation"
        } else if u.is_delete() {
            "branch_deletion"
        } else {
            let merged: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pull_requests
                                 WHERE repo_id = $1 AND merged AND merge_commit_sha = $2
                                   AND 'refs/heads/' || base_ref = $3)",
            )
            .bind(repo_id)
            .bind(&u.new)
            .bind(&u.refname)
            .fetch_one(&mut *conn)
            .await?;
            let fast_forward = match git {
                Some(g) => g.is_ancestor(&u.old, &u.new).await.unwrap_or(true),
                None => true,
            };
            match (merged, fast_forward) {
                (true, _) => "pr_merge",
                (false, true) => "push",
                (false, false) => "force_push",
            }
        };
        sqlx::query(
            "INSERT INTO repo_activity (repo_id, ref, before, after, activity_type, actor_id)
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(repo_id)
        .bind(&u.refname)
        .bind(&u.old)
        .bind(&u.new)
        .bind(kind)
        .bind(actor_id)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct ListQuery {
    direction: Option<String>,
    #[serde(rename = "ref")]
    refname: Option<String>,
    actor: Option<String>,
    time_period: Option<String>,
    activity_type: Option<String>,
}

#[derive(sqlx::FromRow)]
struct Row {
    id: i64,
    #[sqlx(rename = "ref")]
    refname: String,
    before: String,
    after: String,
    activity_type: String,
    actor_id: Option<i64>,
    created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Serialize)]
struct Activity {
    id: i64,
    node_id: String,
    before: String,
    after: String,
    #[serde(rename = "ref")]
    refname: String,
    timestamp: Timestamp,
    activity_type: String,
    actor: Option<SimpleUser>,
}

/// `GET /repos/{owner}/{repo}/activity`
async fn list(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo)): Path<(String, String)>,
    Query(q): Query<ListQuery>,
) -> ApiResult<Page<Activity>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let asc = match q.direction.as_deref() {
        None | Some("desc") => false,
        Some("asc") => true,
        Some(_) => {
            return Err(ApiError::invalid_field(FieldError::invalid(
                "Activity",
                "direction",
            )));
        }
    };
    let interval = match q.time_period.as_deref() {
        None => None,
        Some("day") => Some("1 day"),
        Some("week") => Some("7 days"),
        Some("month") => Some("1 month"),
        Some("quarter") => Some("3 months"),
        Some("year") => Some("1 year"),
        Some(_) => {
            return Err(ApiError::invalid_field(FieldError::invalid(
                "Activity",
                "time_period",
            )));
        }
    };
    if let Some(t) = q.activity_type.as_deref()
        && !TYPES.contains(&t)
    {
        return Err(ApiError::invalid_field(FieldError::invalid(
            "Activity",
            "activity_type",
        )));
    }
    let refname = q.refname.as_deref().map(|r| {
        if r.starts_with("refs/") {
            r.to_string()
        } else {
            format!("refs/heads/{r}")
        }
    });
    let actor_id = match q.actor.as_deref() {
        None => None,
        Some(login) => match db::User::find_by_login(&state.db, login).await? {
            Some(u) => Some(u.id),
            None => return Ok(p.page(vec![])),
        },
    };
    let rows: Vec<Row> = sqlx::query_as(&format!(
        "SELECT id, ref, before, after, activity_type, actor_id, created_at
           FROM repo_activity
          WHERE repo_id = $1
            AND ($2::text IS NULL OR ref = $2)
            AND ($3::bigint IS NULL OR actor_id = $3)
            AND ($4::interval IS NULL OR created_at >= now() - $4::interval)
            AND ($5::text IS NULL OR activity_type = $5)
          ORDER BY id {}
          LIMIT $6 OFFSET $7",
        if asc { "ASC" } else { "DESC" }
    ))
    .bind(access.repo.id)
    .bind(&refname)
    .bind(actor_id)
    .bind(interval)
    .bind(&q.activity_type)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let users = bgh_core::views::users_by_id(&state, rows.iter().map(|r| r.actor_id)).await?;
    Ok(p.page(rows).map(|r| Activity {
        id: r.id,
        node_id: STANDARD.encode(format!("18:RepositoryActivity{}", r.id)),
        before: r.before,
        after: r.after,
        refname: r.refname,
        timestamp: Timestamp(r.created_at),
        activity_type: r.activity_type,
        actor: r
            .actor_id
            .and_then(|id| users.get(&id))
            .map(|u| SimpleUser::new(&state.urls, u)),
    }))
}
