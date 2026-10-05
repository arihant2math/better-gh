//! Write-side building blocks shared by the handlers (and usable by other
//! crates, e.g. bgh-pulls, for the conversation half of pull requests).
//!
//! Every function runs inside the caller's [`Tx`], records sync actions in
//! the `repo:{id}` scope, maintains counters, writes timeline events and
//! queues domain events.

use bgh_core::prelude::*;
use serde_json::{Value, json};

use crate::json::EventRow;

/// Maximum assignees per issue (GitHub's limit).
pub const MAX_ASSIGNEES: usize = 10;

/// Load an issue (or PR) by number; 404 if missing.
pub async fn find_issue(
    db: impl sqlx::PgExecutor<'_>,
    repo_id: i64,
    number: i64,
) -> ApiResult<db::Issue> {
    sqlx::query_as::<_, db::Issue>(&format!(
        "SELECT {} FROM issues WHERE repo_id = $1 AND number = $2",
        db::Issue::COLUMNS
    ))
    .bind(repo_id)
    .bind(number)
    .fetch_optional(db)
    .await?
    .ok_or(ApiError::NotFound)
}

/// Load an issue by id.
pub async fn issue_by_id(db: impl sqlx::PgExecutor<'_>, id: i64) -> ApiResult<db::Issue> {
    sqlx::query_as::<_, db::Issue>(&format!(
        "SELECT {} FROM issues WHERE id = $1",
        db::Issue::COLUMNS
    ))
    .bind(id)
    .fetch_optional(db)
    .await?
    .ok_or(ApiError::NotFound)
}

/// Re-read and row-lock an issue inside a transaction (serializes
/// concurrent writers to the same issue).
pub async fn lock_issue(tx: &mut Tx, id: i64) -> ApiResult<db::Issue> {
    sqlx::query_as::<_, db::Issue>(&format!(
        "SELECT {} FROM issues WHERE id = $1 FOR UPDATE",
        db::Issue::COLUMNS
    ))
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(ApiError::NotFound)
}

/// Append a timeline event and sync it.
pub async fn add_event(
    tx: &mut Tx,
    issue: &db::Issue,
    actor_id: Option<i64>,
    event: &str,
    commit_id: Option<&str>,
    data: Value,
) -> ApiResult<EventRow> {
    let row: EventRow = sqlx::query_as(&format!(
        "INSERT INTO issue_events (issue_id, repo_id, actor_id, event, commit_id, data, created_at)
         VALUES ($1, $2, $3, $4, $5, $6, clock_timestamp()) RETURNING {}",
        EventRow::COLUMNS
    ))
    .bind(issue.id)
    .bind(issue.repo_id)
    .bind(actor_id)
    .bind(event)
    .bind(commit_id)
    .bind(&data)
    .fetch_one(&mut **tx)
    .await?;
    tx.sync_model(SyncModel::IssueEvent, row.id, SyncAction::Insert)
        .await?;
    Ok(row)
}

/// Record a comment (`model: "comment"`) as a sync action (shape from
/// `bgh_core::sync::shapes`).
pub async fn sync_comment(tx: &mut Tx, comment_id: i64, action: SyncAction) -> ApiResult<()> {
    tx.sync_model(SyncModel::Comment, comment_id, action)
        .await?;
    Ok(())
}

/// Bump `updated_at`, then record the issue's current state as a sync
/// action (with `body` for inserts). Returns the fresh row.
pub async fn touch_and_sync(
    tx: &mut Tx,
    issue_id: i64,
    action: SyncAction,
) -> ApiResult<db::Issue> {
    touch_and_sync_with(tx, issue_id, action, action == SyncAction::Insert).await
}

/// [`touch_and_sync`] choosing whether to include the lazy `body`.
pub async fn touch_and_sync_with(
    tx: &mut Tx,
    issue_id: i64,
    action: SyncAction,
    with_body: bool,
) -> ApiResult<db::Issue> {
    let issue: db::Issue = sqlx::query_as(&format!(
        "UPDATE issues SET updated_at = now() WHERE id = $1 RETURNING {}",
        db::Issue::COLUMNS
    ))
    .bind(issue_id)
    .fetch_one(&mut **tx)
    .await?;
    tx.sync_issue(issue.id, action, with_body).await?;
    Ok(issue)
}

/// Record `issue` (as already loaded in `tx`) as a sync action, without
/// touching `updated_at`.
pub async fn sync_issue_row(tx: &mut Tx, issue: &db::Issue, action: SyncAction) -> ApiResult<()> {
    tx.sync_issue(issue.id, action, false).await?;
    Ok(())
}

