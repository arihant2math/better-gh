//! Commit comments.
//!
//! * `GET`/`POST /repos/{o}/{r}/commits/{sha}/comments` (`body`, `path`,
//!   `position` or `line`)
//! * `GET /repos/{o}/{r}/comments`
//! * `GET`/`PATCH`/`DELETE /repos/{o}/{r}/comments/{id}`
//! * `GET`/`POST /repos/{o}/{r}/comments/{id}/reactions`,
//!   `DELETE /repos/{o}/{r}/comments/{id}/reactions/{reaction_id}`
//!
//! Bodies honour the `raw`/`text`/`html`/`full` media types. Creating a
//! comment emits `Event::CommitCommentCreated` (webhook `commit_comment`,
//! activity `CommitCommentEvent`, notifications to the commit author and
//! mentions). Shapes live in `bgh_core::commit_comments`.

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get};
use bgh_core::commit_comments::{self as cc, BodyFormat, CommitCommentRow};
use bgh_core::models::api::{REACTION_CONTENTS, SimpleUser};
use bgh_core::node_id::{self, NodeType};
use bgh_core::perms::RepoAccess;
use bgh_core::prelude::*;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::commits::{cached_diff, resolve};
use crate::identity::users_by_email;
use crate::media::{self, Media};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/repos/{owner}/{repo}/commits/{sha}/comments",
            get(list_for_commit).post(create),
        )
        .route("/repos/{owner}/{repo}/comments", get(list_for_repo))
        .route(
            "/repos/{owner}/{repo}/comments/{id}",
            get(get_one).patch(update).delete(remove),
        )
        .route(
            "/repos/{owner}/{repo}/comments/{id}/reactions",
            get(list_reactions).post(create_reaction),
        )
        .route(
            "/repos/{owner}/{repo}/comments/{id}/reactions/{reaction_id}",
            delete(delete_reaction),
        )
}

fn body_format(headers: &HeaderMap) -> BodyFormat {
    match media::media(headers) {
        Media::Html => BodyFormat {
            body: false,
            text: false,
            html: true,
        },
        Media::Text => BodyFormat {
            body: false,
            text: true,
            html: false,
        },
        Media::Full => BodyFormat::FULL,
        _ => BodyFormat::RAW,
    }
}

async fn render(
    state: &AppState,
    access: &RepoAccess,
    rows: &[CommitCommentRow],
    fmt: BodyFormat,
) -> ApiResult<Vec<Value>> {
    cc::render(state, &access.owner.login, &access.repo, rows, fmt).await
}

async fn render_one(
    state: &AppState,
    access: &RepoAccess,
    row: CommitCommentRow,
    fmt: BodyFormat,
) -> ApiResult<Value> {
    Ok(render(state, access, std::slice::from_ref(&row), fmt)
        .await?
        .remove(0))
}

async fn load(state: &AppState, access: &RepoAccess, id: i64) -> ApiResult<CommitCommentRow> {
    CommitCommentRow::find(&state.db, access.repo.id, id)
        .await?
        .ok_or(ApiError::NotFound)
}

/// `GET /repos/{o}/{r}/commits/{sha}/comments` (oldest first).
async fn list_for_commit(
    State(state): State<AppState>,
    auth: MaybeUser,
    headers: HeaderMap,
    p: Pagination,
    Path((owner, repo, sha)): Path<(String, String, String)>,
) -> ApiResult<Page<Value>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let git = crate::store(&state).cli(access.repo.id)?;
    let sha = resolve(&git, &sha).await?;
    let rows: Vec<CommitCommentRow> = sqlx::query_as(&format!(
        "SELECT {} FROM commit_comments WHERE repo_id = $1 AND commit_id = $2
          ORDER BY id LIMIT $3 OFFSET $4",
        CommitCommentRow::COLUMNS
    ))
    .bind(access.repo.id)
    .bind(&sha)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let page = p.page(rows);
    let items = render(&state, &access, &page.items, body_format(&headers)).await?;
    Ok(Page {
        items,
        link: page.link,
    })
}

