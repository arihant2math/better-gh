//! Writes to the shared `check_suites` / `check_runs` tables (schema in
//! migration 0004; the checks REST API is served by bgh-pulls). Every
//! workflow run owns one check suite (`app_slug = 'actions'`), every job one
//! check run named after the job.

//!
//! Every write records the shared `checkSuite` / `checkRun` sync rows
//! (`tx.sync_model`, BACKEND_PATTERNS.md §8a); bgh-pulls re-syncs the PR
//! rows whose `checks` rollup changed from the `CheckRunUpdated` /
//! `CheckSuiteUpdated` events.

use bgh_core::db::Tx;
use bgh_core::error::ApiResult;
use bgh_core::sync::{SyncAction, shapes::Model as SyncModel};
use serde_json::{Value, json};
use sqlx::PgConnection;

use crate::protocol::Annotation;

pub async fn create_suite(
    tx: &mut Tx,
    repo_id: i64,
    head_sha: &str,
    head_branch: Option<&str>,
) -> ApiResult<i64> {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO check_suites (repo_id, head_sha, head_branch, after_sha, app_slug, status)
         VALUES ($1, $2, $3, $2, 'actions', 'queued') RETURNING id",
    )
    .bind(repo_id)
    .bind(head_sha)
    .bind(head_branch)
    .fetch_one(&mut **tx)
    .await?;
    tx.sync_model(SyncModel::CheckSuite, id, SyncAction::Insert)
        .await?;
    Ok(id)
}