/// Re-sync the repository row (its `openIssues` / `openPulls` changed).
pub async fn sync_repo_open_issues(tx: &mut Tx, repo_id: i64) -> ApiResult<()> {
    tx.sync_model(SyncModel::Repo, repo_id, SyncAction::Update)
        .await?;
    Ok(())
}

/// Recompute `open_issues` / `closed_issues` of milestones and sync them.
pub async fn refresh_milestones(tx: &mut Tx, ids: &[i64]) -> ApiResult<()> {
    let mut ids: Vec<i64> = ids.to_vec();
    ids.sort_unstable();
    ids.dedup();
    if ids.is_empty() {
        return Ok(());
    }
    let rows: Vec<db::Milestone> = sqlx::query_as(&format!(
        "UPDATE milestones m SET
            open_issues = (SELECT count(*) FROM issues i WHERE i.milestone_id = m.id AND i.state = 'open'),
            closed_issues = (SELECT count(*) FROM issues i WHERE i.milestone_id = m.id AND i.state = 'closed')
          WHERE m.id = ANY($1) RETURNING {}",
        db::Milestone::COLUMNS
    ))
    .bind(&ids)
    .fetch_all(&mut **tx)
    .await?;
    let ids: Vec<i64> = rows.iter().map(|m| m.id).collect();
    tx.sync_models(SyncModel::Milestone, &ids, SyncAction::Update)
        .await?;
    Ok(())
}

