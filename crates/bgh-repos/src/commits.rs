//! Commits and compare.
//!
//! * `GET /repos/{o}/{r}/commits` (`sha`, `path`, `author`, `committer`,
//!   `since`, `until`, pagination)
//! * `GET /repos/{o}/{r}/commits/{ref}` (files + stats; `.diff`, `.patch`,
//!   `.sha` media types)
//! * `GET /repos/{o}/{r}/compare/{base}...{head}` (`owner:ref` and
//!   `owner:repo:ref` for cross-fork comparisons; `.diff`/`.patch`)
//!
//! Commit lists are cached by resolved SHA + filters, diffs by the pair of
//! SHAs (immutable), so repeated requests never re-run git.

use std::collections::HashMap;

use axum::Router;
use axum::extract::{RawQuery, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use bgh_core::perms::RepoAccess;
use bgh_core::prelude::*;
use bgh_git::ops::{DiffFile, EMPTY_TREE, GitCli, LogFilter};
use bgh_git::{Commit, is_sha};
use serde::{Deserialize, Serialize};

use crate::cache;
use crate::gitjson::{CommitJson, DiffEntry, RepoRef, commit_emails, commit_json, diff_entries};
use crate::identity::{parse_date, users_by_email};
use crate::media::{self, Media};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/repos/{owner}/{repo}/commits", get(list_commits))
        .route("/repos/{owner}/{repo}/commits/{reference}", get(get_commit))
        .route("/repos/{owner}/{repo}/compare/{*basehead}", get(compare))
}

/// Maximum files returned for one commit / comparison (GitHub: 3000).
const MAX_FILES: usize = 3000;
/// Maximum commits listed in a comparison (GitHub: 250).
const MAX_COMPARE_COMMITS: usize = 250;

fn empty_repo() -> ApiError {
    ApiError::conflict("Git Repository is empty.")
}

/// Resolve `rev` to a commit SHA or 404 (`No commit found for SHA: ..`).
pub async fn resolve(git: &GitCli, rev: &str) -> ApiResult<String> {
    git.resolve_commit(rev).await?.ok_or_else(|| {
        ApiError::Status(
            axum::http::StatusCode::NOT_FOUND,
            format!("No commit found for SHA: {rev}"),
        )
    })
}

/// Changed files between two commits (`from` None = root), cached.
pub async fn cached_diff(
    state: &AppState,
    git: &GitCli,
    from: Option<&str>,
    to: &str,
) -> ApiResult<Vec<DiffFile>> {
    let key = format!("diff:{}:{to}", from.unwrap_or(EMPTY_TREE));
    cache::cached(state, &key, || async {
        let mut files = git.diff(from, to).await?;
        files.truncate(MAX_FILES);
        Ok(files)
    })
    .await
}

/// Render commits with batch-loaded authors/committers.
pub async fn render_commits(
    state: &AppState,
    r: &RepoRef<'_>,
    commits: &[Commit],
) -> ApiResult<Vec<CommitJson>> {
    let users = users_by_email(state, commit_emails(commits)).await?;
    let mut out: Vec<CommitJson> = commits.iter().map(|c| commit_json(r, c, &users)).collect();
    let mut verifications = crate::signatures::verify_commits(state, commits).await?;
    for c in &mut out {
        if let Some(v) = verifications.remove(&c.sha) {
            c.commit.verification = v;
        }
    }
    let shas: Vec<&str> = commits.iter().map(|c| c.sha.as_str()).collect();
    let counts: HashMap<String, i64> = sqlx::query_as(
        "SELECT commit_id, count(*) FROM commit_comments
          WHERE repo_id = $1 AND commit_id = ANY($2) GROUP BY commit_id",
    )
    .bind(r.id)
    .bind(&shas)
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .collect();
    for c in &mut out {
        if let Some(n) = counts.get(&c.sha) {
            c.commit.comment_count = *n;
        }
    }
    Ok(out)
}

#[derive(Debug, Default, Deserialize)]
pub struct ListQuery {
    pub sha: Option<String>,
    pub path: Option<String>,
    pub author: Option<String>,
    pub committer: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
}

