//! Reactions on issues and issue comments.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use bgh_core::models::api::REACTION_CONTENTS;
use bgh_core::perms::RepoAccess;
use bgh_core::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::json::{self, ReactionRow};
use crate::{comments, issues};

/// A reaction target: `(subject_type, subject_id, issue)`.
struct Subject {
    kind: &'static str,
    id: i64,
    issue: db::Issue,
    comment: Option<db::Comment>,
}

/// Re-sync the reacted row (reaction counts live on issues and comments).
async fn sync_subject(
    tx: &mut Tx,
    state: &AppState,
    access: &RepoAccess,
    subject: &Subject,
) -> ApiResult<()> {
    match &subject.comment {
        Some(c) => {
            let info = json::RepoInfo::from_access(access);
            crate::service::sync_comment(tx, state, &info, c, SyncAction::Update).await
        }
        None => {
            let issue = crate::service::issue_by_id(&mut **tx, subject.issue.id).await?;
            crate::service::sync_issue_row(tx, &issue, SyncAction::Update).await
        }
    }
}

async fn issue_subject(
    state: &AppState,
    auth: Option<&AuthContext>,
    owner: &str,
    repo: &str,
    number: i64,
) -> ApiResult<(RepoAccess, Subject)> {
    let (access, issue) = issues::load(state, auth, owner, repo, number).await?;
    Ok((
        access,
        Subject {
            kind: "issue",
            id: issue.id,
            issue,
            comment: None,
        },
    ))
}

async fn comment_subject(
    state: &AppState,
    auth: Option<&AuthContext>,
    owner: &str,
    repo: &str,
    id: i64,
) -> ApiResult<(RepoAccess, Subject)> {
    let access = RepoAccess::load(state, auth, owner, repo).await?;
    let c = comments::find(&state.db, access.repo.id, id).await?;
    let issue = crate::service::issue_by_id(&state.db, c.issue_id).await?;
    Ok((
        access,
        Subject {
            kind: "issue_comment",
            id: c.id,
            issue,
            comment: Some(c),
        },
    ))
}

#[derive(Debug, Default, Deserialize)]
pub struct ListQuery {
    pub content: Option<String>,
}

fn validate_content(c: &str) -> ApiResult<()> {
    if REACTION_CONTENTS.contains(&c) {
        Ok(())
    } else {
        Err(ApiError::invalid_field(FieldError::invalid(
            "Reaction", "content",
        )))
    }
}

async fn list(
    state: &AppState,
    p: Pagination,
    subject: &Subject,
    q: &ListQuery,
) -> ApiResult<Page<json::Reaction>> {
    if let Some(c) = &q.content {
        validate_content(c)?;
    }
    let rows: Vec<ReactionRow> = sqlx::query_as(&format!(
        "SELECT {} FROM reactions WHERE subject_type = $1 AND subject_id = $2
            AND ($3::text IS NULL OR content = $3)
          ORDER BY id LIMIT $4 OFFSET $5",
        ReactionRow::COLUMNS
    ))
    .bind(subject.kind)
    .bind(subject.id)
    .bind(q.content.as_deref())
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let page = p.page(rows);
    Ok(Page {
        items: json::reactions(state, page.items).await?,
        link: page.link,
    })
}

#[derive(Debug, Default, Deserialize)]
pub struct CreateBody {
    pub content: Option<String>,
}

