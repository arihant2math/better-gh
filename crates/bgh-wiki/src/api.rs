//! Private JSON API: `/_bgh/repos/{owner}/{repo}/wiki/...`.

use std::collections::{HashMap, HashSet};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bgh_core::prelude::*;
use bgh_git::write::{self, CommitRequest, FileChange, Identity};
use bgh_git::{GitError, GitRepo, GitResult};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::access::{WikiAccess, store};
use crate::git::ensure_repo;
use crate::pages::{
    self, DEFAULT_BRANCH, FOOTER, HOME, PageFile, SIDEBAR, Snapshot, head_commit, snapshot,
    snapshot_at, validate_title,
};
use crate::render::{WikiLinks, render};

type RepoPath = Path<(String, String)>;
type PagePath = Path<(String, String, String)>;

// ----- JSON shapes -----------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct PageRef {
    pub slug: String,
    pub title: String,
    pub path: String,
}

#[derive(Debug, Serialize)]
pub struct Rendered {
    pub slug: String,
    pub html: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Overview {
    pub exists: bool,
    pub can_edit: bool,
    pub anyone_can_edit: bool,
    pub home: &'static str,
    pub pages: Vec<PageRef>,
    pub sidebar: Option<Rendered>,
    pub footer: Option<Rendered>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitAuthor {
    pub name: String,
    pub email: String,
    pub login: Option<String>,
    pub avatar_url: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CommitJson {
    pub sha: String,
    pub message: String,
    pub author: CommitAuthor,
    pub date: Timestamp,
}

#[derive(Debug, Serialize)]
pub struct PageJson {
    pub slug: String,
    pub title: String,
    pub path: String,
    pub format: &'static str,
    pub raw: String,
    pub html: String,
    /// Blob SHA of the page file.
    pub sha: String,
    /// Latest commit that changed the page (at `rev`).
    pub commit: CommitJson,
    pub sidebar: Option<Rendered>,
    pub footer: Option<Rendered>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub anyone_can_edit: bool,
    pub has_wiki: bool,
}

// ----- helpers ---------------------------------------------------------------

fn text(data: Vec<u8>) -> String {
    String::from_utf8(data).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

fn read_text(r: &GitRepo, page: &PageFile) -> GitResult<String> {
    Ok(text(r.blob(&page.sha)?.data))
}

fn special(r: &GitRepo, snap: &Snapshot, slug: &str) -> GitResult<Option<(PageFile, String)>> {
    match snap.pages.iter().find(|p| p.slug == slug) {
        Some(p) => Ok(Some((p.clone(), read_text(r, p)?))),
        None => Ok(None),
    }
}

fn links<'a>(w: &'a WikiAccess, pages: &'a [PageFile]) -> WikiLinks<'a> {
    WikiLinks {
        owner: w.owner(),
        repo: w.name(),
        pages,
    }
}

fn rendered(
    state: &AppState,
    l: &WikiLinks<'_>,
    item: Option<(PageFile, String)>,
) -> Option<Rendered> {
    item.map(|(p, raw)| Rendered {
        html: render(&state.config.base_url, l, p.format, &raw),
        slug: p.slug,
    })
}

/// Parse an optional JSON body (DELETE may come without one).
fn optional_json<T: DeserializeOwned + Default>(body: &Bytes) -> ApiResult<T> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(T::default());
    }
    serde_json::from_slice(body).map_err(|_| ApiError::bad_request("Problems parsing JSON"))
}

fn message_or(message: Option<String>, default: impl FnOnce() -> String) -> String {
    match message {
        Some(m) if !m.trim().is_empty() => m,
        _ => default(),
    }
}

#[derive(sqlx::FromRow)]
struct EmailUser {
    email: String,
    id: i64,
    login: String,
    avatar_url: Option<String>,
}

#[derive(sqlx::FromRow)]
struct IdUser {
    id: i64,
    login: String,
    avatar_url: Option<String>,
}

/// Build `Commit` JSON, mapping author emails to users in two queries max.
async fn commits_json(
    state: &AppState,
    commits: Vec<bgh_git::Commit>,
) -> ApiResult<Vec<CommitJson>> {
    let noreply = format!("@users.noreply.{}", state.config.hostname());
    let mut emails: HashSet<String> = HashSet::new();
    let mut ids: HashSet<i64> = HashSet::new();
    let mut logins: HashSet<String> = HashSet::new();
    for c in &commits {
        let email = c.author.email.to_lowercase();
        if let Some(local) = email.strip_suffix(&noreply) {
            match local.split_once('+') {
                Some((id, _)) => {
                    if let Ok(id) = id.parse() {
                        ids.insert(id);
                    }
                }
                None => {
                    logins.insert(local.to_string());
                }
            }
        } else {
            emails.insert(email);
        }
    }
    let mut by_email: HashMap<String, (String, String)> = HashMap::new();
    if !emails.is_empty() {
        let list: Vec<String> = emails.into_iter().collect();
        let rows: Vec<EmailUser> = sqlx::query_as(
            "SELECT lower(ue.email) AS email, u.id, u.login, u.avatar_url
               FROM user_emails ue JOIN users u ON u.id = ue.user_id
              WHERE ue.verified AND lower(ue.email) = ANY($1)",
        )
        .bind(&list)
        .fetch_all(&state.db)
        .await?;
        for r in rows {
            let avatar = state.urls.avatar(r.id, r.avatar_url.as_deref());
            by_email.insert(r.email, (r.login, avatar));
        }
    }
    let mut by_id: HashMap<i64, (String, String)> = HashMap::new();
    let mut by_login: HashMap<String, (String, String)> = HashMap::new();
    if !ids.is_empty() || !logins.is_empty() {
        let ids: Vec<i64> = ids.into_iter().collect();
        let logins: Vec<String> = logins.into_iter().collect();
        let rows: Vec<IdUser> = sqlx::query_as(
            "SELECT id, login, avatar_url FROM users WHERE id = ANY($1) OR lower(login) = ANY($2)",
        )
        .bind(&ids)
        .bind(&logins)
        .fetch_all(&state.db)
        .await?;
        for r in rows {
            let v = (
                r.login.clone(),
                state.urls.avatar(r.id, r.avatar_url.as_deref()),
            );
            by_id.insert(r.id, v.clone());
            by_login.insert(r.login.to_lowercase(), v);
        }
    }
    Ok(commits
        .into_iter()
        .map(|c| {
            let email = c.author.email.to_lowercase();
            let user = match email.strip_suffix(&noreply) {
                Some(local) => match local.split_once('+') {
                    Some((id, login)) => id
                        .parse::<i64>()
                        .ok()
                        .and_then(|id| by_id.get(&id))
                        .filter(|(l, _)| l.eq_ignore_ascii_case(login)),
                    None => by_login.get(local),
                },
                None => by_email.get(&email),
            };
            CommitJson {
                sha: c.sha,
                message: c.message.trim_end().to_string(),
                author: CommitAuthor {
                    name: c.author.name,
                    email: c.author.email,
                    login: user.map(|u| u.0.clone()),
                    avatar_url: user.map(|u| u.1.clone()),
                },
                date: Timestamp(c.author.when),
            }
        })
        .collect())
}

/// Commit identity of the signed-in user: name (or login) and primary
/// email, or the noreply address.
async fn identity(state: &AppState, user: &db::User) -> ApiResult<Identity> {
    let email: Option<String> =
        sqlx::query_scalar("SELECT email FROM user_emails WHERE user_id = $1 AND is_primary")
            .bind(user.id)
            .fetch_optional(&state.db)
            .await?;
    let email = email.unwrap_or_else(|| {
        format!(
            "{}+{}@users.noreply.{}",
            user.id,
            user.login,
            state.config.hostname()
        )
    });
    Ok(Identity::new(
        user.name
            .clone()
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| user.login.clone()),
        email,
    ))
}

/// Branch tip and pages for writes.
struct Tip {
    branch: String,
    commit: Option<String>,
    pages: Vec<PageFile>,
}

fn read_tip(r: &GitRepo) -> GitResult<Tip> {
    let branch = r
        .head_branch()?
        .unwrap_or_else(|| DEFAULT_BRANCH.to_string());
    let commit = r
        .find_ref(&format!("refs/heads/{branch}"))?
        .map(|x| x.peeled);
    let pages = match &commit {
        Some(c) => snapshot_at(r, c)?.pages,
        None => Vec::new(),
    };
    Ok(Tip {
        branch,
        commit,
        pages,
    })
}

/// Commit `changes` on top of `tip`; a concurrent update → 409.
async fn commit(
    state: &AppState,
    w: &WikiAccess,
    tip: &Tip,
    changes: &[FileChange],
    message: &str,
    user: &db::User,
) -> ApiResult<String> {
    let author = identity(state, user).await?;
    write::commit_changes(
        &store(state),
        w.repo_id(),
        CommitRequest {
            branch: &tip.branch,
            parent: tip.commit.as_deref(),
            changes,
            message,
            author: &author,
            committer: None,
        },
    )
    .await
    .map_err(|e| match e {
        GitError::Command { ref args, .. } if args.starts_with("update-ref") => {
            ApiError::conflict("The wiki was changed by someone else. Reload and try again.")
        }
        other => other.into(),
    })
}

/// Announce a page write (GitHub's `gollum` webhook, activity `GollumEvent`).
/// The write is a git commit, not a database transaction, so the event is
/// emitted directly (still durable through the outbox writer).
fn gollum(
    state: &AppState,
    w: &WikiAccess,
    user: &db::User,
    action: &str,
    slug: &str,
    sha: &str,
    summary: Option<&str>,
) {
    let html_url = format!(
        "{}/wiki/{}",
        state
            .urls
            .repo_html(&w.access.owner.login, &w.access.repo.name),
        bgh_core::urls::encode_path(slug)
    );
    state.events.emit(Event::WikiPagesUpdated {
        repo_id: w.repo_id(),
        actor_id: user.id,
        pages: serde_json::json!([{
            "page_name": slug,
            "title": pages::title_of(slug),
            "summary": summary,
            "action": action,
            "sha": sha,
            "html_url": html_url,
        }]),
    });
}

struct LoadedPage {
    pages: Vec<PageFile>,
    page: PageFile,
    raw: String,
    commit: bgh_git::Commit,
    sidebar: Option<(PageFile, String)>,
    footer: Option<(PageFile, String)>,
}

fn load_page(r: &GitRepo, rev: Option<&str>, slug: &str) -> GitResult<Option<LoadedPage>> {
    let Some(snap) = snapshot(r, rev)? else {
        return Ok(None);
    };
    let Some(page) = snap.find(slug).cloned() else {
        return Ok(None);
    };
    let raw = read_text(r, &page)?;
    let sidebar = special(r, &snap, SIDEBAR)?;
    let footer = special(r, &snap, FOOTER)?;
    let commit = match r.log(&snap.commit, Some(&page.path), 0, 1)?.pop() {
        Some(c) => c,
        None => r.commit(&snap.commit)?,
    };
    Ok(Some(LoadedPage {
        pages: snap.pages,
        page,
        raw,
        commit,
        sidebar,
        footer,
    }))
}

async fn page_json(
    state: &AppState,
    w: &WikiAccess,
    rev: Option<String>,
    slug: String,
) -> ApiResult<PageJson> {
    let store = store(state);
    if !store.exists(w.repo_id()) {
        return Err(ApiError::NotFound);
    }
    let p = store
        .read(w.repo_id(), move |r| load_page(r, rev.as_deref(), &slug))
        .await?
        .ok_or(ApiError::NotFound)?;
    let l = links(w, &p.pages);
    let html = render(&state.config.base_url, &l, p.page.format, &p.raw);
    let sidebar = rendered(state, &l, p.sidebar);
    let footer = rendered(state, &l, p.footer);
    let commit = commits_json(state, vec![p.commit])
        .await?
        .pop()
        .ok_or(ApiError::NotFound)?;
    Ok(PageJson {
        title: p.page.title(),
        slug: p.page.slug,
        path: p.page.path,
        format: p.page.format,
        raw: p.raw,
        html,
        sha: p.page.sha,
        commit,
        sidebar,
        footer,
    })
}

// ----- handlers --------------------------------------------------------------

/// `GET /_bgh/repos/{owner}/{repo}/wiki`
pub async fn overview(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo)): RepoPath,
) -> ApiResult<Json<Overview>> {
    let w = WikiAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let store = store(&state);
    type Loaded = Option<(
        Vec<PageFile>,
        Option<(PageFile, String)>,
        Option<(PageFile, String)>,
    )>;
    let loaded: Loaded = if store.exists(w.repo_id()) {
        store
            .read(w.repo_id(), |r| {
                let Some(snap) = snapshot(r, None)? else {
                    return Ok(None);
                };
                let sidebar = special(r, &snap, SIDEBAR)?;
                let footer = special(r, &snap, FOOTER)?;
                Ok(Some((snap.pages, sidebar, footer)))
            })
            .await?
    } else {
        None
    };
    let mut out = Overview {
        exists: loaded.is_some(),
        can_edit: w.can_edit(),
        anyone_can_edit: w.anyone_can_edit,
        home: HOME,
        pages: Vec::new(),
        sidebar: None,
        footer: None,
    };
    if let Some((pages, sidebar, footer)) = loaded {
        let l = links(&w, &pages);
        out.sidebar = rendered(&state, &l, sidebar);
        out.footer = rendered(&state, &l, footer);
        out.pages = pages
            .iter()
            .filter(|p| !p.is_special())
            .map(|p| PageRef {
                slug: p.slug.clone(),
                title: p.title(),
                path: p.path.clone(),
            })
            .collect();
    }
    Ok(Json(out))
}

