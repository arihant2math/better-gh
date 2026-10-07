//! Pull request CRUD: create, get (JSON / `.diff` / `.patch`), list,
//! update, merged check, commits, files, and PRs for a commit.

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bgh_core::perms;
use bgh_core::prelude::*;
use bgh_core::sync;
use bgh_core::urls::encode_path;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::git;
use crate::jobs::Refresh;
use crate::json::{self, PullRequest};
use crate::model::{self, PULL_FROM, Pull, pull_columns};
use crate::timeline;

/// Resolve `{owner}/{repo}` and PR `{number}` for the caller.
pub async fn load_pull(
    state: &AppState,
    auth: Option<&AuthContext>,
    owner: &str,
    repo: &str,
    number: i64,
) -> ApiResult<(RepoAccess, Pull)> {
    let access = RepoAccess::load(state, auth, owner, repo).await?;
    let pull = model::load(state, access.repo.id, number).await?;
    Ok((access, pull))
}

fn pr_err(field: &str, code: &str) -> ApiError {
    ApiError::invalid_field(FieldError::new("PullRequest", field, code))
}

fn pr_custom(message: impl Into<String>) -> ApiError {
    ApiError::invalid_field(FieldError::custom("PullRequest", "", message))
}

// ---------------------------------------------------------------------------
// Create
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateBody {
    pub title: Option<String>,
    pub head: Option<String>,
    pub head_repo: Option<String>,
    pub base: Option<String>,
    pub body: Option<String>,
    pub maintainer_can_modify: Option<bool>,
    pub draft: Option<bool>,
    pub issue: Option<i64>,
}

/// Find the head repository for `head` (`branch` or `owner:branch`) within
/// the base repository's fork network.
async fn resolve_head(
    state: &AppState,
    auth: &AuthContext,
    access: &RepoAccess,
    head: &str,
    head_repo: Option<&str>,
) -> ApiResult<(db::Repository, db::User, String)> {
    let (owner_login, branch) = match head.split_once(':') {
        Some((o, b)) => (o.to_string(), b.to_string()),
        None => (access.owner.login.clone(), head.to_string()),
    };
    let network = access.repo.source_id.unwrap_or(access.repo.id);
    let repo: Option<db::Repository> = if let Some(full) = head_repo {
        let (o, n) = full
            .split_once('/')
            .ok_or_else(|| pr_err("head_repo", "invalid"))?;
        let owner = db::User::find_by_login(&state.db, o)
            .await?
            .ok_or_else(|| pr_err("head_repo", "invalid"))?;
        db::Repository::find_by_name(&state.db, owner.id, n).await?
    } else if owner_login.eq_ignore_ascii_case(&access.owner.login) {
        Some(access.repo.clone())
    } else {
        sqlx::query_as(&format!(
            "SELECT {} FROM repositories r
              WHERE r.owner_id = (SELECT id FROM users WHERE lower(login) = lower($1))
                AND coalesce(r.source_id, r.id) = $2
              ORDER BY (lower(r.name) = lower($3)) DESC, r.id LIMIT 1",
            db::prefixed("r", db::Repository::COLUMNS)
        ))
        .bind(&owner_login)
        .bind(network)
        .bind(&access.repo.name)
        .fetch_optional(&state.db)
        .await?
    };
    let repo = repo.ok_or_else(|| pr_err("head", "invalid"))?;
    if repo.source_id.unwrap_or(repo.id) != network {
        return Err(pr_err("head", "invalid"));
    }
    let owner = db::User::find(&state.db, repo.owner_id)
        .await?
        .ok_or_else(|| pr_err("head", "invalid"))?;
    if repo.id != access.repo.id {
        // Caller must be able to read the head repository.
        RepoAccess::for_repo(state, Some(auth), repo.clone(), owner.clone())
            .await
            .map_err(|_| pr_err("head", "invalid"))?;
    }
    Ok((repo, owner, branch))
}

