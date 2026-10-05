//! Review comments (`pull-request-review-comment`): create (line,
//! multi-line, file-level, replies), list (per PR, repo-wide, per review),
//! get / edit / delete, reactions; plus position/diff_hunk computation.

use std::collections::HashMap;

use axum::extract::State;
use axum::http::StatusCode;
use bgh_core::models::api::{AuthorAssociation, REACTION_CONTENTS, ReactionRollup, SimpleUser};
use bgh_core::node_id::{self, NodeType};
use bgh_core::prelude::*;
use bgh_core::time::ts;
use bgh_git::patch::{self, Side};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::git;
use crate::json::{Href, associations};
use crate::model::{self, Pull, ReviewComment};
use crate::pulls::load_pull;

pub const SUBJECT: &str = "pull_request_review_comment";

#[derive(Debug, Clone, Serialize)]
pub struct CommentLinks {
    #[serde(rename = "self")]
    pub self_: Href,
    pub html: Href,
    pub pull_request: Href,
}

/// `pull-request-review-comment`.
#[derive(Debug, Clone, Serialize)]
pub struct ReviewCommentJson {
    pub url: String,
    pub pull_request_review_id: Option<i64>,
    pub id: i64,
    pub node_id: String,
    pub diff_hunk: String,
    pub path: String,
    pub position: Option<i32>,
    pub original_position: Option<i32>,
    pub commit_id: String,
    pub original_commit_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub in_reply_to_id: Option<i64>,
    pub user: SimpleUser,
    pub body: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub html_url: String,
    pub pull_request_url: String,
    pub author_association: AuthorAssociation,
    #[serde(rename = "_links")]
    pub links: CommentLinks,
    pub start_line: Option<i32>,
    pub original_start_line: Option<i32>,
    pub start_side: Option<String>,
    pub line: Option<i32>,
    pub original_line: Option<i32>,
    pub side: Option<String>,
    pub subject_type: String,
    pub reactions: ReactionRollup,
}

pub fn comment_url(state: &AppState, owner: &str, repo: &str, id: i64) -> String {
    state
        .urls
        .api(&format!("/repos/{owner}/{repo}/pulls/comments/{id}"))
}

/// Render comments of one repository (batched users, reactions, numbers).
pub async fn render(
    state: &AppState,
    access: &RepoAccess,
    rows: &[ReviewComment],
) -> ApiResult<Vec<ReviewCommentJson>> {
    if rows.is_empty() {
        return Ok(vec![]);
    }
    let users = bgh_core::views::users_by_id(state, rows.iter().map(|c| c.user_id)).await?;
    let ids: Vec<i64> = rows.iter().map(|c| c.id).collect();
    let pull_ids: Vec<i64> = rows.iter().map(|c| c.pull_id).collect();
    let numbers: HashMap<i64, i64> =
        sqlx::query_as::<_, (i64, i64)>("SELECT id, number FROM issues WHERE id = ANY($1)")
            .bind(&pull_ids)
            .fetch_all(&state.db)
            .await?
            .into_iter()
            .collect();
    let counts: Vec<(i64, String, i64)> = sqlx::query_as(
        "SELECT subject_id, content, count(*) FROM reactions
          WHERE subject_type = $1 AND subject_id = ANY($2) GROUP BY subject_id, content",
    )
    .bind(SUBJECT)
    .bind(&ids)
    .fetch_all(&state.db)
    .await?;
    let mut by_comment: HashMap<i64, Vec<(String, i64)>> = HashMap::new();
    for (id, c, n) in counts {
        by_comment.entry(id).or_default().push((c, n));
    }
    let authors: Vec<i64> = rows.iter().filter_map(|c| c.user_id).collect();
    let assoc = associations(state, &access.repo, &authors).await?;
    let owner = access.owner.login.as_str();
    let name = access.repo.name.as_str();
    Ok(rows
        .iter()
        .map(|c| {
            let number = numbers.get(&c.pull_id).copied().unwrap_or(0);
            let url = comment_url(state, owner, name, c.id);
            let html = format!(
                "{}#discussion_r{}",
                state.urls.pull_html(owner, name, number),
                c.id
            );
            let pr_url = state.urls.pull(owner, name, number);
            ReviewCommentJson {
                pull_request_review_id: c.review_id,
                id: c.id,
                node_id: node_id::encode(NodeType::PullRequestReviewComment, c.id),
                diff_hunk: c.diff_hunk.clone(),
                path: c.path.clone(),
                position: c.position,
                original_position: c.original_position,
                commit_id: c.commit_id.clone(),
                original_commit_id: c.original_commit_id.clone(),
                in_reply_to_id: c.in_reply_to_id,
                user: SimpleUser::or_ghost(&state.urls, c.user_id.and_then(|u| users.get(&u))),
                body: c.body.clone(),
                created_at: c.created_at.into(),
                updated_at: c.updated_at.into(),
                links: CommentLinks {
                    self_: Href { href: url.clone() },
                    html: Href { href: html.clone() },
                    pull_request: Href {
                        href: pr_url.clone(),
                    },
                },
                url,
                html_url: html,
                pull_request_url: pr_url,
                author_association: c
                    .user_id
                    .and_then(|u| assoc.get(&u).copied())
                    .unwrap_or(AuthorAssociation::None),
                start_line: c.start_line,
                original_start_line: c.original_start_line,
                start_side: c.start_line.and(c.start_side.clone()),
                line: c.line,
                original_line: c.original_line,
                side: c.side.clone(),
                subject_type: c.subject_type.clone(),
                reactions: ReactionRollup::from_counts(
                    format!("{}/reactions", comment_url(state, owner, name, c.id)),
                    by_comment.get(&c.id).map(Vec::as_slice).unwrap_or(&[]),
                ),
            }
        })
        .collect())
}