/// `GET /repos/{o}/{r}/comments` (oldest first).
async fn list_for_repo(
    State(state): State<AppState>,
    auth: MaybeUser,
    headers: HeaderMap,
    p: Pagination,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<Page<Value>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let rows: Vec<CommitCommentRow> = sqlx::query_as(&format!(
        "SELECT {} FROM commit_comments WHERE repo_id = $1 ORDER BY id LIMIT $2 OFFSET $3",
        CommitCommentRow::COLUMNS
    ))
    .bind(access.repo.id)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let page = p.page(rows);
    let items = render(&state, &access, &page.items, body_format(&headers)).await?;
    Ok(Page {
        items,
        link: page.link,
    })
}

/// `GET /repos/{o}/{r}/comments/{id}`
async fn get_one(
    State(state): State<AppState>,
    auth: MaybeUser,
    headers: HeaderMap,
    Path((owner, repo, id)): Path<(String, String, i64)>,
) -> ApiResult<Json<Value>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let row = load(&state, &access, id).await?;
    Ok(Json(
        render_one(&state, &access, row, body_format(&headers)).await?,
    ))
}

#[derive(Debug, Deserialize)]
struct CreateBody {
    body: Option<String>,
    path: Option<String>,
    position: Option<i64>,
    line: Option<i64>,
}

/// Map between GitHub's diff `position` (line index below the first `@@`
/// of the file's patch, hunk headers counted) and the line number in the
/// new file. Returns `(position, line)` for the requested one, or `None`
/// when it does not address a new-side line of the patch.
fn locate(patch: &str, position: Option<i64>, line: Option<i64>) -> Option<(i64, i64)> {
    let mut new_line = 0i64;
    for (idx, text) in patch.lines().enumerate() {
        let pos = idx as i64;
        if let Some(rest) = text.strip_prefix("@@") {
            // @@ -a,b +c,d @@
            let plus = rest.split_whitespace().find(|t| t.starts_with('+'))?;
            let start: i64 = plus[1..].split(',').next()?.parse().ok()?;
            new_line = start - 1;
            continue;
        }
        if text.starts_with('-') || text.starts_with('\\') {
            if position == Some(pos) {
                return None;
            }
            continue;
        }
        new_line += 1;
        if position == Some(pos) || (position.is_none() && line == Some(new_line)) {
            return Some((pos, new_line));
        }
    }
    None
}

/// `POST /repos/{o}/{r}/commits/{sha}/comments`
async fn create(
    State(state): State<AppState>,
    auth: RequireUser,
    headers: HeaderMap,
    Path((owner, repo, sha)): Path<(String, String, String)>,
    Json(body): Json<CreateBody>,
) -> ApiResult<Response> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    access.require_not_archived()?;
    let text = body.body.filter(|b| !b.trim().is_empty()).ok_or_else(|| {
        ApiError::invalid_field(FieldError::missing_field("CommitComment", "body"))
    })?;
    let git = crate::store(&state).cli(access.repo.id)?;
    let sha = resolve(&git, &sha).await?;
    let commit = git.commit(&sha).await?;

    let path = body.path.filter(|p| !p.is_empty());
    let (mut position, mut line) = (None, None);
    if let Some(path) = &path {
        if body.position.is_some() || body.line.is_some() {
            let files = cached_diff(
                &state,
                &git,
                commit.parents.first().map(String::as_str),
                &sha,
            )
            .await?;
            let file = files.iter().find(|f| &f.path == path).ok_or_else(|| {
                ApiError::invalid_field(FieldError::invalid("CommitComment", "path"))
            })?;
            match file
                .patch
                .as_deref()
                .and_then(|p| locate(p, body.position, body.line))
            {
                Some((pos, l)) => {
                    position = Some(pos as i32);
                    line = Some(l as i32);
                }
                None if body.position.is_some() => {
                    return Err(ApiError::invalid_field(FieldError::invalid(
                        "CommitComment",
                        "position",
                    )));
                }
                // A line outside the diff: kept (GitHub accepts it) without
                // a position.
                None => line = body.line.map(|l| l as i32),
            }
        }
    } else if body.position.is_some() || body.line.is_some() {
        return Err(ApiError::invalid_field(FieldError::missing_field(
            "CommitComment",
            "path",
        )));
    }

    let commit_author_id = users_by_email(&state, [commit.author.email.as_str()])
        .await?
        .get(&commit.author.email.to_ascii_lowercase())
        .map(|u| u.id);

    let mut tx = Tx::begin(&state).await?;
    let row: CommitCommentRow = sqlx::query_as(&format!(
        "INSERT INTO commit_comments (repo_id, commit_id, path, position, line, body, user_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING {}",
        CommitCommentRow::COLUMNS
    ))
    .bind(access.repo.id)
    .bind(&sha)
    .bind(&path)
    .bind(position)
    .bind(line)
    .bind(&text)
    .bind(auth.user.id)
    .fetch_one(&mut *tx)
    .await?;
    tx.emit(Event::CommitCommentCreated {
        repo_id: access.repo.id,
        comment_id: row.id,
        actor_id: auth.user.id,
        commit_author_id,
    });
    tx.commit().await?;
    let json = render_one(&state, &access, row, body_format(&headers)).await?;
    let location = json["url"].as_str().unwrap_or_default().to_string();
    Ok((
        StatusCode::CREATED,
        [(axum::http::header::LOCATION, location)],
        Json(json),
    )
        .into_response())
}

