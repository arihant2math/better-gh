//! Synchronize pull requests with git after pushes: mirror the new head,
//! detect force-pushes, deleted / restored head branches, base moves and
//! externally merged PRs; recompute stats; remap or outdate review
//! comments; dismiss stale approvals; emit `PullRequestSynchronized`.

use std::collections::HashMap;

use bgh_core::prelude::*;
use bgh_git::patch::{self, Side};
use serde_json::json;

use crate::git;
use crate::jobs::{PushSync, Refresh};
use crate::model::{self, PULL_FROM, Pull, ReviewComment};
use crate::{protection, timeline};

/// Find the open PRs affected by a push and synchronize each.
pub async fn on_push(state: &AppState, job: &PushSync) -> ApiResult<()> {
    let branches: Vec<String> = job
        .updates
        .iter()
        .filter_map(|u| u.branch().map(str::to_string))
        .collect();
    if branches.is_empty() {
        return Ok(());
    }
    let ids: Vec<i64> = sqlx::query_scalar(&format!(
        "SELECT i.id FROM {PULL_FROM}
          WHERE i.state = 'open'
            AND ((p.head_repo_id = $1 AND p.head_ref = ANY($2))
              OR (p.repo_id = $1 AND p.base_ref = ANY($2)))
          ORDER BY i.id"
    ))
    .bind(job.repo_id)
    .bind(&branches)
    .fetch_all(&state.db)
    .await?;
    for id in ids {
        synchronize(state, id, job.pusher_id).await?;
    }
    Ok(())
}

struct CommentUpdate {
    id: i64,
    commit_id: String,
    position: Option<i32>,
    line: Option<i32>,
    start_line: Option<i32>,
}

/// Remap current review comments onto the new diff, or mark them outdated
/// (`position`/`line` = NULL) when the commented content changed.
async fn remap_comments(
    state: &AppState,
    pull: &Pull,
    new_head: &str,
    new_base: &str,
) -> ApiResult<Vec<CommentUpdate>> {
    let comments: Vec<ReviewComment> = sqlx::query_as(&format!(
        "SELECT {} FROM pr_review_comments WHERE pull_id = $1 AND position IS NOT NULL",
        ReviewComment::COLUMNS
    ))
    .bind(pull.id())
    .fetch_all(&state.db)
    .await?;
    if comments.is_empty() {
        return Ok(vec![]);
    }
    let store = git::store(state);
    let repo_id = pull.pr.repo_id;
    let old_base = pull
        .pr
        .merge_base_sha
        .clone()
        .unwrap_or_else(|| pull.pr.base_sha.clone());
    let diff = git::diff(state, repo_id, new_base, new_head).await?;
    let mut blobs: HashMap<(String, String), Option<String>> = HashMap::new();
    let mut updates = Vec::new();
    for c in comments {
        let side = if c.side.as_deref() == Some("LEFT") {
            Side::Left
        } else {
            Side::Right
        };
        let (old_rev, new_rev) = match side {
            Side::Right => (c.commit_id.clone(), new_head.to_string()),
            Side::Left => (old_base.clone(), new_base.to_string()),
        };
        let mut same = true;
        for rev in [&old_rev, &new_rev] {
            let key = (rev.clone(), c.path.clone());
            if let std::collections::hash_map::Entry::Vacant(e) = blobs.entry(key) {
                e.insert(git::blob_at(&store, repo_id, rev, &c.path).await?);
            }
        }
        let a = &blobs[&(old_rev.clone(), c.path.clone())];
        let b = &blobs[&(new_rev.clone(), c.path.clone())];
        if a.is_none() || a != b {
            same = false;
        }
        let file = diff.files.iter().find(|f| f.filename == c.path);
        let mut position = None;
        let mut line = None;
        let mut start_line = None;
        if same && let Some(p) = file.and_then(|f| f.patch.as_deref()) {
            let lines = patch::parse_patch(p);
            if c.subject_type == "file" {
                position = Some(1);
            } else if let Some(l) = c
                .line
                .and_then(|l| patch::find_line(&lines, side, l as u32))
            {
                position = Some(l.position as i32);
                line = c.line;
                start_line = c.start_line;
            }
        } else if same && c.subject_type == "file" && file.is_some() {
            position = Some(1);
        }
        updates.push(CommentUpdate {
            id: c.id,
            commit_id: if position.is_some() {
                new_head.to_string()
            } else {
                c.commit_id.clone()
            },
            position,
            line,
            start_line,
        });
    }
    Ok(updates)
}