/// Compact client shape (model `review_comment`).
/// Compact client row (model `reviewComment`, an extension of the v1 sync
/// protocol; camelCase like the normative models).
pub fn sync_json(c: &ReviewComment) -> Value {
    json!({
        "id": c.id,
        "repoId": c.repo_id,
        "issueId": c.pull_id,
        "reviewId": c.review_id,
        "inReplyToId": c.in_reply_to_id,
        "authorId": c.user_id,
        "body": c.body,
        "path": c.path,
        "commitId": c.commit_id,
        "originalCommitId": c.original_commit_id,
        "subjectType": c.subject_type,
        "side": c.side,
        "startSide": c.start_side,
        "line": c.line,
        "originalLine": c.original_line,
        "startLine": c.start_line,
        "originalStartLine": c.original_start_line,
        "position": c.position,
        "originalPosition": c.original_position,
        "outdated": c.is_outdated(),
        "resolvedAt": ts(c.resolved_at),
        "resolvedById": c.resolved_by_id,
        "createdAt": Timestamp::from(c.created_at),
        "updatedAt": Timestamp::from(c.updated_at),
    })
}

// ---------------------------------------------------------------------------
// Location computation
// ---------------------------------------------------------------------------

/// Where a new comment attaches, from the API parameters.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct LocationInput {
    pub path: Option<String>,
    pub position: Option<i64>,
    pub line: Option<i64>,
    pub side: Option<String>,
    pub start_line: Option<i64>,
    pub start_side: Option<String>,
    pub subject_type: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Location {
    pub path: String,
    pub commit_id: String,
    pub diff_hunk: String,
    pub subject_type: &'static str,
    pub side: Option<&'static str>,
    pub start_side: Option<&'static str>,
    pub line: Option<i32>,
    pub start_line: Option<i32>,
    pub position: Option<i32>,
    pub original_position: Option<i32>,
}

fn thread_err(field: &str, message: &str) -> ApiError {
    ApiError::invalid_field(FieldError::custom(
        "PullRequestReviewComment",
        &format!("pull_request_review_thread.{field}"),
        message,
    ))
}

fn parse_side(s: Option<&str>, field: &str) -> ApiResult<Option<Side>> {
    match s {
        None => Ok(None),
        Some("RIGHT") => Ok(Some(Side::Right)),
        Some("LEFT") => Ok(Some(Side::Left)),
        Some(_) => Err(ApiError::invalid_field(FieldError::invalid(
            "PullRequestReviewComment",
            field,
        ))),
    }
}

