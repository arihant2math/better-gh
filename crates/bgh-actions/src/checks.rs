//! Writes to the shared `check_suites` / `check_runs` tables (schema in
//! migration 0004; the checks REST API is served by bgh-pulls). Every
//! workflow run owns one check suite (`app_slug = 'actions'`), every job one
//! check run named after the job.

use serde_json::{Value, json};
use sqlx::PgConnection;

use crate::protocol::Annotation;

pub async fn create_suite(
    conn: &mut PgConnection,
    repo_id: i64,
    head_sha: &str,
    head_branch: Option<&str>,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "INSERT INTO check_suites (repo_id, head_sha, head_branch, after_sha, app_slug, status)
         VALUES ($1, $2, $3, $2, 'actions', 'queued') RETURNING id",
    )
    .bind(repo_id)
    .bind(head_sha)
    .bind(head_branch)
    .fetch_one(conn)
    .await
}

pub async fn set_suite_status(
    conn: &mut PgConnection,
    suite_id: Option<i64>,
    status: &str,
    conclusion: Option<&str>,
) -> Result<(), sqlx::Error> {
    let Some(id) = suite_id else { return Ok(()) };
    sqlx::query(
        "UPDATE check_suites SET status = $2, conclusion = $3, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(status)
    .bind(conclusion)
    .execute(conn)
    .await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn create_run(
    conn: &mut PgConnection,
    suite_id: Option<i64>,
    repo_id: i64,
    head_sha: &str,
    name: &str,
    external_id: &str,
    status: &str,
    conclusion: Option<&str>,
) -> Result<Option<i64>, sqlx::Error> {
    let Some(suite_id) = suite_id else {
        return Ok(None);
    };
    let completed = status == "completed";
    let id = sqlx::query_scalar(
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
    .fetch_one(conn)
    .await?;
    Ok(Some(id))
}

pub async fn set_details_url(
    conn: &mut PgConnection,
    check_run_id: Option<i64>,
    url: &str,
) -> Result<(), sqlx::Error> {
    if let Some(id) = check_run_id {
        sqlx::query("UPDATE check_runs SET details_url = $2 WHERE id = $1")
            .bind(id)
            .bind(url)
            .execute(conn)
            .await?;
    }
    Ok(())
}

pub async fn start_run(
    conn: &mut PgConnection,
    check_run_id: Option<i64>,
) -> Result<(), sqlx::Error> {
    if let Some(id) = check_run_id {
        sqlx::query(
            "UPDATE check_runs SET status = 'in_progress', started_at = coalesce(started_at, now()),
                    updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .execute(conn)
        .await?;
    }
    Ok(())
}

/// Map a job conclusion to a check run conclusion (same vocabulary).
pub async fn complete_run(
    conn: &mut PgConnection,
    check_run_id: Option<i64>,
    conclusion: &str,
    summary: Option<&str>,
    annotations: &[Annotation],
) -> Result<(), sqlx::Error> {
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