pub async fn create(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo)): Path<(String, String)>,
    Json(body): Json<CreateBody>,
) -> ApiResult<Response> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    access.require(Permission::Read)?;
    access.require_not_archived()?;
    let head = body
        .head
        .as_deref()
        .filter(|h| !h.is_empty())
        .ok_or_else(|| ApiError::invalid_field(FieldError::missing_field("PullRequest", "head")))?;
    let base = body
        .base
        .as_deref()
        .filter(|b| !b.is_empty())
        .ok_or_else(|| ApiError::invalid_field(FieldError::missing_field("PullRequest", "base")))?
        .to_string();
    if body.issue.is_none() && body.title.as_deref().is_none_or(|t| t.trim().is_empty()) {
        return Err(ApiError::invalid_field(FieldError::missing_field(
            "PullRequest",
            "title",
        )));
    }
    let (head_repo, head_owner, head_ref) =
        resolve_head(&state, &auth, &access, head, body.head_repo.as_deref()).await?;
    let store = git::store(&state);
    let base_sha = git::branch_tip(&store, access.repo.id, &base)
        .await?
        .ok_or_else(|| pr_err("base", "invalid"))?;
    let head_tip = git::branch_tip(&store, head_repo.id, &head_ref)
        .await?
        .ok_or_else(|| pr_err("head", "invalid"))?;
    if head_repo.id == access.repo.id && head_ref == base {
        return Err(pr_custom(format!(
            "No commits between {base} and {head_ref}"
        )));
    }
    let existing: Option<i64> = sqlx::query_scalar(&format!(
        "SELECT i.number FROM {PULL_FROM}
          WHERE i.repo_id = $1 AND i.state = 'open' AND p.head_repo_id = $2
            AND p.head_ref = $3 AND p.base_ref = $4 LIMIT 1"
    ))
    .bind(access.repo.id)
    .bind(head_repo.id)
    .bind(&head_ref)
    .bind(&base)
    .fetch_optional(&state.db)
    .await?;
    if existing.is_some() {
        return Err(pr_custom(format!(
            "A pull request already exists for {}:{head_ref}.",
            head_owner.login
        )));
    }

    let mut tx = Tx::begin(&state).await?;
    let (issue_id, number) = if let Some(n) = body.issue {
        let issue: db::Issue = sqlx::query_as(&format!(
            "SELECT {} FROM issues WHERE repo_id = $1 AND number = $2 FOR UPDATE",
            db::Issue::COLUMNS
        ))
        .bind(access.repo.id)
        .bind(n)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| pr_err("issue", "invalid"))?;
        if issue.is_pull_request {
            return Err(pr_custom("The issue is already a pull request"));
        }
        if issue.author_id != Some(auth.user.id) && access.permission < Permission::Triage {
            return Err(pr_err("issue", "invalid"));
        }
        sqlx::query(
            "UPDATE issues SET is_pull_request = true, updated_at = now(),
                    title = coalesce($2, title), body = coalesce($3, body) WHERE id = $1",
        )
        .bind(issue.id)
        .bind(body.title.as_deref().filter(|t| !t.trim().is_empty()))
        .bind(body.body.as_deref())
        .execute(&mut *tx)
        .await?;
        (issue.id, issue.number)
    } else {
        let number: i64 = sqlx::query_scalar(
            "UPDATE repositories SET next_issue_number = next_issue_number + 1,
                    open_issues_count = open_issues_count + 1
              WHERE id = $1 RETURNING next_issue_number - 1",
        )
        .bind(access.repo.id)
        .fetch_one(&mut *tx)
        .await?;
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO issues (repo_id, number, title, body, author_id, is_pull_request)
             VALUES ($1, $2, $3, $4, $5, true) RETURNING id",
        )
        .bind(access.repo.id)
        .bind(number)
        .bind(body.title.as_deref().unwrap_or("").trim())
        .bind(body.body.as_deref())
        .bind(auth.user.id)
        .fetch_one(&mut *tx)
        .await?;
        (id, number)
    };

    let head_sha = git::mirror_head(
        &store,
        access.repo.id,
        head_repo.id,
        &head_ref,
        number,
        &head_tip,
    )
    .await?;
    let stats = git::range_stats(&state, access.repo.id, &base_sha, &head_sha).await?;
    if stats.commits == 0 {
        return Err(pr_custom(format!(
            "No commits between {base} and {}",
            if head_repo.id == access.repo.id {
                head_ref.clone()
            } else {
                format!("{}:{head_ref}", head_owner.login)
            }
        )));
    }
    sqlx::query(
        "INSERT INTO pull_requests (issue_id, repo_id, head_repo_id, head_ref, head_sha,
                base_ref, base_sha, merge_base_sha, draft, maintainer_can_modify,
                additions, deletions, changed_files, commits)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)",
    )
    .bind(issue_id)
    .bind(access.repo.id)
    .bind(head_repo.id)
    .bind(&head_ref)
    .bind(&head_sha)
    .bind(&base)
    .bind(&base_sha)
    .bind(&stats.merge_base)
    .bind(body.draft.unwrap_or(false))
    .bind(
        body.maintainer_can_modify
            .unwrap_or(head_repo.id != access.repo.id),
    )
    .bind(stats.additions)
    .bind(stats.deletions)
    .bind(stats.changed_files)
    .bind(stats.commits)
    .execute(&mut *tx)
    .await?;
    let pull = model::find_by_id(&mut *tx, issue_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    tx.sync_issue(issue_id, SyncAction::Insert, true).await?;
    // `openPulls` of the repository row changed.
    tx.sync_model(SyncModel::Repo, access.repo.id, SyncAction::Update)
        .await?;
    tx.enqueue(&Refresh {
        pull_id: issue_id,
        codeowners: !pull.pr.draft,
    })
    .await?;
    tx.emit(Event::PullRequestOpened {
        repo_id: access.repo.id,
        pull_id: issue_id,
        actor_id: auth.user.id,
    });
    tx.commit().await?;

    let out = json::render_full(&state, Some(&auth), &access, &pull).await?;
    let mut resp = (StatusCode::CREATED, axum::Json(&out)).into_response();
    if let Ok(v) = HeaderValue::from_str(&out.url) {
        resp.headers_mut().insert(header::LOCATION, v);
    }
    Ok(resp)
}

