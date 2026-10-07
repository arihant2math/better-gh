//! Locking, the issue events API and the timeline API.

use std::collections::HashMap;

use axum::extract::State;
use axum::http::StatusCode;
use bgh_core::perms::{self, RepoAccess};
use bgh_core::prelude::*;
use bgh_core::views;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::json::{self, BodyFormat, EventRow, IssueOpts};
use crate::{issues, service};

pub const LOCK_REASONS: [&str; 4] = ["off-topic", "too heated", "resolved", "spam"];

/// Events only shown in the timeline, not in the events API.
const TIMELINE_ONLY: &str = "'cross-referenced'";

// ---------------------------------------------------------------------------
// Lock
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct LockBody {
    pub lock_reason: Option<String>,
}

/// `PUT /repos/{owner}/{repo}/issues/{issue_number}/lock` → 204.
pub async fn lock(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    Json(body): Json<LockBody>,
) -> ApiResult<StatusCode> {
    let (access, issue) = issues::load(&state, Some(&auth), &owner, &repo, number).await?;
    access.require(Permission::Triage)?;
    access.require_not_archived()?;
    if let Some(r) = &body.lock_reason
        && !LOCK_REASONS.contains(&r.as_str())
    {
        return Err(ApiError::invalid_field(FieldError::invalid(
            "Issue",
            "lock_reason",
        )));
    }
    let mut tx = Tx::begin(&state).await?;
    let issue = service::lock_issue(&mut tx, issue.id).await?;
    if issue.locked {
        return Ok(StatusCode::NO_CONTENT);
    }
    sqlx::query("UPDATE issues SET locked = true, active_lock_reason = $2 WHERE id = $1")
        .bind(issue.id)
        .bind(body.lock_reason.as_deref())
        .execute(&mut *tx)
        .await?;
    service::add_event(
        &mut tx,
        &issue,
        Some(auth.user.id),
        "locked",
        None,
        json!({ "lock_reason": body.lock_reason }),
    )
    .await?;
    service::touch_and_sync(&mut tx, issue.id, SyncAction::Update).await?;
    tx.emit(Event::IssueLocked {
        repo_id: issue.repo_id,
        issue_id: issue.id,
        actor_id: auth.user.id,
    });
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /repos/{owner}/{repo}/issues/{issue_number}/lock` → 204.
pub async fn unlock(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, number)): Path<(String, String, i64)>,
) -> ApiResult<StatusCode> {
    let (access, issue) = issues::load(&state, Some(&auth), &owner, &repo, number).await?;
    access.require(Permission::Triage)?;
    access.require_not_archived()?;
    let mut tx = Tx::begin(&state).await?;
    let issue = service::lock_issue(&mut tx, issue.id).await?;
    if !issue.locked {
        return Ok(StatusCode::NO_CONTENT);
    }
    sqlx::query("UPDATE issues SET locked = false, active_lock_reason = NULL WHERE id = $1")
        .bind(issue.id)
        .execute(&mut *tx)
        .await?;
    service::add_event(
        &mut tx,
        &issue,
        Some(auth.user.id),
        "unlocked",
        None,
        json!({}),
    )
    .await?;
    service::touch_and_sync(&mut tx, issue.id, SyncAction::Update).await?;
    tx.emit(Event::IssueUnlocked {
        repo_id: issue.repo_id,
        issue_id: issue.id,
        actor_id: auth.user.id,
    });
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Events API
// ---------------------------------------------------------------------------

/// `GET /repos/{owner}/{repo}/issues/events` (newest first).
pub async fn list_for_repo(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<Page<Value>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let rows: Vec<EventRow> = sqlx::query_as(&format!(
        "SELECT {} FROM issue_events WHERE repo_id = $1 AND event NOT IN ({TIMELINE_ONLY})
          ORDER BY id DESC LIMIT $2 OFFSET $3",
        EventRow::COLUMNS
    ))
    .bind(access.repo.id)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let page = p.page(rows);
    Ok(Page {
        items: json::events(&state, &page.items, &json::repo_map(&access), true).await?,
        link: page.link,
    })
}

/// `GET /repos/{owner}/{repo}/issues/events/{event_id}`
pub async fn get(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, id)): Path<(String, String, i64)>,
) -> ApiResult<Json<Value>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let row: EventRow = sqlx::query_as(&format!(
        "SELECT {} FROM issue_events WHERE repo_id = $1 AND id = $2 AND event NOT IN ({TIMELINE_ONLY})",
        EventRow::COLUMNS
    ))
    .bind(access.repo.id)
    .bind(id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)?;
    let mut out = json::events(&state, &[row], &json::repo_map(&access), true).await?;
    Ok(Json(out.pop().ok_or(ApiError::NotFound)?))
}

/// `GET /repos/{owner}/{repo}/issues/{issue_number}/events` (oldest first).
pub async fn list_for_issue(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo, number)): Path<(String, String, i64)>,
) -> ApiResult<Page<Value>> {
    let (access, issue) = issues::load(&state, auth.as_ref(), &owner, &repo, number).await?;
    let rows: Vec<EventRow> = sqlx::query_as(&format!(
        "SELECT {} FROM issue_events WHERE issue_id = $1 AND event NOT IN ({TIMELINE_ONLY})
          ORDER BY created_at, id LIMIT $2 OFFSET $3",
        EventRow::COLUMNS
    ))
    .bind(issue.id)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let page = p.page(rows);
    Ok(Page {
        items: json::events(&state, &page.items, &json::repo_map(&access), false).await?,
        link: page.link,
    })
}

// ---------------------------------------------------------------------------
// Timeline
// ---------------------------------------------------------------------------

/// `GET /repos/{owner}/{repo}/issues/{issue_number}/timeline`: events and
/// comments (`event: "commented"`) in chronological order, plus
/// `cross-referenced` items whose source the caller can read.
pub async fn timeline(
    State(state): State<AppState>,
    auth: MaybeUser,
    fmt: BodyFormat,
    p: Pagination,
    Path((owner, repo, number)): Path<(String, String, i64)>,
) -> ApiResult<Page<Value>> {
    let (access, issue) = issues::load(&state, auth.as_ref(), &owner, &repo, number).await?;
    let items: Vec<(String, i64)> = sqlx::query_as(
        "SELECT kind, id FROM (
             SELECT 'e' AS kind, id, created_at FROM issue_events WHERE issue_id = $1
             UNION ALL
             SELECT 'c' AS kind, id, created_at FROM comments WHERE issue_id = $1
         ) t ORDER BY created_at, kind DESC, id LIMIT $2 OFFSET $3",
    )
    .bind(issue.id)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let page = p.page(items);
    let event_ids: Vec<i64> = page
        .items
        .iter()
        .filter(|(k, _)| k == "e")
        .map(|(_, id)| *id)
        .collect();
    let comment_ids: Vec<i64> = page
        .items
        .iter()
        .filter(|(k, _)| k == "c")
        .map(|(_, id)| *id)
        .collect();
    let events: Vec<EventRow> = sqlx::query_as(&format!(
        "SELECT {} FROM issue_events WHERE id = ANY($1)",
        EventRow::COLUMNS
    ))
    .bind(&event_ids)
    .fetch_all(&state.db)
    .await?;
    let comments: Vec<db::Comment> = sqlx::query_as(&format!(
        "SELECT {} FROM comments WHERE id = ANY($1)",
        db::Comment::COLUMNS
    ))
    .bind(&comment_ids)
    .fetch_all(&state.db)
    .await?;
    let repos = json::repo_map(&access);
    let info = &repos[&access.repo.id];

    // Comments.
    let rendered_comments = json::comments(&state, fmt, &comments, &repos).await?;
    let mut comment_json: HashMap<i64, Value> = HashMap::new();
    for (c, r) in comments.iter().zip(rendered_comments) {
        let mut v = serde_json::to_value(r)?;
        if let Value::Object(m) = &mut v {
            m.insert("event".into(), json!("commented"));
            m.insert(
                "actor".into(),
                m.get("user").cloned().unwrap_or(Value::Null),
            );
        }
        comment_json.insert(c.id, v);
    }

    // Cross-references: render source issues the caller can read.
    let xref_sources: Vec<i64> = events
        .iter()
        .filter(|e| e.event == "cross-referenced")
        .filter_map(|e| e.data.get("source_issue_id").and_then(Value::as_i64))
        .collect();
    let mut sources: HashMap<i64, Value> = HashMap::new();
    if !xref_sources.is_empty() {
        let rows: Vec<db::Issue> = sqlx::query_as(&format!(
            "SELECT {} FROM issues WHERE id = ANY($1)",
            db::Issue::COLUMNS
        ))
        .bind(&xref_sources)
        .fetch_all(&state.db)
        .await?;
        let src_repos = json::load_repos(&state, rows.iter().map(|i| i.repo_id)).await?;
        let repo_rows: Vec<db::Repository> = src_repos.values().map(|r| r.repo.clone()).collect();
        let raw = perms::repo_permissions(&state.db, auth.user_id(), &repo_rows).await?;
        let readable: Vec<db::Issue> = rows
            .into_iter()
            .filter(|i| {
                src_repos.get(&i.repo_id).is_some_and(|r| {
                    perms::effective(
                        auth.as_ref(),
                        &r.repo,
                        raw.get(&i.repo_id).copied().unwrap_or(Permission::None),
                    ) >= Permission::Read
                })
            })
            .collect();
        let mut rendered = json::issues(
            &state,
            BodyFormat::Raw,
            &readable,
            &src_repos,
            IssueOpts::default(),
        )
        .await?;
        json::attach_repositories(&state, auth.as_ref(), &src_repos, &mut rendered, &readable)
            .await?;
        for i in rendered {
            sources.insert(i.id, serde_json::to_value(i)?);
        }
    }

    let users = views::users_by_id(&state, json::event_user_ids(&events)).await?;
    let events: HashMap<i64, EventRow> = events.into_iter().map(|e| (e.id, e)).collect();
    let mut out = Vec::with_capacity(page.items.len());
    for (kind, id) in &page.items {
        if kind == "c" {
            if let Some(v) = comment_json.remove(id) {
                out.push(v);
            }
            continue;
        }
        let Some(e) = events.get(id) else { continue };
        if e.event == "cross-referenced" {
            let Some(src) = e
                .data
                .get("source_issue_id")
                .and_then(Value::as_i64)
                .and_then(|s| sources.get(&s))
            else {
                continue;
            };
            let actor = serde_json::to_value(bgh_core::models::api::SimpleUser::or_ghost(
                &state.urls,
                e.actor_id.and_then(|a| users.get(&a)),
            ))?;
            out.push(json!({
                "actor": actor,
                "created_at": Timestamp::from(e.created_at),
                "updated_at": Timestamp::from(e.created_at),
                "source": { "type": "issue", "issue": src },
                "event": "cross-referenced",
            }));
            continue;
        }
        out.push(json::timeline_event(&state, info, e, &users));
    }
    Ok(Page {
        items: out,
        link: page.link,
    })
}
