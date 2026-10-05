//! Turning domain events into workflow runs.
//!
//! Domain events (push, pull request, release, issues, issue comments) are
//! turned into a durable `actions.trigger` job by an event listener; the job
//! reads `.github/workflows/*.yml|yaml` at the relevant commit, matches each
//! workflow's `on:` filters and creates runs ([`engine::create_run`]).
//! `workflow_dispatch` ([`dispatch`]) and `schedule` ([`schedule_tick`])
//! create runs directly.

use std::sync::Arc;

use bgh_core::AppState;
use bgh_core::events::{Event, RefUpdate};
use bgh_core::jobs::JobPayload;
use bgh_core::models::db;
use bgh_git::{PathLookup, RepoStore, TreeEntryKind};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::context;
use crate::engine::{self, NewRun};
use crate::models::WorkflowRow;
use crate::workflow::{self, CronSchedule, Workflow};

pub const WORKFLOWS_DIR: &str = ".github/workflows";
const MAX_WORKFLOW_SIZE: u64 = 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TriggerKind {
    Push {
        pusher_id: Option<i64>,
        updates: Vec<RefUpdate>,
    },
    PullRequest {
        pull_id: i64,
        action: String,
        actor_id: Option<i64>,
        before: Option<String>,
        /// `pull_request` (with `pull_request_target`) when unset, else
        /// `pull_request_review` / `pull_request_review_comment`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        event: Option<String>,
        /// Payload hints ([`crate::trigger_events::Hints`]).
        #[serde(default, skip_serializing_if = "Value::is_null")]
        extra: Value,
    },
    Release {
        release_id: i64,
        action: String,
        actor_id: i64,
        #[serde(default, skip_serializing_if = "Value::is_null")]
        extra: Value,
    },
    Issue {
        issue_id: i64,
        action: String,
        actor_id: i64,
        #[serde(default, skip_serializing_if = "Value::is_null")]
        extra: Value,
    },
    IssueComment {
        issue_id: i64,
        comment_id: i64,
        action: String,
        actor_id: i64,
        #[serde(default, skip_serializing_if = "Value::is_null")]
        extra: Value,
    },
    /// Events evaluated against the default branch's workflows (`label`,
    /// `milestone`, `watch`, `fork`, `public`, `gollum`, `check_run`,
    /// `check_suite`, `workflow_run`), see [`crate::trigger_events`].
    Repo {
        event: String,
        action: Option<String>,
        actor_id: Option<i64>,
        #[serde(default)]
        extra: Value,
    },
}

/// Durable trigger evaluation for one domain event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trigger {
    pub repo_id: i64,
    #[serde(flatten)]
    pub kind: TriggerKind,
}

impl JobPayload for Trigger {
    const KIND: &'static str = "actions.trigger";
    const MAX_ATTEMPTS: i32 = 3;
}

/// Event listener: enqueue a [`Trigger`] for events that can start workflows.
pub async fn on_event(state: AppState, event: Arc<Event>) -> anyhow::Result<()> {
    if !state.config.actions.enabled {
        return Ok(());
    }
    let (repo_id, kind) = match &*event {
        // Imports and mirror syncs fetch history; they never run workflows.
        Event::Push(p) if p.is_fetched() => return Ok(()),
        Event::Push(p) => (
            p.repo_id,
            TriggerKind::Push {
                pusher_id: p.pusher_id,
                updates: p.updates.clone(),
            },
        ),
        Event::PullRequestOpened {
            repo_id,
            pull_id,
            actor_id,
        } => (*repo_id, pr(*pull_id, "opened", Some(*actor_id), None)),
        Event::PullRequestReopened {
            repo_id,
            pull_id,
            actor_id,
        } => (*repo_id, pr(*pull_id, "reopened", Some(*actor_id), None)),
        Event::PullRequestClosed {
            repo_id,
            pull_id,
            actor_id,
        } => (*repo_id, pr(*pull_id, "closed", Some(*actor_id), None)),
        Event::PullRequestSynchronized {
            repo_id,
            pull_id,
            actor_id,
            before,
            ..
        } => (
            *repo_id,
            pr(*pull_id, "synchronize", *actor_id, Some(before.clone())),
        ),
        Event::ReleasePublished {
            repo_id,
            release_id,
            actor_id,
        } => (
            *repo_id,
            TriggerKind::Release {
                release_id: *release_id,
                action: "published".into(),
                actor_id: *actor_id,
                extra: Value::Null,
            },
        ),
        Event::IssueOpened {
            repo_id,
            issue_id,
            actor_id,
        } => (*repo_id, issue(*issue_id, "opened", *actor_id)),
        Event::IssueClosed {
            repo_id,
            issue_id,
            actor_id,
        } => (*repo_id, issue(*issue_id, "closed", *actor_id)),
        Event::IssueReopened {
            repo_id,
            issue_id,
            actor_id,
        } => (*repo_id, issue(*issue_id, "reopened", *actor_id)),
        Event::IssueEdited {
            repo_id,
            issue_id,
            actor_id,
            ..
        } => (*repo_id, issue(*issue_id, "edited", *actor_id)),
        Event::IssueCommentCreated {
            repo_id,
            issue_id,
            comment_id,
            actor_id,
        } => (
            *repo_id,
            TriggerKind::IssueComment {
                issue_id: *issue_id,
                comment_id: *comment_id,
                action: "created".into(),
                actor_id: *actor_id,
                extra: Value::Null,
            },
        ),
        other => match crate::trigger_events::map_event(other) {
            Some(mapped) => mapped,
            None => return Ok(()),
        },
    };
    if !may_have_workflows(&state, repo_id, &kind).await? {
        return Ok(());
    }
    // At-least-once delivery: a redelivered event must not start runs twice.
    let mut tx = state.db.begin().await?;
    if bgh_core::events::claim_effect(&mut tx).await? {
        bgh_core::jobs::enqueue_job(&mut *tx, &Trigger { repo_id, kind }).await?;
        tx.commit().await?;
    }
    Ok(())
}