// ---------------------------------------------------------------------------
// Get / list
// ---------------------------------------------------------------------------

/// Requested representation from the `Accept` header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaType {
    Json,
    Diff,
    Patch,
}

pub fn media_type(headers: &HeaderMap) -> MediaType {
    let accept = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if accept.contains(".diff") || accept.contains("diff+") {
        MediaType::Diff
    } else if accept.contains(".patch") {
        MediaType::Patch
    } else {
        MediaType::Json
    }
}

/// GitHub's limits for the `.diff` / `.patch` media types.
const DIFF_MAX_FILES: usize = 300;
const DIFF_MAX_LINES: u64 = 20_000;

fn too_large(what: &str) -> ApiError {
    ApiError::Status(
        StatusCode::NOT_ACCEPTABLE,
        format!(
            "Sorry, the diff exceeded the maximum number of {what}. Consider using 'List pull requests files' API or locally cloning the repository instead."
        ),
    )
}

pub async fn diff_response(state: &AppState, pull: &Pull, media: MediaType) -> ApiResult<Response> {
    let diff = git::pull_diff(state, pull).await?;
    if diff.total_files > DIFF_MAX_FILES {
        return Err(too_large(&format!("files ({DIFF_MAX_FILES})")));
    }
    if diff.additions + diff.deletions > DIFF_MAX_LINES {
        return Err(too_large(&format!("lines ({DIFF_MAX_LINES})")));
    }
    let store = git::store(state);
    let base = pull
        .pr
        .merge_base_sha
        .clone()
        .unwrap_or_else(|| pull.pr.base_sha.clone());
    let (body, ct, mt) = match media {
        MediaType::Patch => (
            Body::from_stream(bgh_git::patch::stream_patch(
                &store,
                pull.pr.repo_id,
                &base,
                &pull.pr.head_sha,
            )?),
            "text/x-patch; charset=utf-8",
            "github.v3; param=patch",
        ),
        _ => (
            Body::from_stream(bgh_git::patch::stream_diff(
                &store,
                pull.pr.repo_id,
                &base,
                &pull.pr.head_sha,
            )?),
            "text/x-diff; charset=utf-8",
            "github.v3; param=diff",
        ),
    };
    let mut resp = Response::new(body);
    let h = resp.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(ct));
    h.insert("x-github-media-type", HeaderValue::from_static(mt));
    if let Ok(v) = HeaderValue::from_str(&format!("\"{base}...{}\"", pull.pr.head_sha)) {
        h.insert(header::ETAG, v);
    }
    Ok(resp)
}

pub async fn get(
    State(state): State<AppState>,
    auth: MaybeUser,
    headers: HeaderMap,
    Path((owner, repo, number)): Path<(String, String, i64)>,
) -> ApiResult<Response> {
    let (access, pull) = load_pull(&state, auth.as_ref(), &owner, &repo, number).await?;
    match media_type(&headers) {
        MediaType::Json => {
            let out = json::render_full(&state, auth.as_ref(), &access, &pull).await?;
            Ok(axum::Json(out).into_response())
        }
        m => diff_response(&state, &pull, m).await,
    }
}

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    pub state: Option<String>,
    pub head: Option<String>,
    pub base: Option<String>,
    pub sort: Option<String>,
    pub direction: Option<String>,
}

