//! Links between issues and the pull requests that close them
//! (`issue_pr_links`): closing keywords in a PR body (`Fixes #12`,
//! `closes other/repo#3`, issue URLs) and manual links from the
//! Development section.
//!
//! * Keyword links are reconciled when a PR is opened or its body edited
//!   (listener on [`Event::PullRequestOpened`] / [`Event::PullRequestEdited`]);
//!   each new or removed link writes `connected` / `disconnected` events on
//!   both sides and emits [`Event::IssueConnected`] / [`Event::IssueDisconnected`].
//! * When a PR merges into its repository's default branch
//!   ([`Event::PullRequestMerged`]), every linked open issue is closed as
//!   completed, with a `closed` event carrying the merge commit and the PR.
//! * Manual links: `/_bgh/repos/{o}/{r}/issues/{n}/links` (write access on
//!   both sides).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use bgh_core::perms::{self, RepoAccess};
use bgh_core::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::json::RepoInfo;
use crate::{issues, refs, service};

/// Upper bound of keyword links per pull request.
const MAX_KEYWORD_LINKS: usize = 50;

pub const SOURCE_KEYWORD: &str = "keyword";
pub const SOURCE_MANUAL: &str = "manual";

fn anyhow_err(e: ApiError) -> anyhow::Error {
    anyhow::anyhow!("{e}")
}

/// `owner/name` and privacy of a repository.
#[derive(Debug, Clone, sqlx::FromRow)]
struct RepoName {
    id: i64,
    full_name: String,
    private: bool,
}

async fn repo_names(
    db: impl sqlx::PgExecutor<'_>,
    ids: &[i64],
) -> ApiResult<HashMap<i64, RepoName>> {
    let rows: Vec<RepoName> = sqlx::query_as(
        "SELECT r.id, u.login || '/' || r.name AS full_name, r.visibility <> 'public' AS private
           FROM repositories r JOIN users u ON u.id = r.owner_id WHERE r.id = ANY($1)",
    )
    .bind(ids)
    .fetch_all(db)
    .await?;
    Ok(rows.into_iter().map(|r| (r.id, r)).collect())
}

/// Event data naming `source` (shown on `target`'s timeline). A source in
/// another, non-public repository stays anonymous so the target's readers
/// don't learn about it.
fn source_data(source: &db::Issue, source_repo: &RepoName, target: &db::Issue) -> Value {
    if source.repo_id != target.repo_id && source_repo.private {
        return json!({});
    }
    json!({
        "source_issue_id": source.id,
        "source_number": source.number,
        "source_repository": source_repo.full_name,
        "source_is_pull_request": source.is_pull_request,
    })
}

/// Write `connected` / `disconnected` on both sides, re-sync both rows and
/// emit the domain event.
async fn record_change(
    tx: &mut Tx,
    issue: &db::Issue,
    pull: &db::Issue,
    actor_id: i64,
    connected: bool,
) -> ApiResult<()> {
    let names = repo_names(&mut **tx, &[issue.repo_id, pull.repo_id]).await?;
    let (Some(issue_repo), Some(pull_repo)) = (names.get(&issue.repo_id), names.get(&pull.repo_id))
    else {
        return Err(ApiError::NotFound);
    };
    let event = if connected {
        "connected"
    } else {
        "disconnected"
    };
    service::add_event(
        tx,
        issue,
        Some(actor_id),
        event,
        None,
        source_data(pull, pull_repo, issue),
    )
    .await?;
    service::add_event(
        tx,
        pull,
        Some(actor_id),
        event,
        None,
        source_data(issue, issue_repo, pull),
    )
    .await?;
    tx.sync_models(SyncModel::Issue, &[issue.id, pull.id], SyncAction::Update)
        .await?;
    tx.emit(if connected {
        Event::IssueConnected {
            repo_id: issue.repo_id,
            issue_id: issue.id,
            pull_id: pull.id,
            actor_id,
        }
    } else {
        Event::IssueDisconnected {
            repo_id: issue.repo_id,
            issue_id: issue.id,
            pull_id: pull.id,
            actor_id,
        }
    });
    Ok(())
}