fn side_str(s: Side) -> &'static str {
    match s {
        Side::Left => "LEFT",
        Side::Right => "RIGHT",
    }
}

/// Resolve a comment location against the PR diff at `commit_id`.
pub async fn locate(
    state: &AppState,
    pull: &Pull,
    commit_id: &str,
    input: &LocationInput,
) -> ApiResult<Location> {
    let path = input
        .path
        .clone()
        .filter(|p| !p.is_empty())
        .ok_or_else(|| {
            ApiError::invalid_field(FieldError::missing_field(
                "PullRequestReviewComment",
                "path",
            ))
        })?;
    let base = pull
        .pr
        .merge_base_sha
        .clone()
        .unwrap_or_else(|| pull.pr.base_sha.clone());
    let diff = git::diff(state, pull.pr.repo_id, &base, commit_id).await?;
    let file = diff
        .files
        .iter()
        .find(|f| f.filename == path)
        .ok_or_else(|| thread_err("path", "could not be resolved"))?;
    let at_head = commit_id == pull.pr.head_sha;

    if input.subject_type.as_deref() == Some("file") {
        return Ok(Location {
            path,
            commit_id: commit_id.to_string(),
            diff_hunk: String::new(),
            subject_type: "file",
            side: None,
            start_side: None,
            line: None,
            start_line: None,
            position: None,
            original_position: None,
        });
    }
    let patch_text = file
        .patch
        .as_deref()
        .ok_or_else(|| thread_err("diff_hunk", "can't be blank"))?;
    let lines = patch::parse_patch(patch_text);

    let (target, side) = if let Some(line) = input.line {
        let side = parse_side(input.side.as_deref(), "side")?.unwrap_or(Side::Right);
        let t = u32::try_from(line)
            .ok()
            .and_then(|l| patch::find_line(&lines, side, l))
            .ok_or_else(|| thread_err("line", "could not be resolved"))?;
        (t, side)
    } else if let Some(pos) = input.position {
        let t = u32::try_from(pos)
            .ok()
            .and_then(|p| patch::find_position(&lines, p))
            .ok_or_else(|| thread_err("position", "could not be resolved"))?;
        let side = if t.kind == patch::LineKind::Delete {
            Side::Left
        } else {
            Side::Right
        };
        (t, side)
    } else {
        return Err(ApiError::invalid_field(FieldError::missing_field(
            "PullRequestReviewComment",
            "line",
        )));
    };
    let line_no = match side {
        Side::Right => target.new,
        Side::Left => target.old,
    }
    .map(|n| n as i32);

    let mut start = None;
    if let Some(sl) = input.start_line {
        let ss = parse_side(input.start_side.as_deref(), "start_side")?.unwrap_or(side);
        let st = u32::try_from(sl)
            .ok()
            .and_then(|l| patch::find_line(&lines, ss, l))
            .ok_or_else(|| thread_err("start_line", "could not be resolved"))?;
        if st.position > target.position {
            return Err(thread_err("start_line", "must precede the end line."));
        }
        if st.hunk_start != target.hunk_start {
            return Err(thread_err(
                "start_line",
                "must be part of the same hunk as the line.",
            ));
        }
        if st.position < target.position {
            start = Some((sl as i32, ss));
        }
    }
    let position = target.position as i32;
    Ok(Location {
        path,
        commit_id: commit_id.to_string(),
        diff_hunk: patch::diff_hunk(patch_text, &lines, target),
        subject_type: "line",
        side: Some(side_str(side)),
        start_side: start.map(|(_, s)| side_str(s)),
        line: line_no,
        start_line: start.map(|(l, _)| l),
        position: at_head.then_some(position),
        original_position: Some(position),
    })
}