pub async fn list(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo)): Path<(String, String)>,
    Query(q): Query<ListQuery>,
) -> ApiResult<Page<PullRequest>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let st = match q.state.as_deref().unwrap_or("open") {
        s @ ("open" | "closed") => Some(s),
        "all" => None,
        _ => {
            return Err(ApiError::invalid_field(FieldError::invalid(
                "PullRequest",
                "state",
            )));
        }
    };
    let (head_owner, head_ref) = match q.head.as_deref() {
        Some(h) => match h.split_once(':') {
            Some((o, r)) => (Some(o.to_string()), Some(r.to_string())),
            None => (Some(h.to_string()), None),
        },
        None => (None, None),
    };
    let sort = q.sort.as_deref().unwrap_or("created");
    let column = match sort {
        "created" => "i.created_at",
        "updated" => "i.updated_at",
        "popularity" => "i.comments_count",
        "long-running" => "(coalesce(i.closed_at, now()) - i.created_at)",
        _ => {
            return Err(ApiError::invalid_field(FieldError::invalid(
                "PullRequest",
                "sort",
            )));
        }
    };
    let default_dir = if sort == "created" || q.sort.is_none() {
        "desc"
    } else {
        "asc"
    };
    let dir = match q.direction.as_deref().unwrap_or(default_dir) {
        "asc" => "ASC",
        "desc" => "DESC",
        _ => {
            return Err(ApiError::invalid_field(FieldError::invalid(
                "PullRequest",
                "direction",
            )));
        }
    };
    let rows: Vec<Pull> = sqlx::query_as(&format!(
        "SELECT {} FROM {PULL_FROM}
          WHERE i.repo_id = $1 AND i.is_pull_request
            AND ($2::text IS NULL OR i.state = $2)
            AND ($3::text IS NULL OR p.base_ref = $3)
            AND ($4::text IS NULL OR p.head_repo_id IN (
                    SELECT r.id FROM repositories r JOIN users u ON u.id = r.owner_id
                     WHERE lower(u.login) = lower($4)))
            AND ($5::text IS NULL OR p.head_ref = $5)
          ORDER BY {column} {dir}, i.id {dir}
          LIMIT $6 OFFSET $7",
        pull_columns()
    ))
    .bind(access.repo.id)
    .bind(st)
    .bind(q.base.as_deref())
    .bind(head_owner.as_deref())
    .bind(head_ref.as_deref())
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let page = p.page(rows);
    let items = json::render(&state, auth.as_ref(), &access, &page.items, false).await?;
    Ok(Page {
        items,
        link: page.link,
    })
}

// ---------------------------------------------------------------------------
// Update
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct UpdateBody {
    pub title: Option<String>,
    pub body: Option<String>,
    pub state: Option<String>,
    pub base: Option<String>,
    pub maintainer_can_modify: Option<bool>,
}