/// Adjust the repository's `open_issues_count` (issues + PRs, like GitHub).
pub async fn adjust_open_count(tx: &mut Tx, repo_id: i64, delta: i64) -> ApiResult<()> {
    sqlx::query(
        "UPDATE repositories SET open_issues_count = GREATEST(open_issues_count + $2, 0) WHERE id = $1",
    )
    .bind(repo_id)
    .bind(delta)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Allocate the next issue/PR number of a repository and count the new
/// open item in `open_issues_count`.
pub async fn allocate_number(tx: &mut Tx, repo_id: i64) -> ApiResult<i64> {
    Ok(sqlx::query_scalar(
        "UPDATE repositories SET next_issue_number = next_issue_number + 1,
                open_issues_count = open_issues_count + 1
          WHERE id = $1 RETURNING next_issue_number - 1",
    )
    .bind(repo_id)
    .fetch_one(&mut **tx)
    .await?)
}

/// Subscribe a user to an issue thread (no-op if a row exists, so
/// explicit unsubscribes are kept). Returns whether a row was created.
pub async fn subscribe(
    tx: &mut Tx,
    issue: &db::Issue,
    user_id: i64,
    reason: &str,
) -> ApiResult<bool> {
    let subject_type = if issue.is_pull_request {
        "PullRequest"
    } else {
        "Issue"
    };
    let n = sqlx::query(
        "INSERT INTO thread_subscriptions (user_id, subject_type, subject_id, repo_id, reason)
         VALUES ($1, $2, $3, $4, $5) ON CONFLICT DO NOTHING",
    )
    .bind(user_id)
    .bind(subject_type)
    .bind(issue.id)
    .bind(issue.repo_id)
    .bind(reason)
    .execute(&mut **tx)
    .await?
    .rows_affected();
    Ok(n > 0)
}

/// Close or reopen. Returns `false` when nothing changed. Does not sync
/// the issue row (callers do once after all changes).
pub async fn set_state(
    tx: &mut Tx,
    issue: &db::Issue,
    actor_id: i64,
    new_state: &str,
    reason: Option<&str>,
    commit_id: Option<&str>,
) -> ApiResult<bool> {
    set_state_with(tx, issue, actor_id, new_state, reason, commit_id, json!({})).await
}

/// [`set_state`] with `extra` fields merged into the `closed` / `reopened`
/// event's data (e.g. the pull request that closed the issue).
pub async fn set_state_with(
    tx: &mut Tx,
    issue: &db::Issue,
    actor_id: i64,
    new_state: &str,
    reason: Option<&str>,
    commit_id: Option<&str>,
    extra: Value,
) -> ApiResult<bool> {
    let event_data = |reason: &str| {
        let mut d = json!({ "state_reason": reason });
        if let (Value::Object(m), Value::Object(x)) = (&mut d, &extra) {
            m.extend(x.clone());
        }
        d
    };
    if issue.state == new_state {
        if new_state == "closed"
            && let Some(r) = reason
            && issue.state_reason.as_deref() != Some(r)
        {
            sqlx::query("UPDATE issues SET state_reason = $2 WHERE id = $1")
                .bind(issue.id)
                .bind(r)
                .execute(&mut **tx)
                .await?;
            return Ok(true);
        }
        return Ok(false);
    }
    let (repo_id, issue_id) = (issue.repo_id, issue.id);
    if new_state == "closed" {
        let reason = reason.unwrap_or("completed");
        sqlx::query(
            "UPDATE issues SET state = 'closed', state_reason = $2, closed_at = now(),
                    closed_by_id = $3 WHERE id = $1",
        )
        .bind(issue_id)
        .bind(reason)
        .bind(actor_id)
        .execute(&mut **tx)
        .await?;
        adjust_open_count(tx, repo_id, -1).await?;
        add_event(
            tx,
            issue,
            Some(actor_id),
            "closed",
            commit_id,
            event_data(reason),
        )
        .await?;
        tx.emit(if issue.is_pull_request {
            Event::PullRequestClosed {
                repo_id,
                pull_id: issue_id,
                actor_id,
            }
        } else {
            Event::IssueClosed {
                repo_id,
                issue_id,
                actor_id,
            }
        });
    } else {
        sqlx::query(
            "UPDATE issues SET state = 'open', state_reason = 'reopened', closed_at = NULL,
                    closed_by_id = NULL WHERE id = $1",
        )
        .bind(issue_id)
        .execute(&mut **tx)
        .await?;
        adjust_open_count(tx, repo_id, 1).await?;
        add_event(
            tx,
            issue,
            Some(actor_id),
            "reopened",
            commit_id,
            event_data("reopened"),
        )
        .await?;
        tx.emit(if issue.is_pull_request {
            Event::PullRequestReopened {
                repo_id,
                pull_id: issue_id,
                actor_id,
            }
        } else {
            Event::IssueReopened {
                repo_id,
                issue_id,
                actor_id,
            }
        });
    }
    if let Some(m) = issue.milestone_id {
        refresh_milestones(tx, &[m]).await?;
    }
    if !issue.is_pull_request {
        sync_repo_open_issues(tx, repo_id).await?;
    }
    Ok(true)
}

/// Normalize a label color (`#FFAA00` → `ffaa00`); `None` if invalid.
pub fn normalize_color(c: &str) -> Option<String> {
    let c = c.trim().trim_start_matches('#');
    (c.len() == 6 && c.bytes().all(|b| b.is_ascii_hexdigit())).then(|| c.to_ascii_lowercase())
}

/// Resolve label names in a repository. Missing labels are created (like
/// GitHub's add-labels endpoint) when `create_missing`, else rejected with
/// 422.
pub async fn resolve_labels(
    tx: &mut Tx,
    repo_id: i64,
    names: &[String],
    create_missing: bool,
) -> ApiResult<Vec<db::Label>> {
    let mut out: Vec<db::Label> = Vec::new();
    let names: Vec<String> = names
        .iter()
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
        .collect();
    if names.is_empty() {
        return Ok(out);
    }
    let lower: Vec<String> = names.iter().map(|n| n.to_lowercase()).collect();
    let existing: Vec<db::Label> = sqlx::query_as(&format!(
        "SELECT {} FROM labels WHERE repo_id = $1 AND lower(name) = ANY($2)",
        db::Label::COLUMNS
    ))
    .bind(repo_id)
    .bind(&lower)
    .fetch_all(&mut **tx)
    .await?;
    for name in &names {
        if out.iter().any(|l| l.name.eq_ignore_ascii_case(name)) {
            continue;
        }
        if let Some(l) = existing
            .iter()
            .find(|l| l.name.to_lowercase() == name.to_lowercase())
        {
            out.push(l.clone());
            continue;
        }
        if !create_missing {
            return Err(ApiError::invalid_field(FieldError::invalid(
                "Label", "name",
            )));
        }
        if name.chars().count() > 50 {
            return Err(ApiError::invalid_field(FieldError::invalid(
                "Label", "name",
            )));
        }
        let label: db::Label = sqlx::query_as(&format!(
            "INSERT INTO labels (repo_id, name) VALUES ($1, $2)
             ON CONFLICT (repo_id, lower(name)) DO UPDATE SET name = labels.name
             RETURNING {}",
            db::Label::COLUMNS
        ))
        .bind(repo_id)
        .bind(name)
        .fetch_one(&mut **tx)
        .await?;
        tx.sync_model(SyncModel::Label, label.id, SyncAction::Insert)
            .await?;
        out.push(label);
    }
    Ok(out)
}

fn label_data(l: &db::Label) -> Value {
    json!({ "label": { "name": l.name, "color": l.color }, "label_id": l.id })
}

/// Apply labels to an issue; returns the labels newly added.
pub async fn add_labels(
    tx: &mut Tx,
    issue: &db::Issue,
    actor_id: i64,
    labels: &[db::Label],
) -> ApiResult<Vec<db::Label>> {
    let mut added = Vec::new();
    for l in labels {
        let n = sqlx::query(
            "INSERT INTO issue_labels (issue_id, label_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
        )
        .bind(issue.id)
        .bind(l.id)
        .execute(&mut **tx)
        .await?
        .rows_affected();
        if n > 0 {
            add_event(tx, issue, Some(actor_id), "labeled", None, label_data(l)).await?;
            tx.emit(Event::IssueLabeled {
                repo_id: issue.repo_id,
                issue_id: issue.id,
                label_id: l.id,
                actor_id,
            });
            added.push(l.clone());
        }
    }
    Ok(added)
}

/// Remove labels from an issue; returns the labels actually removed.
pub async fn remove_labels(
    tx: &mut Tx,
    issue: &db::Issue,
    actor_id: i64,
    labels: &[db::Label],
) -> ApiResult<Vec<db::Label>> {
    let mut removed = Vec::new();
    for l in labels {
        let n = sqlx::query("DELETE FROM issue_labels WHERE issue_id = $1 AND label_id = $2")
            .bind(issue.id)
            .bind(l.id)
            .execute(&mut **tx)
            .await?
            .rows_affected();
        if n > 0 {
            add_event(tx, issue, Some(actor_id), "unlabeled", None, label_data(l)).await?;
            tx.emit(Event::IssueUnlabeled {
                repo_id: issue.repo_id,
                issue_id: issue.id,
                label_id: l.id,
                actor_id,
            });
            removed.push(l.clone());
        }
    }
    Ok(removed)
}

/// Current labels of an issue.
pub async fn current_labels(tx: &mut Tx, issue_id: i64) -> ApiResult<Vec<db::Label>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {} FROM labels l JOIN issue_labels il ON il.label_id = l.id
          WHERE il.issue_id = $1 ORDER BY lower(l.name)",
        db::prefixed("l", db::Label::COLUMNS)
    ))
    .bind(issue_id)
    .fetch_all(&mut **tx)
    .await?)
}

