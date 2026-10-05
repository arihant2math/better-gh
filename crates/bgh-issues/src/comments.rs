//! Issue comments.

use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bgh_core::perms::RepoAccess;
use bgh_core::prelude::*;
use bgh_core::sync;
use serde::Deserialize;

use crate::issues::{self, parse_since};
use crate::json::{self, BodyFormat, RepoInfo};
use crate::{refs, service};

const MAX_BODY: usize = 65536;

/// Load a comment of the repository; 404 if missing.
pub async fn find(db: impl sqlx::PgExecutor<'_>, repo_id: i64, id: i64) -> ApiResult<db::Comment> {
    sqlx::query_as::<_, db::Comment>(&format!(
        "SELECT {} FROM comments WHERE repo_id = $1 AND id = $2",
        db::Comment::COLUMNS
    ))
    .bind(repo_id)
    .bind(id)
    .fetch_optional(db)
    .await?
    .ok_or(ApiError::NotFound)
}

/// 403 when the issue is locked and the caller isn't a triager.
pub fn require_unlocked(access: &RepoAccess, issue: &db::Issue) -> ApiResult<()> {
    if issue.locked && access.permission < Permission::Triage {
        Err(ApiError::forbidden(
            "Unable to create comment because issue is locked.",
        ))
    } else {
        Ok(())
    }
}

fn validate_body(body: Option<&str>) -> ApiResult<&str> {
    match body {
        Some(b) if !b.trim().is_empty() => {
            if b.len() > MAX_BODY {
                Err(ApiError::invalid_field(FieldError::custom(
                    "IssueComment",
                    "body",
                    "body is too long (maximum is 65536 characters)",
                )))
            } else {
                Ok(b)
            }
        }
        _ => Err(ApiError::invalid_field(FieldError::missing_field(
            "IssueComment",
            "body",
        ))),
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct RepoListQuery {
    pub sort: Option<String>,
    pub direction: Option<String>,
    pub since: Option<String>,
}

/// `GET /repos/{owner}/{repo}/issues/comments`
pub async fn list_for_repo(
    State(state): State<AppState>,
    auth: MaybeUser,
    fmt: BodyFormat,
    p: Pagination,
    Path((owner, repo)): Path<(String, String)>,
    Query(q): Query<RepoListQuery>,
) -> ApiResult<Page<json::IssueComment>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let since = parse_since(q.since.as_deref())?;
    let order = match q.sort.as_deref() {
        None => "id ASC".to_string(),
        Some(s @ ("created" | "updated")) => {
            let dir = match q.direction.as_deref().unwrap_or("desc") {
                "asc" => "ASC",
                "desc" => "DESC",
                _ => {
                    return Err(ApiError::invalid_field(FieldError::invalid(
                        "IssueComment",
                        "direction",
                    )));
                }
            };
            format!("{s}_at {dir}, id {dir}")
        }
        Some(_) => {
            return Err(ApiError::invalid_field(FieldError::invalid(
                "IssueComment",
                "sort",
            )));
        }
    };
    let rows: Vec<db::Comment> = sqlx::query_as(&format!(
        "SELECT {} FROM comments WHERE repo_id = $1 AND ($2::timestamptz IS NULL OR updated_at >= $2)
          ORDER BY {order} LIMIT $3 OFFSET $4",
        db::Comment::COLUMNS
    ))
    .bind(access.repo.id)
    .bind(since)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let page = p.page(rows);
    Ok(Page {
        items: json::comments(&state, fmt, &page.items, &json::repo_map(&access)).await?,
        link: page.link,
    })
}

#[derive(Debug, Default, Deserialize)]
pub struct IssueListQuery {
    pub since: Option<String>,
}

/// `GET /repos/{owner}/{repo}/issues/{issue_number}/comments`
pub async fn list_for_issue(
    State(state): State<AppState>,
    auth: MaybeUser,
    fmt: BodyFormat,
    p: Pagination,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    Query(q): Query<IssueListQuery>,
) -> ApiResult<Page<json::IssueComment>> {
    let (access, issue) = issues::load(&state, auth.as_ref(), &owner, &repo, number).await?;
    let since = parse_since(q.since.as_deref())?;
    let rows: Vec<db::Comment> = sqlx::query_as(&format!(
        "SELECT {} FROM comments WHERE issue_id = $1 AND ($2::timestamptz IS NULL OR updated_at >= $2)
          ORDER BY created_at, id LIMIT $3 OFFSET $4",
        db::Comment::COLUMNS
    ))
    .bind(issue.id)
    .bind(since)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let page = p.page(rows);
    Ok(Page {
        items: json::comments(&state, fmt, &page.items, &json::repo_map(&access)).await?,
        link: page.link,
    })
}

/// `GET /repos/{owner}/{repo}/issues/comments/{comment_id}`
pub async fn get(
    State(state): State<AppState>,
    auth: MaybeUser,
    fmt: BodyFormat,
    Path((owner, repo, id)): Path<(String, String, i64)>,
) -> ApiResult<Json<json::IssueComment>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let c = find(&state.db, access.repo.id, id).await?;
    let mut out = json::comments(&state, fmt, &[c], &json::repo_map(&access)).await?;
    Ok(Json(out.pop().ok_or(ApiError::NotFound)?))
}