pub async fn update(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    Json(body): Json<UpdateBody>,
) -> ApiResult<axum::Json<PullRequest>> {
    let (access, pull) = load_pull(&state, Some(&auth), &owner, &repo, number).await?;
    let is_author = pull.issue.author_id == Some(auth.user.id);
    if !is_author {
        access.require(Permission::Write)?;
    }
    access.require_not_archived()?;
    if let Some(s) = &body.state
        && s != "open"
        && s != "closed"
    {
        return Err(ApiError::invalid_field(FieldError::invalid(
            "PullRequest",
            "state",
        )));
    }
    if let Some(t) = &body.title
        && t.trim().is_empty()
    {
        return Err(ApiError::invalid_field(FieldError::missing_field(
            "PullRequest",
            "title",
        )));
    }
    let store = git::store(&state);
    let new_base = match body.base.as_deref() {
        Some(b) if b != pull.pr.base_ref => {
            if !pull.is_open() {
                return Err(pr_custom(
                    "Cannot change the base branch of a closed pull request.",
                ));
            }
            let sha = git::branch_tip(&store, access.repo.id, b)
                .await?
                .ok_or_else(|| pr_err("base", "invalid"))?;
            if pull.pr.head_repo_id == Some(access.repo.id) && pull.pr.head_ref == b {
                return Err(pr_custom(format!("No commits between {b} and {b}")));
            }
            Some((b.to_string(), sha))
        }
        _ => None,
    };
    let reopen = body.state.as_deref() == Some("open") && !pull.is_open();
    let close = body.state.as_deref() == Some("closed") && pull.is_open();
    if reopen {
        if pull.pr.merged {
            return Err(pr_custom(
                "state cannot be changed. The pull request has been merged.",
            ));
        }
        let head_repo = model::head_repo(&pull);
        if pull.pr.head_repo_id.is_none()
            || git::branch_tip(&store, head_repo, &pull.pr.head_ref)
                .await?
                .is_none()
        {
            return Err(pr_custom(
                "state cannot be changed. The head branch was deleted.",
            ));
        }
        let dup: Option<i64> = sqlx::query_scalar(&format!(
            "SELECT i.number FROM {PULL_FROM}
              WHERE i.repo_id = $1 AND i.state = 'open' AND p.head_repo_id = $2
                AND p.head_ref = $3 AND p.base_ref = $4 AND i.id <> $5 LIMIT 1"
        ))
        .bind(access.repo.id)
        .bind(pull.pr.head_repo_id)
        .bind(&pull.pr.head_ref)
        .bind(&pull.pr.base_ref)
        .bind(pull.id())
        .fetch_optional(&state.db)
        .await?;
        if dup.is_some() {
            return Err(pr_custom(format!(
                "A pull request already exists for {}.",
                pull.pr.head_ref
            )));
        }
    }

    // Base change: recompute range stats outside the transaction.
    let base_stats = match &new_base {
        Some((_, sha)) => {
            Some(git::range_stats(&state, access.repo.id, sha, &pull.pr.head_sha).await?)
        }
        None => None,
    };

    let mut tx = Tx::begin(&state).await?;
    let locked = model::lock(&mut *tx, pull.id())
        .await?
        .ok_or(ApiError::NotFound)?;
    let mut changes = serde_json::Map::new();
    if let Some(t) = body.title.as_deref().map(str::trim)
        && t != locked.issue.title
    {
        changes.insert("title".into(), json!({"from": locked.issue.title}));
        timeline::record(
            &mut tx,
            access.repo.id,
            pull.id(),
            Some(auth.user.id),
            "renamed",
            None,
            json!({"rename": {"from": locked.issue.title, "to": t}}),
        )
        .await?;
    }
    if let Some(b) = &body.body
        && Some(b) != locked.issue.body.as_ref()
    {
        changes.insert("body".into(), json!({"from": locked.issue.body}));
        bgh_core::moderation::record_edit(
            &mut tx,
            access.repo.id,
            bgh_core::moderation::ContentKind::Issue,
            pull.id(),
            auth.user.id,
            locked.issue.body.as_deref().unwrap_or(""),
            b,
        )
        .await?;
    }
    sqlx::query(
        "UPDATE issues SET title = coalesce($2, title), body = coalesce($3, body),
                updated_at = now() WHERE id = $1",
    )
    .bind(pull.id())
    .bind(body.title.as_deref().map(str::trim))
    .bind(body.body.as_deref())
    .execute(&mut *tx)
    .await?;
    if let Some(m) = body.maintainer_can_modify {
        sqlx::query("UPDATE pull_requests SET maintainer_can_modify = $2 WHERE issue_id = $1")
            .bind(pull.id())
            .bind(m && pull.is_cross_repo())
            .execute(&mut *tx)
            .await?;
    }
    if let (Some((b, sha)), Some(stats)) = (&new_base, &base_stats) {
        changes.insert(
            "base".into(),
            json!({"ref": {"from": locked.pr.base_ref}, "sha": {"from": locked.pr.base_sha}}),
        );
        sqlx::query(
            "UPDATE pull_requests SET base_ref = $2, base_sha = $3, merge_base_sha = $4,
                    commits = $5, additions = $6, deletions = $7, changed_files = $8,
                    mergeable = NULL, rebaseable = NULL, mergeable_state = 'unknown'
              WHERE issue_id = $1",
        )
        .bind(pull.id())
        .bind(b)
        .bind(sha)
        .bind(&stats.merge_base)
        .bind(stats.commits)
        .bind(stats.additions)
        .bind(stats.deletions)
        .bind(stats.changed_files)
        .execute(&mut *tx)
        .await?;
        timeline::record(
            &mut tx,
            access.repo.id,
            pull.id(),
            Some(auth.user.id),
            "base_ref_changed",
            None,
            json!({"from": locked.pr.base_ref, "to": b}),
        )
        .await?;
        tx.enqueue(&Refresh {
            pull_id: pull.id(),
            codeowners: false,
        })
        .await?;
    }
    if close {
        close_in_tx(&mut tx, &access, &locked, auth.user.id).await?;
        tx.emit(Event::PullRequestClosed {
            repo_id: access.repo.id,
            pull_id: pull.id(),
            actor_id: auth.user.id,
        });
    }
    if reopen {
        sqlx::query(
            "UPDATE issues SET state = 'open', state_reason = 'reopened', closed_at = NULL,
                    closed_by_id = NULL WHERE id = $1",
        )
        .bind(pull.id())
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE repositories SET open_issues_count = open_issues_count + 1 WHERE id = $1",
        )
        .bind(access.repo.id)
        .execute(&mut *tx)
        .await?;
        timeline::record(
            &mut tx,
            access.repo.id,
            pull.id(),
            Some(auth.user.id),
            "reopened",
            None,
            json!({}),
        )
        .await?;
        tx.enqueue(&crate::jobs::SyncPull { pull_id: pull.id() })
            .await?;
        tx.emit(Event::PullRequestReopened {
            repo_id: access.repo.id,
            pull_id: pull.id(),
            actor_id: auth.user.id,
        });
    }
    if !changes.is_empty() {
        tx.emit(Event::PullRequestEdited {
            repo_id: access.repo.id,
            pull_id: pull.id(),
            actor_id: auth.user.id,
            changes: serde_json::Value::Object(changes),
        });
    }
    let updated = if body.body.is_some() {
        json::sync_pull_with_body(&mut tx, &access.scope(), pull.id()).await?
    } else {
        json::sync_pull(&mut tx, &access.scope(), pull.id()).await?
    };
    tx.commit().await?;
    Ok(axum::Json(
        json::render_full(&state, Some(&auth), &access, &updated).await?,
    ))
}