/// Cheap pre-check so repositories without workflows never get trigger
/// jobs: known workflow rows, or a `.github/workflows` tree at a pushed /
/// PR head commit.
async fn may_have_workflows(
    state: &AppState,
    repo_id: i64,
    kind: &TriggerKind,
) -> anyhow::Result<bool> {
    let known: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM actions_workflows WHERE repo_id = $1 AND state <> 'deleted')",
    )
    .bind(repo_id)
    .fetch_one(&state.db)
    .await?;
    if known {
        return Ok(true);
    }
    let (repo, shas): (i64, Vec<String>) = match kind {
        TriggerKind::Push { updates, .. } => (
            repo_id,
            updates
                .iter()
                .filter(|u| !u.is_delete())
                .map(|u| u.new.clone())
                .collect(),
        ),
        TriggerKind::PullRequest { pull_id, .. } => {
            let row: Option<(Option<i64>, String)> = sqlx::query_as(
                "SELECT head_repo_id, head_sha FROM pull_requests WHERE issue_id = $1",
            )
            .bind(pull_id)
            .fetch_optional(&state.db)
            .await?;
            match row {
                Some((head_repo, sha)) => (head_repo.unwrap_or(repo_id), vec![sha]),
                None => return Ok(false),
            }
        }
        _ => return Ok(false),
    };
    if shas.is_empty() {
        return Ok(false);
    }
    Ok(store(state)
        .read(repo, move |r| {
            Ok(shas.iter().any(|sha| {
                matches!(
                    r.lookup_path(sha, WORKFLOWS_DIR),
                    Ok(PathLookup::Tree { .. })
                )
            }))
        })
        .await
        .unwrap_or(false))
}

pub(crate) fn pr(
    pull_id: i64,
    action: &str,
    actor_id: Option<i64>,
    before: Option<String>,
) -> TriggerKind {
    TriggerKind::PullRequest {
        pull_id,
        action: action.into(),
        actor_id,
        before,
        event: None,
        extra: Value::Null,
    }
}

pub(crate) fn issue(issue_id: i64, action: &str, actor_id: i64) -> TriggerKind {
    TriggerKind::Issue {
        issue_id,
        action: action.into(),
        actor_id,
        extra: Value::Null,
    }
}

pub fn store(state: &AppState) -> RepoStore {
    RepoStore::from_config(&state.config)
}

/// A workflow file read from git.
pub struct WorkflowFile {
    pub path: String,
    pub yaml: String,
    pub def: Result<Workflow, String>,
}

/// Read and parse `.github/workflows/*.yml|yaml` at `rev` of `repo_id`.
pub async fn load_workflows(
    state: &AppState,
    repo_id: i64,
    rev: &str,
) -> anyhow::Result<Vec<WorkflowFile>> {
    let rev = rev.to_string();
    let files: Vec<(String, Vec<u8>)> = store(state)
        .read(repo_id, move |r| {
            let entries = match r.lookup_path(&rev, WORKFLOWS_DIR) {
                Ok(PathLookup::Tree { entries, .. }) => entries,
                Ok(PathLookup::Entry(_)) | Err(bgh_git::GitError::NotFound(_)) => {
                    return Ok(vec![]);
                }
                Err(e) => return Err(e),
            };
            let mut out = Vec::new();
            for e in entries {
                if e.kind != TreeEntryKind::Blob
                    || !(e.name.ends_with(".yml") || e.name.ends_with(".yaml"))
                {
                    continue;
                }
                match r.blob_with_limit(&e.sha, MAX_WORKFLOW_SIZE) {
                    Ok(b) => out.push((format!("{WORKFLOWS_DIR}/{}", e.name), b.data)),
                    Err(err) => tracing::warn!(%err, file = %e.name, "skipping workflow file"),
                }
            }
            Ok(out)
        })
        .await?;
    let mut out: Vec<WorkflowFile> = files
        .into_iter()
        .map(|(path, data)| {
            let yaml = String::from_utf8_lossy(&data).into_owned();
            let def = workflow::parse_workflow(&yaml).map_err(|e| e.to_string());
            WorkflowFile { path, yaml, def }
        })
        .collect();
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

/// Files changed between two commits (`base...head` when `merge_base`),
/// `None` when unknown.
pub async fn changed_paths(
    state: &AppState,
    repo_id: i64,
    base: Option<&str>,
    head: &str,
    merge_base: bool,
) -> Option<Vec<String>> {
    let git_dir = store(state).git_dir(repo_id).ok()?;
    let mut cmd = tokio::process::Command::new(&state.config.git_bin);
    cmd.env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .arg("--git-dir")
        .arg(&git_dir);
    match base.filter(|b| !b.bytes().all(|c| c == b'0')) {
        Some(base) => {
            let range = if merge_base {
                format!("{base}...{head}")
            } else {
                format!("{base}..{head}")
            };
            cmd.args(["diff", "--name-only", "--no-renames", &range]);
        }
        None => {
            cmd.args([
                "diff-tree",
                "--no-commit-id",
                "--name-only",
                "-r",
                "--root",
                head,
            ]);
        }
    }
    let out = cmd.output().await.ok()?;
    if !out.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect(),
    )
}