/// Insert a link (no-op if it exists); returns whether it was created.
async fn insert_link(
    tx: &mut Tx,
    issue: &db::Issue,
    pull: &db::Issue,
    source: &str,
    actor_id: i64,
) -> ApiResult<bool> {
    let created = sqlx::query(
        "INSERT INTO issue_pr_links (issue_id, pull_id, source, created_by)
         VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
    )
    .bind(issue.id)
    .bind(pull.id)
    .bind(source)
    .bind(actor_id)
    .execute(&mut **tx)
    .await?
    .rows_affected()
        > 0;
    if created {
        record_change(tx, issue, pull, actor_id, true).await?;
    }
    Ok(created)
}

/// Issues a pull request body links with closing keywords, resolved as
/// `author` sees them: same-repository issues, and issues of other
/// repositories where the author has at least triage.
async fn keyword_targets(
    tx: &mut Tx,
    state: &AppState,
    pull: &db::Issue,
) -> ApiResult<Vec<db::Issue>> {
    let body = pull.body.as_deref().unwrap_or("");
    let found = refs::closing_issue_refs(body, &state.config.base_url);
    if found.is_empty() {
        return Ok(vec![]);
    }
    let Some(repo) = db::Repository::find(&mut **tx, pull.repo_id).await? else {
        return Ok(vec![]);
    };
    let Some(owner) = db::User::find(&mut **tx, repo.owner_id).await? else {
        return Ok(vec![]);
    };
    let author = match pull.author_id {
        Some(id) => db::User::find(&mut **tx, id).await?,
        None => None,
    };
    let info = RepoInfo { repo, owner };
    let mut out: Vec<db::Issue> = vec![];
    let mut triage: HashMap<i64, bool> = HashMap::new();
    for r in found.iter().take(MAX_KEYWORD_LINKS) {
        let Some(issue) = refs::resolve_ref(tx, state, &info, r, author.as_ref()).await? else {
            continue;
        };
        if issue.is_pull_request || out.iter().any(|i| i.id == issue.id) {
            continue;
        }
        if issue.repo_id != pull.repo_id {
            let allowed = match triage.get(&issue.repo_id) {
                Some(a) => *a,
                None => {
                    let a = match db::Repository::find(&mut **tx, issue.repo_id).await? {
                        Some(target) => {
                            perms::repo_permission(
                                &state.db,
                                author.as_ref().map(|a| a.id),
                                &target,
                            )
                            .await?
                                >= Permission::Triage
                        }
                        None => false,
                    };
                    triage.insert(issue.repo_id, a);
                    a
                }
            };
            if !allowed {
                continue;
            }
        }
        out.push(issue);
    }
    Ok(out)
}

