//! Record GitHub-style activity events from domain events.
//!
//! Each event stores a payload snapshot (like GitHub, payloads describe the
//! object at the time of the event); actor/repo/org are rendered at read
//! time.

use std::sync::Arc;

use bgh_core::events::{Event, PushEvent};
use bgh_core::models::api::{MinimalRepository, SimpleUser};
use bgh_core::node_id::{self, NodeType};
use bgh_core::prelude::*;
use bgh_core::time::ts;
use bgh_git::RepoStore;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use crate::render::{IssueContext, issues_by_id};

/// Commits listed in a PushEvent payload.
const PUSH_COMMITS: usize = 20;
/// Commits counted for a PushEvent's `size`.
const PUSH_COUNT_LIMIT: usize = 2048;

/// Insert one activity event for `repo`.
pub async fn insert(
    state: &AppState,
    kind: &str,
    actor_id: i64,
    repo: &db::Repository,
    payload: Value,
) -> anyhow::Result<i64> {
    let owner = db::User::find(&state.db, repo.owner_id).await?;
    let (repo_name, org_id) = match &owner {
        Some(o) => (
            format!("{}/{}", o.login, repo.name),
            o.is_org().then_some(o.id),
        ),
        None => (repo.name.clone(), None),
    };
    // Events are delivered at least once: key the row by the outbox event
    // (and its position within this invocation) so redelivery is a no-op.
    let (event_id, event_seq) = bgh_core::events::effect_key().unzip();
    let id: Option<i64> = sqlx::query_scalar(
        "INSERT INTO activity_events
                (type, actor_id, repo_id, repo_name, org_id, public, payload, event_id, event_seq)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
         ON CONFLICT (event_id, event_seq) WHERE event_id IS NOT NULL DO NOTHING
         RETURNING id",
    )
    .bind(kind)
    .bind(actor_id)
    .bind(repo.id)
    .bind(repo_name)
    .bind(org_id)
    .bind(!repo.is_private())
    .bind(payload)
    .bind(event_id)
    .bind(event_seq)
    .fetch_optional(&state.db)
    .await?;
    match id {
        Some(id) => Ok(id),
        None => Ok(sqlx::query_scalar(
            "SELECT id FROM activity_events WHERE event_id = $1 AND event_seq = $2",
        )
        .bind(event_id)
        .bind(event_seq)
        .fetch_one(&state.db)
        .await?),
    }
}

async fn repo(state: &AppState, id: i64) -> anyhow::Result<Option<db::Repository>> {
    Ok(db::Repository::find(&state.db, id).await?)
}

/// Event listener entry point.
pub async fn on_event(state: AppState, event: Arc<Event>) -> anyhow::Result<()> {
    record(&state, &event).await
}