async fn repo_and_owner(
    state: &AppState,
    repo_id: i64,
) -> anyhow::Result<Option<(db::Repository, db::User)>> {
    let Some(repo) = db::Repository::find(&state.db, repo_id).await? else {
        return Ok(None);
    };
    let Some(owner) = db::User::find(&state.db, repo.owner_id).await? else {
        return Ok(None);
    };
    Ok(Some((repo, owner)))
}

pub(crate) async fn user(state: &AppState, id: Option<i64>) -> anyhow::Result<Option<db::User>> {
    Ok(match id {
        Some(id) => db::User::find(&state.db, id).await?,
        None => None,
    })
}

/// Commit payload object (`head_commit`) for push events.
async fn commit_payload(state: &AppState, repo_id: i64, sha: &str) -> Value {
    let sha_owned = sha.to_string();
    let c = store(state)
        .read(repo_id, move |r| r.commit(&sha_owned))
        .await
        .ok();
    match c {
        None => Value::Null,
        Some(c) => json!({
            "id": c.sha,
            "tree_id": c.tree,
            "message": c.message.trim_end(),
            "timestamp": bgh_core::time::Timestamp(c.committer.when),
            "author": {"name": c.author.name, "email": c.author.email},
            "committer": {"name": c.committer.name, "email": c.committer.email},
        }),
    }
}