/// Insert a review comment (inside `tx`) and record its sync action.
#[allow(clippy::too_many_arguments)]
pub async fn insert(
    tx: &mut Tx,
    pull: &Pull,
    review_id: i64,
    user_id: i64,
    body: &str,
    loc: &Location,
    in_reply_to: Option<i64>,
    visible: bool,
) -> ApiResult<ReviewComment> {
    let row: ReviewComment = sqlx::query_as(&format!(
        "INSERT INTO pr_review_comments (pull_id, repo_id, review_id, in_reply_to_id, user_id,
                body, path, commit_id, original_commit_id, diff_hunk, subject_type, side,
                start_side, line, original_line, start_line, original_start_line, position,
                original_position)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $8, $9, $10, $11, $12, $13, $13, $14, $14, $15, $16)
         RETURNING {}",
        ReviewComment::COLUMNS
    ))
    .bind(pull.id())
    .bind(pull.pr.repo_id)
    .bind(review_id)
    .bind(in_reply_to)
    .bind(user_id)
    .bind(body)
    .bind(&loc.path)
    .bind(&loc.commit_id)
    .bind(&loc.diff_hunk)
    .bind(loc.subject_type)
    .bind(loc.side)
    .bind(loc.start_side)
    .bind(loc.line)
    .bind(loc.start_line)
    .bind(loc.position)
    .bind(loc.original_position)
    .fetch_one(&mut **tx)
    .await?;
    if visible {
        sqlx::query(
            "UPDATE pull_requests SET review_comments_count = review_comments_count + 1
              WHERE issue_id = $1",
        )
        .bind(pull.id())
        .execute(&mut **tx)
        .await?;
        tx.sync(
            &bgh_core::sync::repo_scope(pull.pr.repo_id),
            "reviewComment",
            row.id,
            SyncAction::Insert,
            &sync_json(&row),
        )
        .await?;
    }
    Ok(row)
}

/// Location for a reply: same thread as the root comment.
pub fn reply_location(root: &ReviewComment) -> Location {
    Location {
        path: root.path.clone(),
        commit_id: root.commit_id.clone(),
        diff_hunk: root.diff_hunk.clone(),
        subject_type: if root.subject_type == "file" {
            "file"
        } else {
            "line"
        },
        side: match root.side.as_deref() {
            Some("LEFT") => Some("LEFT"),
            Some(_) => Some("RIGHT"),
            None => None,
        },
        start_side: match root.start_side.as_deref() {
            Some("LEFT") => Some("LEFT"),
            Some(_) => Some("RIGHT"),
            None => None,
        },
        line: root.line.or(root.original_line),
        start_line: root.start_line.or(root.original_start_line),
        position: root.position,
        original_position: root.original_position,
    }
}

async fn find_comment(state: &AppState, repo_id: i64, id: i64) -> ApiResult<ReviewComment> {
    let c: ReviewComment = sqlx::query_as(&format!(
        "SELECT {} FROM pr_review_comments c WHERE c.id = $1 AND c.repo_id = $2",
        ReviewComment::COLUMNS
    ))
    .bind(id)
    .bind(repo_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)?;
    Ok(c)
}

/// A comment visible to `viewer` (pending review comments only to their author).
async fn visible_comment(
    state: &AppState,
    repo_id: i64,
    id: i64,
    viewer: Option<i64>,
) -> ApiResult<ReviewComment> {
    let c = find_comment(state, repo_id, id).await?;
    if let Some(rid) = c.review_id {
        let st: Option<String> = sqlx::query_scalar("SELECT state FROM pr_reviews WHERE id = $1")
            .bind(rid)
            .fetch_optional(&state.db)
            .await?;
        if st.as_deref() == Some(model::PENDING) && c.user_id != viewer {
            return Err(ApiError::NotFound);
        }
    }
    Ok(c)
}

/// Visibility predicate on `c` (alias of pr_review_comments) for `$viewer`.
fn visible_sql(viewer_param: &str) -> String {
    format!(
        "(c.review_id IS NULL OR NOT EXISTS (SELECT 1 FROM pr_reviews r WHERE r.id = c.review_id
            AND r.state = 'PENDING' AND r.user_id IS DISTINCT FROM {viewer_param}))"
    )
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    pub sort: Option<String>,
    pub direction: Option<String>,
    pub since: Option<String>,
}

fn order(q: &ListQuery) -> ApiResult<(&'static str, &'static str)> {
    let col = match q.sort.as_deref().unwrap_or("created") {
        "created" => "c.created_at",
        "updated" => "c.updated_at",
        _ => {
            return Err(ApiError::invalid_field(FieldError::invalid(
                "PullRequestReviewComment",
                "sort",
            )));
        }
    };
    let dir = match q.direction.as_deref().unwrap_or("asc") {
        "asc" => "ASC",
        "desc" => "DESC",
        _ => {
            return Err(ApiError::invalid_field(FieldError::invalid(
                "PullRequestReviewComment",
                "direction",
            )));
        }
    };
    Ok((col, dir))
}