/// Record activity for one domain event (no-op for events without a
/// GitHub counterpart).
pub async fn record(state: &AppState, event: &Event) -> anyhow::Result<()> {
    if event.is_quiet() {
        return Ok(());
    }
    match event {
        Event::Push(p) => push(state, p).await,
        Event::RepositoryCreated { repo_id, actor_id } => {
            let Some(r) = repo(state, *repo_id).await? else {
                return Ok(());
            };
            if r.fork {
                return Ok(()); // recorded as ForkEvent on the parent
            }
            let payload = json!({
                "ref": null,
                "ref_type": "repository",
                "master_branch": r.default_branch,
                "description": r.description,
                "pusher_type": "user",
            });
            insert(state, "CreateEvent", *actor_id, &r, payload).await?;
            Ok(())
        }
        Event::RepositoryPublicized { repo_id, actor_id } => {
            if let Some(r) = repo(state, *repo_id).await? {
                insert(state, "PublicEvent", *actor_id, &r, json!({})).await?;
            }
            Ok(())
        }
        // GitHub records WatchEvent for stars only (not unstars).
        Event::RepositoryStarred {
            repo_id,
            actor_id,
            starred: true,
        } => {
            if let Some(r) = repo(state, *repo_id).await? {
                insert(
                    state,
                    "WatchEvent",
                    *actor_id,
                    &r,
                    json!({"action": "started"}),
                )
                .await?;
            }
            Ok(())
        }
        Event::RepositoryForked {
            repo_id,
            fork_id,
            actor_id,
        } => {
            let (Some(parent), Some(fork)) =
                (repo(state, *repo_id).await?, repo(state, *fork_id).await?)
            else {
                return Ok(());
            };
            let Some(owner) = db::User::find(&state.db, fork.owner_id).await? else {
                return Ok(());
            };
            let forkee = MinimalRepository::new(&state.urls, &fork, &owner, None);
            insert(
                state,
                "ForkEvent",
                *actor_id,
                &parent,
                json!({ "forkee": forkee }),
            )
            .await?;
            Ok(())
        }
        Event::CollaboratorAdded {
            repo_id,
            user_id,
            actor_id,
            ..
        } => {
            let (Some(r), Some(member)) = (
                repo(state, *repo_id).await?,
                db::User::find(&state.db, *user_id).await?,
            ) else {
                return Ok(());
            };
            let payload = json!({
                "action": "added",
                "member": SimpleUser::new(&state.urls, &member),
            });
            insert(state, "MemberEvent", *actor_id, &r, payload).await?;
            Ok(())
        }
        Event::IssueOpened {
            issue_id, actor_id, ..
        } => issue_event(state, *issue_id, *actor_id, "opened", None).await,
        Event::IssueClosed {
            issue_id, actor_id, ..
        } => issue_event(state, *issue_id, *actor_id, "closed", None).await,
        Event::IssueReopened {
            issue_id, actor_id, ..
        } => issue_event(state, *issue_id, *actor_id, "reopened", None).await,
        Event::IssueEdited {
            issue_id,
            actor_id,
            changes,
            ..
        } => issue_event(state, *issue_id, *actor_id, "edited", Some(changes.clone())).await,
        Event::IssueCommentCreated {
            issue_id,
            comment_id,
            actor_id,
            ..
        } => comment_event(state, *issue_id, *comment_id, *actor_id).await,
        Event::PullRequestOpened {
            pull_id, actor_id, ..
        } => pull_event(state, *pull_id, *actor_id, "opened", false).await,
        Event::PullRequestReopened {
            pull_id, actor_id, ..
        } => pull_event(state, *pull_id, *actor_id, "reopened", false).await,
        Event::PullRequestClosed {
            pull_id, actor_id, ..
        } => pull_event(state, *pull_id, *actor_id, "closed", true).await,
        Event::PullRequestMerged {
            pull_id, actor_id, ..
        } => pull_event(state, *pull_id, *actor_id, "closed", false).await,
        Event::PullRequestReviewSubmitted {
            pull_id,
            review_id,
            actor_id,
            ..
        } => review_event(state, *pull_id, *review_id, *actor_id).await,
        Event::PullRequestReviewCommentCreated {
            pull_id,
            comment_id,
            actor_id,
            ..
        } => review_comment_event(state, *pull_id, *comment_id, *actor_id).await,
        Event::ReleasePublished {
            repo_id,
            release_id,
            actor_id,
        } => release_event(state, *repo_id, *release_id, *actor_id).await,
        Event::CommitCommentCreated {
            repo_id,
            comment_id,
            actor_id,
            ..
        } => commit_comment_event(state, *repo_id, *comment_id, *actor_id).await,
        _ => Ok(()),
    }
}

async fn issue_event(
    state: &AppState,
    issue_id: i64,
    actor_id: i64,
    action: &str,
    changes: Option<Value>,
) -> anyhow::Result<()> {
    let issues = issues_by_id(state, &[issue_id]).await?;
    let Some(issue) = issues.first() else {
        return Ok(());
    };
    if issue.is_pull_request {
        // Pull requests get PullRequestEvent from the pull_request_* events.
        return Ok(());
    }
    let ctx = IssueContext::load(state, &issues).await?;
    let (Some(json), Some(r)) = (ctx.issue(state, issue), ctx.repos.get(&issue.repo_id)) else {
        return Ok(());
    };
    let mut payload = json!({ "action": action, "issue": json });
    if let Some(c) = changes.filter(|c| !c.is_null()) {
        payload["changes"] = c;
    }
    insert(state, "IssuesEvent", actor_id, r, payload).await?;
    Ok(())
}