/// Update `actions_workflows` from the default branch head: names,
/// schedules, and `deleted` for removed files.
pub async fn sync_workflows(state: &AppState, repo: &db::Repository) -> anyhow::Result<()> {
    let files = match load_workflows(
        state,
        repo.id,
        &format!("refs/heads/{}", repo.default_branch),
    )
    .await
    {
        Ok(f) => f,
        Err(err) => {
            tracing::debug!(%err, repo_id = repo.id, "no default branch to sync workflows from");
            return Ok(());
        }
    };
    let mut tx = state.db.begin().await?;
    let mut paths = Vec::new();
    for f in &files {
        let name = engine::workflow_name(f.def.as_ref().ok(), &f.path);
        let schedules = f.def.as_ref().map(|d| d.on.schedules()).unwrap_or_default();
        let id = engine::ensure_workflow(&mut tx, repo.id, &f.path, &name).await?;
        sqlx::query(
            "UPDATE actions_workflows SET name = $2, schedules = $3,
                    schedule_checked_at = CASE WHEN schedules = $3 THEN schedule_checked_at ELSE now() END,
                    updated_at = CASE WHEN name <> $2 OR schedules <> $3 THEN now() ELSE updated_at END
              WHERE id = $1",
        )
        .bind(id)
        .bind(&name)
        .bind(&schedules)
        .execute(&mut *tx)
        .await?;
        paths.push(f.path.clone());
    }
    sqlx::query(
        "UPDATE actions_workflows SET state = 'deleted', schedules = '{}', updated_at = now()
          WHERE repo_id = $1 AND NOT (path = ANY($2)) AND state <> 'deleted'",
    )
    .bind(repo.id)
    .bind(&paths)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Sync workflows if the default branch moved since the last sync
/// (cheap check through Redis; used by the workflows API).
pub async fn sync_workflows_if_stale(
    state: &AppState,
    repo: &db::Repository,
) -> anyhow::Result<()> {
    let branch = format!("refs/heads/{}", repo.default_branch);
    let head = store(state)
        .read(repo.id, move |r| r.resolve(&branch))
        .await
        .ok()
        .flatten();
    let Some(head) = head else { return Ok(()) };
    let key = state.redis_key(&format!("actions:wfsync:{}", repo.id));
    let mut redis = state.redis.clone();
    let seen: Option<String> = redis::cmd("GET")
        .arg(&key)
        .query_async(&mut redis)
        .await
        .unwrap_or(None);
    if seen.as_deref() == Some(head.as_str()) {
        return Ok(());
    }
    sync_workflows(state, repo).await?;
    let _: Result<(), _> = redis::cmd("SET")
        .arg(&key)
        .arg(&head)
        .arg("EX")
        .arg(86_400)
        .query_async(&mut redis)
        .await;
    Ok(())
}

async fn workflow_disabled(state: &AppState, repo_id: i64, path: &str) -> anyhow::Result<bool> {
    let st: Option<String> =
        sqlx::query_scalar("SELECT state FROM actions_workflows WHERE repo_id = $1 AND path = $2")
            .bind(repo_id)
            .bind(path)
            .fetch_optional(&state.db)
            .await?;
    Ok(st.is_some_and(|s| s.starts_with("disabled")))
}

/// Job handler for [`Trigger`].
pub async fn trigger_job(state: AppState, job: Trigger) -> anyhow::Result<()> {
    let Some((repo, owner)) = repo_and_owner(&state, job.repo_id).await? else {
        return Ok(());
    };
    if repo.archived || repo.disabled {
        return Ok(());
    }
    match job.kind {
        TriggerKind::Push { pusher_id, updates } => {
            on_push(&state, &repo, &owner, pusher_id, &updates).await
        }
        TriggerKind::PullRequest {
            pull_id,
            action,
            actor_id,
            before,
            event,
            extra,
        } => {
            on_pull_request(
                &state,
                &repo,
                &owner,
                PullEvent {
                    pull_id,
                    event: event.as_deref().unwrap_or("pull_request"),
                    action: &action,
                    actor_id,
                    before,
                    extra: &extra,
                },
            )
            .await
        }
        TriggerKind::Release {
            release_id,
            action,
            actor_id,
            extra,
        } => on_release(&state, &repo, &owner, release_id, &action, actor_id, &extra).await,
        TriggerKind::Issue {
            issue_id,
            action,
            actor_id,
            extra,
        } => {
            // Issue-level changes of a pull request are `pull_request`
            // activity on GitHub, never `issues`.
            if crate::trigger_events::is_pull_request(&state, issue_id).await? {
                if !crate::trigger_events::PR_ISSUE_ACTIONS.contains(&action.as_str()) {
                    return Ok(());
                }
                return on_pull_request(
                    &state,
                    &repo,
                    &owner,
                    PullEvent {
                        pull_id: issue_id,
                        event: "pull_request",
                        action: &action,
                        actor_id: Some(actor_id),
                        before: None,
                        extra: &extra,
                    },
                )
                .await;
            }
            on_issue(
                &state,
                &repo,
                &owner,
                IssueEvent {
                    issue_id,
                    comment_id: None,
                    event: "issues",
                    action: &action,
                    actor_id,
                    extra: &extra,
                },
            )
            .await
        }
        TriggerKind::IssueComment {
            issue_id,
            comment_id,
            action,
            actor_id,
            extra,
        } => {
            on_issue(
                &state,
                &repo,
                &owner,
                IssueEvent {
                    issue_id,
                    comment_id: Some(comment_id),
                    event: "issue_comment",
                    action: &action,
                    actor_id,
                    extra: &extra,
                },
            )
            .await
        }
        TriggerKind::Repo {
            event,
            action,
            actor_id,
            extra,
        } => {
            crate::trigger_events::on_repo_event(
                &state,
                &repo,
                &owner,
                &event,
                action.as_deref(),
                actor_id,
                &extra,
            )
            .await
        }
    }
}

async fn on_push(
    state: &AppState,
    repo: &db::Repository,
    owner: &db::User,
    pusher_id: Option<i64>,
    updates: &[RefUpdate],
) -> anyhow::Result<()> {
    let pusher = user(state, pusher_id).await?;
    for u in updates {
        if u.branch() == Some(repo.default_branch.as_str()) {
            sync_workflows(state, repo).await?;
        }
        if u.is_delete() {
            continue;
        }
        let files = load_workflows(state, repo.id, &u.new).await?;
        if files.is_empty() {
            continue;
        }
        let changed = if u.tag().is_some() || u.is_create() {
            None
        } else {
            changed_paths(state, repo.id, Some(&u.old), &u.new, false).await
        };
        let head_commit = commit_payload(state, repo.id, &u.new).await;
        let payload = json!({
            "ref": u.refname,
            "before": u.old,
            "after": u.new,
            "created": u.is_create(),
            "deleted": false,
            "forced": false,
            "base_ref": null,
            "compare": state.urls.html(&format!("/{}/{}/compare/{}...{}", owner.login, repo.name, &u.old[..12.min(u.old.len())], &u.new[..12.min(u.new.len())])),
            "commits": if head_commit.is_null() { json!([]) } else { json!([head_commit.clone()]) },
            "head_commit": head_commit,
            "repository": context::repo_payload(state, repo, owner),
            "pusher": pusher.as_ref().map(|p| json!({"name": p.login, "email": p.email})),
            "sender": context::sender_payload(state, pusher.as_ref()),
        });
        let head_branch = u.branch().or(u.tag()).map(String::from);
        for f in files {
            let matches = match &f.def {
                Ok(d) => d.on.matches_push(&u.refname, changed.as_deref()),
                // Invalid file: report a startup failure on pushes touching it.
                Err(_) => changed
                    .as_ref()
                    .is_none_or(|c| c.iter().any(|p| p == &f.path)),
            };
            if !matches || workflow_disabled(state, repo.id, &f.path).await? {
                continue;
            }
            engine::create_run(
                state,
                NewRun {
                    repo: repo.clone(),
                    owner: owner.clone(),
                    path: f.path,
                    yaml: f.yaml,
                    def: f.def,
                    event: "push".into(),
                    git_ref: u.refname.clone(),
                    head_branch: head_branch.clone(),
                    head_sha: u.new.clone(),
                    head_repo_id: Some(repo.id),
                    actor_id: pusher_id,
                    payload: payload.clone(),
                    inputs: None,
                    pull_request_ids: vec![],
                },
            )
            .await?;
        }
    }
    on_create_delete(state, repo, owner, pusher.as_ref(), updates).await
}

/// `create` / `delete` for branch and tag creations and deletions. Like
/// GitHub, `create` is not fired when a push creates more than three tags.
async fn on_create_delete(
    state: &AppState,
    repo: &db::Repository,
    owner: &db::User,
    pusher: Option<&db::User>,
    updates: &[RefUpdate],
) -> anyhow::Result<()> {
    let created_tags = updates
        .iter()
        .filter(|u| u.is_create() && u.tag().is_some())
        .count();
    for u in updates {
        let (name, ref_type) = match (u.branch(), u.tag()) {
            (Some(b), _) => (b, "branch"),
            (_, Some(t)) => (t, "tag"),
            _ => continue,
        };
        let mut payload = json!({
            "ref": name,
            "ref_type": ref_type,
            "pusher_type": "user",
            "repository": context::repo_payload(state, repo, owner),
            "sender": context::sender_payload(state, pusher),
        });
        if u.is_delete() {
            let Some((git_ref, sha)) = default_head(state, repo).await else {
                continue;
            };
            run_default_branch_event(
                state,
                repo,
                owner,
                "delete",
                "",
                &git_ref,
                &sha,
                pusher.map(|p| p.id),
                payload,
            )
            .await?;
        } else if u.is_create() && !(ref_type == "tag" && created_tags > 3) {
            payload["master_branch"] = json!(repo.default_branch);
            payload["description"] = json!(repo.description);
            // `create` runs the workflows of the created ref.
            run_default_branch_event(
                state,
                repo,
                owner,
                "create",
                "",
                &u.refname,
                &u.new,
                pusher.map(|p| p.id),
                payload,
            )
            .await?;
        }
    }
    Ok(())
}

/// A pull request activity to evaluate.
pub(crate) struct PullEvent<'a> {
    pub pull_id: i64,
    /// `pull_request`, `pull_request_review` or `pull_request_review_comment`.
    pub event: &'a str,
    pub action: &'a str,
    pub actor_id: Option<i64>,
    pub before: Option<String>,
    pub extra: &'a Value,
}