fn parse_since(s: Option<&str>) -> ApiResult<Option<chrono::DateTime<chrono::Utc>>> {
    match s {
        None => Ok(None),
        Some(s) => chrono::DateTime::parse_from_rfc3339(s)
            .map(|d| Some(d.with_timezone(&chrono::Utc)))
            .map_err(|_| {
                ApiError::invalid_field(FieldError::invalid("PullRequestReviewComment", "since"))
            }),
    }
}

pub async fn list_for_pull(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    Query(q): Query<ListQuery>,
) -> ApiResult<Page<ReviewCommentJson>> {
    let (access, pull) = load_pull(&state, auth.as_ref(), &owner, &repo, number).await?;
    let (col, dir) = order(&q)?;
    let since = parse_since(q.since.as_deref())?;
    let rows: Vec<ReviewComment> = sqlx::query_as(&format!(
        "SELECT {} FROM pr_review_comments c
          WHERE c.pull_id = $1 AND {} AND ($3::timestamptz IS NULL OR c.updated_at >= $3)
          ORDER BY {col} {dir}, c.id {dir} LIMIT $4 OFFSET $5",
        db::prefixed("c", ReviewComment::COLUMNS),
        visible_sql("$2")
    ))
    .bind(pull.id())
    .bind(auth.user_id())
    .bind(since)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let page = p.page(rows);
    let items = render(&state, &access, &page.items).await?;
    Ok(Page {
        items,
        link: page.link,
    })
}

pub async fn list_for_repo(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo)): Path<(String, String)>,
    Query(q): Query<ListQuery>,
) -> ApiResult<Page<ReviewCommentJson>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let (col, dir) = order(&q)?;
    let since = parse_since(q.since.as_deref())?;
    let rows: Vec<ReviewComment> = sqlx::query_as(&format!(
        "SELECT {} FROM pr_review_comments c
          WHERE c.repo_id = $1 AND {} AND ($3::timestamptz IS NULL OR c.updated_at >= $3)
          ORDER BY {col} {dir}, c.id {dir} LIMIT $4 OFFSET $5",
        db::prefixed("c", ReviewComment::COLUMNS),
        visible_sql("$2")
    ))
    .bind(access.repo.id)
    .bind(auth.user_id())
    .bind(since)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let page = p.page(rows);
    let items = render(&state, &access, &page.items).await?;
    Ok(Page {
        items,
        link: page.link,
    })
}

pub async fn get(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, id)): Path<(String, String, i64)>,
) -> ApiResult<axum::Json<ReviewCommentJson>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let c = visible_comment(&state, access.repo.id, id, auth.user_id()).await?;
    Ok(axum::Json(render(&state, &access, &[c]).await?.remove(0)))
}

#[derive(Debug, Deserialize)]
pub struct CreateBody {
    pub body: Option<String>,
    pub commit_id: Option<String>,
    pub in_reply_to: Option<i64>,
    #[serde(flatten)]
    pub location: LocationInput,
}