/// Close an open PR inside `tx` (state, counters, auto-merge, timeline).
pub async fn close_in_tx(
    tx: &mut Tx,
    access: &RepoAccess,
    pull: &Pull,
    actor_id: i64,
) -> ApiResult<()> {
    sqlx::query(
        "UPDATE issues SET state = 'closed', state_reason = NULL, closed_at = now(),
                closed_by_id = $2, updated_at = now() WHERE id = $1",
    )
    .bind(pull.id())
    .bind(actor_id)
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "UPDATE repositories SET open_issues_count = greatest(open_issues_count - 1, 0) WHERE id = $1",
    )
    .bind(access.repo.id)
    .execute(&mut **tx)
    .await?;
    if pull.pr.auto_merge.is_some() {
        sqlx::query("UPDATE pull_requests SET auto_merge = NULL WHERE issue_id = $1")
            .bind(pull.id())
            .execute(&mut **tx)
            .await?;
        timeline::record(
            tx,
            access.repo.id,
            pull.id(),
            Some(actor_id),
            "auto_merge_disabled",
            None,
            json!({"reason": "closed"}),
        )
        .await?;
    }
    timeline::record(
        tx,
        access.repo.id,
        pull.id(),
        Some(actor_id),
        "closed",
        None,
        json!({}),
    )
    .await?;
    Ok(())
}

/// `GET /pulls/{n}/merge`: 204 if merged, 404 otherwise.
pub async fn check_merged(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, number)): Path<(String, String, i64)>,
) -> ApiResult<StatusCode> {
    let (_, pull) = load_pull(&state, auth.as_ref(), &owner, &repo, number).await?;
    if pull.pr.merged {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound)
    }
}

// ---------------------------------------------------------------------------
// Commits and files
// ---------------------------------------------------------------------------

const MAX_COMMITS: usize = 250;