pub async fn set_suite_status(
    tx: &mut Tx,
    suite_id: Option<i64>,
    status: &str,
    conclusion: Option<&str>,
) -> ApiResult<()> {
    let Some(id) = suite_id else { return Ok(()) };
    sqlx::query(
        "UPDATE check_suites SET status = $2, conclusion = $3, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(status)
    .bind(conclusion)
    .execute(&mut **tx)
    .await?;
    tx.sync_model(SyncModel::CheckSuite, id, SyncAction::Update)
        .await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn create_run(
    tx: &mut Tx,
    suite_id: Option<i64>,
    repo_id: i64,
    head_sha: &str,
    name: &str,
    external_id: &str,
    status: &str,
    conclusion: Option<&str>,
) -> ApiResult<Option<i64>> {
    let Some(suite_id) = suite_id else {
        return Ok(None);
    };
    let completed = status == "completed";
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO check_runs (check_suite_id, repo_id, head_sha, name, status, conclusion,
                                 external_id, started_at, completed_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7,
                 CASE WHEN $8 THEN now() END, CASE WHEN $8 THEN now() END)
         RETURNING id",
    )
    .bind(suite_id)
    .bind(repo_id)
    .bind(head_sha)
    .bind(name)
    .bind(status)
    .bind(conclusion)
    .bind(external_id)
    .bind(completed)
    .fetch_one(&mut **tx)
    .await?;
    sqlx::query(
        "UPDATE check_suites SET latest_check_runs_count = latest_check_runs_count + 1
          WHERE id = $1",
    )
    .bind(suite_id)
    .execute(&mut **tx)
    .await?;
    tx.sync_model(SyncModel::CheckRun, id, SyncAction::Insert)
        .await?;
    tx.sync_model(SyncModel::CheckSuite, suite_id, SyncAction::Update)
        .await?;
    Ok(Some(id))
}

/// Reuse a check run that `POST /check-runs/{id}/rerequest` (or the suite
/// variant) reset to `queued` for the re-run of its job, so the rerequested
/// check moves queued → completed instead of being replaced.
pub async fn reuse_run(
    tx: &mut Tx,
    id: i64,
    name: &str,
    status: &str,
    conclusion: Option<&str>,
) -> ApiResult<()> {
    let completed = status == "completed";
    sqlx::query(
        "UPDATE check_runs SET name = $2, status = $3, conclusion = $4, output = '{}',
                started_at = CASE WHEN $5 THEN now() END,
                completed_at = CASE WHEN $5 THEN now() END, updated_at = now()
          WHERE id = $1",
    )
    .bind(id)
    .bind(name)
    .bind(status)
    .bind(conclusion)
    .bind(completed)
    .execute(&mut **tx)
    .await?;
    tx.sync_model(SyncModel::CheckRun, id, SyncAction::Update)
        .await?;
    Ok(())
}

pub async fn set_details_url(tx: &mut Tx, check_run_id: Option<i64>, url: &str) -> ApiResult<()> {
    if let Some(id) = check_run_id {
        sqlx::query("UPDATE check_runs SET details_url = $2 WHERE id = $1")
            .bind(id)
            .bind(url)
            .execute(&mut **tx)
            .await?;
        tx.sync_model(SyncModel::CheckRun, id, SyncAction::Update)
            .await?;
    }
    Ok(())
}

pub async fn start_run(tx: &mut Tx, check_run_id: Option<i64>) -> ApiResult<()> {
    if let Some(id) = check_run_id {
        sqlx::query(
            "UPDATE check_runs SET status = 'in_progress', started_at = coalesce(started_at, now()),
                    updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .execute(&mut **tx)
        .await?;
        tx.sync_model(SyncModel::CheckRun, id, SyncAction::Update)
            .await?;
    }
    Ok(())
}

/// Map a job conclusion to a check run conclusion (same vocabulary).
pub async fn complete_run(
    tx: &mut Tx,
    check_run_id: Option<i64>,
    conclusion: &str,
    summary: Option<&str>,
    annotations: &[Annotation],
) -> ApiResult<()> {
    let Some(id) = check_run_id else {
        return Ok(());
    };
    let output = output_json(summary, annotations);
    sqlx::query(
        "UPDATE check_runs SET status = 'completed', conclusion = $2, output = $3,
                started_at = coalesce(started_at, now()), completed_at = now(), updated_at = now()
          WHERE id = $1",
    )
    .bind(id)
    .bind(conclusion)
    .bind(output)
    .execute(&mut **tx)
    .await?;
    insert_annotations(tx, id, annotations).await?;
    tx.sync_model(SyncModel::CheckRun, id, SyncAction::Update)
        .await?;
    Ok(())
}

/// Rows of `check_run_annotations` (served by the checks API,
/// `GET /check-runs/{id}/annotations`), one batched insert.
async fn insert_annotations(
    conn: &mut PgConnection,
    check_run_id: i64,
    annotations: &[Annotation],
) -> Result<(), sqlx::Error> {
    if annotations.is_empty() {
        return Ok(());
    }
    let mut paths = Vec::new();
    let mut starts = Vec::new();
    let mut ends = Vec::new();
    let mut start_cols = Vec::new();
    let mut end_cols = Vec::new();
    let mut levels = Vec::new();
    let mut titles = Vec::new();
    let mut messages = Vec::new();
    for a in annotations {
        let start = a.start_line.unwrap_or(1).clamp(1, i32::MAX as i64) as i32;
        paths.push(a.path.clone().unwrap_or_else(|| ".github".into()));
        starts.push(start);
        ends.push(
            a.end_line
                .map_or(start, |e| e.clamp(1, i32::MAX as i64) as i32)
                .max(start),
        );
        start_cols.push(a.start_column.map(|c| c.clamp(0, i32::MAX as i64) as i32));
        end_cols.push(a.end_column.map(|c| c.clamp(0, i32::MAX as i64) as i32));
        levels.push(match a.level.as_str() {
            "failure" | "error" => "failure",
            "warning" => "warning",
            _ => "notice",
        });
        titles.push(a.title.clone());
        messages.push(a.message.clone());
    }
    sqlx::query(
        "INSERT INTO check_run_annotations
                (check_run_id, path, start_line, end_line, start_column, end_column,
                 annotation_level, title, message)
         SELECT $1, * FROM UNNEST($2::text[], $3::int[], $4::int[], $5::int[], $6::int[],
                                  $7::text[], $8::text[], $9::text[])",
    )
    .bind(check_run_id)
    .bind(paths)
    .bind(starts)
    .bind(ends)
    .bind(start_cols)
    .bind(end_cols)
    .bind(levels)
    .bind(titles)
    .bind(messages)
    .execute(conn)
    .await?;
    Ok(())
}

/// `check_runs.output`: GitHub's `{title, summary, text, annotations_count}`;
/// annotations are kept under `annotations` (GitHub check-run annotation
/// shape) for the checks API to serve.
pub fn output_json(summary: Option<&str>, annotations: &[Annotation]) -> Value {
    let anns: Vec<Value> = annotations
        .iter()
        .map(|a| {
            json!({
                "path": a.path.clone().unwrap_or_else(|| ".github".into()),
                "start_line": a.start_line.unwrap_or(1),
                "end_line": a.end_line.or(a.start_line).unwrap_or(1),
                "start_column": a.start_column,
                "end_column": a.end_column,
                "annotation_level": a.level,
                "title": a.title,
                "message": a.message,
                "raw_details": null,
            })
        })
        .collect();
    json!({
        "title": null,
        "summary": summary.filter(|s| !s.is_empty()),
        "text": null,
        "annotations_count": anns.len(),
        "annotations": anns,
    })
}

/// `check_run` webhook event for an actions check run.
pub fn check_run_event(
    repo_id: i64,
    check_run_id: Option<i64>,
    action: &str,
    actor_id: Option<i64>,
) -> Option<bgh_core::events::Event> {
    check_run_id.map(|id| bgh_core::events::Event::CheckRunUpdated {
        repo_id,
        check_run_id: id,
        action: action.to_string(),
        actor_id,
    })
}

/// `check_suite` webhook event (also drives `ci_activity` notifications).
pub fn check_suite_event(
    repo_id: i64,
    check_suite_id: Option<i64>,
    action: &str,
    actor_id: Option<i64>,
) -> Option<bgh_core::events::Event> {
    check_suite_id.map(|id| bgh_core::events::Event::CheckSuiteUpdated {
        repo_id,
        check_suite_id: id,
        action: action.to_string(),
        actor_id,
    })
}