#[derive(Debug, Default, Deserialize)]
pub struct CommentBody {
    pub body: Option<String>,
}

/// `POST /repos/{owner}/{repo}/issues/{issue_number}/comments` → 201.
pub async fn create(
    State(state): State<AppState>,
    auth: RequireUser,
    fmt: BodyFormat,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    Json(body): Json<CommentBody>,
) -> ApiResult<Response> {
    let (access, issue) = issues::load(&state, Some(&auth), &owner, &repo, number).await?;
    access.require_not_archived()?;
    require_unlocked(&access, &issue)?;
    let text = validate_body(body.body.as_deref())?;
    let info = RepoInfo::from_access(&access);
    let mut tx = Tx::begin(&state).await?;
    let issue = service::lock_issue(&mut tx, issue.id).await?;
    let c: db::Comment = sqlx::query_as(&format!(
        "INSERT INTO comments (issue_id, repo_id, author_id, body, created_at, updated_at)
         VALUES ($1, $2, $3, $4, clock_timestamp(), clock_timestamp()) RETURNING {}",
        db::Comment::COLUMNS
    ))
    .bind(issue.id)
    .bind(issue.repo_id)
    .bind(auth.user.id)
    .bind(text)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query("UPDATE issues SET comments_count = comments_count + 1 WHERE id = $1")
        .bind(issue.id)
        .execute(&mut *tx)
        .await?;
    service::sync_comment(&mut tx, c.id, SyncAction::Insert).await?;
    service::subscribe(&mut tx, &issue, auth.user.id, "comment").await?;
    refs::process(
        &mut tx,
        &state,
        &info,
        &issue,
        Some(c.id),
        None,
        text,
        &auth.user,
    )
    .await?;
    service::touch_and_sync(&mut tx, issue.id, SyncAction::Update).await?;
    tx.emit(Event::IssueCommentCreated {
        repo_id: issue.repo_id,
        issue_id: issue.id,
        comment_id: c.id,
        actor_id: auth.user.id,
    });
    tx.commit().await?;
    let rendered = json::comments(&state, fmt, &[c], &json::repo_map(&access))
        .await?
        .pop()
        .ok_or(ApiError::NotFound)?;
    let mut headers = HeaderMap::new();
    if let Ok(v) = HeaderValue::from_str(&rendered.url) {
        headers.insert(header::LOCATION, v);
    }
    Ok((StatusCode::CREATED, headers, Json(rendered)).into_response())
}