pub async fn commits(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo, number)): Path<(String, String, i64)>,
) -> ApiResult<Page<crate::commits::CommitJson>> {
    let (access, pull) = load_pull(&state, auth.as_ref(), &owner, &repo, number).await?;
    let store = git::store(&state);
    let shas = bgh_git::merge::rev_list(
        &store,
        access.repo.id,
        Some(&pull.pr.base_sha),
        &pull.pr.head_sha,
        MAX_COMMITS,
    )
    .await?;
    let total = shas.len() as i64;
    let page: Vec<String> = shas
        .into_iter()
        .skip(p.offset() as usize)
        .take(p.limit() as usize)
        .collect();
    let commits = store
        .read(access.repo.id, move |r| {
            page.iter()
                .map(|s| r.commit(s))
                .collect::<Result<Vec<_>, _>>()
        })
        .await?;
    let items = crate::commits::render_many(
        &state,
        access.repo.id,
        &access.owner.login,
        &access.repo.name,
        &commits,
    )
    .await?;
    Ok(p.page_with_total(items, total))
}

/// `diff-entry` shape.
#[derive(Debug, Clone, Serialize)]
pub struct DiffEntry {
    pub sha: String,
    pub filename: String,
    pub status: String,
    pub additions: u64,
    pub deletions: u64,
    pub changes: u64,
    pub blob_url: String,
    pub raw_url: String,
    pub contents_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_filename: Option<String>,
}

pub fn diff_entry(
    state: &AppState,
    owner: &str,
    repo: &str,
    head_sha: &str,
    f: &bgh_git::patch::FileDiff,
) -> DiffEntry {
    let path = encode_path(&f.filename);
    DiffEntry {
        sha: f
            .new_sha
            .clone()
            .or_else(|| f.old_sha.clone())
            .unwrap_or_else(|| bgh_git::ZERO_SHA.to_string()),
        filename: f.filename.clone(),
        status: f.status.as_str().to_string(),
        additions: f.additions,
        deletions: f.deletions,
        changes: f.changes(),
        blob_url: state
            .urls
            .html(&format!("/{owner}/{repo}/blob/{head_sha}/{path}")),
        raw_url: state
            .urls
            .html(&format!("/{owner}/{repo}/raw/{head_sha}/{path}")),
        contents_url: state.urls.api(&format!(
            "/repos/{owner}/{repo}/contents/{path}?ref={head_sha}"
        )),
        patch: f.patch.clone(),
        previous_filename: f.previous_filename.clone(),
    }
}

pub async fn files(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo, number)): Path<(String, String, i64)>,
) -> ApiResult<Page<DiffEntry>> {
    let (access, pull) = load_pull(&state, auth.as_ref(), &owner, &repo, number).await?;
    let diff = git::pull_diff(&state, &pull).await?;
    let total = diff.files.len() as i64;
    let items: Vec<DiffEntry> = diff
        .files
        .iter()
        .skip(p.offset() as usize)
        .take(p.limit() as usize)
        .map(|f| {
            diff_entry(
                &state,
                &access.owner.login,
                &access.repo.name,
                &pull.pr.head_sha,
                f,
            )
        })
        .collect();
    Ok(p.page_with_total(items, total))
}

/// `GET /repos/{o}/{r}/commits/{sha}/pulls`: PRs whose head is the commit or
/// that were merged by it.
pub async fn for_commit(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo, sha)): Path<(String, String, String)>,
) -> ApiResult<Page<PullRequest>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let store = git::store(&state);
    let sha = git::resolve_commit(&store, access.repo.id, &sha)
        .await?
        .ok_or_else(|| ApiError::unprocessable(format!("No commit found for SHA: {sha}")))?;
    let rows: Vec<Pull> = sqlx::query_as(&format!(
        "SELECT {} FROM {PULL_FROM}
          WHERE i.repo_id = $1 AND (p.head_sha = $2 OR p.merge_commit_sha = $2)
          ORDER BY i.number DESC LIMIT $3 OFFSET $4",
        pull_columns()
    ))
    .bind(access.repo.id)
    .bind(&sha)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let page = p.page(rows);
    let items = json::render(&state, auth.as_ref(), &access, &page.items, false).await?;
    Ok(Page {
        items,
        link: page.link,
    })
}

/// Raw permission of `user_id` on `repo` ignoring public visibility (who is
/// an actual collaborator), used for reviewer/code owner eligibility.
pub async fn member_permission(
    state: &AppState,
    repo: &db::Repository,
    user_id: i64,
) -> ApiResult<Permission> {
    let mut private = repo.clone();
    private.visibility = "private".into();
    Ok(perms::repo_permission(&state.db, Some(user_id), &private).await?)
}

pub fn scope(repo_id: i64) -> String {
    sync::repo_scope(repo_id)
}
