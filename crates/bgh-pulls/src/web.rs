//! Private web-client endpoints (`/_bgh/...`) for features without a
//! GitHub REST equivalent (GraphQL-only on GitHub): review threads
//! (list / resolve / unresolve), draft ⇄ ready for review, auto-merge, and
//! a detailed merge-requirements view for the merge box. The underlying
//! service functions are public for bgh-graphql.
//!
//! Plus the pull request page's data endpoints (pulls-web): a consistent
//! per-PR sync snapshot (`/sync`), adding comments to the viewer's pending
//! review (`/reviews/pending/comments`) and a single file's patch with
//! optional whitespace-insensitive diffing (`/patch`).

use std::collections::BTreeSet;

use axum::extract::State;
use axum::http::StatusCode;
use bgh_core::models::api::SimpleUser;
use bgh_core::node_id::{self, NodeType};
use bgh_core::prelude::*;
use bgh_core::sync::shapes::{self, Filter, Model, Opts};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::comments::{self, ReviewCommentJson};
use crate::jobs::Refresh;
use crate::json::{self, PullRequest};
use crate::model::{PENDING, Pull, Review, ReviewComment};
use crate::pulls::load_pull;
use crate::{git, protection, timeline};

#[derive(Debug, Serialize)]
pub struct ThreadJson {
    pub id: i64,
    pub node_id: String,
    pub path: String,
    pub subject_type: String,
    pub line: Option<i32>,
    pub original_line: Option<i32>,
    pub start_line: Option<i32>,
    pub original_start_line: Option<i32>,
    pub side: Option<String>,
    pub start_side: Option<String>,
    pub is_resolved: bool,
    pub is_outdated: bool,
    pub resolved_by: Option<SimpleUser>,
    pub comments: Vec<ReviewCommentJson>,
}

pub async fn list_threads(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, number)): Path<(String, String, i64)>,
) -> ApiResult<axum::Json<Vec<ThreadJson>>> {
    let (access, pull) = load_pull(&state, auth.as_ref(), &owner, &repo, number).await?;
    let rows: Vec<ReviewComment> = sqlx::query_as(&format!(
        "SELECT {} FROM pr_review_comments c
          WHERE c.pull_id = $1 AND (c.review_id IS NULL OR NOT EXISTS (
                SELECT 1 FROM pr_reviews r WHERE r.id = c.review_id AND r.state = 'PENDING'
                   AND r.user_id IS DISTINCT FROM $2))
          ORDER BY c.id",
        db::prefixed("c", ReviewComment::COLUMNS)
    ))
    .bind(pull.id())
    .bind(auth.user_id())
    .fetch_all(&state.db)
    .await?;
    let rendered = comments::render(&state, &access, &rows).await?;
    let resolvers =
        bgh_core::views::users_by_id(&state, rows.iter().map(|c| c.resolved_by_id)).await?;
    let mut threads: Vec<ThreadJson> = Vec::new();
    for (row, json) in rows.iter().zip(rendered) {
        if row.in_reply_to_id.is_none() {
            threads.push(ThreadJson {
                id: row.id,
                node_id: node_id::encode(NodeType::PullRequestReviewThread, row.id),
                path: row.path.clone(),
                subject_type: row.subject_type.clone(),
                line: row.line,
                original_line: row.original_line,
                start_line: row.start_line,
                original_start_line: row.original_start_line,
                side: row.side.clone(),
                start_side: row.start_side.clone(),
                is_resolved: row.resolved_at.is_some(),
                is_outdated: row.is_outdated(),
                resolved_by: row
                    .resolved_by_id
                    .and_then(|u| resolvers.get(&u))
                    .map(|u| SimpleUser::new(&state.urls, u)),
                comments: vec![json],
            });
        } else if let Some(t) = threads
            .iter_mut()
            .find(|t| Some(t.id) == row.in_reply_to_id)
        {
            t.comments.push(json);
        }
    }
    Ok(axum::Json(threads))
}