/// Bring the keyword links of a pull request in line with its body.
/// Merged pull requests are left alone unless `even_if_merged`.
pub async fn reconcile_keywords(
    state: &AppState,
    pull_id: i64,
    actor_id: i64,
    even_if_merged: bool,
) -> ApiResult<()> {
    let mut tx = Tx::begin(state).await?;
    // Serializes reconciliation per pull request.
    let Ok(pull) = service::lock_issue(&mut tx, pull_id).await else {
        return Ok(());
    };
    if !pull.is_pull_request {
        return Ok(());
    }
    let merged: Option<bool> =
        sqlx::query_scalar("SELECT merged FROM pull_requests WHERE issue_id = $1")
            .bind(pull_id)
            .fetch_optional(&mut *tx)
            .await?;
    if merged.is_none() || (merged == Some(true) && !even_if_merged) {
        return Ok(());
    }
    let wanted = keyword_targets(&mut tx, state, &pull).await?;
    let existing: Vec<(i64, String)> =
        sqlx::query_as("SELECT issue_id, source FROM issue_pr_links WHERE pull_id = $1")
            .bind(pull_id)
            .fetch_all(&mut *tx)
            .await?;
    let linked: HashSet<i64> = existing.iter().map(|(id, _)| *id).collect();
    let wanted_ids: HashSet<i64> = wanted.iter().map(|i| i.id).collect();
    for issue in wanted.iter().filter(|i| !linked.contains(&i.id)) {
        insert_link(&mut tx, issue, &pull, SOURCE_KEYWORD, actor_id).await?;
    }
    for (issue_id, _) in existing
        .iter()
        .filter(|(id, source)| source == SOURCE_KEYWORD && !wanted_ids.contains(id))
    {
        sqlx::query("DELETE FROM issue_pr_links WHERE issue_id = $1 AND pull_id = $2")
            .bind(issue_id)
            .bind(pull_id)
            .execute(&mut *tx)
            .await?;
        let issue = service::issue_by_id(&mut *tx, *issue_id).await?;
        record_change(&mut tx, &issue, &pull, actor_id, false).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Close every open issue linked to a pull request merged into its
/// repository's default branch.
pub async fn close_linked(
    state: &AppState,
    pull_id: i64,
    actor_id: i64,
    merge_sha: &str,
) -> ApiResult<()> {
    // The body may have changed after the last reconciliation was missed.
    reconcile_keywords(state, pull_id, actor_id, true).await?;
    let mut tx = Tx::begin(state).await?;
    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT p.base_ref, r.default_branch FROM pull_requests p
           JOIN repositories r ON r.id = p.repo_id
          WHERE p.issue_id = $1 AND p.merged",
    )
    .bind(pull_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((base_ref, default_branch)) = row else {
        return Ok(());
    };
    if base_ref != default_branch {
        return Ok(());
    }
    let pull = service::issue_by_id(&mut *tx, pull_id).await?;
    let issue_ids: Vec<i64> = sqlx::query_scalar(
        "SELECT k.issue_id FROM issue_pr_links k JOIN issues i ON i.id = k.issue_id
          WHERE k.pull_id = $1 AND i.state = 'open' ORDER BY k.created_at, k.issue_id",
    )
    .bind(pull_id)
    .fetch_all(&mut *tx)
    .await?;
    if issue_ids.is_empty() {
        return Ok(());
    }
    let names = repo_names(&mut *tx, &[pull.repo_id]).await?;
    let pull_repo = names.get(&pull.repo_id).ok_or(ApiError::NotFound)?;
    for id in issue_ids {
        let issue = service::lock_issue(&mut tx, id).await?;
        if issue.state != "open" || issue.is_pull_request {
            continue;
        }
        let mut extra = source_data(&pull, pull_repo, &issue);
        let anonymous = extra.as_object().is_some_and(|m| m.is_empty());
        if issue.repo_id != pull.repo_id
            && let Value::Object(m) = &mut extra
        {
            m.insert("commit_repository".into(), json!(pull_repo.full_name));
        }
        let commit = (!anonymous).then_some(merge_sha);
        if service::set_state_with(
            &mut tx,
            &issue,
            actor_id,
            "closed",
            Some("completed"),
            commit,
            extra,
        )
        .await?
        {
            service::touch_and_sync(&mut tx, issue.id, SyncAction::Update).await?;
        }
    }
    tx.commit().await?;
    Ok(())
}

/// Listener: keyword reconciliation on open / body edit, closing on merge.
pub async fn on_event(state: AppState, event: Arc<Event>) -> anyhow::Result<()> {
    match &*event {
        Event::PullRequestOpened {
            pull_id, actor_id, ..
        } => reconcile_keywords(&state, *pull_id, *actor_id, false)
            .await
            .map_err(anyhow_err),
        Event::PullRequestEdited {
            pull_id,
            actor_id,
            changes,
            ..
        } if changes.get("body").is_some() => {
            reconcile_keywords(&state, *pull_id, *actor_id, false)
                .await
                .map_err(anyhow_err)
        }
        Event::PullRequestMerged {
            pull_id,
            actor_id,
            merge_commit_sha,
            ..
        } => close_linked(&state, *pull_id, *actor_id, merge_commit_sha)
            .await
            .map_err(anyhow_err),
        _ => Ok(()),
    }
}

// ---------------------------------------------------------------------------
// Web-client API
// ---------------------------------------------------------------------------

/// A linked issue or pull request as the Development section shows it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkItem {
    pub id: i64,
    pub repo_id: i64,
    pub repository: String,
    pub number: i64,
    pub title: String,
    pub state: String,
    pub state_reason: Option<String>,
    pub is_pr: bool,
    pub draft: bool,
    pub merged: bool,
    pub html_url: String,
    /// `keyword` | `manual`.
    pub source: String,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, Serialize)]