/// Replace the label set of an issue. Returns whether anything changed.
pub async fn replace_labels(
    tx: &mut Tx,
    issue: &db::Issue,
    actor_id: i64,
    labels: &[db::Label],
) -> ApiResult<bool> {
    let current = current_labels(tx, issue.id).await?;
    let remove: Vec<db::Label> = current
        .iter()
        .filter(|c| !labels.iter().any(|l| l.id == c.id))
        .cloned()
        .collect();
    let removed = remove_labels(tx, issue, actor_id, &remove).await?;
    let added = add_labels(tx, issue, actor_id, labels).await?;
    Ok(!removed.is_empty() || !added.is_empty())
}

/// Assign users; returns the ids newly assigned. Silently stops at
/// [`MAX_ASSIGNEES`].
pub async fn add_assignees(
    tx: &mut Tx,
    issue: &db::Issue,
    actor_id: i64,
    user_ids: &[i64],
) -> ApiResult<Vec<i64>> {
    let mut count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM issue_assignees WHERE issue_id = $1")
            .bind(issue.id)
            .fetch_one(&mut **tx)
            .await?;
    let mut added = Vec::new();
    for &uid in user_ids {
        if count >= MAX_ASSIGNEES as i64 {
            break;
        }
        let n = sqlx::query(
            "INSERT INTO issue_assignees (issue_id, user_id, created_at) VALUES ($1, $2, clock_timestamp())
             ON CONFLICT DO NOTHING",
        )
        .bind(issue.id)
        .bind(uid)
        .execute(&mut **tx)
        .await?
        .rows_affected();
        if n > 0 {
            count += 1;
            add_event(
                tx,
                issue,
                Some(actor_id),
                "assigned",
                None,
                json!({ "assignee_id": uid, "assigner_id": actor_id }),
            )
            .await?;
            subscribe(tx, issue, uid, "assign").await?;
            tx.emit(Event::IssueAssigned {
                repo_id: issue.repo_id,
                issue_id: issue.id,
                assignee_id: uid,
                actor_id,
            });
            added.push(uid);
        }
    }
    Ok(added)
}