/// The site identity that commits PR test merges (same as bgh-pulls, so
/// both compute the same `refs/pull/{n}/merge` sha).
fn site_committer(state: &AppState) -> bgh_git::write::Identity {
    bgh_git::write::Identity::new(
        state.config.site_name.clone(),
        format!("noreply@{}", state.config.hostname()),
    )
}

async fn on_pull_request(
    state: &AppState,
    repo: &db::Repository,
    owner: &db::User,
    ev: PullEvent<'_>,
) -> anyhow::Result<()> {
    let PullEvent {
        pull_id,
        event,
        action,
        actor_id,
        before,
        extra,
    } = ev;
    let Some((issue, pr, pr_json)) =
        context::pull_request_payload(state, repo, owner, pull_id).await?
    else {
        return Ok(());
    };
    let actor_id = actor_id.or(issue.author_id);
    let actor = user(state, actor_id).await?;
    let mut payload = json!({
        "action": action,
        "number": issue.number,
        "pull_request": pr_json,
        "repository": context::repo_payload(state, repo, owner),
        "sender": context::sender_payload(state, actor.as_ref()),
    });
    if let Some(before) = &before {
        payload["before"] = json!(before);
        payload["after"] = json!(pr.head_sha);
    }
    crate::trigger_events::apply_hints(state, repo, owner, extra, &mut payload).await?;
    let head_repo_id = pr.head_repo_id.unwrap_or(repo.id);
    let review_event = event != "pull_request";
    let changed = if review_event {
        None
    } else {
        changed_paths(state, head_repo_id, Some(&pr.base_sha), &pr.head_sha, true).await
    };

    // pull_request (and the review events): workflows from the PR head, run
    // on the test merge commit `refs/pull/{n}/merge`. A conflicting PR has
    // no merge commit and doesn't run them (as on GitHub).
    let merge_ref = format!("refs/pull/{}/merge", issue.number);
    let merge_sha = if let (true, Some(sha)) = (pr.merged, &pr.merge_commit_sha) {
        // A merged PR: the real merge commit.
        Some(sha.clone())
    } else {
        bgh_git::merge::test_merge(
            &store(state),
            repo.id,
            &merge_ref,
            &pr.base_sha,
            &pr.head_sha,
            &site_committer(state),
        )
        .await
        .map_err(|err| tracing::warn!(%err, pull_id, "test merge failed"))
        .ok()
        .flatten()
    };
    if let Some(merge_sha) = merge_sha {
        let head_files = load_workflows(state, head_repo_id, &pr.head_sha)
            .await
            .unwrap_or_default();
        for f in head_files {
            let Ok(d) = &f.def else { continue };
            let matches = if review_event {
                d.on.matches_activity(event, action)
            } else {
                d.on.matches_pull_request(event, action, &pr.base_ref, changed.as_deref())
            };
            if !matches || workflow_disabled(state, repo.id, &f.path).await? {
                continue;
            }
            engine::create_run(
                state,
                NewRun {
                    repo: repo.clone(),
                    owner: owner.clone(),
                    path: f.path,
                    yaml: f.yaml,
                    def: f.def,
                    event: event.into(),
                    git_ref: merge_ref.clone(),
                    head_branch: Some(pr.head_ref.clone()),
                    head_sha: merge_sha.clone(),
                    head_repo_id: Some(head_repo_id),
                    actor_id,
                    payload: payload.clone(),
                    inputs: None,
                    pull_request_ids: vec![issue.id],
                },
            )
            .await?;
        }
    }
    if review_event {
        return Ok(());
    }

    // pull_request_target: workflows from the base branch, run on it.
    let base_ref = format!("refs/heads/{}", pr.base_ref);
    let base_sha = {
        let r = base_ref.clone();
        store(state)
            .read(repo.id, move |g| g.resolve(&r))
            .await
            .ok()
            .flatten()
    };
    let Some(base_sha) = base_sha else {
        return Ok(());
    };
    let base_files = load_workflows(state, repo.id, &base_sha)
        .await
        .unwrap_or_default();
    for f in base_files {
        let Ok(d) = &f.def else { continue };
        if !d.on.matches_pull_request(
            "pull_request_target",
            action,
            &pr.base_ref,
            changed.as_deref(),
        ) || workflow_disabled(state, repo.id, &f.path).await?
        {
            continue;
        }
        engine::create_run(
            state,
            NewRun {
                repo: repo.clone(),
                owner: owner.clone(),
                path: f.path,
                yaml: f.yaml,
                def: f.def,
                event: "pull_request_target".into(),
                git_ref: base_ref.clone(),
                head_branch: Some(pr.head_ref.clone()),
                head_sha: base_sha.clone(),
                head_repo_id: Some(head_repo_id),
                actor_id,
                payload: payload.clone(),
                inputs: None,
                pull_request_ids: vec![issue.id],
            },
        )
        .await?;
    }
    Ok(())
}