/// Authors may edit/delete their own comments; others need write.
fn require_comment_owner(access: &RepoAccess, c: &db::Comment, user: &db::User) -> ApiResult<()> {
    if c.author_id == Some(user.id) || access.permission >= Permission::Write {
        Ok(())
    } else {
        Err(ApiError::forbidden(
            "You do not have permission to modify this comment.",
        ))
    }
}

/// `PATCH /repos/{owner}/{repo}/issues/comments/{comment_id}`
pub async fn update(
    State(state): State<AppState>,
    auth: RequireUser,
    fmt: BodyFormat,
    Path((owner, repo, id)): Path<(String, String, i64)>,
    Json(body): Json<CommentBody>,
) -> ApiResult<Json<json::IssueComment>> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    access.require_not_archived()?;
    let old = find(&state.db, access.repo.id, id).await?;
    require_comment_owner(&access, &old, &auth.user)?;
    let text = validate_body(body.body.as_deref())?;
    let info = RepoInfo::from_access(&access);
    let mut tx = Tx::begin(&state).await?;
    let issue = service::lock_issue(&mut tx, old.issue_id).await?;
    let c: db::Comment = sqlx::query_as(&format!(
        "UPDATE comments SET body = $2, updated_at = now() WHERE id = $1 RETURNING {}",
        db::Comment::COLUMNS
    ))
    .bind(old.id)
    .bind(text)
    .fetch_one(&mut *tx)
    .await?;
    bgh_core::moderation::record_edit(
        &mut tx,
        c.repo_id,
        bgh_core::moderation::ContentKind::Comment,
        c.id,
        auth.user.id,
        &old.body,
        text,
    )
    .await?;
    service::sync_comment(&mut tx, c.id, SyncAction::Update).await?;
    refs::process(
        &mut tx,
        &state,
        &info,
        &issue,
        Some(c.id),
        Some(&old.body),
        text,
        &auth.user,
    )
    .await?;
    tx.emit(Event::IssueCommentEdited {
        repo_id: c.repo_id,
        issue_id: c.issue_id,
        comment_id: c.id,
        actor_id: auth.user.id,
        changes: serde_json::json!({ "body": { "from": old.body } }),
    });
    tx.commit().await?;
    let mut out = json::comments(&state, fmt, &[c], &json::repo_map(&access)).await?;
    Ok(Json(out.pop().ok_or(ApiError::NotFound)?))
}

/// `DELETE /repos/{owner}/{repo}/issues/comments/{comment_id}` → 204.
pub async fn delete(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, id)): Path<(String, String, i64)>,
) -> ApiResult<StatusCode> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    access.require_not_archived()?;
    let c = find(&state.db, access.repo.id, id).await?;
    require_comment_owner(&access, &c, &auth.user)?;
    // Webhook `deleted` payloads carry the full comment: snapshot it first.
    let snapshot = json::comments(
        &state,
        BodyFormat::default(),
        std::slice::from_ref(&c),
        &json::repo_map(&access),
    )
    .await?
    .pop()
    .map(serde_json::to_value)
    .transpose()?
    .unwrap_or_default();
    let mut tx = Tx::begin(&state).await?;
    service::lock_issue(&mut tx, c.issue_id).await?;
    sqlx::query("DELETE FROM comments WHERE id = $1")
        .bind(c.id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM reactions WHERE subject_type = 'issue_comment' AND subject_id = $1")
        .bind(c.id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE issues SET comments_count = GREATEST(comments_count - 1, 0) WHERE id = $1")
        .bind(c.issue_id)
        .execute(&mut *tx)
        .await?;
    tx.sync_delete(&sync::repo_scope(c.repo_id), SyncModel::Comment, c.id)
        .await?;
    tx.sync_issue(c.issue_id, SyncAction::Update, false).await?;
    tx.emit(Event::IssueCommentDeleted {
        repo_id: c.repo_id,
        issue_id: c.issue_id,
        comment_id: c.id,
        actor_id: auth.user.id,
        comment: snapshot,
    });
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