/// Resolve or unresolve the thread rooted at `comment_id` (PR author or
/// users with write access).
pub async fn set_resolved(
    state: &AppState,
    access: &RepoAccess,
    pull: &Pull,
    user: &db::User,
    comment_id: i64,
    resolved: bool,
) -> ApiResult<()> {
    if pull.issue.author_id != Some(user.id) {
        access.require(Permission::Write)?;
    }
    let root: ReviewComment = sqlx::query_as(&format!(
        "SELECT {} FROM pr_review_comments WHERE id = $1 AND pull_id = $2",
        ReviewComment::COLUMNS
    ))
    .bind(comment_id)
    .bind(pull.id())
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)?;
    let root_id = root.thread_id();
    let mut tx = Tx::begin(state).await?;
    let row: ReviewComment = sqlx::query_as(&format!(
        "UPDATE pr_review_comments
            SET resolved_at = CASE WHEN $2 THEN coalesce(resolved_at, now()) END,
                resolved_by_id = CASE WHEN $2 THEN coalesce(resolved_by_id, $3) END
          WHERE id = $1 RETURNING {}",
        ReviewComment::COLUMNS
    ))
    .bind(root_id)
    .bind(resolved)
    .bind(user.id)
    .fetch_one(&mut *tx)
    .await?;
    tx.sync_model(SyncModel::ReviewComment, row.id, SyncAction::Update)
        .await?;
    tx.enqueue(&Refresh {
        pull_id: pull.id(),
        codeowners: false,
    })
    .await?;
    tx.emit(if resolved {
        Event::PullRequestReviewThreadResolved {
            repo_id: access.repo.id,
            pull_id: pull.id(),
            comment_id: root_id,
            actor_id: user.id,
        }
    } else {
        Event::PullRequestReviewThreadUnresolved {
            repo_id: access.repo.id,
            pull_id: pull.id(),
            comment_id: root_id,
            actor_id: user.id,
        }
    });
    tx.commit().await?;
    Ok(())
}

pub async fn resolve_thread(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, number, id)): Path<(String, String, i64, i64)>,
) -> ApiResult<axum::Json<serde_json::Value>> {
    let (access, pull) = load_pull(&state, Some(&auth), &owner, &repo, number).await?;
    set_resolved(&state, &access, &pull, &auth.user, id, true).await?;
    Ok(axum::Json(json!({"id": id, "is_resolved": true})))
}

pub async fn unresolve_thread(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, number, id)): Path<(String, String, i64, i64)>,
) -> ApiResult<axum::Json<serde_json::Value>> {
    let (access, pull) = load_pull(&state, Some(&auth), &owner, &repo, number).await?;
    set_resolved(&state, &access, &pull, &auth.user, id, false).await?;
    Ok(axum::Json(json!({"id": id, "is_resolved": false})))
}

/// Mark ready for review (`draft = false`) or convert to draft.
pub async fn set_draft(
    state: &AppState,
    access: &RepoAccess,
    pull: &Pull,
    user: &db::User,
    draft: bool,
) -> ApiResult<()> {
    if pull.issue.author_id != Some(user.id) {
        access.require(Permission::Write)?;
    }
    if !pull.is_open() {
        return Err(ApiError::unprocessable("Pull request is closed"));
    }
    if pull.pr.draft == draft {
        return Ok(());
    }
    let mut tx = Tx::begin(state).await?;
    sqlx::query(
        "UPDATE pull_requests SET draft = $2, mergeable_state = CASE WHEN $2 THEN 'draft' ELSE 'unknown' END
          WHERE issue_id = $1",
    )
    .bind(pull.id())
    .bind(draft)
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE issues SET updated_at = now() WHERE id = $1")
        .bind(pull.id())
        .execute(&mut *tx)
        .await?;
    timeline::record(
        &mut tx,
        access.repo.id,
        pull.id(),
        Some(user.id),
        if draft {
            "convert_to_draft"
        } else {
            "ready_for_review"
        },
        None,
        json!({}),
    )
    .await?;
    json::sync_pull(&mut tx, &access.scope(), pull.id()).await?;
    tx.enqueue(&Refresh {
        pull_id: pull.id(),
        codeowners: !draft,
    })
    .await?;
    tx.emit(if draft {
        Event::PullRequestConvertedToDraft {
            repo_id: access.repo.id,
            pull_id: pull.id(),
            actor_id: user.id,
        }
    } else {
        Event::PullRequestReadyForReview {
            repo_id: access.repo.id,
            pull_id: pull.id(),
            actor_id: user.id,
        }
    });
    tx.commit().await?;
    Ok(())
}