pub struct LinkedBranch {
    pub name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Links {
    /// Pull requests (for an issue) or issues (for a pull request).
    pub links: Vec<LinkItem>,
    /// Branches named `{number}-…` (issues only; `gh issue develop`).
    pub branches: Vec<LinkedBranch>,
}

#[derive(sqlx::FromRow)]
struct LinkRow {
    id: i64,
    repo_id: i64,
    number: i64,
    title: String,
    state: String,
    state_reason: Option<String>,
    is_pull_request: bool,
    draft: Option<bool>,
    merged: Option<bool>,
    source: String,
    created_at: chrono::DateTime<chrono::Utc>,
}

/// Linked items of `issue` the viewer can read, oldest link first.
async fn load_links(
    state: &AppState,
    viewer: Option<i64>,
    issue: &db::Issue,
) -> ApiResult<Vec<LinkItem>> {
    let (mine, other) = if issue.is_pull_request {
        ("pull_id", "issue_id")
    } else {
        ("issue_id", "pull_id")
    };
    let rows: Vec<LinkRow> = sqlx::query_as(&format!(
        "SELECT i.id, i.repo_id, i.number, i.title, i.state,
                CASE i.state_reason WHEN 'duplicate' THEN 'not_planned' ELSE i.state_reason END AS state_reason,
                i.is_pull_request, p.draft, p.merged, k.source, k.created_at
           FROM issue_pr_links k
           JOIN issues i ON i.id = k.{other}
           LEFT JOIN pull_requests p ON p.issue_id = i.id
          WHERE k.{mine} = $1
          ORDER BY k.created_at, i.id"
    ))
    .bind(issue.id)
    .fetch_all(&state.db)
    .await?;
    let repo_ids: Vec<i64> = rows.iter().map(|r| r.repo_id).collect();
    let repos: Vec<db::Repository> = sqlx::query_as(&format!(
        "SELECT {} FROM repositories WHERE id = ANY($1)",
        db::Repository::COLUMNS
    ))
    .bind(&repo_ids)
    .fetch_all(&state.db)
    .await?;
    let readable = perms::repo_permissions(&state.db, viewer, &repos).await?;
    let names = repo_names(&state.db, &repo_ids).await?;
    let mut out = vec![];
    for r in rows {
        let ok = r.repo_id == issue.repo_id
            || readable
                .get(&r.repo_id)
                .is_some_and(|p| *p >= Permission::Read);
        let Some(name) = names.get(&r.repo_id).filter(|_| ok) else {
            continue;
        };
        let (o, n) = name.full_name.split_once('/').unwrap_or_default();
        out.push(LinkItem {
            id: r.id,
            repo_id: r.repo_id,
            repository: name.full_name.clone(),
            number: r.number,
            title: r.title,
            state: r.state,
            state_reason: r.state_reason,
            is_pr: r.is_pull_request,
            draft: r.draft.unwrap_or(false),
            merged: r.merged.unwrap_or(false),
            html_url: if r.is_pull_request {
                state.urls.pull_html(o, n, r.number)
            } else {
                state.urls.issue_html(o, n, r.number)
            },
            source: r.source,
            created_at: Timestamp::from(r.created_at),
        });
    }
    Ok(out)
}

async fn linked_branches(state: &AppState, repo_id: i64, number: i64) -> Vec<LinkedBranch> {
    let store = bgh_git::RepoStore::from_config(&state.config);
    let prefix = format!("{number}-");
    store
        .read(repo_id, move |r| r.branches())
        .await
        .map(|branches| {
            branches
                .into_iter()
                .filter_map(|b| b.name.strip_prefix("refs/heads/").map(String::from))
                .filter(|name| name.starts_with(&prefix))
                .map(|name| LinkedBranch { name })
                .collect()
        })
        .unwrap_or_default()
}

/// `GET /_bgh/repos/{owner}/{repo}/issues/{number}/links`
pub async fn list(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, number)): Path<(String, String, i64)>,
) -> ApiResult<Json<Links>> {
    let (access, issue) = issues::load(&state, auth.as_ref(), &owner, &repo, number).await?;
    let viewer = auth.as_ref().map(|a| a.user.id);
    let links = load_links(&state, viewer, &issue).await?;
    let branches = if issue.is_pull_request {
        vec![]
    } else {
        linked_branches(&state, access.repo.id, issue.number).await
    };
    Ok(Json(Links { links, branches }))
}

#[derive(Debug, Deserialize)]
pub struct LinkBody {
    /// `owner/name` of the other side; defaults to the same repository.
    pub repository: Option<String>,
    pub number: i64,
}