/// Create a single comment (wrapped in its own COMMENTED review, like
/// GitHub) or a reply.
#[allow(clippy::too_many_arguments)]
async fn create_comment(
    state: &AppState,
    auth: &AuthContext,
    access: &RepoAccess,
    pull: &Pull,
    body: &str,
    commit_id: Option<&str>,
    in_reply_to: Option<i64>,
    location: &LocationInput,
) -> ApiResult<ReviewComment> {
    if body.trim().is_empty() {
        return Err(ApiError::invalid_field(FieldError::missing_field(
            "PullRequestReviewComment",
            "body",
        )));
    }
    if pull.issue.locked && access.permission < Permission::Write {
        return Err(ApiError::forbidden(
            "Unable to create comment because issue is locked.",
        ));
    }
    let (loc, reply_to) = if let Some(parent_id) = in_reply_to {
        let parent = visible_comment(state, access.repo.id, parent_id, Some(auth.user.id)).await?;
        if parent.pull_id != pull.id() {
            return Err(ApiError::NotFound);
        }
        let root = if let Some(r) = parent.in_reply_to_id {
            find_comment(state, access.repo.id, r).await?
        } else {
            parent
        };
        (reply_location(&root), Some(root.id))
    } else {
        let commit = commit_id.unwrap_or(&pull.pr.head_sha).to_string();
        if !bgh_git::is_sha(&commit) {
            return Err(ApiError::invalid_field(FieldError::invalid(
                "PullRequestReviewComment",
                "commit_id",
            )));
        }
        (locate(state, pull, &commit, location).await?, None)
    };
    let pending_review: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM pr_reviews WHERE pull_id = $1 AND user_id = $2 AND state = 'PENDING'",
    )
    .bind(pull.id())
    .bind(auth.user.id)
    .fetch_optional(&state.db)
    .await?;
    let mut tx = Tx::begin(state).await?;
    // Replies while a pending review exists go into that review (GitHub).
    let (review_id, visible) = match pending_review {
        Some(id) if reply_to.is_some() => (id, false),
        _ => {
            let id: i64 = sqlx::query_scalar(
                "INSERT INTO pr_reviews (pull_id, repo_id, user_id, body, state, commit_id, submitted_at)
                 VALUES ($1, $2, $3, '', 'COMMENTED', $4, now()) RETURNING id",
            )
            .bind(pull.id())
            .bind(access.repo.id)
            .bind(auth.user.id)
            .bind(&loc.commit_id)
            .fetch_one(&mut *tx)
            .await?;
            let review: model::Review = sqlx::query_as(&format!(
                "SELECT {} FROM pr_reviews WHERE id = $1",
                model::Review::COLUMNS
            ))
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
            tx.sync(
                &access.scope(),
                "review",
                id,
                SyncAction::Insert,
                &crate::reviews::sync_json(&review),
            )
            .await?;
            (id, true)
        }
    };
    let row = insert(
        &mut tx,
        pull,
        review_id,
        auth.user.id,
        body,
        &loc,
        reply_to,
        visible,
    )
    .await?;
    sqlx::query("UPDATE issues SET updated_at = now() WHERE id = $1")
        .bind(pull.id())
        .execute(&mut *tx)
        .await?;
    if visible {
        tx.emit(Event::PullRequestReviewCommentCreated {
            repo_id: access.repo.id,
            pull_id: pull.id(),
            comment_id: row.id,
            actor_id: auth.user.id,
        });
    }
    crate::json::sync_pull(&mut tx, &access.scope(), pull.id()).await?;
    tx.commit().await?;
    Ok(row)
}

pub async fn create(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    Json(body): Json<CreateBody>,
) -> ApiResult<(StatusCode, axum::Json<ReviewCommentJson>)> {
    let (access, pull) = load_pull(&state, Some(&auth), &owner, &repo, number).await?;
    access.require_not_archived()?;
    let row = create_comment(
        &state,
        &auth,
        &access,
        &pull,
        body.body.as_deref().unwrap_or(""),
        body.commit_id.as_deref(),
        body.in_reply_to,
        &body.location,
    )
    .await?;
    Ok((
        StatusCode::CREATED,
        axum::Json(render(&state, &access, &[row]).await?.remove(0)),
    ))
}

#[derive(Debug, Deserialize)]
pub struct ReplyBody {
    pub body: Option<String>,
}

pub async fn reply(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, number, comment_id)): Path<(String, String, i64, i64)>,
    Json(body): Json<ReplyBody>,
) -> ApiResult<(StatusCode, axum::Json<ReviewCommentJson>)> {
    let (access, pull) = load_pull(&state, Some(&auth), &owner, &repo, number).await?;
    access.require_not_archived()?;
    let row = create_comment(
        &state,
        &auth,
        &access,
        &pull,
        body.body.as_deref().unwrap_or(""),
        None,
        Some(comment_id),
        &LocationInput::default(),
    )
    .await?;
    Ok((
        StatusCode::CREATED,
        axum::Json(render(&state, &access, &[row]).await?.remove(0)),
    ))
}

#[derive(Debug, Deserialize)]
pub struct EditBody {
    pub body: Option<String>,
}

