//! Re-running Actions jobs from the Checks API / Checks tab.
//!
//! `POST /check-runs/{id}/rerequest` and `POST /check-suites/{id}/rerequest`
//! (bgh-pulls) reset the check to `queued` and emit `CheckRunRerequested` /
//! `CheckSuiteRerequested`. For Actions' own checks this re-runs the job
//! (plus its dependents) or the whole run as a new attempt; the new jobs
//! reuse the reset check runs ([`crate::engine`]), which therefore move
//! queued → completed. A rerequest while the run is still going restores
//! the check run from its job.

use std::collections::HashSet;
use std::sync::Arc;

use bgh_core::AppState;
use bgh_core::db::Tx;
use bgh_core::events::Event;
use bgh_core::jobs::JobPayload;
use serde::{Deserialize, Serialize};

use crate::checks;
use crate::engine;
use crate::models::{JobRow, RunRow};

/// Durable re-run for a rerequested check run or suite.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rerequest {
    pub check_run_id: Option<i64>,
    pub check_suite_id: Option<i64>,
    pub actor_id: i64,
}

impl JobPayload for Rerequest {
    const KIND: &'static str = "actions.rerequest";
    const MAX_ATTEMPTS: i32 = 3;
}

/// Event listener: rerequests of Actions checks → [`Rerequest`].
pub async fn on_event(state: AppState, event: Arc<Event>) -> anyhow::Result<()> {
    let job = match &*event {
        Event::CheckRunRerequested {
            check_run_id,
            actor_id,
            ..
        } => Rerequest {
            check_run_id: Some(*check_run_id),
            check_suite_id: None,
            actor_id: *actor_id,
        },
        Event::CheckSuiteRerequested {
            check_suite_id,
            actor_id,
            ..
        } => Rerequest {
            check_run_id: None,
            check_suite_id: Some(*check_suite_id),
            actor_id: *actor_id,
        },
        _ => return Ok(()),
    };
    let mut tx = state.db.begin().await?;
    if bgh_core::events::claim_effect(&mut tx).await? {
        bgh_core::jobs::enqueue_job(&mut *tx, &job).await?;
        tx.commit().await?;
    }
    Ok(())
}

pub async fn rerequest_job(state: AppState, job: Rerequest) -> anyhow::Result<()> {
    if let Some(id) = job.check_run_id {
        // The job of the run's latest attempt that reports to this check.
        let row: Option<JobRow> = sqlx::query_as(&format!(
            "SELECT {} FROM actions_jobs j
              WHERE j.check_run_id = $1
                AND j.run_attempt = (SELECT run_attempt FROM actions_runs WHERE id = j.run_id)
              ORDER BY j.id DESC LIMIT 1",
            JobRow::COLUMNS
                .split(", ")
                .map(|c| format!("j.{c}"))
                .collect::<Vec<_>>()
                .join(", ")
        ))
        .bind(id)
        .fetch_optional(&state.db)
        .await?;
        let Some(row) = row else { return Ok(()) };
        let Some(run) = RunRow::find(&state.db, row.run_id).await? else {
            return Ok(());
        };
        if run.status != "completed" {
            return restore(&state, &row).await;
        }
        let only: HashSet<String> = [row.job_key.clone()].into_iter().collect();
        return engine::rerun(&state, run.id, job.actor_id, Some(only)).await;
    }
    if let Some(suite) = job.check_suite_id {
        let run: Option<RunRow> = sqlx::query_as(&format!(
            "SELECT {} FROM actions_runs WHERE check_suite_id = $1",
            RunRow::COLUMNS
        ))
        .bind(suite)
        .fetch_optional(&state.db)
        .await?;
        let Some(run) = run else { return Ok(()) };
        if run.status != "completed" {
            let rows: Vec<JobRow> = sqlx::query_as(&format!(
                "SELECT {} FROM actions_jobs WHERE run_id = $1 AND run_attempt = $2",
                JobRow::COLUMNS
            ))
            .bind(run.id)
            .bind(run.run_attempt)
            .fetch_all(&state.db)
            .await?;
            for r in &rows {
                restore(&state, r).await?;
            }
            return Ok(());
        }
        return engine::rerun(&state, run.id, job.actor_id, None).await;
    }
    Ok(())
}

/// Put a reset check run back in line with its (not re-run) job.
async fn restore(state: &AppState, row: &JobRow) -> anyhow::Result<()> {
    let Some(id) = row.check_run_id else {
        return Ok(());
    };
    let mut tx = Tx::begin(state).await?;
    match row.status.as_str() {
        "completed" => {
            let conclusion = row.conclusion.as_deref().unwrap_or("neutral");
            checks::complete_run(&mut tx, Some(id), conclusion, None, &[]).await?;
        }
        "in_progress" => checks::start_run(&mut tx, Some(id)).await?,
        _ => {}
    }
    if let Some(ev) = checks::check_run_event(row.repo_id, Some(id), &row.status, None) {
        tx.emit(ev);
    }
    tx.commit().await?;
    Ok(())
}