/// Resolve the other side of a manual link and check write access on it.
async fn counterpart(
    state: &AppState,
    auth: &AuthContext,
    access: &RepoAccess,
    body: &LinkBody,
) -> ApiResult<db::Issue> {
    let other_access = match body.repository.as_deref() {
        Some(full)
            if !full
                .eq_ignore_ascii_case(&format!("{}/{}", access.owner.login, access.repo.name)) =>
        {
            let (o, r) = full.split_once('/').ok_or_else(|| {
                ApiError::invalid_field(FieldError::invalid("IssueLink", "repository"))
            })?;
            RepoAccess::load(state, Some(auth), o, r).await?
        }
        _ => access.clone(),
    };
    other_access.require(Permission::Write)?;
    other_access.require_not_archived()?;
    service::find_issue(&state.db, other_access.repo.id, body.number).await
}

/// `POST /_bgh/repos/{owner}/{repo}/issues/{number}/links` with
/// `{"repository": "owner/name"?, "number": n}`: link an issue to a pull
/// request (either side may be the path). 201 with the linked item, 200
/// if the link already existed.
pub async fn create(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    Json(body): Json<LinkBody>,
) -> ApiResult<(StatusCode, Json<LinkItem>)> {
    let (access, here) = issues::load(&state, Some(&auth), &owner, &repo, number).await?;
    access.require(Permission::Write)?;
    access.require_not_archived()?;
    let there = counterpart(&state, &auth, &access, &body).await?;
    if here.is_pull_request == there.is_pull_request {
        return Err(ApiError::unprocessable(
            "An issue can only be linked to a pull request",
        ));
    }
    let (issue, pull) = if here.is_pull_request {
        (&there, &here)
    } else {
        (&here, &there)
    };
    let mut tx = Tx::begin(&state).await?;
    let created = insert_link(&mut tx, issue, pull, SOURCE_MANUAL, auth.user.id).await?;
    tx.commit().await?;
    let item = load_links(&state, Some(auth.user.id), &here)
        .await?
        .into_iter()
        .find(|l| l.id == there.id)
        .ok_or(ApiError::NotFound)?;
    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(item)))
}

/// `DELETE /_bgh/repos/{owner}/{repo}/issues/{number}/links/{linked_id}`
/// → 204. Only manual links can be removed; keyword links follow the
/// pull request body (422).
pub async fn delete(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, number, linked_id)): Path<(String, String, i64, i64)>,
) -> ApiResult<StatusCode> {
    let (access, here) = issues::load(&state, Some(&auth), &owner, &repo, number).await?;
    access.require(Permission::Write)?;
    access.require_not_archived()?;
    let there = service::issue_by_id(&state.db, linked_id).await?;
    let (issue, pull) = if here.is_pull_request {
        (&there, &here)
    } else {
        (&here, &there)
    };
    let source: Option<String> = sqlx::query_scalar(
        "SELECT source FROM issue_pr_links WHERE issue_id = $1 AND pull_id = $2",
    )
    .bind(issue.id)
    .bind(pull.id)
    .fetch_optional(&state.db)
    .await?;
    let Some(source) = source else {
        return Err(ApiError::NotFound);
    };
    if there.repo_id != access.repo.id {
        let Some(other) = db::Repository::find(&state.db, there.repo_id).await? else {
            return Err(ApiError::NotFound);
        };
        let p = perms::repo_permission(&state.db, Some(auth.user.id), &other).await?;
        if p < Permission::Read {
            return Err(ApiError::NotFound);
        }
        if p < Permission::Write {
            return Err(ApiError::forbidden(
                "Must have write access to both repositories.",
            ));
        }
    }
    if source == SOURCE_KEYWORD {
        return Err(ApiError::unprocessable(
            "This link comes from a closing keyword in the pull request description; edit the description to remove it",
        ));
    }
    let mut tx = Tx::begin(&state).await?;
    let removed = sqlx::query("DELETE FROM issue_pr_links WHERE issue_id = $1 AND pull_id = $2")
        .bind(issue.id)
        .bind(pull.id)
        .execute(&mut *tx)
        .await?
        .rows_affected()
        > 0;
    if removed {
        record_change(&mut tx, issue, pull, auth.user.id, false).await?;
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