#[derive(Debug, Deserialize)]
pub struct RevQuery {
    pub rev: Option<String>,
}

/// `GET /_bgh/repos/{owner}/{repo}/wiki/pages/{slug}?rev=`
pub async fn get_page(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, slug)): PagePath,
    Query(q): Query<RevQuery>,
) -> ApiResult<Json<PageJson>> {
    let w = WikiAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    Ok(Json(page_json(&state, &w, q.rev, slug).await?))
}

/// `GET /_bgh/repos/{owner}/{repo}/wiki/pages/{slug}/raw?rev=`
pub async fn raw_page(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, slug)): PagePath,
    Query(q): Query<RevQuery>,
) -> ApiResult<Response> {
    let w = WikiAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let store = store(&state);
    if !store.exists(w.repo_id()) {
        return Err(ApiError::NotFound);
    }
    let data = store
        .read(w.repo_id(), move |r| {
            let Some(snap) = snapshot(r, q.rev.as_deref())? else {
                return Ok(None);
            };
            match snap.find(&slug) {
                Some(p) => Ok(Some(r.blob(&p.sha)?.data)),
                None => Ok(None),
            }
        })
        .await?
        .ok_or(ApiError::NotFound)?;
    let mut resp = data.into_response();
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    Ok(resp)
}