async fn draft_handler(
    state: AppState,
    auth: RequireUser,
    owner: String,
    repo: String,
    number: i64,
    draft: bool,
) -> ApiResult<axum::Json<PullRequest>> {
    let (access, pull) = load_pull(&state, Some(&auth), &owner, &repo, number).await?;
    set_draft(&state, &access, &pull, &auth.user, draft).await?;
    let pull = crate::model::load(&state, access.repo.id, number).await?;
    Ok(axum::Json(
        json::render_full(&state, Some(&auth), &access, &pull).await?,
    ))
}

pub async fn ready_for_review(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, number)): Path<(String, String, i64)>,
) -> ApiResult<axum::Json<PullRequest>> {
    draft_handler(state, auth, owner, repo, number, false).await
}

pub async fn convert_to_draft(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, number)): Path<(String, String, i64)>,
) -> ApiResult<axum::Json<PullRequest>> {
    draft_handler(state, auth, owner, repo, number, true).await
}

#[derive(Debug, Serialize)]
pub struct Requirements {
    pub mergeable: Option<bool>,
    pub rebaseable: Option<bool>,
    pub mergeable_state: String,
    pub protected: bool,
    pub blockers: Vec<String>,
    /// Every unmet requirement with its source (classic rule or ruleset).
    pub requirements: Vec<protection::Blocker>,
    pub approvals: i64,
    pub required_approvals: i64,
    pub changes_requested: bool,
    pub behind: bool,
    pub unstable: bool,
    pub required_checks: Vec<String>,
    pub linear_history: bool,
    pub allowed_merge_methods: Vec<&'static str>,
    pub can_bypass: bool,
    /// Latest deployment of the head commit per environment ("This branch
    /// was successfully deployed").
    pub deployments: Vec<bgh_core::deployments::EnvironmentDeployment>,
}

pub async fn requirements(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, number)): Path<(String, String, i64)>,
) -> ApiResult<axum::Json<Requirements>> {
    let (access, pull) = load_pull(&state, auth.as_ref(), &owner, &repo, number).await?;
    let rules = protection::rules_for(&state.db, access.repo.id, &pull.pr.base_ref).await?;
    let ev = protection::evaluate(&state, &access.repo, &pull, &rules).await?;
    // Whether the viewer may merge without meeting the requirements.
    let can_bypass = match auth.as_ref() {
        Some(a) if access.permission >= Permission::Write => {
            let actor = bgh_repos::protection::Actor::for_user(
                &state,
                &access.owner,
                a.user.id,
                access.permission,
            )
            .await?;
            !rules.restricts(&actor) && ev.blockers.iter().all(|b| rules.bypassed_by(b, &actor))
        }
        _ => false,
    };
    let mut methods = Vec::new();
    if access.repo.allow_merge_commit && !rules.linear_history {
        methods.push("merge");
    }
    if access.repo.allow_squash_merge {
        methods.push("squash");
    }
    if access.repo.allow_rebase_merge {
        methods.push("rebase");
    }
    Ok(axum::Json(Requirements {
        mergeable: pull.pr.mergeable,
        rebaseable: pull.pr.rebaseable,
        mergeable_state: pull.pr.mergeable_state.clone(),
        protected: rules.protected,
        blockers: ev.messages(),
        requirements: ev.blockers.clone(),
        approvals: ev.approvals,
        required_approvals: rules
            .reviews
            .as_ref()
            .map(|r| r.required_approving_review_count)
            .unwrap_or(0),
        changes_requested: ev.changes_requested,
        behind: ev.behind,
        unstable: ev.unstable,
        required_checks: rules
            .checks
            .as_ref()
            .map(protection::CheckRules::contexts)
            .unwrap_or_default(),
        linear_history: rules.linear_history,
        allowed_merge_methods: methods,
        can_bypass,
        deployments: bgh_core::deployments::latest_for_sha(
            &state.db,
            access.repo.id,
            &pull.pr.head_sha,
        )
        .await?,
    }))
}