/// 201 when created, 200 when the user already reacted with this content.
async fn create(
    state: &AppState,
    auth: &AuthContext,
    access: &RepoAccess,
    subject: &Subject,
    body: CreateBody,
) -> ApiResult<Response> {
    access.require_not_archived()?;
    if subject.issue.locked && access.permission < Permission::Triage {
        return Err(ApiError::forbidden(
            "Unable to create reaction because issue is locked.",
        ));
    }
    let content = body.content.unwrap_or_default();
    if content.is_empty() {
        return Err(ApiError::invalid_field(FieldError::missing_field(
            "Reaction", "content",
        )));
    }
    validate_content(&content)?;
    let mut tx = Tx::begin(state).await?;
    let inserted: Option<ReactionRow> = sqlx::query_as(&format!(
        "INSERT INTO reactions (subject_type, subject_id, user_id, content) VALUES ($1, $2, $3, $4)
         ON CONFLICT DO NOTHING RETURNING {}",
        ReactionRow::COLUMNS
    ))
    .bind(subject.kind)
    .bind(subject.id)
    .bind(auth.user.id)
    .bind(&content)
    .fetch_optional(&mut *tx)
    .await?;
    let (row, status) = match inserted {
        Some(r) => {
            sync_subject(&mut tx, state, access, subject).await?;
            tx.emit(Event::ReactionCreated {
                repo_id: access.repo.id,
                subject_type: subject.kind.to_string(),
                subject_id: subject.id,
                reaction_id: r.id,
                actor_id: auth.user.id,
            });
            (r, StatusCode::CREATED)
        }
        None => {
            let r: ReactionRow = sqlx::query_as(&format!(
                "SELECT {} FROM reactions WHERE subject_type = $1 AND subject_id = $2
                    AND user_id = $3 AND content = $4",
                ReactionRow::COLUMNS
            ))
            .bind(subject.kind)
            .bind(subject.id)
            .bind(auth.user.id)
            .bind(&content)
            .fetch_one(&mut *tx)
            .await?;
            (r, StatusCode::OK)
        }
    };
    tx.commit().await?;
    let mut out = json::reactions(state, vec![row]).await?;
    Ok((status, Json(out.pop().ok_or(ApiError::NotFound)?)).into_response())
}

