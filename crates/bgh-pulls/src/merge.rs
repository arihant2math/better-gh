//! Merging: `PUT /pulls/{n}/merge` (merge / squash / rebase), branch
//! protection enforcement, `delete_branch_on_merge` (with retargeting of
//! dependent PRs) and `PUT /pulls/{n}/update-branch`.
//!
//! Merges never touch a working tree: `git merge-tree --write-tree`
//! computes the tree, `commit-tree` the commit, and the base ref is moved
//! with an old-value check so concurrent pushes are never clobbered.

use axum::extract::State;
use axum::http::StatusCode;
use bgh_core::events::RefUpdate;
use bgh_core::prelude::*;
use bgh_git::merge::{MergeTree, Person, RebaseResult};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::git;
use crate::model::{self, PULL_FROM, Pull};
use crate::protection::{self, Rules};
use crate::pulls::load_pull;
use crate::timeline;
use bgh_repos::protection::Actor;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MergeMethod {
    Merge,
    Squash,
    Rebase,
}

impl MergeMethod {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "merge" => Some(Self::Merge),
            "squash" => Some(Self::Squash),
            "rebase" => Some(Self::Rebase),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Merge => "merge",
            Self::Squash => "squash",
            Self::Rebase => "rebase",
        }
    }

    fn allowed(self, repo: &db::Repository) -> Result<(), &'static str> {
        match self {
            Self::Merge if !repo.allow_merge_commit => {
                Err("Merge commits are not allowed on this repository.")
            }
            Self::Squash if !repo.allow_squash_merge => {
                Err("Squash merges are not allowed on this repository.")
            }
            Self::Rebase if !repo.allow_rebase_merge => {
                Err("Rebase merges are not allowed on this repository.")
            }
            _ => Ok(()),
        }
    }
}

fn not_allowed(msg: impl Into<String>) -> ApiError {
    ApiError::Status(StatusCode::METHOD_NOT_ALLOWED, msg.into())
}