async fn comment_event(
    state: &AppState,
    issue_id: i64,
    comment_id: i64,
    actor_id: i64,
) -> anyhow::Result<()> {
    let issues = issues_by_id(state, &[issue_id]).await?;
    let Some(issue) = issues.first() else {
        return Ok(());
    };
    let comment: Option<db::Comment> = sqlx::query_as(&format!(
        "SELECT {} FROM comments WHERE id = $1",
        db::Comment::COLUMNS
    ))
    .bind(comment_id)
    .fetch_optional(&state.db)
    .await?;
    let Some(comment) = comment else {
        return Ok(());
    };
    let ctx = IssueContext::load(state, &issues).await?;
    let author = match comment.author_id {
        Some(a) => db::User::find(&state.db, a).await?,
        None => None,
    };
    let (Some(issue_json), Some(comment_json), Some(r)) = (
        ctx.issue(state, issue),
        ctx.comment(state, issue, &comment, author.as_ref()),
        ctx.repos.get(&issue.repo_id),
    ) else {
        return Ok(());
    };
    let payload = json!({ "action": "created", "issue": issue_json, "comment": comment_json });
    insert(state, "IssueCommentEvent", actor_id, r, payload).await?;
    Ok(())
}

async fn pull_event(
    state: &AppState,
    pull_id: i64,
    actor_id: i64,
    action: &str,
    skip_if_merged: bool,
) -> anyhow::Result<()> {
    let issues = issues_by_id(state, &[pull_id]).await?;
    let Some(issue) = issues.first() else {
        return Ok(());
    };
    let ctx = IssueContext::load(state, &issues).await?;
    let Some(pr) = ctx.pull(state, issue) else {
        return Ok(());
    };
    if skip_if_merged && pr.merged {
        return Ok(()); // the merge event records it
    }
    let Some(r) = ctx.repos.get(&issue.repo_id) else {
        return Ok(());
    };
    let payload = json!({ "action": action, "number": issue.number, "pull_request": pr });
    insert(state, "PullRequestEvent", actor_id, r, payload).await?;
    Ok(())
}

#[derive(sqlx::FromRow)]
struct ReviewRow {
    id: i64,
    user_id: Option<i64>,
    body: String,
    state: String,
    commit_id: Option<String>,
    submitted_at: Option<DateTime<Utc>>,
}

async fn review_event(
    state: &AppState,
    pull_id: i64,
    review_id: i64,
    actor_id: i64,
) -> anyhow::Result<()> {
    let review: Option<ReviewRow> = sqlx::query_as(
        "SELECT id, user_id, body, state, commit_id, submitted_at FROM pr_reviews WHERE id = $1",
    )
    .bind(review_id)
    .fetch_optional(&state.db)
    .await?;
    let Some(review) = review else {
        return Ok(());
    };
    let issues = issues_by_id(state, &[pull_id]).await?;
    let Some(issue) = issues.first() else {
        return Ok(());
    };
    let ctx = IssueContext::load(state, &issues).await?;
    let (Some(pr), Some(r)) = (ctx.pull(state, issue), ctx.repos.get(&issue.repo_id)) else {
        return Ok(());
    };
    let user = match review.user_id {
        Some(u) => db::User::find(&state.db, u).await?,
        None => None,
    };
    let html = format!("{}#pullrequestreview-{}", pr.html_url, review.id);
    let review_json = json!({
        "id": review.id,
        "node_id": node_id::encode(NodeType::PullRequestReview, review.id),
        "user": SimpleUser::or_ghost(&state.urls, user.as_ref()),
        "body": review.body,
        "commit_id": review.commit_id,
        "submitted_at": ts(review.submitted_at),
        "state": review.state.to_lowercase(),
        "html_url": html,
        "pull_request_url": pr.url,
        "author_association": pr.author_association,
        "_links": {
            "html": {"href": html},
            "pull_request": {"href": pr.url},
        },
    });
    let payload = json!({ "action": "created", "review": review_json, "pull_request": pr });
    insert(state, "PullRequestReviewEvent", actor_id, r, payload).await?;
    Ok(())
}