#[derive(Debug, Deserialize)]
pub struct CreateBody {
    pub title: Option<String>,
    pub body: Option<String>,
    pub message: Option<String>,
}

fn already_exists() -> ApiError {
    ApiError::invalid_field(FieldError {
        message: Some("A page with this name already exists".into()),
        ..FieldError::already_exists("WikiPage", "title")
    })
}

/// `POST /_bgh/repos/{owner}/{repo}/wiki/pages`
pub async fn create_page(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo)): RepoPath,
    Json(body): Json<CreateBody>,
) -> ApiResult<(StatusCode, Json<PageJson>)> {
    let w = WikiAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    w.require_edit(auth.as_ref())?;
    let user = &auth.as_ref().ok_or_else(ApiError::requires_auth)?.user;
    let slug = validate_title(body.title.as_deref().unwrap_or(""))?;
    let content = body
        .body
        .ok_or_else(|| ApiError::invalid_field(FieldError::missing_field("WikiPage", "body")))?;
    ensure_repo(&state, w.repo_id()).await?;
    let tip = store(&state).read(w.repo_id(), read_tip).await?;
    if pages::find(&tip.pages, &slug).is_some() {
        return Err(already_exists());
    }
    let summary = body.message.clone().filter(|m| !m.trim().is_empty());
    let message = message_or(body.message, || {
        format!("Created {} (markdown)", pages::title_of(&slug))
    });
    let changes = [FileChange::write(format!("{slug}.md"), content)];
    let sha = commit(&state, &w, &tip, &changes, &message, user).await?;
    gollum(&state, &w, user, "created", &slug, &sha, summary.as_deref());
    let page = page_json(&state, &w, Some(sha), slug).await?;
    Ok((StatusCode::CREATED, Json(page)))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateBody {
    pub title: Option<String>,
    pub body: Option<String>,
    pub message: Option<String>,
    pub expected_commit: Option<String>,
}

/// `PUT /_bgh/repos/{owner}/{repo}/wiki/pages/{slug}`
pub async fn update_page(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, slug)): PagePath,
    Json(body): Json<UpdateBody>,
) -> ApiResult<Json<PageJson>> {
    let w = WikiAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    w.require_edit(auth.as_ref())?;
    let user = &auth.as_ref().ok_or_else(ApiError::requires_auth)?.user;
    let new_slug = match body.title.as_deref() {
        Some(t) => Some(validate_title(t)?),
        None => None,
    };
    let store = store(&state);
    if !store.exists(w.repo_id()) {
        return Err(ApiError::NotFound);
    }
    let expected = body.expected_commit.clone();
    // Tip, current page and contents, and whether `expectedCommit` is
    // still current (it contains the page's latest change).
    let (tip, page, raw, fresh) = store
        .read(w.repo_id(), move |r| {
            let tip = read_tip(r)?;
            let Some(page) = pages::find(&tip.pages, &slug).cloned() else {
                return Ok(None);
            };
            let raw = read_text(r, &page)?;
            let fresh = match (&expected, &tip.commit) {
                (Some(exp), Some(tip_sha)) if !exp.is_empty() => {
                    let last = r
                        .log(tip_sha, Some(&page.path), 0, 1)?
                        .pop()
                        .map(|c| c.sha)
                        .unwrap_or_default();
                    match r.resolve(exp)? {
                        Some(_) => match r.resolve_commit(exp) {
                            Ok(exp) => exp == last || r.is_ancestor(&last, &exp)?,
                            Err(_) => false,
                        },
                        None => false,
                    }
                }
                _ => true,
            };
            Ok(Some((tip, page, raw, fresh)))
        })
        .await?
        .ok_or(ApiError::NotFound)?;
    if !fresh {
        return Err(ApiError::conflict(
            "This page was changed by someone else since you started editing it.",
        ));
    }
    let target = new_slug.unwrap_or_else(|| page.slug.clone());
    let renamed = target != page.slug;
    if renamed && pages::find(&tip.pages, &target).is_some_and(|other| other.path != page.path) {
        return Err(already_exists());
    }
    let content = body.body.unwrap_or_else(|| raw.clone());
    if !renamed && content == raw {
        return Ok(Json(page_json(&state, &w, tip.commit, page.slug).await?));
    }
    let new_path = format!("{target}.{}", page.ext());
    let mut changes = Vec::new();
    if new_path != page.path {
        changes.push(FileChange::Delete {
            path: page.path.clone(),
        });
    }
    changes.push(FileChange::write(new_path, content));
    let summary = body.message.clone().filter(|m| !m.trim().is_empty());
    let message = message_or(body.message, || {
        format!("Updated {} ({})", pages::title_of(&target), page.format)
    });
    let sha = commit(&state, &w, &tip, &changes, &message, user).await?;
    gollum(
        &state,
        &w,
        user,
        "edited",
        &target,
        &sha,
        summary.as_deref(),
    );
    Ok(Json(page_json(&state, &w, Some(sha), target).await?))
}