/// Parameters for [`perform_merge`].
#[derive(Debug, Clone)]
pub struct MergeRequest {
    pub method: MergeMethod,
    pub commit_title: Option<String>,
    pub commit_message: Option<String>,
    /// Expected head SHA (409 if it differs).
    pub sha: Option<String>,
    /// The merge queue merges this PR: the `merge_queue` rule's blocker
    /// ("Changes must be made through the merge queue") does not apply.
    /// Every other merge (REST, GraphQL, auto-merge) passes `false`.
    pub via_merge_queue: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct MergeResult {
    pub sha: String,
    pub merged: bool,
    pub message: String,
}

/// `owner/branch` of a PR's head (merge commit titles).
pub(crate) async fn head_label(state: &AppState, pull: &Pull) -> ApiResult<String> {
    let head_owner = match pull.pr.head_repo_id {
        Some(id) => match db::Repository::find(&state.db, id).await? {
            Some(r) => db::User::find(&state.db, r.owner_id)
                .await?
                .map(|u| u.login),
            None => None,
        },
        None => None,
    };
    Ok(format!(
        "{}/{}",
        head_owner.as_deref().unwrap_or("unknown"),
        pull.pr.head_ref
    ))
}

/// Commit title and message for a merge per repository settings.
pub(crate) async fn messages(
    state: &AppState,
    repo: &db::Repository,
    pull: &Pull,
    head_label: &str,
    req: &MergeRequest,
) -> ApiResult<(String, String)> {
    let n = pull.number();
    let body = pull.issue.body.clone().unwrap_or_default();
    let (title, message) = match req.method {
        MergeMethod::Merge => {
            let title = match repo.merge_commit_title.as_str() {
                "PR_TITLE" => format!("{} (#{n})", pull.issue.title),
                _ => format!("Merge pull request #{n} from {head_label}"),
            };
            let message = match repo.merge_commit_message.as_str() {
                "PR_BODY" => body,
                "BLANK" => String::new(),
                _ => pull.issue.title.clone(),
            };
            (title, message)
        }
        MergeMethod::Squash => {
            let store = git::store(state);
            let shas = bgh_git::merge::rev_list(
                &store,
                repo.id,
                Some(&pull.pr.base_sha),
                &pull.pr.head_sha,
                250,
            )
            .await?;
            let commits = store
                .read(repo.id, move |r| {
                    shas.iter()
                        .map(|s| r.commit(s))
                        .collect::<Result<Vec<_>, _>>()
                })
                .await?;
            let title =
                if repo.squash_merge_commit_title == "COMMIT_OR_PR_TITLE" && commits.len() == 1 {
                    format!("{} (#{n})", commits[0].summary())
                } else {
                    format!("{} (#{n})", pull.issue.title)
                };
            let message = match repo.squash_merge_commit_message.as_str() {
                "PR_BODY" => body,
                "BLANK" => String::new(),
                _ => commits
                    .iter()
                    .map(|c| format!("* {}", c.message.trim_end()))
                    .collect::<Vec<_>>()
                    .join("\n\n"),
            };
            (title, message)
        }
        MergeMethod::Rebase => (String::new(), String::new()),
    };
    Ok((
        req.commit_title.clone().unwrap_or(title),
        req.commit_message.clone().unwrap_or(message),
    ))
}

/// Admins bypass protection unless `enforce_admins`.
fn bypasses(rules: &Rules, permission: Permission) -> bool {
    permission >= Permission::Admin && !rules.enforce_admins
}

/// Merge `pull` as `merger` (whose effective permission on the base repo is
/// `permission`). Used by the REST endpoint and auto-merge.
pub async fn perform_merge(
    state: &AppState,
    repo: &db::Repository,
    repo_owner: &db::User,
    pull: &Pull,
    merger: &db::User,
    permission: Permission,
    req: &MergeRequest,
) -> ApiResult<MergeResult> {
    if pull.pr.merged || !pull.is_open() {
        return Err(not_allowed("Pull Request is not mergeable"));
    }
    if pull.pr.draft {
        return Err(not_allowed("Pull Request is still a draft"));
    }
    if let Some(sha) = &req.sha
        && *sha != pull.pr.head_sha
    {
        return Err(ApiError::conflict(
            "Head branch was modified. Review and try the merge again.",
        ));
    }
    req.method.allowed(repo).map_err(not_allowed)?;
    let rules = protection::rules_for(&state.db, repo.id, &pull.pr.base_ref).await?;
    if rules.lock_branch && !bypasses(&rules, permission) {
        return Err(not_allowed(format!("{} is locked.", pull.pr.base_ref)));
    }
    if rules.linear_history && req.method == MergeMethod::Merge {
        return Err(not_allowed("Merge commits are not allowed on this branch."));
    }
    let store = git::store(state);
    let base_tip = git::branch_tip(&store, repo.id, &pull.pr.base_ref)
        .await?
        .ok_or_else(|| not_allowed("Base branch was deleted"))?;
    if base_tip != pull.pr.base_sha {
        // Base moved and the push isn't processed yet: evaluate against the
        // stored state but merge onto the real tip below.
        tracing::debug!(pull = pull.id(), "base moved since last sync");
    }
    let actor = Actor::for_user(state, repo_owner, merger.id, permission).await?;
    if rules.restricts(&actor) {
        return Err(not_allowed("You're not authorized to push to this branch."));
    }
    let mut ev = protection::evaluate(state, repo, pull, &rules).await?;
    if req.via_merge_queue {
        ev = ev.without_merge_queue();
    }
    if let Some(suite) = protection::merge_suite(
        repo.id,
        &rules,
        &ev,
        &actor,
        &pull.pr.base_ref,
        &base_tip,
        &pull.pr.head_sha,
    ) {
        bgh_repos::rule_eval::record_all(&state.db, &[suite]).await;
    }
    let blocking = ev.unbypassed(&rules, &actor);
    if !blocking.is_empty() {
        return Err(not_allowed(protection::violation_message(&blocking)));
    }

    let head_label = head_label(state, pull).await?;
    let (title, message) = messages(state, repo, pull, &head_label, req).await?;
    let full_message = if message.is_empty() {
        format!("{title}\n")
    } else {
        format!("{title}\n\n{message}\n")
    };
    let merger_id = git::identity(state, merger).await?;
    let committer = Person::from(&git::site_committer(state));
    let merger_person = Person::from(&merger_id);

    let new_tip = match req.method {
        MergeMethod::Merge | MergeMethod::Squash => {
            let tree = match bgh_git::merge::merge_tree(
                &store,
                repo.id,
                &base_tip,
                &pull.pr.head_sha,
                None,
            )
            .await?
            {
                MergeTree::Clean { tree } => tree,
                MergeTree::Conflict { .. } => {
                    return Err(not_allowed("Pull Request is not mergeable"));
                }
            };
            if req.method == MergeMethod::Merge {
                bgh_git::merge::commit_tree(
                    &store,
                    repo.id,
                    &tree,
                    &[&base_tip, &pull.pr.head_sha],
                    &full_message,
                    &merger_person,
                    &committer,
                )
                .await?
            } else {
                // Squash: authored by the PR author.
                let author = match pull.issue.author_id {
                    Some(a) => match db::User::find(&state.db, a).await? {
                        Some(u) => Person::from(&git::identity(state, &u).await?),
                        None => merger_person.clone(),
                    },
                    None => merger_person.clone(),
                };
                bgh_git::merge::commit_tree(
                    &store,
                    repo.id,
                    &tree,
                    &[&base_tip],
                    &full_message,
                    &author,
                    &committer,
                )
                .await?
            }
        }
        MergeMethod::Rebase => {
            let base = pull
                .pr
                .merge_base_sha
                .clone()
                .unwrap_or_else(|| pull.pr.base_sha.clone());
            match bgh_git::merge::rebase(
                &store,
                repo.id,
                &base_tip,
                &base,
                &pull.pr.head_sha,
                &git::site_committer(state),
            )
            .await?
            {
                RebaseResult::Done { head } => head,
                RebaseResult::Conflict { .. } => {
                    return Err(not_allowed("This branch can't be rebased"));
                }
                RebaseResult::HasMerges => {
                    return Err(not_allowed(
                        "This branch can't be rebased because it contains merge commits",
                    ));
                }
            }
        }
    };

    let base_ref = format!("refs/heads/{}", pull.pr.base_ref);
    if !bgh_git::merge::compare_and_swap_ref(&store, repo.id, &base_ref, &new_tip, &base_tip)
        .await?
    {
        return Err(ApiError::conflict(
            "Base branch was modified. Review and try the merge again.",
        ));
    }

    let scope = bgh_core::sync::repo_scope(repo.id);
    let mut updates = vec![RefUpdate {
        old: base_tip.clone(),
        new: new_tip.clone(),
        refname: base_ref,
    }];
    let mut tx = Tx::begin(state).await?;
    sqlx::query(
        "UPDATE pull_requests SET merged = true, merged_at = now(), merged_by_id = $2,
                merge_commit_sha = $3, base_sha = $4, mergeable = NULL, rebaseable = NULL,
                mergeable_state = 'unknown', auto_merge = NULL
          WHERE issue_id = $1",
    )
    .bind(pull.id())
    .bind(merger.id)
    .bind(&new_tip)
    .bind(&base_tip)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE issues SET state = 'closed', state_reason = NULL, closed_at = now(),
                closed_by_id = $2, updated_at = now() WHERE id = $1",
    )
    .bind(pull.id())
    .bind(merger.id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE repositories SET open_issues_count = greatest(open_issues_count - 1, 0)
          WHERE id = $1",
    )
    .bind(repo.id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM pr_requested_reviewers WHERE pull_id = $1")
        .bind(pull.id())
        .execute(&mut *tx)
        .await?;
    timeline::record(
        &mut tx,
        repo.id,
        pull.id(),
        Some(merger.id),
        "merged",
        Some(&new_tip),
        json!({}),
    )
    .await?;
    timeline::record(
        &mut tx,
        repo.id,
        pull.id(),
        Some(merger.id),
        "closed",
        Some(&new_tip),
        json!({}),
    )
    .await?;
    crate::json::sync_pull(&mut tx, &scope, pull.id()).await?;
    tx.emit(Event::PullRequestMerged {
        repo_id: repo.id,
        pull_id: pull.id(),
        actor_id: merger.id,
        merge_commit_sha: new_tip.clone(),
    });
    tx.commit().await?;

    if repo.delete_branch_on_merge
        && let Some(u) = delete_head_branch(state, repo, pull, merger.id).await?
    {
        updates.push(u);
    }
    // pushed_at / size / Event::Push (which re-syncs PRs targeting the base).
    bgh_core::jobs::enqueue_job(
        &state.db,
        &bgh_repos::jobs::PostReceive {
            repo_id: repo.id,
            pusher_id: Some(merger.id),
            updates,
        },
    )
    .await?;
    let _ = repo_owner;
    Ok(MergeResult {
        sha: new_tip,
        merged: true,
        message: "Pull Request successfully merged".into(),
    })
}

/// Delete a merged PR's head branch (same repository only, not the default
/// branch, not protected, not the head of another open PR). Open PRs based
/// on it are retargeted to the merged PR's base.
pub async fn delete_head_branch(
    state: &AppState,
    repo: &db::Repository,
    pull: &Pull,
    actor_id: i64,
) -> ApiResult<Option<RefUpdate>> {
    if pull.pr.head_repo_id != Some(repo.id) || pull.pr.head_ref == repo.default_branch {
        return Ok(None);
    }
    let rules = protection::rules_for(&state.db, repo.id, &pull.pr.head_ref).await?;
    if rules.protected {
        return Ok(None);
    }
    let other_heads: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM {PULL_FROM}
          WHERE i.state = 'open' AND p.head_repo_id = $1 AND p.head_ref = $2 AND i.id <> $3"
    ))
    .bind(repo.id)
    .bind(&pull.pr.head_ref)
    .bind(pull.id())
    .fetch_one(&state.db)
    .await?;
    if other_heads > 0 {
        return Ok(None);
    }
    let store = git::store(state);
    let Some(tip) = git::branch_tip(&store, repo.id, &pull.pr.head_ref).await? else {
        return Ok(None);
    };
    if tip != pull.pr.head_sha {
        return Ok(None); // new commits were pushed: keep the branch
    }

    // Retarget dependents first so they never point at a missing base.
    let dependents: Vec<i64> = sqlx::query_scalar(&format!(
        "SELECT i.id FROM {PULL_FROM}
          WHERE i.state = 'open' AND p.repo_id = $1 AND p.base_ref = $2"
    ))
    .bind(repo.id)
    .bind(&pull.pr.head_ref)
    .fetch_all(&state.db)
    .await?;
    let new_base_tip = git::branch_tip(&store, repo.id, &pull.pr.base_ref).await?;
    if let Some(base_tip) = &new_base_tip {
        for id in dependents {
            let Some(dep) = model::find_by_id(&state.db, id).await? else {
                continue;
            };
            let stats = git::range_stats(state, repo.id, base_tip, &dep.pr.head_sha).await?;
            let mut tx = Tx::begin(state).await?;
            sqlx::query(
                "UPDATE pull_requests SET base_ref = $2, base_sha = $3, merge_base_sha = $4,
                        commits = $5, additions = $6, deletions = $7, changed_files = $8,
                        mergeable = NULL, rebaseable = NULL, mergeable_state = 'unknown'
                  WHERE issue_id = $1",
            )
            .bind(id)
            .bind(&pull.pr.base_ref)
            .bind(base_tip)
            .bind(&stats.merge_base)
            .bind(stats.commits)
            .bind(stats.additions)
            .bind(stats.deletions)
            .bind(stats.changed_files)
            .execute(&mut *tx)
            .await?;
            timeline::record(
                &mut tx,
                repo.id,
                id,
                Some(actor_id),
                "base_ref_changed",
                None,
                json!({"from": pull.pr.head_ref, "to": pull.pr.base_ref}),
            )
            .await?;
            crate::json::sync_pull(&mut tx, &bgh_core::sync::repo_scope(repo.id), id).await?;
            tx.enqueue(&crate::jobs::Refresh {
                pull_id: id,
                codeowners: false,
            })
            .await?;
            tx.emit(Event::PullRequestEdited {
                repo_id: repo.id,
                pull_id: id,
                actor_id,
                changes: json!({"base": {"ref": {"from": pull.pr.head_ref}}}),
            });
            tx.commit().await?;
        }
    }

    let refname = format!("refs/heads/{}", pull.pr.head_ref);
    if bgh_git::write::delete_ref(&store, repo.id, &refname, Some(&tip))
        .await
        .is_err()
    {
        return Ok(None);
    }
    let mut tx = Tx::begin(state).await?;
    timeline::record(
        &mut tx,
        repo.id,
        pull.id(),
        Some(actor_id),
        "head_ref_deleted",
        None,
        json!({"ref": pull.pr.head_ref}),
    )
    .await?;
    tx.commit().await?;
    Ok(Some(RefUpdate {
        old: tip,
        new: bgh_git::ZERO_SHA.to_string(),
        refname,
    }))
}