/// Default branch ref and head sha.
pub(crate) async fn default_head(
    state: &AppState,
    repo: &db::Repository,
) -> Option<(String, String)> {
    let r = format!("refs/heads/{}", repo.default_branch);
    let r2 = r.clone();
    let sha = store(state)
        .read(repo.id, move |g| g.resolve(&r2))
        .await
        .ok()
        .flatten()?;
    Some((r, sha))
}

/// Runs for events evaluated against the default branch's workflows.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_default_branch_event(
    state: &AppState,
    repo: &db::Repository,
    owner: &db::User,
    event: &str,
    action: &str,
    git_ref: &str,
    sha: &str,
    actor_id: Option<i64>,
    payload: Value,
) -> anyhow::Result<()> {
    run_default_branch_matching(
        state,
        repo,
        owner,
        event,
        git_ref,
        sha,
        actor_id,
        payload,
        |d| d.on.matches_activity(event, action),
    )
    .await
    .map(drop)
}

/// Runs of the workflows at `sha` accepted by `matches` (`git_ref` is the
/// run's `GITHUB_REF`).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_default_branch_matching(
    state: &AppState,
    repo: &db::Repository,
    owner: &db::User,
    event: &str,
    git_ref: &str,
    sha: &str,
    actor_id: Option<i64>,
    payload: Value,
    matches: impl Fn(&Workflow) -> bool,
) -> anyhow::Result<Vec<i64>> {
    let files = load_workflows(state, repo.id, sha)
        .await
        .unwrap_or_default();
    let mut created = Vec::new();
    for f in files {
        let Ok(d) = &f.def else { continue };
        if !matches(d) || workflow_disabled(state, repo.id, &f.path).await? {
            continue;
        }
        let branch = context::ref_name(git_ref).0.to_string();
        created.push(
            engine::create_run(
                state,
                NewRun {
                    repo: repo.clone(),
                    owner: owner.clone(),
                    path: f.path,
                    yaml: f.yaml,
                    def: f.def,
                    event: event.into(),
                    git_ref: git_ref.into(),
                    head_branch: Some(branch),
                    head_sha: sha.into(),
                    head_repo_id: Some(repo.id),
                    actor_id,
                    payload: payload.clone(),
                    inputs: None,
                    pull_request_ids: vec![],
                },
            )
            .await?,
        );
    }
    Ok(created)
}

async fn on_release(
    state: &AppState,
    repo: &db::Repository,
    owner: &db::User,
    release_id: i64,
    action: &str,
    actor_id: i64,
    extra: &Value,
) -> anyhow::Result<()> {
    #[derive(sqlx::FromRow)]
    struct Rel {
        tag_name: String,
        name: Option<String>,
        body: Option<String>,
        draft: bool,
        prerelease: bool,
        target_commitish: String,
    }
    let rel: Option<Rel> = sqlx::query_as(
        "SELECT tag_name, name, body, draft, prerelease, target_commitish FROM releases WHERE id = $1",
    )
    .bind(release_id)
    .fetch_optional(&state.db)
    .await?;
    // A deleted release is gone from the table: use the event's snapshot.
    let (release, tag_name, draft) = match rel {
        Some(rel) => (
            json!({
                "id": release_id,
                "tag_name": rel.tag_name,
                "name": rel.name,
                "body": rel.body,
                "draft": rel.draft,
                "prerelease": rel.prerelease,
                "target_commitish": rel.target_commitish,
                "html_url": state.urls.html(&format!("/{}/{}/releases/tag/{}", owner.login, repo.name, rel.tag_name)),
            }),
            rel.tag_name,
            rel.draft,
        ),
        None => match extra.get("release") {
            Some(r) if r.is_object() => (
                r.clone(),
                r["tag_name"].as_str().unwrap_or_default().to_string(),
                r["draft"].as_bool().unwrap_or(false),
            ),
            _ => return Ok(()),
        },
    };
    // GitHub: drafts don't trigger created / edited / deleted.
    if draft && matches!(action, "created" | "edited" | "deleted") {
        return Ok(());
    }
    // Workflows from the tagged commit; a missing tag (draft, deleted) falls
    // back to the default branch.
    let tag_ref = format!("refs/tags/{tag_name}");
    let sha = if tag_name.is_empty() {
        None
    } else {
        let r = tag_ref.clone();
        store(state)
            .read(repo.id, move |g| Ok(g.resolve_commit(&r).ok()))
            .await
            .ok()
            .flatten()
    };
    let (git_ref, sha) = match sha {
        Some(sha) => (tag_ref, sha),
        None => match default_head(state, repo).await {
            Some(h) => h,
            None => return Ok(()),
        },
    };
    let actor = user(state, Some(actor_id)).await?;
    let mut payload = json!({
        "action": action,
        "release": release,
        "repository": context::repo_payload(state, repo, owner),
        "sender": context::sender_payload(state, actor.as_ref()),
    });
    if let Some(changes) = extra.get("changes").filter(|c| c.is_object()) {
        payload["changes"] = changes.clone();
    }
    run_default_branch_event(
        state,
        repo,
        owner,
        "release",
        action,
        &git_ref,
        &sha,
        Some(actor_id),
        payload,
    )
    .await
}