// ---------------------------------------------------------------------------
// Pull request page data (pulls-web)
// ---------------------------------------------------------------------------

/// Compact rows of `model` (the shared sync shapes) as plain JSON.
async fn load_rows(
    conn: &mut sqlx::PgConnection,
    opts: Opts,
    model: Model,
    filter: Filter<'_>,
) -> ApiResult<Vec<Value>> {
    Ok(shapes::load(conn, model, filter, opts)
        .await?
        .into_iter()
        .map(|r| r.data)
        .collect())
}

#[derive(sqlx::FromRow)]
struct ReactionSyncRow {
    id: i64,
    subject_id: i64,
    user_id: i64,
    content: String,
}

/// `GET /_bgh/repos/{o}/{r}/pulls/{n}/sync`: every row the pull request
/// page needs beyond partial sync (review comments incl. the viewer's
/// pending ones, reviews incl. the viewer's pending one, reactions on the
/// comments, check suites/runs and commit statuses of the head, the
/// viewer's viewed files, referenced users), read in one `REPEATABLE READ`
/// snapshot; `lastSyncId` is the sync watermark taken just before it, so
/// applying deltas `> lastSyncId` afterwards is safe.
pub async fn pull_sync(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, number)): Path<(String, String, i64)>,
) -> ApiResult<axum::Json<Value>> {
    let (access, pull) = load_pull(&state, auth.as_ref(), &owner, &repo, number).await?;
    let viewer = auth.user_id();
    let repo_id = access.repo.id;
    // The commit-order watermark, taken before the snapshot: every action
    // <= it is committed, so the snapshot reflects it.
    let last_sync_id = bgh_core::seqlog::advance(&state.db, bgh_core::seqlog::Log::Sync).await?;
    let mut tx = state.db.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    // Re-read the head inside the snapshot (a push may have landed since).
    let head_sha: String =
        sqlx::query_scalar("SELECT head_sha FROM pull_requests WHERE issue_id = $1")
            .bind(pull.id())
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(ApiError::NotFound)?;

    // Rows come from the shared sync shapes (`bgh_core::sync::shapes`), so
    // this snapshot equals what bootstrap/partial sync and deltas send.
    let pull_ids = [pull.id()];
    let opts = Opts {
        viewer,
        ..Opts::default()
    };
    let comment_rows = load_rows(
        &mut tx,
        opts,
        Model::ReviewComment,
        Filter::Issues(&pull_ids),
    )
    .await?;
    let review_rows = load_rows(&mut tx, opts, Model::Review, Filter::Issues(&pull_ids)).await?;
    let comment_ids: Vec<i64> = comment_rows
        .iter()
        .filter_map(|c| c["id"].as_i64())
        .collect();
    // The viewer-independent per-user reaction rows (who reacted with
    // what) are not a synced model; rows carry `reactions` counts.
    let reaction_rows: Vec<ReactionSyncRow> = if comment_ids.is_empty() {
        Vec::new()
    } else {
        sqlx::query_as(
            "SELECT id, subject_id, user_id, content FROM reactions
              WHERE subject_type = $1 AND subject_id = ANY($2) ORDER BY id",
        )
        .bind(comments::SUBJECT)
        .bind(&comment_ids)
        .fetch_all(&mut *tx)
        .await?
    };
    let suite_ids: Vec<i64> =
        sqlx::query_scalar("SELECT id FROM check_suites WHERE repo_id = $1 AND head_sha = $2")
            .bind(repo_id)
            .bind(&head_sha)
            .fetch_all(&mut *tx)
            .await?;
    let run_ids: Vec<i64> =
        sqlx::query_scalar("SELECT id FROM check_runs WHERE repo_id = $1 AND head_sha = $2")
            .bind(repo_id)
            .bind(&head_sha)
            .fetch_all(&mut *tx)
            .await?;
    let status_ids: Vec<i64> =
        sqlx::query_scalar("SELECT id FROM commit_statuses WHERE repo_id = $1 AND sha = $2")
            .bind(repo_id)
            .bind(&head_sha)
            .fetch_all(&mut *tx)
            .await?;
    let suites = load_rows(&mut tx, opts, Model::CheckSuite, Filter::Ids(&suite_ids)).await?;
    let runs = load_rows(&mut tx, opts, Model::CheckRun, Filter::Ids(&run_ids)).await?;
    let status_rows =
        load_rows(&mut tx, opts, Model::CommitStatus, Filter::Ids(&status_ids)).await?;
    // The viewer's "Viewed" files (private rows: only with a viewer).
    let viewed_rows = match viewer {
        Some(_) => load_rows(&mut tx, opts, Model::ViewedFile, Filter::Issues(&pull_ids)).await?,
        None => Vec::new(),
    };

    let mut user_ids = BTreeSet::new();
    for (model, rows) in [
        (Model::ReviewComment, &comment_rows),
        (Model::Review, &review_rows),
        (Model::CommitStatus, &status_rows),
    ] {
        for row in rows {
            shapes::referenced_users(model.name(), row, &mut user_ids);
        }
    }
    user_ids.extend(reaction_rows.iter().map(|r| r.user_id));
    let user_ids: Vec<i64> = user_ids.into_iter().collect();
    let users = load_rows(&mut tx, opts, Model::User, Filter::Ids(&user_ids)).await?;
    tx.commit().await?;

    let pull_id = pull.id();
    Ok(axum::Json(json!({
        "lastSyncId": last_sync_id,
        "models": {
            "reviewComment": comment_rows,
            "review": review_rows,
            "reaction": reaction_rows.iter().map(|r| json!({
                "id": r.id, "subjectType": comments::SUBJECT, "subjectId": r.subject_id,
                "userId": r.user_id, "content": r.content, "issueId": pull_id,
            })).collect::<Vec<_>>(),
            "checkSuite": suites,
            "checkRun": runs,
            "commitStatus": status_rows,
            "viewedFile": viewed_rows,
            "user": users,
        }
    })))
}