/// The comment's author, or anyone with write access, may edit or delete
/// it (403 for other readers).
fn require_editor(
    auth: &AuthContext,
    access: &RepoAccess,
    row: &CommitCommentRow,
) -> ApiResult<()> {
    access.require_not_archived()?;
    if row.user_id == Some(auth.user.id) {
        return Ok(());
    }
    access.require(Permission::Write)
}

#[derive(Debug, Deserialize)]
struct UpdateBody {
    body: Option<String>,
}

/// `PATCH /repos/{o}/{r}/comments/{id}`
async fn update(
    State(state): State<AppState>,
    auth: RequireUser,
    headers: HeaderMap,
    Path((owner, repo, id)): Path<(String, String, i64)>,
    Json(body): Json<UpdateBody>,
) -> ApiResult<Json<Value>> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    let row = load(&state, &access, id).await?;
    require_editor(&auth, &access, &row)?;
    let text = body.body.filter(|b| !b.trim().is_empty()).ok_or_else(|| {
        ApiError::invalid_field(FieldError::missing_field("CommitComment", "body"))
    })?;
    let mut tx = Tx::begin(&state).await?;
    let old_body = row.body;
    let row: CommitCommentRow = sqlx::query_as(&format!(
        "UPDATE commit_comments SET body = $2, updated_at = now() WHERE id = $1 RETURNING {}",
        CommitCommentRow::COLUMNS
    ))
    .bind(row.id)
    .bind(&text)
    .fetch_one(&mut *tx)
    .await?;
    bgh_core::moderation::record_edit(
        &mut tx,
        access.repo.id,
        bgh_core::moderation::ContentKind::CommitComment,
        row.id,
        auth.user.id,
        &old_body,
        &text,
    )
    .await?;
    tx.commit().await?;
    Ok(Json(
        render_one(&state, &access, row, body_format(&headers)).await?,
    ))
}

/// `DELETE /repos/{o}/{r}/comments/{id}` (its reactions go with it).
async fn remove(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, id)): Path<(String, String, i64)>,
) -> ApiResult<StatusCode> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    let row = load(&state, &access, id).await?;
    require_editor(&auth, &access, &row)?;
    let mut tx = Tx::begin(&state).await?;
    sqlx::query("DELETE FROM reactions WHERE subject_type = 'commit_comment' AND subject_id = $1")
        .bind(row.id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM commit_comments WHERE id = $1")
        .bind(row.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

// ----- reactions -------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct ReactionRow {
    id: i64,
    user_id: i64,
    content: String,
    created_at: DateTime<Utc>,
}

/// GitHub's `reaction` object.
#[derive(Debug, Serialize)]
struct Reaction {
    id: i64,
    node_id: String,
    user: Option<SimpleUser>,
    content: String,
    created_at: Timestamp,
}

async fn render_reactions(state: &AppState, rows: Vec<ReactionRow>) -> ApiResult<Vec<Reaction>> {
    let users = bgh_core::views::users_by_id(state, rows.iter().map(|r| Some(r.user_id))).await?;
    Ok(rows
        .into_iter()
        .map(|r| Reaction {
            id: r.id,
            node_id: node_id::encode(NodeType::Reaction, r.id),
            user: users
                .get(&r.user_id)
                .map(|u| SimpleUser::new(&state.urls, u)),
            content: r.content,
            created_at: r.created_at.into(),
        })
        .collect())
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

#[derive(Debug, Default, Deserialize)]
struct ReactionQuery {
    content: Option<String>,
}

/// `GET /repos/{o}/{r}/comments/{id}/reactions`
async fn list_reactions(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo, id)): Path<(String, String, i64)>,
    Query(q): Query<ReactionQuery>,
) -> ApiResult<Page<Reaction>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let row = load(&state, &access, id).await?;
    if let Some(c) = q.content.as_deref() {
        validate_content(c)?;
    }
    let rows: Vec<ReactionRow> = sqlx::query_as(
        "SELECT id, user_id, content, created_at FROM reactions
          WHERE subject_type = 'commit_comment' AND subject_id = $1
            AND ($2::text IS NULL OR content = $2)
          ORDER BY id LIMIT $3 OFFSET $4",
    )
    .bind(row.id)
    .bind(&q.content)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let page = p.page(rows);
    let items = render_reactions(&state, page.items).await?;
    Ok(Page {
        items,
        link: page.link,
    })
}