/// Unassign users; returns the ids actually removed.
pub async fn remove_assignees(
    tx: &mut Tx,
    issue: &db::Issue,
    actor_id: i64,
    user_ids: &[i64],
) -> ApiResult<Vec<i64>> {
    let mut removed = Vec::new();
    for &uid in user_ids {
        let n = sqlx::query("DELETE FROM issue_assignees WHERE issue_id = $1 AND user_id = $2")
            .bind(issue.id)
            .bind(uid)
            .execute(&mut **tx)
            .await?
            .rows_affected();
        if n > 0 {
            add_event(
                tx,
                issue,
                Some(actor_id),
                "unassigned",
                None,
                json!({ "assignee_id": uid, "assigner_id": actor_id }),
            )
            .await?;
            tx.emit(Event::IssueUnassigned {
                repo_id: issue.repo_id,
                issue_id: issue.id,
                assignee_id: uid,
                actor_id,
            });
            removed.push(uid);
        }
    }
    Ok(removed)
}

/// Replace the assignee set. Returns whether anything changed.
pub async fn replace_assignees(
    tx: &mut Tx,
    issue: &db::Issue,
    actor_id: i64,
    user_ids: &[i64],
) -> ApiResult<bool> {
    let current: Vec<i64> =
        sqlx::query_scalar("SELECT user_id FROM issue_assignees WHERE issue_id = $1")
            .bind(issue.id)
            .fetch_all(&mut **tx)
            .await?;
    let remove: Vec<i64> = current
        .iter()
        .copied()
        .filter(|c| !user_ids.contains(c))
        .collect();
    let removed = remove_assignees(tx, issue, actor_id, &remove).await?;
    let added = add_assignees(tx, issue, actor_id, user_ids).await?;
    Ok(!removed.is_empty() || !added.is_empty())
}

/// Set or clear the milestone. Returns whether it changed.
pub async fn set_milestone(
    tx: &mut Tx,
    issue: &db::Issue,
    actor_id: i64,
    milestone: Option<&db::Milestone>,
) -> ApiResult<bool> {
    let new_id = milestone.map(|m| m.id);
    if issue.milestone_id == new_id {
        return Ok(false);
    }
    sqlx::query("UPDATE issues SET milestone_id = $2 WHERE id = $1")
        .bind(issue.id)
        .bind(new_id)
        .execute(&mut **tx)
        .await?;
    if let Some(old) = issue.milestone_id {
        let title: Option<String> =
            sqlx::query_scalar("SELECT title FROM milestones WHERE id = $1")
                .bind(old)
                .fetch_optional(&mut **tx)
                .await?;
        add_event(
            tx,
            issue,
            Some(actor_id),
            "demilestoned",
            None,
            json!({ "milestone": { "title": title.unwrap_or_default() } }),
        )
        .await?;
        tx.emit(Event::IssueDemilestoned {
            repo_id: issue.repo_id,
            issue_id: issue.id,
            milestone_id: old,
            actor_id,
        });
    }
    if let Some(m) = milestone {
        add_event(
            tx,
            issue,
            Some(actor_id),
            "milestoned",
            None,
            json!({ "milestone": { "title": m.title } }),
        )
        .await?;
        tx.emit(Event::IssueMilestoned {
            repo_id: issue.repo_id,
            issue_id: issue.id,
            milestone_id: m.id,
            actor_id,
        });
    }
    let ids: Vec<i64> = issue.milestone_id.into_iter().chain(new_id).collect();
    refresh_milestones(tx, &ids).await?;
    Ok(true)
}

/// Whether `user_id` may be assigned issues in `repo` (triage or above).
pub async fn is_assignable(
    state: &AppState,
    repo: &db::Repository,
    user: &db::User,
) -> ApiResult<bool> {
    if user.is_org() || user.is_suspended() {
        return Ok(false);
    }
    let p = bgh_core::perms::repo_permission(&state.db, Some(user.id), repo).await?;
    Ok(p >= Permission::Triage)
}

#[cfg(test)]
mod tests {
    #[test]
    fn colors() {
        assert_eq!(super::normalize_color("#FFaa00").as_deref(), Some("ffaa00"));
        assert_eq!(super::normalize_color("fff"), None);
        assert_eq!(super::normalize_color("gggggg"), None);
    }
}