/// `POST /_bgh/repos/{o}/{r}/pulls/{n}/reviews/pending/comments`: add a
/// comment (or a reply, `in_reply_to`) to the viewer's pending review,
/// creating that review first. Pending rows are private: no sync actions.
pub async fn create_pending_comment(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    Json(body): Json<comments::CreateBody>,
) -> ApiResult<(StatusCode, axum::Json<Value>)> {
    let (access, pull) = load_pull(&state, Some(&auth), &owner, &repo, number).await?;
    access.require_not_archived()?;
    let text = body.body.as_deref().unwrap_or("");
    if text.trim().is_empty() {
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
    let review_commit = body
        .commit_id
        .clone()
        .unwrap_or_else(|| pull.pr.head_sha.clone());
    if !bgh_git::is_sha(&review_commit) {
        return Err(ApiError::invalid_field(FieldError::invalid(
            "PullRequestReviewComment",
            "commit_id",
        )));
    }
    let (loc, reply_to) = if let Some(parent_id) = body.in_reply_to {
        let parent =
            comments::visible_comment(&state, access.repo.id, parent_id, Some(auth.user.id))
                .await?;
        if parent.pull_id != pull.id() {
            return Err(ApiError::NotFound);
        }
        let root = match parent.in_reply_to_id {
            Some(r) => comments::find_comment(&state, access.repo.id, r).await?,
            None => parent,
        };
        (comments::reply_location(&root), Some(root.id))
    } else {
        (
            comments::locate(&state, &pull, &review_commit, &body.location).await?,
            None,
        )
    };
    let mut tx = Tx::begin(&state).await?;
    let existing: Option<Review> = sqlx::query_as(&format!(
        "SELECT {} FROM pr_reviews WHERE pull_id = $1 AND user_id = $2 AND state = 'PENDING'
          FOR UPDATE",
        Review::COLUMNS
    ))
    .bind(pull.id())
    .bind(auth.user.id)
    .fetch_optional(&mut *tx)
    .await?;
    let review = match existing {
        Some(r) => r,
        None => sqlx::query_as(&format!(
            "INSERT INTO pr_reviews (pull_id, repo_id, user_id, body, state, commit_id, submitted_at)
             VALUES ($1, $2, $3, '', $4, $5, NULL) RETURNING {}",
            Review::COLUMNS
        ))
        .bind(pull.id())
        .bind(access.repo.id)
        .bind(auth.user.id)
        .bind(PENDING)
        .bind(&review_commit)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| match bgh_core::db::unique_violation(&e).as_deref() {
            Some("pr_reviews_one_pending_key") => {
                ApiError::conflict("A pending review was created concurrently; retry")
            }
            _ => e.into(),
        })?,
    };
    let row = comments::insert(
        &mut tx,
        &pull,
        review.id,
        auth.user.id,
        text,
        &loc,
        reply_to,
        false,
    )
    .await?;
    // Private rows (not synced): rendered by the shared shapes for the
    // viewer, so they equal what `/sync` returns.
    let opts = Opts {
        viewer: Some(auth.user.id),
        ..Opts::default()
    };
    let review_json = shapes::load_one(&mut tx, Model::Review, review.id, opts)
        .await?
        .map(|r| r.data);
    let comment_json = shapes::load_one(&mut tx, Model::ReviewComment, row.id, opts)
        .await?
        .map(|r| r.data);
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        axum::Json(json!({
            "review": review_json,
            "comment": comment_json,
        })),
    ))
}