#[derive(Debug, Deserialize)]
pub struct MergeBody {
    pub commit_title: Option<String>,
    pub commit_message: Option<String>,
    pub sha: Option<String>,
    pub merge_method: Option<String>,
}

pub async fn merge(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    Json(body): Json<MergeBody>,
) -> ApiResult<axum::Json<MergeResult>> {
    let (access, pull) = load_pull(&state, Some(&auth), &owner, &repo, number).await?;
    access.require(Permission::Write)?;
    access.require_not_archived()?;
    access.require_not_mirror()?;
    let method = match body.merge_method.as_deref() {
        None => MergeMethod::Merge,
        Some(m) => MergeMethod::parse(m).ok_or_else(|| {
            ApiError::invalid_field(FieldError::invalid("PullRequest", "merge_method"))
        })?,
    };
    if let Some(sha) = &body.sha
        && !bgh_git::is_sha(sha)
    {
        return Err(ApiError::invalid_field(FieldError::invalid(
            "PullRequest",
            "sha",
        )));
    }
    let req = MergeRequest {
        method,
        commit_title: body.commit_title,
        commit_message: body.commit_message,
        sha: body.sha,
        via_merge_queue: false,
    };
    let out = perform_merge(
        &state,
        &access.repo,
        &access.owner,
        &pull,
        &auth.user,
        access.permission,
        &req,
    )
    .await?;
    Ok(axum::Json(out))
}