/// Turn an `author` / `committer` filter (login or email) into literal
/// patterns: the account's verified emails and noreply address.
async fn identity_patterns(state: &AppState, who: &str) -> ApiResult<Vec<String>> {
    if who.contains('@') {
        return Ok(vec![who.to_string()]);
    }
    let Some(user) = db::User::find_by_login(&state.db, who).await? else {
        return Ok(vec![format!("<{who}@nonexistent.invalid>")]);
    };
    let mut emails: Vec<String> =
        sqlx::query_scalar("SELECT email FROM user_emails WHERE user_id = $1 AND verified")
            .bind(user.id)
            .fetch_all(&state.db)
            .await?;
    emails.push(crate::identity::noreply_email(state, &user));
    Ok(emails.into_iter().map(|e| format!("<{e}>")).collect())
}

fn invalid_time(field: &str) -> ApiError {
    ApiError::invalid_field(FieldError::invalid("Commit", field))
}

/// `GET /repos/{owner}/{repo}/commits`
async fn list_commits(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo)): Path<(String, String)>,
    Query(q): Query<ListQuery>,
) -> ApiResult<Page<CommitJson>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let git = crate::store(&state).cli(access.repo.id)?;
    let rev = q
        .sha
        .clone()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| access.repo.default_branch.clone());
    let head = match git.resolve_commit(&rev).await? {
        Some(h) => h,
        None if q.sha.is_none() => return Err(empty_repo()),
        None => {
            return Err(ApiError::Status(
                axum::http::StatusCode::NOT_FOUND,
                format!("No commit found for SHA: {rev}"),
            ));
        }
    };
    let mut filter = LogFilter {
        rev: head.clone(),
        path: q.path.clone().filter(|p| !p.is_empty()),
        skip: p.offset() as usize,
        limit: p.limit_plus_one() as usize,
        ..Default::default()
    };
    if let Some(a) = q.author.as_deref().filter(|s| !s.is_empty()) {
        filter.authors = identity_patterns(&state, a).await?;
    }
    if let Some(c) = q.committer.as_deref().filter(|s| !s.is_empty()) {
        filter.committers = identity_patterns(&state, c).await?;
    }
    filter.since = match &q.since {
        Some(s) => Some(parse_date(s).ok_or_else(|| invalid_time("since"))?),
        None => None,
    };
    filter.until = match &q.until {
        Some(s) => Some(parse_date(s).ok_or_else(|| invalid_time("until"))?),
        None => None,
    };
    let key = format!(
        "log:{head}:{}:{}:{}:{:?}:{:?}:{}:{}",
        filter.path.as_deref().unwrap_or(""),
        filter.authors.join(","),
        filter.committers.join(","),
        filter.since.map(|t| t.timestamp()),
        filter.until.map(|t| t.timestamp()),
        filter.skip,
        filter.limit
    );
    let shas: Vec<String> =
        cache::cached(&state, &key, || async { Ok(git.log(&filter).await?) }).await?;
    let commits = git.commits(&shas).await?;
    let r = RepoRef::new(&state.urls, &access);
    let page = p.page(commits);
    let items = render_commits(&state, &r, &page.items).await?;
    Ok(Page {
        items,
        link: page.link,
    })
}

/// `GET /repos/{owner}/{repo}/commits/{ref}`
async fn get_commit(
    State(state): State<AppState>,
    auth: MaybeUser,
    headers: HeaderMap,
    Path((owner, repo, reference)): Path<(String, String, String)>,
) -> ApiResult<Response> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let git = crate::store(&state).cli(access.repo.id)?;
    let sha = resolve(&git, &reference).await?;
    let immutable = is_sha(&reference) && reference.eq_ignore_ascii_case(&sha);
    let private = access.repo.is_private();
    let finish = |resp: Response| {
        if immutable {
            media::immutable(resp, private)
        } else {
            resp
        }
    };
    let m = media::media(&headers);
    match m {
        Media::Sha => return Ok(media::body(m, "text/plain; charset=utf-8", sha)),
        Media::Diff | Media::Patch => {
            let commit = git.commit(&sha).await?;
            let bytes = if m == Media::Diff {
                git.diff_text(commit.parents.first().map(String::as_str), &sha)
                    .await?
            } else {
                git.format_patch(None, &sha).await?
            };
            return Ok(finish(media::body(m, "text/plain; charset=utf-8", bytes)));
        }
        _ => {}
    }
    let commit = git.commit(&sha).await?;
    let files = cached_diff(
        &state,
        &git,
        commit.parents.first().map(String::as_str),
        &sha,
    )
    .await?;
    let r = RepoRef::new(&state.urls, &access);
    let mut json = render_commits(&state, &r, std::slice::from_ref(&commit))
        .await?
        .remove(0);
    let (entries, stats) = diff_entries(&r, &sha, &files);
    json.stats = Some(stats);
    json.files = Some(entries);
    Ok(finish(Json(json).into_response()))
}