/// Largest single-file patch returned by [`file_patch`].
pub const FILE_PATCH_MAX: usize = 5 * 1024 * 1024;

#[derive(Debug, Deserialize)]
pub struct PatchQuery {
    pub path: Option<String>,
    pub w: Option<String>,
    /// Commit range (P38, see [`crate::ranges`]); default: the PR diff.
    pub base_sha: Option<String>,
    pub head_sha: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct FilePatch {
    pub filename: String,
    pub previous_filename: Option<String>,
    pub status: &'static str,
    pub additions: u64,
    pub deletions: u64,
    pub patch: Option<String>,
    pub truncated: bool,
}

/// `GET /_bgh/repos/{o}/{r}/pulls/{n}/patch?path=…&w=1`: one file of the
/// PR diff (merge base → head, or `base_sha`/`head_sha`), optionally
/// ignoring whitespace.
pub async fn file_patch(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    Query(q): Query<PatchQuery>,
) -> ApiResult<axum::Json<FilePatch>> {
    let (_access, pull) = load_pull(&state, auth.as_ref(), &owner, &repo, number).await?;
    let path = q
        .path
        .filter(|p| !p.is_empty())
        .ok_or_else(|| ApiError::invalid_field(FieldError::missing_field("PullRequest", "path")))?;
    let ignore_ws = matches!(q.w.as_deref(), Some("1" | "true"));
    let range = crate::ranges::RangeQuery {
        base_sha: q.base_sha,
        head_sha: q.head_sha,
    };
    let (base, head) = crate::ranges::resolve_range(&state, &pull, &range).await?;
    let file = bgh_git::patch::diff_file(
        &git::store(&state),
        pull.pr.repo_id,
        &base,
        &head,
        &path,
        ignore_ws,
        FILE_PATCH_MAX,
    )
    .await?
    .ok_or(ApiError::NotFound)?;
    Ok(axum::Json(FilePatch {
        status: file.status.as_str(),
        filename: file.filename,
        previous_filename: file.previous_filename,
        additions: file.additions,
        deletions: file.deletions,
        patch: file.patch,
        truncated: file.patch_truncated,
    }))
}