#[derive(Debug, Default, Deserialize)]
pub struct DeleteBody {
    pub message: Option<String>,
}

/// `DELETE /_bgh/repos/{owner}/{repo}/wiki/pages/{slug}`
pub async fn delete_page(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, slug)): PagePath,
    body: Bytes,
) -> ApiResult<StatusCode> {
    let body: DeleteBody = optional_json(&body)?;
    let w = WikiAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    w.require_edit(auth.as_ref())?;
    let user = &auth.as_ref().ok_or_else(ApiError::requires_auth)?.user;
    let store = store(&state);
    if !store.exists(w.repo_id()) {
        return Err(ApiError::NotFound);
    }
    let tip = store.read(w.repo_id(), read_tip).await?;
    let page = pages::find(&tip.pages, &slug)
        .cloned()
        .ok_or(ApiError::NotFound)?;
    let summary = body.message.clone().filter(|m| !m.trim().is_empty());
    let message = message_or(body.message, || {
        format!("Destroyed {} ({})", page.title(), page.format)
    });
    let changes = [FileChange::Delete {
        path: page.path.clone(),
    }];
    let sha = commit(&state, &w, &tip, &changes, &message, user).await?;
    gollum(
        &state,
        &w,
        user,
        "deleted",
        &page.slug,
        &sha,
        summary.as_deref(),
    );
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /_bgh/repos/{owner}/{repo}/wiki/pages/{slug}/history`
pub async fn page_history(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo, slug)): PagePath,
) -> ApiResult<Page<CommitJson>> {
    let w = WikiAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let store = store(&state);
    if !store.exists(w.repo_id()) {
        return Err(ApiError::NotFound);
    }
    let (skip, limit) = (p.offset() as usize, p.limit_plus_one() as usize);
    let first_page = p.page == 1;
    let commits = store
        .read(w.repo_id(), move |r| {
            let Some(snap) = snapshot(r, None)? else {
                return Ok(None);
            };
            // Deleted pages keep their history under the Markdown name.
            let (path, exists) = match snap.find(&slug) {
                Some(page) => (page.path.clone(), true),
                None => (format!("{slug}.md"), false),
            };
            let commits = r.log(&snap.commit, Some(&path), skip, limit)?;
            if !exists && first_page && commits.is_empty() {
                return Ok(None);
            }
            Ok(Some(commits))
        })
        .await?
        .ok_or(ApiError::NotFound)?;
    let page = p.page(commits);
    let link = page.link;
    Ok(Page {
        items: commits_json(&state, page.items).await?,
        link,
    })
}

/// `GET /_bgh/repos/{owner}/{repo}/wiki/history`
pub async fn wiki_history(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo)): RepoPath,
) -> ApiResult<Page<CommitJson>> {
    let w = WikiAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let store = store(&state);
    let (skip, limit) = (p.offset() as usize, p.limit_plus_one() as usize);
    let commits = if store.exists(w.repo_id()) {
        store
            .read(w.repo_id(), move |r| match head_commit(r)? {
                Some(head) => r.log(&head, None, skip, limit),
                None => Ok(Vec::new()),
            })
            .await?
    } else {
        Vec::new()
    };
    let page = p.page(commits);
    let link = page.link;
    Ok(Page {
        items: commits_json(&state, page.items).await?,
        link,
    })
}

#[derive(Debug, Deserialize)]
pub struct RevertBody {
    pub sha: Option<String>,
    pub message: Option<String>,
}

/// `POST /_bgh/repos/{owner}/{repo}/wiki/pages/{slug}/revert`
pub async fn revert_page(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, slug)): PagePath,
    Json(body): Json<RevertBody>,
) -> ApiResult<Json<PageJson>> {
    let w = WikiAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    w.require_edit(auth.as_ref())?;
    let user = &auth.as_ref().ok_or_else(ApiError::requires_auth)?.user;
    let sha = body
        .sha
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| ApiError::invalid_field(FieldError::missing_field("WikiPage", "sha")))?;
    let store = store(&state);
    if !store.exists(w.repo_id()) {
        return Err(ApiError::NotFound);
    }
    struct Plan {
        tip: Tip,
        old: PageFile,
        old_data: Vec<u8>,
        current: Option<PageFile>,
        commit: String,
    }
    enum Outcome {
        NoCommit,
        NoPage,
        Ready(Box<Plan>),
    }
    let lookup_sha = sha.clone();
    let outcome = store
        .read(w.repo_id(), move |r| {
            let commit = match r.resolve(&lookup_sha)? {
                Some(_) => match r.resolve_commit(&lookup_sha) {
                    Ok(c) => c,
                    Err(GitError::NotFound(_)) => return Ok(Outcome::NoCommit),
                    Err(e) => return Err(e),
                },
                None => return Ok(Outcome::NoCommit),
            };
            let then = snapshot_at(r, &commit)?;
            let Some(old) = then.find(&slug).cloned() else {
                return Ok(Outcome::NoPage);
            };
            let old_data = r.blob(&old.sha)?.data;
            let tip = read_tip(r)?;
            let current = pages::find(&tip.pages, &old.slug).cloned();
            Ok(Outcome::Ready(Box::new(Plan {
                tip,
                old,
                old_data,
                current,
                commit,
            })))
        })
        .await?;
    let (tip, old, old_data, current, reverted) = match outcome {
        Outcome::NoCommit => {
            return Err(ApiError::invalid_field(FieldError::custom(
                "WikiPage",
                "sha",
                "commit not found",
            )));
        }
        Outcome::NoPage => {
            return Err(ApiError::unprocessable(
                "The page did not exist at that commit.",
            ));
        }
        Outcome::Ready(plan) => {
            let Plan {
                tip,
                old,
                old_data,
                current,
                commit,
            } = *plan;
            (tip, old, old_data, current, commit)
        }
    };
    if current
        .as_ref()
        .is_some_and(|c| c.path == old.path && c.sha == old.sha)
    {
        return Ok(Json(page_json(&state, &w, tip.commit, old.slug).await?));
    }
    let mut changes = Vec::new();
    if let Some(c) = current.as_ref().filter(|c| c.path != old.path) {
        changes.push(FileChange::Delete {
            path: c.path.clone(),
        });
    }
    changes.push(FileChange::write(old.path.clone(), old_data));
    let summary = body.message.clone().filter(|m| !m.trim().is_empty());
    let message = message_or(body.message, || {
        format!("Reverted {} to {}", old.title(), &reverted[..7])
    });
    let sha = commit(&state, &w, &tip, &changes, &message, user).await?;
    gollum(
        &state,
        &w,
        user,
        "edited",
        &old.slug,
        &sha,
        summary.as_deref(),
    );
    Ok(Json(page_json(&state, &w, Some(sha), old.slug).await?))
}

#[derive(Debug, Deserialize)]
pub struct CompareQuery {
    pub slug: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Compare {
    pub base: String,
    pub head: String,
    pub diff: String,
}

/// `GET /_bgh/repos/{owner}/{repo}/wiki/compare/{base}...{head}?slug=`
pub async fn compare(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, range)): PagePath,
    Query(q): Query<CompareQuery>,
) -> ApiResult<Json<Compare>> {
    let w = WikiAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let (base, head) = range
        .split_once("...")
        .or_else(|| range.split_once(".."))
        .filter(|(b, h)| !b.is_empty() && !h.is_empty())
        .map(|(b, h)| (b.to_string(), h.to_string()))
        .ok_or(ApiError::NotFound)?;
    let store = store(&state);
    if !store.exists(w.repo_id()) {
        return Err(ApiError::NotFound);
    }
    let out = store
        .read(w.repo_id(), move |r| {
            let base = r.resolve_commit(&base)?;
            let head = r.resolve_commit(&head)?;
            let paths: Vec<String> = match q.slug.as_deref().filter(|s| !s.is_empty()) {
                Some(slug) => {
                    let mut paths = Vec::new();
                    for c in [&base, &head] {
                        if let Some(p) = snapshot_at(r, c)?.find(slug)
                            && !paths.contains(&p.path)
                        {
                            paths.push(p.path.clone());
                        }
                    }
                    if paths.is_empty() {
                        return Ok(Compare {
                            base,
                            head,
                            diff: String::new(),
                        });
                    }
                    paths
                }
                None => Vec::new(),
            };
            let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
            let diff = r.diff(&base, &head, &refs)?;
            Ok(Compare { base, head, diff })
        })
        .await?;
    Ok(Json(out))
}

#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    pub q: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SearchResult {
    pub slug: String,
    pub title: String,
    pub snippet: String,
}

#[derive(Debug, Serialize)]
pub struct SearchResults {
    pub results: Vec<SearchResult>,
}

const MAX_RESULTS: usize = 100;
const SNIPPET_CHARS: usize = 200;

fn snippet_of(line: &str, needle: &str) -> String {
    let line = line.trim();
    let chars: Vec<char> = line.chars().collect();
    if chars.len() <= SNIPPET_CHARS {
        return line.to_string();
    }
    // Center the window on the match (char-based, so it is UTF-8 safe).
    let lower: Vec<char> = line.to_lowercase().chars().collect();
    let needle: Vec<char> = needle.chars().collect();
    let pos = if lower.len() == chars.len() && !needle.is_empty() {
        lower
            .windows(needle.len())
            .position(|w| w == needle.as_slice())
            .unwrap_or(0)
    } else {
        0
    };
    let start = pos.saturating_sub(SNIPPET_CHARS / 3);
    let end = (start + SNIPPET_CHARS).min(chars.len());
    let mut s: String = chars[start..end].iter().collect();
    if start > 0 {
        s.insert(0, '…');
    }
    if end < chars.len() {
        s.push('…');
    }
    s
}

/// `GET /_bgh/repos/{owner}/{repo}/wiki/search?q=`
pub async fn search(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo)): RepoPath,
    Query(q): Query<SearchQuery>,
) -> ApiResult<Json<SearchResults>> {
    let w = WikiAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let needle = q.q.unwrap_or_default().trim().to_lowercase();
    let store = store(&state);
    if needle.is_empty() || !store.exists(w.repo_id()) {
        return Ok(Json(SearchResults {
            results: Vec::new(),
        }));
    }
    let results = store
        .read(w.repo_id(), move |r| {
            let Some(snap) = snapshot(r, None)? else {
                return Ok(Vec::new());
            };
            let mut by_title = Vec::new();
            let mut by_content = Vec::new();
            for page in snap.listed() {
                let title = page.title();
                let title_hit = title.to_lowercase().contains(&needle)
                    || page.slug.to_lowercase().contains(&needle);
                let content = match r.blob(&page.sha) {
                    Ok(b) if !b.is_binary() => text(b.data),
                    _ => String::new(),
                };
                let line_hit = content.lines().find(|l| l.to_lowercase().contains(&needle));
                let snippet = match line_hit {
                    Some(line) => snippet_of(line, &needle),
                    None if title_hit => content
                        .lines()
                        .find(|l| !l.trim().is_empty())
                        .map(|l| snippet_of(l, ""))
                        .unwrap_or_default(),
                    None => continue,
                };
                let result = SearchResult {
                    slug: page.slug.clone(),
                    title,
                    snippet,
                };
                if title_hit {
                    by_title.push(result);
                } else {
                    by_content.push(result);
                }
            }
            by_title.extend(by_content);
            by_title.truncate(MAX_RESULTS);
            Ok(by_title)
        })
        .await?;
    Ok(Json(SearchResults { results }))
}

/// `GET /_bgh/repos/{owner}/{repo}/wiki/settings`
pub async fn get_settings(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo)): RepoPath,
) -> ApiResult<Json<Settings>> {
    let w = WikiAccess::load_any(&state, auth.as_ref(), &owner, &repo).await?;
    Ok(Json(Settings {
        anyone_can_edit: w.anyone_can_edit,
        has_wiki: w.access.repo.has_wiki,
    }))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsBody {
    pub anyone_can_edit: Option<bool>,
}

/// `PATCH /_bgh/repos/{owner}/{repo}/wiki/settings` (admin)
pub async fn update_settings(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo)): RepoPath,
    Json(body): Json<SettingsBody>,
) -> ApiResult<Json<Settings>> {
    let w = WikiAccess::load_any(&state, auth.as_ref(), &owner, &repo).await?;
    if auth.as_ref().is_none() {
        return Err(ApiError::requires_auth());
    }
    w.access.require(Permission::Admin)?;
    let anyone = match body.anyone_can_edit {
        Some(v) => {
            sqlx::query_scalar(
                "UPDATE repositories SET wiki_anyone_can_edit = $2
                  WHERE id = $1 RETURNING wiki_anyone_can_edit",
            )
            .bind(w.repo_id())
            .bind(v)
            .fetch_one(&state.db)
            .await?
        }
        None => w.anyone_can_edit,
    };
    Ok(Json(Settings {
        anyone_can_edit: anyone,
        has_wiki: w.access.repo.has_wiki,
    }))
}