/// Users may delete their own reactions; admins any.
async fn delete(
    state: &AppState,
    auth: &AuthContext,
    access: &RepoAccess,
    subject: &Subject,
    reaction_id: i64,
) -> ApiResult<StatusCode> {
    access.require_not_archived()?;
    let row: ReactionRow = sqlx::query_as(&format!(
        "SELECT {} FROM reactions WHERE id = $1 AND subject_type = $2 AND subject_id = $3",
        ReactionRow::COLUMNS
    ))
    .bind(reaction_id)
    .bind(subject.kind)
    .bind(subject.id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)?;
    if row.user_id != auth.user.id && access.permission < Permission::Admin {
        return Err(ApiError::forbidden(
            "You do not have permission to delete this reaction.",
        ));
    }
    let mut tx = Tx::begin(state).await?;
    sqlx::query("DELETE FROM reactions WHERE id = $1")
        .bind(row.id)
        .execute(&mut *tx)
        .await?;
    sync_subject(&mut tx, state, access, subject).await?;
    tx.emit(Event::ReactionDeleted {
        repo_id: access.repo.id,
        subject_type: subject.kind.to_string(),
        subject_id: subject.id,
        reaction_id: row.id,
        actor_id: auth.user.id,
    });
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /repos/{owner}/{repo}/issues/{issue_number}/reactions`
pub async fn list_for_issue(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    Query(q): Query<ListQuery>,
) -> ApiResult<Page<json::Reaction>> {
    let (_, s) = issue_subject(&state, auth.as_ref(), &owner, &repo, number).await?;
    list(&state, p, &s, &q).await
}

/// `POST /repos/{owner}/{repo}/issues/{issue_number}/reactions`
pub async fn create_for_issue(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    Json(body): Json<CreateBody>,
) -> ApiResult<Response> {
    let (access, s) = issue_subject(&state, Some(&auth), &owner, &repo, number).await?;
    create(&state, &auth, &access, &s, body).await
}

/// `DELETE /repos/{owner}/{repo}/issues/{issue_number}/reactions/{reaction_id}`
pub async fn delete_for_issue(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, number, id)): Path<(String, String, i64, i64)>,
) -> ApiResult<StatusCode> {
    let (access, s) = issue_subject(&state, Some(&auth), &owner, &repo, number).await?;
    delete(&state, &auth, &access, &s, id).await
}

/// `GET /repos/{owner}/{repo}/issues/comments/{comment_id}/reactions`
pub async fn list_for_comment(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo, id)): Path<(String, String, i64)>,
    Query(q): Query<ListQuery>,
) -> ApiResult<Page<json::Reaction>> {
    let (_, s) = comment_subject(&state, auth.as_ref(), &owner, &repo, id).await?;
    list(&state, p, &s, &q).await
}

/// `POST /repos/{owner}/{repo}/issues/comments/{comment_id}/reactions`
pub async fn create_for_comment(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, id)): Path<(String, String, i64)>,
    Json(body): Json<CreateBody>,
) -> ApiResult<Response> {
    let (access, s) = comment_subject(&state, Some(&auth), &owner, &repo, id).await?;
    create(&state, &auth, &access, &s, body).await
}

/// `DELETE /repos/{owner}/{repo}/issues/comments/{comment_id}/reactions/{reaction_id}`
pub async fn delete_for_comment(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, id, rid)): Path<(String, String, i64, i64)>,
) -> ApiResult<StatusCode> {
    let (access, s) = comment_subject(&state, Some(&auth), &owner, &repo, id).await?;
    delete(&state, &auth, &access, &s, rid).await
}

// ---------------------------------------------------------------------------
// Web client helpers (`/_bgh`): the viewer's own reactions. GitHub's REST
// API needs a reaction id to delete one; the web client only knows counts.
// ---------------------------------------------------------------------------

/// Reaction contents the viewer used on an issue and on each of its comments.
#[derive(Debug, Serialize)]
pub struct ViewerReactions {
    pub issue: Vec<String>,
    /// Comment id (as a string key) → contents.
    pub comments: BTreeMap<String, Vec<String>>,
}

/// `GET /_bgh/repos/{owner}/{repo}/issues/{issue_number}/viewer-reactions`
pub async fn viewer_reactions(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, number)): Path<(String, String, i64)>,
) -> ApiResult<Json<ViewerReactions>> {
    let (_, issue) = issues::load(&state, Some(&auth), &owner, &repo, number).await?;
    let rows: Vec<(String, i64, String)> = sqlx::query_as(
        "SELECT subject_type, subject_id, content FROM reactions
          WHERE user_id = $1
            AND ((subject_type = 'issue' AND subject_id = $2)
              OR (subject_type = 'issue_comment'
                  AND subject_id IN (SELECT id FROM comments WHERE issue_id = $2)))
          ORDER BY id",
    )
    .bind(auth.user.id)
    .bind(issue.id)
    .fetch_all(&state.db)
    .await?;
    let mut out = ViewerReactions {
        issue: Vec::new(),
        comments: BTreeMap::new(),
    };
    for (kind, id, content) in rows {
        if kind == "issue" {
            out.issue.push(content);
        } else {
            out.comments
                .entry(id.to_string())
                .or_default()
                .push(content);
        }
    }
    Ok(Json(out))
}

/// Delete the viewer's reaction with `content` (204 also when there is none).
async fn delete_own(
    state: &AppState,
    auth: &AuthContext,
    access: &RepoAccess,
    subject: &Subject,
    content: &str,
) -> ApiResult<StatusCode> {
    validate_content(content)?;
    let id: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM reactions WHERE subject_type = $1 AND subject_id = $2
            AND user_id = $3 AND content = $4",
    )
    .bind(subject.kind)
    .bind(subject.id)
    .bind(auth.user.id)
    .bind(content)
    .fetch_optional(&state.db)
    .await?;
    match id {
        Some(id) => delete(state, auth, access, subject, id).await,
        None => Ok(StatusCode::NO_CONTENT),
    }
}

/// `DELETE /_bgh/repos/{owner}/{repo}/issues/{issue_number}/reactions/{content}`
pub async fn delete_own_for_issue(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, number, content)): Path<(String, String, i64, String)>,
) -> ApiResult<StatusCode> {
    let (access, s) = issue_subject(&state, Some(&auth), &owner, &repo, number).await?;
    delete_own(&state, &auth, &access, &s, &content).await
}

/// `DELETE /_bgh/repos/{owner}/{repo}/issues/comments/{comment_id}/reactions/{content}`
pub async fn delete_own_for_comment(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, id, content)): Path<(String, String, i64, String)>,
) -> ApiResult<StatusCode> {
    let (access, s) = comment_subject(&state, Some(&auth), &owner, &repo, id).await?;
    delete_own(&state, &auth, &access, &s, &content).await
}