#[derive(Debug, Default, Deserialize)]
struct ReactionBody {
    content: Option<String>,
}

/// `POST /repos/{o}/{r}/comments/{id}/reactions`: 201 when created, 200
/// with the existing reaction otherwise.
async fn create_reaction(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, id)): Path<(String, String, i64)>,
    Json(body): Json<ReactionBody>,
) -> ApiResult<Response> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    access.require_not_archived()?;
    let row = load(&state, &access, id).await?;
    let content = body
        .content
        .ok_or_else(|| ApiError::invalid_field(FieldError::missing_field("Reaction", "content")))?;
    validate_content(&content)?;
    let inserted: Option<ReactionRow> = sqlx::query_as(
        "INSERT INTO reactions (subject_type, subject_id, user_id, content)
         VALUES ('commit_comment', $1, $2, $3)
         ON CONFLICT (subject_type, subject_id, user_id, content) DO NOTHING
         RETURNING id, user_id, content, created_at",
    )
    .bind(row.id)
    .bind(auth.user.id)
    .bind(&content)
    .fetch_optional(&state.db)
    .await?;
    let (status, reaction) = match inserted {
        Some(r) => (StatusCode::CREATED, r),
        None => (
            StatusCode::OK,
            sqlx::query_as(
                "SELECT id, user_id, content, created_at FROM reactions
                  WHERE subject_type = 'commit_comment' AND subject_id = $1
                    AND user_id = $2 AND content = $3",
            )
            .bind(row.id)
            .bind(auth.user.id)
            .bind(&content)
            .fetch_one(&state.db)
            .await?,
        ),
    };
    let json = render_reactions(&state, vec![reaction])
        .await?
        .pop()
        .expect("one reaction");
    Ok((status, Json(json)).into_response())
}

/// `DELETE /repos/{o}/{r}/comments/{id}/reactions/{reaction_id}` (own
/// reactions; repository admins may delete any).
async fn delete_reaction(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, id, reaction_id)): Path<(String, String, i64, i64)>,
) -> ApiResult<StatusCode> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    let row = load(&state, &access, id).await?;
    let deleted = sqlx::query(
        "DELETE FROM reactions
          WHERE id = $1 AND subject_type = 'commit_comment' AND subject_id = $2
            AND (user_id = $3 OR $4)",
    )
    .bind(reaction_id)
    .bind(row.id)
    .bind(auth.user.id)
    .bind(access.permission >= Permission::Admin)
    .execute(&state.db)
    .await?;
    if deleted.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::locate;

    const PATCH: &str = "@@ -1,3 +1,4 @@\n a\n-b\n+B\n+c\n d\n@@ -10,2 +11,2 @@\n x\n+y";

    #[test]
    fn position_and_line_map_both_ways() {
        // position 1 = " a" (line 1); 2 = "-b" (no new line); 3 = "+B" (2)
        assert_eq!(locate(PATCH, Some(1), None), Some((1, 1)));
        assert_eq!(locate(PATCH, Some(2), None), None);
        assert_eq!(locate(PATCH, Some(3), None), Some((3, 2)));
        assert_eq!(locate(PATCH, None, Some(3)), Some((4, 3)));
        // second hunk: header at 6, " x" = line 11, "+y" = line 12
        assert_eq!(locate(PATCH, Some(8), None), Some((8, 12)));
        assert_eq!(locate(PATCH, None, Some(12)), Some((8, 12)));
        assert_eq!(locate(PATCH, None, Some(7)), None);
        assert_eq!(locate(PATCH, Some(6), None), None);
    }
}
