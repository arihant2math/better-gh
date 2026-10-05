//! Database rows of the actions tables (migration 1000).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::FromRow;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct WorkflowRow {
    pub id: i64,
    pub repo_id: i64,
    pub path: String,
    pub name: String,
    pub state: String,
    pub next_run_number: i64,
    pub schedules: Vec<String>,
    pub schedule_checked_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl WorkflowRow {
    pub const COLUMNS: &'static str = "id, repo_id, path, name, state, next_run_number, schedules, \
        schedule_checked_at, created_at, updated_at";
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct RunRow {
    pub id: i64,
    pub repo_id: i64,
    pub workflow_id: i64,
    pub run_number: i64,
    pub run_attempt: i32,
    pub name: String,
    pub display_title: String,
    pub event: String,
    pub status: String,
    pub conclusion: Option<String>,
    #[sqlx(rename = "ref")]
    pub git_ref: String,
    pub head_branch: Option<String>,
    pub head_sha: String,
    pub head_repo_id: Option<i64>,
    pub actor_id: Option<i64>,
    pub triggering_actor_id: Option<i64>,
    pub check_suite_id: Option<i64>,
    pub pull_request_ids: Vec<i64>,
    pub event_payload: Value,
    pub workflow_yaml: String,
    pub workflow_def: Value,
    pub inputs: Option<Value>,
    pub concurrency_group: Option<String>,
    pub cancel_requested: bool,
    pub run_started_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl RunRow {
    pub const COLUMNS: &'static str = "id, repo_id, workflow_id, run_number, run_attempt, name, \
        display_title, event, status, conclusion, ref, head_branch, head_sha, head_repo_id, \
        actor_id, triggering_actor_id, check_suite_id, pull_request_ids, event_payload, \
        workflow_yaml, workflow_def, inputs, concurrency_group, cancel_requested, run_started_at, created_at, \
        updated_at";

    pub async fn find(db: impl sqlx::PgExecutor<'_>, id: i64) -> Result<Option<Self>, sqlx::Error> {
        sqlx::query_as(&format!(
            "SELECT {} FROM actions_runs WHERE id = $1",
            Self::COLUMNS
        ))
        .bind(id)
        .fetch_optional(db)
        .await
    }
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct JobRow {
    pub id: i64,
    pub run_id: i64,
    pub repo_id: i64,
    pub run_attempt: i32,
    pub job_key: String,
    pub name: String,
    pub matrix: Option<Value>,
    pub status: String,
    pub conclusion: Option<String>,
    pub head_sha: String,
    pub head_branch: Option<String>,
    pub labels: Vec<String>,
    pub runner_id: Option<i64>,
    pub runner_name: Option<String>,
    pub check_run_id: Option<i64>,
    pub spec: Option<Value>,
    pub steps: Value,
    pub outputs: Value,
    pub continue_on_error: bool,
    pub timeout_minutes: i32,
    pub cancel_requested: bool,
    pub token_id: Option<i64>,
    pub logs_job_id: Option<i64>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl JobRow {
    pub const COLUMNS: &'static str = "id, run_id, repo_id, run_attempt, job_key, name, matrix, \
        status, conclusion, head_sha, head_branch, labels, runner_id, runner_name, check_run_id, \
        spec, steps, outputs, continue_on_error, timeout_minutes, cancel_requested, token_id, \
        logs_job_id, started_at, completed_at, created_at, updated_at";

    pub async fn find(db: impl sqlx::PgExecutor<'_>, id: i64) -> Result<Option<Self>, sqlx::Error> {
        sqlx::query_as(&format!(
            "SELECT {} FROM actions_jobs WHERE id = $1",
            Self::COLUMNS
        ))
        .bind(id)
        .fetch_optional(db)
        .await
    }

    /// Job whose log files hold this job's logs (itself unless copied).
    pub fn log_owner(&self) -> i64 {
        self.logs_job_id.unwrap_or(self.id)
    }
}

/// One entry of `actions_jobs.steps`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StepState {
    pub number: i64,
    pub name: String,
    pub status: String,
    pub conclusion: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct RunnerRow {
    pub id: i64,
    pub repo_id: Option<i64>,
    pub org_id: Option<i64>,
    pub name: String,
    pub os: String,
    pub system_labels: Vec<String>,
    pub labels: Vec<String>,
    pub ephemeral: bool,
    pub builtin: bool,
    pub busy: bool,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

impl RunnerRow {
    pub const COLUMNS: &'static str = "id, repo_id, org_id, name, os, system_labels, labels, \
        ephemeral, builtin, busy, last_seen_at, created_at";

    /// Online when seen within the last two minutes (runners poll every ~30 s).
    pub fn online(&self) -> bool {
        self.last_seen_at
            .is_some_and(|t| Utc::now() - t < chrono::Duration::minutes(2))
    }

    pub fn all_labels(&self) -> Vec<String> {
        self.system_labels
            .iter()
            .chain(self.labels.iter())
            .cloned()
            .collect()
    }
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct ArtifactRow {
    pub id: i64,
    pub repo_id: i64,
    pub run_id: i64,
    pub job_id: Option<i64>,
    pub name: String,
    pub size_in_bytes: i64,
    pub digest: Option<String>,
    pub expired: bool,
    pub expires_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ArtifactRow {
    pub const COLUMNS: &'static str = "id, repo_id, run_id, job_id, name, size_in_bytes, digest, \
        expired, expires_at, created_at, updated_at";
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct EnvironmentRow {
    pub id: i64,
    pub repo_id: i64,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Minutes a job waits before it may start (0 = none).
    pub wait_timer: i32,
    pub prevent_self_review: bool,
    pub can_admins_bypass: bool,
    /// `None` (any ref), `protected` or `custom`.
    pub branch_policy: Option<String>,
}

impl EnvironmentRow {
    pub const COLUMNS: &'static str = "id, repo_id, name, created_at, updated_at, wait_timer, \
        prevent_self_review, can_admins_bypass, branch_policy";
}

#[derive(Debug, Clone, FromRow)]
pub struct SecretRow {
    pub id: i64,
    pub name: String,
    pub value_enc: Vec<u8>,
    pub visibility: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl SecretRow {
    pub const COLUMNS: &'static str = "id, name, value_enc, visibility, created_at, updated_at";
}

#[derive(Debug, Clone, FromRow)]
pub struct VariableRow {
    pub id: i64,
    pub name: String,
    pub value: String,
    pub visibility: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl VariableRow {
    pub const COLUMNS: &'static str = "id, name, value, visibility, created_at, updated_at";
}