// ---------------------------------------------------------------------------
// Update branch
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, Default)]
pub struct UpdateBranchBody {
    pub expected_head_sha: Option<String>,
}

pub async fn update_branch(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    Json(body): Json<UpdateBranchBody>,
) -> ApiResult<(StatusCode, axum::Json<serde_json::Value>)> {
    let (access, pull) = load_pull(&state, Some(&auth), &owner, &repo, number).await?;
    if !pull.is_open() {
        return Err(ApiError::unprocessable("Pull request is closed"));
    }
    let head_repo_id = pull
        .pr
        .head_repo_id
        .ok_or_else(|| ApiError::unprocessable("The head repository does not exist"))?;
    // Write access to the head repository, or maintainer edits + write on base.
    let head_repo = db::Repository::find(&state.db, head_repo_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let head_owner = db::User::find(&state.db, head_repo.owner_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let head_access = if head_repo_id == access.repo.id {
        Some(access.clone())
    } else {
        RepoAccess::for_repo(&state, Some(&auth), head_repo.clone(), head_owner.clone())
            .await
            .ok()
    };
    let can_write_head = head_access
        .as_ref()
        .is_some_and(|a| a.permission >= Permission::Write);
    let maintainer = pull.pr.maintainer_can_modify && access.permission >= Permission::Write;
    if !can_write_head && !maintainer {
        return Err(ApiError::forbidden(
            "Resource not accessible by integration",
        ));
    }
    if let Some(expected) = &body.expected_head_sha
        && *expected != pull.pr.head_sha
    {
        return Err(ApiError::unprocessable(
            "expected head sha didn't match current head ref.",
        ));
    }
    let store = git::store(&state);
    let base_tip = git::branch_tip(&store, access.repo.id, &pull.pr.base_ref)
        .await?
        .ok_or_else(|| ApiError::unprocessable("The base branch does not exist"))?;
    if bgh_git::merge::is_ancestor(&store, access.repo.id, &base_tip, &pull.pr.head_sha).await? {
        return Err(ApiError::unprocessable(
            "There are no new commits on the base branch.",
        ));
    }
    let tree = match bgh_git::merge::merge_tree(
        &store,
        access.repo.id,
        &pull.pr.head_sha,
        &base_tip,
        None,
    )
    .await?
    {
        MergeTree::Clean { tree } => tree,
        MergeTree::Conflict { .. } => {
            return Err(ApiError::unprocessable(
                "merge conflict between base and head",
            ));
        }
    };
    let me = Person::from(&git::identity(&state, &auth.user).await?);
    let committer = Person::from(&git::site_committer(&state));
    let msg = format!(
        "Merge branch '{}' into {}\n",
        pull.pr.base_ref, pull.pr.head_ref
    );
    let commit = bgh_git::merge::commit_tree(
        &store,
        access.repo.id,
        &tree,
        &[&pull.pr.head_sha, &base_tip],
        &msg,
        &me,
        &committer,
    )
    .await?;
    let refname = format!("refs/heads/{}", pull.pr.head_ref);
    if head_repo_id == access.repo.id {
        if !bgh_git::merge::compare_and_swap_ref(
            &store,
            head_repo_id,
            &refname,
            &commit,
            &pull.pr.head_sha,
        )
        .await?
        {
            return Err(ApiError::unprocessable(
                "expected head sha didn't match current head ref.",
            ));
        }
    } else if bgh_git::merge::push_ref(
        &store,
        access.repo.id,
        head_repo_id,
        &commit,
        &refname,
        &pull.pr.head_sha,
    )
    .await
    .is_err()
    {
        return Err(ApiError::unprocessable(
            "expected head sha didn't match current head ref.",
        ));
    }
    bgh_core::jobs::enqueue_job(
        &state.db,
        &bgh_repos::jobs::PostReceive {
            repo_id: head_repo_id,
            pusher_id: Some(auth.user.id),
            updates: vec![RefUpdate {
                old: pull.pr.head_sha.clone(),
                new: commit,
                refname,
            }],
        },
    )
    .await?;
    Ok((
        StatusCode::ACCEPTED,
        axum::Json(json!({
            "message": "Updating pull request branch.",
            "url": state.urls.pull(&access.owner.login, &access.repo.name, pull.number()),
        })),
    ))
}