/// An issue or issue comment activity to evaluate.
pub(crate) struct IssueEvent<'a> {
    pub issue_id: i64,
    pub comment_id: Option<i64>,
    /// `issues` or `issue_comment`.
    pub event: &'a str,
    pub action: &'a str,
    pub actor_id: i64,
    pub extra: &'a Value,
}

async fn on_issue(
    state: &AppState,
    repo: &db::Repository,
    owner: &db::User,
    ev: IssueEvent<'_>,
) -> anyhow::Result<()> {
    let IssueEvent {
        issue_id,
        comment_id,
        event,
        action,
        actor_id,
        extra,
    } = ev;
    let Some((git_ref, sha)) = default_head(state, repo).await else {
        return Ok(());
    };
    let issue: Option<db::Issue> = sqlx::query_as(&format!(
        "SELECT {} FROM issues WHERE id = $1",
        db::Issue::COLUMNS
    ))
    .bind(issue_id)
    .fetch_optional(&state.db)
    .await?;
    let actor = user(state, Some(actor_id)).await?;
    let issue_json = match issue {
        Some(issue) => {
            let author = user(state, issue.author_id).await?;
            let mut issue_json = json!({
                "id": issue.id,
                "number": issue.number,
                "title": issue.title,
                "body": issue.body,
                "state": issue.state,
                "locked": issue.locked,
                "user": context::sender_payload(state, author.as_ref()),
                "html_url": state.urls.issue_html(&owner.login, &repo.name, issue.number),
                "url": state.urls.issue(&owner.login, &repo.name, issue.number),
                "created_at": bgh_core::time::Timestamp(issue.created_at),
                "updated_at": bgh_core::time::Timestamp(issue.updated_at),
            });
            if issue.is_pull_request {
                issue_json["pull_request"] = json!({
                    "url": state.urls.pull(&owner.login, &repo.name, issue.number),
                    "html_url": state.urls.pull_html(&owner.login, &repo.name, issue.number),
                });
            }
            issue_json
        }
        // Deleted issues: the event's snapshot.
        None => match extra.get("issue") {
            Some(i) if i.is_object() => i.clone(),
            _ => return Ok(()),
        },
    };
    let number = issue_json["number"].as_i64().unwrap_or_default();
    let mut payload = json!({
        "action": action,
        "issue": issue_json,
        "repository": context::repo_payload(state, repo, owner),
        "sender": context::sender_payload(state, actor.as_ref()),
    });
    if let Some(cid) = comment_id {
        let c: Option<db::Comment> = sqlx::query_as(&format!(
            "SELECT {} FROM comments WHERE id = $1",
            db::Comment::COLUMNS
        ))
        .bind(cid)
        .fetch_optional(&state.db)
        .await?;
        if let Some(c) = c {
            let cu = user(state, c.author_id).await?;
            payload["comment"] = json!({
                "id": c.id,
                "body": c.body,
                "user": context::sender_payload(state, cu.as_ref()),
                "created_at": bgh_core::time::Timestamp(c.created_at),
                "updated_at": bgh_core::time::Timestamp(c.updated_at),
                "html_url": state.urls.issue_comment_html(&owner.login, &repo.name, number, c.id),
            });
        } else if let Some(c) = extra.get("comment").filter(|c| c.is_object()) {
            payload["comment"] = c.clone();
        } else {
            return Ok(());
        }
    }
    crate::trigger_events::apply_hints(state, repo, owner, extra, &mut payload).await?;
    run_default_branch_event(
        state,
        repo,
        owner,
        event,
        action,
        &git_ref,
        &sha,
        Some(actor_id),
        payload,
    )
    .await
}

// ---------------------------------------------------------------------------
// workflow_dispatch
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum DispatchError {
    #[error("No ref found for: {0}")]
    NoRef(String),
    #[error("Workflow does not have 'workflow_dispatch' trigger")]
    NoTrigger,
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Validate and normalize dispatch inputs against the workflow's
/// `workflow_dispatch.inputs` (defaults applied, booleans typed).
pub fn dispatch_inputs(def: &Workflow, given: &Map<String, Value>) -> Result<Value, DispatchError> {
    let trig = def
        .on
        .get("workflow_dispatch")
        .ok_or(DispatchError::NoTrigger)?;
    let mut out = Map::new();
    for k in given.keys() {
        if !trig.inputs.contains_key(k) {
            return Err(DispatchError::Invalid(format!(
                "Unexpected inputs provided: [\"{k}\"]"
            )));
        }
    }
    for (name, def) in &trig.inputs {
        let raw = given.get(name).map(|v| match v {
            Value::String(s) => s.clone(),
            other => crate::expr::to_display_string(other),
        });
        let value = match raw.or_else(|| def.default.clone()) {
            Some(v) => v,
            None if def.required => {
                return Err(DispatchError::Invalid(format!(
                    "Required input '{name}' not provided"
                )));
            }
            None => String::new(),
        };
        let typed = match def.r#type.as_str() {
            "boolean" => match value.as_str() {
                "true" => Value::Bool(true),
                "false" | "" => Value::Bool(false),
                _ => {
                    return Err(DispatchError::Invalid(format!(
                        "Provided value '{value}' for input '{name}' not in the list of allowed values"
                    )));
                }
            },
            "number" if !value.is_empty() => match value.parse::<f64>() {
                Ok(n) => serde_json::Number::from_f64(n)
                    .map(|n| {
                        if n.as_f64().is_some_and(|f| f.fract() == 0.0) {
                            json!(n.as_f64().unwrap_or_default() as i64)
                        } else {
                            Value::Number(n)
                        }
                    })
                    .unwrap_or(Value::String(value.clone())),
                Err(_) => {
                    return Err(DispatchError::Invalid(format!(
                        "Provided value '{value}' for input '{name}' is not a number"
                    )));
                }
            },
            "choice" => {
                if !value.is_empty() && !def.options.contains(&value) {
                    return Err(DispatchError::Invalid(format!(
                        "Provided value '{value}' for input '{name}' not in the list of allowed values"
                    )));
                }
                Value::String(value)
            }
            _ => Value::String(value),
        };
        out.insert(name.clone(), typed);
    }
    Ok(Value::Object(out))
}