#[derive(sqlx::FromRow)]
struct ReviewCommentRow {
    id: i64,
    review_id: Option<i64>,
    in_reply_to_id: Option<i64>,
    user_id: Option<i64>,
    body: String,
    path: String,
    commit_id: String,
    original_commit_id: String,
    diff_hunk: String,
    line: Option<i32>,
    original_line: Option<i32>,
    side: Option<String>,
    position: Option<i32>,
    original_position: Option<i32>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

async fn review_comment_event(
    state: &AppState,
    pull_id: i64,
    comment_id: i64,
    actor_id: i64,
) -> anyhow::Result<()> {
    let c: Option<ReviewCommentRow> = sqlx::query_as(
        "SELECT id, review_id, in_reply_to_id, user_id, body, path, commit_id, original_commit_id,
                diff_hunk, line, original_line, side, position, original_position, created_at,
                updated_at
           FROM pr_review_comments WHERE id = $1",
    )
    .bind(comment_id)
    .fetch_optional(&state.db)
    .await?;
    let Some(c) = c else { return Ok(()) };
    let issues = issues_by_id(state, &[pull_id]).await?;
    let Some(issue) = issues.first() else {
        return Ok(());
    };
    let ctx = IssueContext::load(state, &issues).await?;
    let (Some(pr), Some(r)) = (ctx.pull(state, issue), ctx.repos.get(&issue.repo_id)) else {
        return Ok(());
    };
    let user = match c.user_id {
        Some(u) => db::User::find(&state.db, u).await?,
        None => None,
    };
    let url = pr
        .review_comment_url
        .replace("{/number}", &format!("/{}", c.id));
    let html = format!("{}#discussion_r{}", pr.html_url, c.id);
    let comment = json!({
        "url": url,
        "pull_request_review_id": c.review_id,
        "id": c.id,
        "node_id": node_id::encode(NodeType::PullRequestReviewComment, c.id),
        "diff_hunk": c.diff_hunk,
        "path": c.path,
        "position": c.position,
        "original_position": c.original_position,
        "commit_id": c.commit_id,
        "original_commit_id": c.original_commit_id,
        "in_reply_to_id": c.in_reply_to_id,
        "user": SimpleUser::or_ghost(&state.urls, user.as_ref()),
        "body": c.body,
        "created_at": Timestamp::from(c.created_at),
        "updated_at": Timestamp::from(c.updated_at),
        "html_url": html,
        "pull_request_url": pr.url,
        "author_association": "NONE",
        "line": c.line,
        "original_line": c.original_line,
        "side": c.side,
        "_links": {
            "self": {"href": url},
            "html": {"href": html},
            "pull_request": {"href": pr.url},
        },
    });
    let payload = json!({ "action": "created", "comment": comment, "pull_request": pr });
    insert(state, "PullRequestReviewCommentEvent", actor_id, r, payload).await?;
    Ok(())
}

#[derive(sqlx::FromRow)]
struct ReleaseRow {
    id: i64,
    tag_name: String,
    target_commitish: String,
    name: Option<String>,
    body: Option<String>,
    draft: bool,
    prerelease: bool,
    author_id: Option<i64>,
    published_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
struct AssetRow {
    id: i64,
    name: String,
    label: Option<String>,
    state: String,
    content_type: String,
    size: i64,
    download_count: i64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

async fn commit_comment_event(
    state: &AppState,
    repo_id: i64,
    comment_id: i64,
    actor_id: i64,
) -> anyhow::Result<()> {
    use bgh_core::commit_comments::{BodyFormat, CommitCommentRow, render};
    let Some(r) = repo(state, repo_id).await? else {
        return Ok(());
    };
    let Some(owner) = db::User::find(&state.db, r.owner_id).await? else {
        return Ok(());
    };
    let Some(c) = CommitCommentRow::find(&state.db, repo_id, comment_id).await? else {
        return Ok(());
    };
    let Some(comment) = render(
        state,
        &owner.login,
        &r,
        std::slice::from_ref(&c),
        BodyFormat::RAW,
    )
    .await
    .map_err(|e| anyhow::anyhow!("rendering commit comment {comment_id}: {e:?}"))?
    .pop() else {
        return Ok(());
    };
    insert(
        state,
        "CommitCommentEvent",
        actor_id,
        &r,
        json!({ "action": "created", "comment": comment }),
    )
    .await?;
    Ok(())
}

async fn release_event(
    state: &AppState,
    repo_id: i64,
    release_id: i64,
    actor_id: i64,
) -> anyhow::Result<()> {
    let Some(r) = repo(state, repo_id).await? else {
        return Ok(());
    };
    let Some(owner) = db::User::find(&state.db, r.owner_id).await? else {
        return Ok(());
    };
    let rel: Option<ReleaseRow> = sqlx::query_as(
        "SELECT id, tag_name, target_commitish, name, body, draft, prerelease, author_id,
                published_at, created_at, updated_at
           FROM releases WHERE id = $1 AND repo_id = $2",
    )
    .bind(release_id)
    .bind(repo_id)
    .fetch_optional(&state.db)
    .await?;
    let Some(rel) = rel else { return Ok(()) };
    let assets: Vec<AssetRow> = sqlx::query_as(
        "SELECT id, name, label, state, content_type, size, download_count, created_at, updated_at
           FROM release_assets WHERE release_id = $1 ORDER BY id",
    )
    .bind(rel.id)
    .fetch_all(&state.db)
    .await?;
    let author = match rel.author_id {
        Some(a) => db::User::find(&state.db, a).await?,
        None => None,
    };
    let urls = &state.urls;
    let (o, n) = (&owner.login, &r.name);
    let tag = bgh_core::urls::encode_path(&rel.tag_name);
    let url = urls.api(&format!("/repos/{o}/{n}/releases/{}", rel.id));
    let assets: Vec<Value> = assets
        .iter()
        .map(|a| {
            json!({
                "url": urls.api(&format!("/repos/{o}/{n}/releases/assets/{}", a.id)),
                "browser_download_url": urls.html(&format!(
                    "/{o}/{n}/releases/download/{tag}/{}",
                    bgh_core::urls::encode_segment(&a.name)
                )),
                "id": a.id,
                "node_id": node_id::encode(NodeType::ReleaseAsset, a.id),
                "name": a.name,
                "label": a.label,
                "state": a.state,
                "content_type": a.content_type,
                "size": a.size,
                "download_count": a.download_count,
                "created_at": Timestamp::from(a.created_at),
                "updated_at": Timestamp::from(a.updated_at),
                "uploader": null,
            })
        })
        .collect();
    let release = json!({
        "url": url,
        "assets_url": format!("{url}/assets"),
        "upload_url": urls.html(&format!("/api/uploads/repos/{o}/{n}/releases/{}/assets{{?name,label}}", rel.id)),
        "html_url": urls.html(&format!("/{o}/{n}/releases/tag/{tag}")),
        "id": rel.id,
        "author": SimpleUser::or_ghost(urls, author.as_ref()),
        "node_id": node_id::encode(NodeType::Release, rel.id),
        "tag_name": rel.tag_name,
        "target_commitish": rel.target_commitish,
        "name": rel.name,
        "draft": rel.draft,
        "prerelease": rel.prerelease,
        "created_at": Timestamp::from(rel.created_at),
        "updated_at": Timestamp::from(rel.updated_at),
        "published_at": ts(rel.published_at),
        "assets": assets,
        "tarball_url": urls.api(&format!("/repos/{o}/{n}/tarball/{tag}")),
        "zipball_url": urls.api(&format!("/repos/{o}/{n}/zipball/{tag}")),
        "body": rel.body,
    });
    insert(
        state,
        "ReleaseEvent",
        actor_id,
        &r,
        json!({ "action": "published", "release": release }),
    )
    .await?;
    Ok(())
}

struct PushCommit {
    sha: String,
    name: String,
    email: String,
    message: String,
}

async fn push(state: &AppState, p: &PushEvent) -> anyhow::Result<()> {
    let Some(actor_id) = p.pusher_id else {
        return Ok(());
    };
    let Some(r) = repo(state, p.repo_id).await? else {
        return Ok(());
    };
    let Some(owner) = db::User::find(&state.db, r.owner_id).await? else {
        return Ok(());
    };
    let store = RepoStore::from_config(&state.config);
    for u in &p.updates {
        let (ref_type, short) = match (u.branch(), u.tag()) {
            (Some(b), _) => ("branch", b.to_string()),
            (_, Some(t)) => ("tag", t.to_string()),
            _ => continue,
        };
        if u.is_delete() {
            let payload = json!({ "ref": short, "ref_type": ref_type, "pusher_type": "user" });
            insert(state, "DeleteEvent", actor_id, &r, payload).await?;
            continue;
        }
        if u.is_create() {
            let payload = json!({
                "ref": short,
                "ref_type": ref_type,
                "master_branch": r.default_branch,
                "description": r.description,
                "pusher_type": "user",
            });
            insert(state, "CreateEvent", actor_id, &r, payload).await?;
            if ref_type == "tag" {
                continue;
            }
        }
        if ref_type != "branch" {
            continue;
        }
        // Commits new to this ref (for a new branch: not on the default branch).
        let new = u.new.clone();
        let exclude: Option<String> = if u.is_create() {
            (short != r.default_branch).then(|| format!("refs/heads/{}", r.default_branch))
        } else {
            Some(u.old.clone())
        };
        let commits: Vec<PushCommit> = store
            .read(r.id, move |g| {
                let ex: Vec<String> = exclude
                    .into_iter()
                    .filter(|e| g.resolve(e).ok().flatten().is_some())
                    .collect();
                let ex: Vec<&str> = ex.iter().map(String::as_str).collect();
                let shas = g.rev_list(&new, &ex, PUSH_COUNT_LIMIT)?;
                let mut out = Vec::new();
                for (i, sha) in shas.iter().enumerate() {
                    if i < PUSH_COMMITS {
                        let c = g.commit(sha)?;
                        out.push(PushCommit {
                            sha: sha.clone(),
                            name: c.author.name.clone(),
                            email: c.author.email.clone(),
                            message: c.message.trim_end().to_string(),
                        });
                    } else {
                        out.push(PushCommit {
                            sha: sha.clone(),
                            name: String::new(),
                            email: String::new(),
                            message: String::new(),
                        });
                    }
                }
                Ok(out)
            })
            .await
            .unwrap_or_default();
        if u.is_create() && commits.is_empty() {
            continue;
        }
        let size = commits.len();
        let listed: Vec<Value> = commits
            .iter()
            .take(PUSH_COMMITS)
            .rev()
            .map(|c| {
                json!({
                    "sha": c.sha,
                    "author": {"email": c.email, "name": c.name},
                    "message": c.message,
                    "distinct": true,
                    "url": state.urls.commit(&owner.login, &r.name, &c.sha),
                })
            })
            .collect();
        let payload = json!({
            "repository_id": r.id,
            "push_id": Utc::now().timestamp_micros(),
            "size": size,
            "distinct_size": size,
            "ref": u.refname,
            "head": u.new,
            "before": u.old,
            "commits": listed,
        });
        insert(state, "PushEvent", actor_id, &r, payload).await?;
    }
    Ok(())
}