pub async fn edit(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, id)): Path<(String, String, i64)>,
    Json(body): Json<EditBody>,
) -> ApiResult<axum::Json<ReviewCommentJson>> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    let c = visible_comment(&state, access.repo.id, id, Some(auth.user.id)).await?;
    if c.user_id != Some(auth.user.id) {
        access.require(Permission::Write)?;
    }
    access.require_not_archived()?;
    let text = body.body.filter(|b| !b.trim().is_empty()).ok_or_else(|| {
        ApiError::invalid_field(FieldError::missing_field(
            "PullRequestReviewComment",
            "body",
        ))
    })?;
    let mut tx = Tx::begin(&state).await?;
    let row: ReviewComment = sqlx::query_as(&format!(
        "UPDATE pr_review_comments SET body = $2, updated_at = now() WHERE id = $1 RETURNING {}",
        ReviewComment::COLUMNS
    ))
    .bind(id)
    .bind(&text)
    .fetch_one(&mut *tx)
    .await?;
    tx.sync(
        &access.scope(),
        "reviewComment",
        id,
        SyncAction::Update,
        &sync_json(&row),
    )
    .await?;
    tx.emit(Event::PullRequestReviewCommentEdited {
        repo_id: access.repo.id,
        pull_id: row.pull_id,
        comment_id: id,
        actor_id: auth.user.id,
    });
    tx.commit().await?;
    Ok(axum::Json(render(&state, &access, &[row]).await?.remove(0)))
}

pub async fn delete(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, id)): Path<(String, String, i64)>,
) -> ApiResult<StatusCode> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    let c = visible_comment(&state, access.repo.id, id, Some(auth.user.id)).await?;
    if c.user_id != Some(auth.user.id) {
        access.require(Permission::Write)?;
    }
    access.require_not_archived()?;
    let pending: bool = match c.review_id {
        Some(r) => sqlx::query_scalar("SELECT state = 'PENDING' FROM pr_reviews WHERE id = $1")
            .bind(r)
            .fetch_optional(&state.db)
            .await?
            .unwrap_or(false),
        None => false,
    };
    let mut tx = Tx::begin(&state).await?;
    // Promote the first reply to thread root so the thread survives.
    if c.in_reply_to_id.is_none() {
        let first: Option<i64> = sqlx::query_scalar(
            "SELECT id FROM pr_review_comments WHERE in_reply_to_id = $1 ORDER BY id LIMIT 1",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(first) = first {
            sqlx::query(
                "UPDATE pr_review_comments SET in_reply_to_id = NULL,
                        resolved_at = $2, resolved_by_id = $3 WHERE id = $1",
            )
            .bind(first)
            .bind(c.resolved_at)
            .bind(c.resolved_by_id)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "UPDATE pr_review_comments SET in_reply_to_id = $2 WHERE in_reply_to_id = $1",
            )
            .bind(id)
            .bind(first)
            .execute(&mut *tx)
            .await?;
        }
    }
    sqlx::query("DELETE FROM reactions WHERE subject_type = $1 AND subject_id = $2")
        .bind(SUBJECT)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM pr_review_comments WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    if !pending {
        sqlx::query(
            "UPDATE pull_requests SET review_comments_count = greatest(review_comments_count - 1, 0)
              WHERE issue_id = $1",
        )
        .bind(c.pull_id)
        .execute(&mut *tx)
        .await?;
        tx.sync(
            &access.scope(),
            "reviewComment",
            id,
            SyncAction::Delete,
            &json!({"id": id}),
        )
        .await?;
        crate::json::sync_pull(&mut tx, &access.scope(), c.pull_id).await?;
        tx.emit(Event::PullRequestReviewCommentDeleted {
            repo_id: access.repo.id,
            pull_id: c.pull_id,
            comment_id: id,
            actor_id: auth.user.id,
        });
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Reactions
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct ReactionJson {
    pub id: i64,
    pub node_id: String,
    pub user: SimpleUser,
    pub content: String,
    pub created_at: Timestamp,
}

#[derive(sqlx::FromRow)]
struct ReactionRow {
    id: i64,
    user_id: i64,
    content: String,
    created_at: chrono::DateTime<chrono::Utc>,
}

async fn render_reactions(
    state: &AppState,
    rows: Vec<ReactionRow>,
) -> ApiResult<Vec<ReactionJson>> {
    let users = bgh_core::views::users_by_id(state, rows.iter().map(|r| Some(r.user_id))).await?;
    Ok(rows
        .into_iter()
        .map(|r| ReactionJson {
            id: r.id,
            node_id: node_id::encode(NodeType::Reaction, r.id),
            user: SimpleUser::or_ghost(&state.urls, users.get(&r.user_id)),
            content: r.content,
            created_at: r.created_at.into(),
        })
        .collect())
}

#[derive(Debug, Deserialize)]
pub struct ReactionQuery {
    pub content: Option<String>,
}

pub async fn list_reactions(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo, id)): Path<(String, String, i64)>,
    Query(q): Query<ReactionQuery>,
) -> ApiResult<Page<ReactionJson>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    visible_comment(&state, access.repo.id, id, auth.user_id()).await?;
    let rows: Vec<ReactionRow> = sqlx::query_as(
        "SELECT id, user_id, content, created_at FROM reactions
          WHERE subject_type = $1 AND subject_id = $2 AND ($3::text IS NULL OR content = $3)
          ORDER BY id LIMIT $4 OFFSET $5",
    )
    .bind(SUBJECT)
    .bind(id)
    .bind(q.content.as_deref())
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