// ----- compare ---------------------------------------------------------------------

/// `compare` response.
#[derive(Debug, Serialize)]
pub struct Comparison {
    pub url: String,
    pub html_url: String,
    pub permalink_url: String,
    pub diff_url: String,
    pub patch_url: String,
    pub base_commit: CommitJson,
    pub merge_base_commit: CommitJson,
    pub status: &'static str,
    pub ahead_by: u64,
    pub behind_by: u64,
    pub total_commits: u64,
    pub commits: Vec<CommitJson>,
    pub files: Vec<DiffEntry>,
}

/// One side of `base...head`: `ref`, `owner:ref` or `owner:repo:ref`.
struct Side {
    owner: Option<String>,
    repo: Option<String>,
    rev: String,
}

fn parse_side(s: &str) -> Side {
    let parts: Vec<&str> = s.splitn(3, ':').collect();
    match parts.as_slice() {
        [owner, repo, rev] => Side {
            owner: Some(owner.to_string()),
            repo: Some(repo.to_string()),
            rev: rev.to_string(),
        },
        [owner, rev] => Side {
            owner: Some(owner.to_string()),
            repo: None,
            rev: rev.to_string(),
        },
        _ => Side {
            owner: None,
            repo: None,
            rev: s.to_string(),
        },
    }
}

/// Repository a compare side refers to: the base repository itself, an
/// explicit `owner:repo`, or `owner`'s repository in the same fork network.
async fn side_repo(
    state: &AppState,
    auth: Option<&AuthContext>,
    base: &RepoAccess,
    side: &Side,
) -> ApiResult<RepoAccess> {
    let Some(owner) = &side.owner else {
        return Ok(base.clone());
    };
    if let Some(repo) = &side.repo {
        return RepoAccess::load(state, auth, owner, repo).await;
    }
    if owner.eq_ignore_ascii_case(&base.owner.login) {
        return Ok(base.clone());
    }
    let network = base.repo.source_id.unwrap_or(base.repo.id);
    let user = db::User::find_by_login(&state.db, owner)
        .await?
        .ok_or(ApiError::NotFound)?;
    let repo: db::Repository = sqlx::query_as(&format!(
        "SELECT {} FROM repositories
          WHERE owner_id = $1 AND (id = $2 OR source_id = $2) ORDER BY id LIMIT 1",
        db::Repository::COLUMNS
    ))
    .bind(user.id)
    .bind(network)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)?;
    RepoAccess::for_repo(state, auth, repo, user).await
}

#[derive(Serialize, Deserialize)]
struct CompareData {
    merge_base: String,
    ahead: u64,
    behind: u64,
    /// `base..head` commits, oldest first (capped).
    commits: Vec<String>,
}