/// Synchronize one PR with the current state of its head and base
/// branches. Idempotent.
pub async fn synchronize(state: &AppState, pull_id: i64, actor_id: Option<i64>) -> ApiResult<()> {
    let Some(pull) = model::find_by_id(&state.db, pull_id).await? else {
        return Ok(());
    };
    if !pull.is_open() || pull.pr.merged {
        return Ok(());
    }
    let store = git::store(state);
    let repo_id = pull.pr.repo_id;

    // Head.
    let head_tip = match pull.pr.head_repo_id {
        Some(h) => git::branch_tip(&store, h, &pull.pr.head_ref).await?,
        None => None,
    };
    let last_head_event: Option<String> = sqlx::query_scalar(
        "SELECT event FROM issue_events
          WHERE issue_id = $1 AND event IN ('head_ref_deleted', 'head_ref_restored')
          ORDER BY id DESC LIMIT 1",
    )
    .bind(pull.id())
    .fetch_optional(&state.db)
    .await?;
    let head_deleted_before = last_head_event.as_deref() == Some("head_ref_deleted");
    let mut new_head = pull.pr.head_sha.clone();
    let mut head_event: Option<(&str, serde_json::Value)> = None;
    match &head_tip {
        None => {
            if !head_deleted_before {
                head_event = Some(("head_ref_deleted", json!({"ref": pull.pr.head_ref})));
            }
        }
        Some(tip) => {
            if head_deleted_before {
                head_event = Some(("head_ref_restored", json!({"ref": pull.pr.head_ref})));
            }
            if *tip != pull.pr.head_sha {
                new_head = git::mirror_head(
                    &store,
                    repo_id,
                    model::head_repo(&pull),
                    &pull.pr.head_ref,
                    pull.number(),
                    tip,
                )
                .await?;
            }
        }
    }
    let head_changed = new_head != pull.pr.head_sha;
    let force_pushed = head_changed
        && !bgh_git::merge::is_ancestor(&store, repo_id, &pull.pr.head_sha, &new_head)
            .await
            .unwrap_or(false);

    // Base.
    let base_tip = git::branch_tip(&store, repo_id, &pull.pr.base_ref).await?;
    let new_base = base_tip.clone().unwrap_or_else(|| pull.pr.base_sha.clone());
    let base_changed = new_base != pull.pr.base_sha;

    if !head_changed && !base_changed && head_event.is_none() {
        return Ok(());
    }

    // Merged outside the platform (head reachable from the new base tip).
    let externally_merged = base_changed
        && bgh_git::merge::is_ancestor(&store, repo_id, &new_head, &new_base)
            .await
            .unwrap_or(false);

    let stats = if head_changed || base_changed {
        Some(git::range_stats(state, repo_id, &new_base, &new_head).await?)
    } else {
        None
    };
    let comment_updates = match (&stats, head_changed || base_changed) {
        (Some(s), true) => {
            let mb = s.merge_base.clone().unwrap_or_else(|| new_base.clone());
            remap_comments(state, &pull, &new_head, &mb).await?
        }
        _ => vec![],
    };
    let rules = protection::rules_for(&state.db, repo_id, &pull.pr.base_ref).await?;
    let dismiss_stale = head_changed
        && rules
            .reviews
            .as_ref()
            .is_some_and(|r| r.dismiss_stale_reviews);

    let scope = bgh_core::sync::repo_scope(repo_id);
    let mut tx = Tx::begin(state).await?;
    let Some(locked) = model::lock(&mut *tx, pull.id()).await? else {
        return Ok(());
    };
    if locked.pr.head_sha != pull.pr.head_sha || locked.pr.base_sha != pull.pr.base_sha {
        // Raced with another synchronization; it (or a retry) wins.
        return Ok(());
    }
    if let Some((event, data)) = &head_event {
        timeline::record(
            &mut tx,
            repo_id,
            pull.id(),
            actor_id,
            event,
            None,
            data.clone(),
        )
        .await?;
    }
    if let Some(s) = &stats {
        sqlx::query(
            "UPDATE pull_requests SET head_sha = $2, base_sha = $3, merge_base_sha = $4,
                    commits = $5, additions = $6, deletions = $7, changed_files = $8,
                    mergeable = NULL, rebaseable = NULL, mergeable_state = 'unknown'
              WHERE issue_id = $1",
        )
        .bind(pull.id())
        .bind(&new_head)
        .bind(&new_base)
        .bind(&s.merge_base)
        .bind(s.commits)
        .bind(s.additions)
        .bind(s.deletions)
        .bind(s.changed_files)
        .execute(&mut *tx)
        .await?;
        sqlx::query("UPDATE issues SET updated_at = now() WHERE id = $1")
            .bind(pull.id())
            .execute(&mut *tx)
            .await?;
    }
    for u in &comment_updates {
        let row: ReviewComment = sqlx::query_as(&format!(
            "UPDATE pr_review_comments SET commit_id = $2, position = $3, line = $4,
                    start_line = $5
              WHERE id = $1 RETURNING {}",
            ReviewComment::COLUMNS
        ))
        .bind(u.id)
        .bind(&u.commit_id)
        .bind(u.position)
        .bind(u.line)
        .bind(u.start_line)
        .fetch_one(&mut *tx)
        .await?;
        tx.sync(
            &scope,
            "reviewComment",
            row.id,
            SyncAction::Update,
            &crate::comments::sync_json(&row),
        )
        .await?;
    }
    if force_pushed {
        timeline::record(
            &mut tx,
            repo_id,
            pull.id(),
            actor_id,
            "head_ref_force_pushed",
            Some(&new_head),
            json!({"before": pull.pr.head_sha, "after": new_head}),
        )
        .await?;
    }
    if dismiss_stale {
        let dismissed: Vec<i64> = sqlx::query_scalar(
            "UPDATE pr_reviews SET state = 'DISMISSED', dismissed_at = now(), updated_at = now(),
                    dismissal_message = 'Stale review dismissed after new commits were pushed.'
              WHERE pull_id = $1 AND state = 'APPROVED' RETURNING id",
        )
        .bind(pull.id())
        .fetch_all(&mut *tx)
        .await?;
        for id in dismissed {
            timeline::record(
                &mut tx,
                repo_id,
                pull.id(),
                actor_id,
                "review_dismissed",
                None,
                json!({"dismissed_review": {"review_id": id, "state": "approved",
                        "dismissal_message": null, "dismissal_commit_id": new_head}}),
            )
            .await?;
            tx.sync(
                &scope,
                "review",
                id,
                SyncAction::Update,
                &json!({"id": id, "state": "DISMISSED"}),
            )
            .await?;
            tx.emit(Event::PullRequestReviewDismissed {
                repo_id,
                pull_id: pull.id(),
                review_id: id,
                actor_id,
            });
        }
    }
    if externally_merged {
        sqlx::query(
            "UPDATE pull_requests SET merged = true, merged_at = now(), merged_by_id = $2,
                    merge_commit_sha = $3, mergeable = NULL, rebaseable = NULL,
                    mergeable_state = 'unknown', auto_merge = NULL
              WHERE issue_id = $1",
        )
        .bind(pull.id())
        .bind(actor_id)
        .bind(&new_base)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE issues SET state = 'closed', closed_at = now(), closed_by_id = $2,
                    updated_at = now() WHERE id = $1",
        )
        .bind(pull.id())
        .bind(actor_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE repositories SET open_issues_count = greatest(open_issues_count - 1, 0)
              WHERE id = $1",
        )
        .bind(repo_id)
        .execute(&mut *tx)
        .await?;
        timeline::record(
            &mut tx,
            repo_id,
            pull.id(),
            actor_id,
            "merged",
            Some(&new_base),
            json!({}),
        )
        .await?;
        timeline::record(
            &mut tx,
            repo_id,
            pull.id(),
            actor_id,
            "closed",
            Some(&new_base),
            json!({}),
        )
        .await?;
        if let Some(a) = actor_id {
            tx.emit(Event::PullRequestMerged {
                repo_id,
                pull_id: pull.id(),
                actor_id: a,
                merge_commit_sha: new_base.clone(),
            });
        }
    } else if head_changed || base_changed {
        tx.enqueue(&Refresh {
            pull_id: pull.id(),
            codeowners: head_changed,
        })
        .await?;
    }
    if head_changed {
        tx.emit(Event::PullRequestSynchronized {
            repo_id,
            pull_id: pull.id(),
            actor_id,
            before: pull.pr.head_sha.clone(),
            after: new_head.clone(),
        });
    }
    crate::json::sync_pull(&mut tx, &scope, pull.id()).await?;
    tx.commit().await?;
    Ok(())
}