/// `POST .../dispatches`: run `workflow` at `git_ref` with `inputs`.
pub async fn dispatch(
    state: &AppState,
    repo: &db::Repository,
    owner: &db::User,
    workflow: &WorkflowRow,
    git_ref: &str,
    inputs: &Map<String, Value>,
    actor: &db::User,
) -> Result<i64, DispatchError> {
    let candidates = if git_ref.starts_with("refs/") {
        vec![git_ref.to_string()]
    } else {
        vec![
            format!("refs/heads/{git_ref}"),
            format!("refs/tags/{git_ref}"),
        ]
    };
    let mut resolved = None;
    for c in candidates {
        let c2 = c.clone();
        if let Some(sha) = store(state)
            .read(repo.id, move |g| Ok(g.resolve_commit(&c2).ok()))
            .await
            .map_err(|e| DispatchError::Other(e.into()))?
        {
            resolved = Some((c, sha));
            break;
        }
    }
    let (full_ref, sha) = resolved.ok_or_else(|| DispatchError::NoRef(git_ref.into()))?;
    let files = load_workflows(state, repo.id, &sha).await?;
    let file = files
        .into_iter()
        .find(|f| f.path == workflow.path)
        .ok_or(DispatchError::NoTrigger)?;
    let def = file
        .def
        .as_ref()
        .map_err(|e| DispatchError::Invalid(e.clone()))?;
    let inputs = dispatch_inputs(def, inputs)?;
    let payload = json!({
        "inputs": inputs,
        "ref": full_ref,
        "repository": context::repo_payload(state, repo, owner),
        "sender": context::sender_payload(state, Some(actor)),
        "workflow": workflow.path,
    });
    let head_branch = context::ref_name(&full_ref).0.to_string();
    Ok(engine::create_run(
        state,
        NewRun {
            repo: repo.clone(),
            owner: owner.clone(),
            path: file.path,
            yaml: file.yaml,
            def: file.def,
            event: "workflow_dispatch".into(),
            git_ref: full_ref,
            head_branch: Some(head_branch),
            head_sha: sha,
            head_repo_id: Some(repo.id),
            actor_id: Some(actor.id),
            payload,
            inputs: Some(inputs),
            pull_request_ids: vec![],
        },
    )
    .await?)
}

// ---------------------------------------------------------------------------
// schedule
// ---------------------------------------------------------------------------

/// Fire due `schedule` crons (time window `(schedule_checked_at, now]`).
/// Safe to call concurrently from several processes. Returns runs created.
pub async fn schedule_tick(state: &AppState, now: DateTime<Utc>) -> anyhow::Result<usize> {
    let rows: Vec<WorkflowRow> = sqlx::query_as(&format!(
        "SELECT {} FROM actions_workflows WHERE state = 'active' AND schedules <> '{{}}'",
        WorkflowRow::COLUMNS
    ))
    .fetch_all(&state.db)
    .await?;
    let mut created = 0;
    for wf in rows {
        let since = wf.schedule_checked_at.unwrap_or(wf.updated_at);
        let due: Vec<String> = wf
            .schedules
            .iter()
            .filter(|c| {
                CronSchedule::parse(c)
                    .ok()
                    .and_then(|s| s.next_after(since))
                    .is_some_and(|t| t <= now)
            })
            .cloned()
            .collect();
        // Claim the window; another process may have done it already.
        let claimed = sqlx::query(
            "UPDATE actions_workflows SET schedule_checked_at = $2
              WHERE id = $1 AND schedule_checked_at IS NOT DISTINCT FROM $3",
        )
        .bind(wf.id)
        .bind(now)
        .bind(wf.schedule_checked_at)
        .execute(&state.db)
        .await?
        .rows_affected()
            == 1;
        if !claimed || due.is_empty() {
            continue;
        }
        let Some((repo, owner)) = repo_and_owner(state, wf.repo_id).await? else {
            continue;
        };
        if repo.archived || repo.disabled {
            continue;
        }
        let Some((git_ref, sha)) = default_head(state, &repo).await else {
            continue;
        };
        let files = load_workflows(state, repo.id, &sha).await?;
        let Some(file) = files.into_iter().find(|f| f.path == wf.path) else {
            continue;
        };
        if file.def.is_err() {
            continue;
        }
        for cron in due {
            let payload = json!({
                "schedule": cron,
                "repository": context::repo_payload(state, &repo, &owner),
            });
            engine::create_run(
                state,
                NewRun {
                    repo: repo.clone(),
                    owner: owner.clone(),
                    path: file.path.clone(),
                    yaml: file.yaml.clone(),
                    def: file.def.clone(),
                    event: "schedule".into(),
                    git_ref: git_ref.clone(),
                    head_branch: Some(repo.default_branch.clone()),
                    head_sha: sha.clone(),
                    head_repo_id: Some(repo.id),
                    actor_id: None,
                    payload,
                    inputs: None,
                    pull_request_ids: vec![],
                },
            )
            .await?;
            created += 1;
        }
    }
    Ok(created)
}