/// `GET /repos/{owner}/{repo}/compare/{basehead}`
async fn compare(
    State(state): State<AppState>,
    auth: MaybeUser,
    headers: HeaderMap,
    p: Pagination,
    RawQuery(query): RawQuery,
    Path((owner, repo, basehead)): Path<(String, String, String)>,
) -> ApiResult<Response> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let (base_s, head_s) = basehead
        .split_once("...")
        .or_else(|| basehead.split_once(".."))
        .ok_or(ApiError::NotFound)?;
    let (base_side, head_side) = (parse_side(base_s), parse_side(head_s));
    let base_repo = side_repo(&state, auth.as_ref(), &access, &base_side).await?;
    let head_repo = side_repo(&state, auth.as_ref(), &access, &head_side).await?;

    let store = crate::store(&state);
    let base_git = store.cli(base_repo.repo.id)?;
    let head_git = store.cli(head_repo.repo.id)?;
    let base_sha = resolve(&base_git, &base_side.rev).await?;
    let head_sha = resolve(&head_git, &head_side.rev).await?;
    // Operate in the requested repository with both sides' objects visible.
    let git = store
        .cli(access.repo.id)?
        .with_objects_of(base_git.dir())
        .with_objects_of(head_git.dir());

    let data: CompareData = cache::cached(
        &state,
        &format!("compare:{base_sha}:{head_sha}"),
        || async {
            let merge_base = git.merge_base(&base_sha, &head_sha).await?.ok_or_else(|| {
                ApiError::Status(
                    axum::http::StatusCode::NOT_FOUND,
                    format!("No common ancestor between {base_s} and {head_s}."),
                )
            })?;
            let (ahead, behind) = git.ahead_behind(&base_sha, &head_sha).await?;
            let mut commits = git
                .log(&LogFilter {
                    rev: head_sha.clone(),
                    exclude: vec![base_sha.clone()],
                    reverse: true,
                    ..Default::default()
                })
                .await?;
            commits.truncate(MAX_COMPARE_COMMITS);
            Ok(CompareData {
                merge_base,
                ahead,
                behind,
                commits,
            })
        },
    )
    .await?;

    let m = media::media(&headers);
    if matches!(m, Media::Diff | Media::Patch) {
        let bytes = if m == Media::Diff {
            git.diff_text(Some(&data.merge_base), &head_sha).await?
        } else {
            git.format_patch(Some(&data.merge_base), &head_sha).await?
        };
        return Ok(media::body(m, "text/plain; charset=utf-8", bytes));
    }

    // Commits page (GitHub paginates the commit list when asked).
    let paginate = query
        .as_deref()
        .unwrap_or("")
        .split('&')
        .any(|kv| kv.starts_with("page=") || kv.starts_with("per_page="));
    let listed: Vec<String> = if paginate {
        data.commits
            .iter()
            .skip(p.offset() as usize)
            .take(p.limit() as usize)
            .cloned()
            .collect()
    } else {
        data.commits.clone()
    };
    let mut wanted = vec![base_sha.clone(), data.merge_base.clone()];
    wanted.extend(listed.iter().cloned());
    let objs = git.commits(&wanted).await?;
    let by_sha: HashMap<&str, &Commit> = objs.iter().map(|c| (c.sha.as_str(), c)).collect();
    let r = RepoRef::new(&state.urls, &access);
    let users = users_by_email(&state, commit_emails(&objs)).await?;
    let verifications = crate::signatures::verify_commits(&state, &objs).await?;
    let render = |sha: &str| -> ApiResult<CommitJson> {
        let c = by_sha.get(sha).ok_or(ApiError::NotFound)?;
        let mut json = commit_json(&r, c, &users);
        if let Some(v) = verifications.get(sha) {
            json.commit.verification = v.clone();
        }
        Ok(json)
    };
    let files = cached_diff(&state, &git, Some(&data.merge_base), &head_sha).await?;
    let (files, _) = diff_entries(&r, &head_sha, &files);
    let status = match (data.ahead, data.behind) {
        (0, 0) => "identical",
        (_, 0) => "ahead",
        (0, _) => "behind",
        _ => "diverged",
    };
    let html = r.html(&format!("/compare/{basehead}"));
    let base_label = format!("{}:{}", base_repo.owner.login, base_sha);
    let head_label = format!("{}:{}", head_repo.owner.login, head_sha);
    let cmp = Comparison {
        url: r.api(&format!("/compare/{basehead}")),
        permalink_url: r.html(&format!("/compare/{base_label}...{head_label}")),
        diff_url: format!("{html}.diff"),
        patch_url: format!("{html}.patch"),
        html_url: html,
        base_commit: render(&base_sha)?,
        merge_base_commit: render(&data.merge_base)?,
        status,
        ahead_by: data.ahead,
        behind_by: data.behind,
        total_commits: data.ahead,
        commits: listed
            .iter()
            .map(|s| render(s))
            .collect::<ApiResult<Vec<_>>>()?,
        files,
    };
    let mut resp = Json(cmp).into_response();
    if paginate {
        let has_next = p.offset() as usize + listed.len() < data.commits.len();
        if let Some(link) = p.link_header(has_next, Some(data.commits.len() as i64))
            && let Ok(v) = axum::http::HeaderValue::from_str(&link)
        {
            resp.headers_mut().insert(axum::http::header::LINK, v);
        }
    }
    Ok(resp)
}