#[derive(Debug, Deserialize)]
pub struct ReactionBody {
    pub content: Option<String>,
}

pub async fn create_reaction(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, id)): Path<(String, String, i64)>,
    Json(body): Json<ReactionBody>,
) -> ApiResult<(StatusCode, axum::Json<ReactionJson>)> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    let c = visible_comment(&state, access.repo.id, id, Some(auth.user.id)).await?;
    let content = body
        .content
        .filter(|c| REACTION_CONTENTS.contains(&c.as_str()))
        .ok_or_else(|| ApiError::invalid_field(FieldError::invalid("Reaction", "content")))?;
    let existing: Option<ReactionRow> = sqlx::query_as(
        "SELECT id, user_id, content, created_at FROM reactions
          WHERE subject_type = $1 AND subject_id = $2 AND user_id = $3 AND content = $4",
    )
    .bind(SUBJECT)
    .bind(id)
    .bind(auth.user.id)
    .bind(&content)
    .fetch_optional(&state.db)
    .await?;
    if let Some(r) = existing {
        return Ok((
            StatusCode::OK,
            axum::Json(render_reactions(&state, vec![r]).await?.remove(0)),
        ));
    }
    let mut tx = Tx::begin(&state).await?;
    let row: ReactionRow = sqlx::query_as(
        "INSERT INTO reactions (subject_type, subject_id, user_id, content)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (subject_type, subject_id, user_id, content)
            DO UPDATE SET content = EXCLUDED.content
         RETURNING id, user_id, content, created_at",
    )
    .bind(SUBJECT)
    .bind(id)
    .bind(auth.user.id)
    .bind(&content)
    .fetch_one(&mut *tx)
    .await?;
    tx.sync(
        &access.scope(),
        "reaction",
        row.id,
        SyncAction::Insert,
        &json!({"id": row.id, "subjectType": SUBJECT, "subjectId": id,
                "userId": auth.user.id, "content": content, "issueId": c.pull_id}),
    )
    .await?;
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        axum::Json(render_reactions(&state, vec![row]).await?.remove(0)),
    ))
}

pub async fn delete_reaction(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, id, reaction_id)): Path<(String, String, i64, i64)>,
) -> ApiResult<StatusCode> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    visible_comment(&state, access.repo.id, id, Some(auth.user.id)).await?;
    let mut tx = Tx::begin(&state).await?;
    let n = sqlx::query(
        "DELETE FROM reactions WHERE id = $1 AND subject_type = $2 AND subject_id = $3
            AND user_id = $4",
    )
    .bind(reaction_id)
    .bind(SUBJECT)
    .bind(id)
    .bind(auth.user.id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if n == 0 {
        return Err(ApiError::NotFound);
    }
    tx.sync(
        &access.scope(),
        "reaction",
        reaction_id,
        SyncAction::Delete,
        &json!({"id": reaction_id}),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
