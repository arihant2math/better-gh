//! Runner protocol: the job specification a runner receives, what it
//! reports back, and the [`Backend`] trait through which a runner talks to
//! the server — in-process for the built-in runner
//! ([`crate::server::LocalBackend`]), over HTTP long-polling for external
//! `bgh-runner` processes ([`crate::runner::http::HttpBackend`]).
//!
//! HTTP endpoints (`Authorization: RunnerToken <token>`):
//!
//! | Method | Path | Body → Response |
//! |---|---|---|
//! | POST | `/_bgh/actions/runner/register` | [`RegisterRequest`] (no auth) → [`RegisterResponse`] |
//! | POST | `/_bgh/actions/runner/acquire?wait=30` | → 200 [`JobSpec`] or 204 |
//! | POST | `/_bgh/actions/runner/jobs/{id}/logs?step=N` | text/plain chunk → 204 |
//! | POST | `/_bgh/actions/runner/jobs/{id}/steps` | `[StepState]` → [`Heartbeat`] |
//! | POST | `/_bgh/actions/runner/jobs/{id}/complete` | [`JobCompletion`] → 204 |
//! | GET  | `/_bgh/actions/runner/jobs/{id}/artifacts` | → `[ArtifactInfo]` (same run) |
//! | PUT  | `/_bgh/actions/runner/jobs/{id}/artifacts/{name}?retention_days=N` | zip → [`ArtifactInfo`] |
//! | GET  | `/_bgh/actions/runner/jobs/{id}/artifacts/{artifact_id}/zip` | → zip |
//! | DELETE | `/_bgh/actions/runner/self` | unregister (ephemeral runners) |

use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use crate::models::StepState;
pub use crate::workflow::{RunDefaults, Step};

/// Everything a runner needs to execute one job. Job-level expressions
/// (`runs-on`, `env`, `container`, `services`, `timeout-minutes`, ...) are
/// already evaluated; step-level ones are evaluated by the runner.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobSpec {
    pub job_id: i64,
    pub run_id: i64,
    pub run_number: i64,
    pub run_attempt: i32,
    /// Job id in the workflow file.
    pub job_key: String,
    /// Display name (`build (ubuntu-latest, 18)`).
    pub name: String,
    pub workflow_name: String,
    /// `.github/workflows/ci.yml`
    pub workflow_path: String,
    /// `owner/name`
    pub repository: String,
    pub repository_id: i64,
    pub repository_owner: String,
    pub server_url: String,
    pub api_url: String,
    /// `GITHUB_TOKEN` (filled in when the job is acquired).
    #[serde(default)]
    pub token: String,
    /// The `github` context without `token`.
    pub github: Value,
    /// Workflow + job `env`, evaluated.
    pub env: IndexMap<String, String>,
    pub vars: Value,
    /// Filled in when the job is acquired; never stored.
    #[serde(default)]
    pub secrets: IndexMap<String, String>,
    pub matrix: Value,
    pub needs: Value,
    pub inputs: Value,
    pub strategy: Value,
    pub defaults: RunDefaults,
    pub container: Option<ContainerSpec>,
    pub services: IndexMap<String, ContainerSpec>,
    pub steps: Vec<Step>,
    /// Raw `outputs:` expressions, evaluated by the runner at the end.
    pub outputs: IndexMap<String, String>,
    pub timeout_minutes: u64,
    pub environment: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContainerSpec {
    pub image: String,
    pub env: IndexMap<String, String>,
    pub ports: Vec<String>,
    pub volumes: Vec<String>,
    pub options: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
}

/// A check-run annotation from `::error file=..,line=..::msg`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Annotation {
    /// `notice` | `warning` | `failure`
    pub level: String,
    pub message: String,
    pub title: Option<String>,
    pub path: Option<String>,
    pub start_line: Option<i64>,
    pub end_line: Option<i64>,
    pub start_column: Option<i64>,
    pub end_column: Option<i64>,
}

/// Final report of a job.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct JobCompletion {
    /// `success` | `failure` | `cancelled` | `skipped`
    pub conclusion: String,
    pub outputs: IndexMap<String, String>,
    pub steps: Vec<StepState>,
    pub annotations: Vec<Annotation>,
    /// Concatenated `GITHUB_STEP_SUMMARY` markdown.
    pub summary: Option<String>,
}

/// Response to a step update: tells the runner to stop.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Heartbeat {
    pub cancel: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactInfo {
    pub id: i64,
    pub name: String,
    pub size_in_bytes: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterRequest {
    /// Registration token from `POST .../actions/runners/registration-token`.
    pub token: String,
    pub name: String,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub ephemeral: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterResponse {
    pub id: i64,
    pub name: String,
    /// Secret runner token for `Authorization: RunnerToken <token>`.
    pub token: String,
}

/// How a runner talks to the server.
#[async_trait]
pub trait Backend: Send + Sync {
    /// Claim the next job this runner can run, waiting up to `wait`.
    async fn acquire(&self, wait: Duration) -> anyhow::Result<Option<JobSpec>>;
    /// Append raw log text (complete lines) to a step's log.
    async fn append_log(&self, job_id: i64, step: i64, text: &str) -> anyhow::Result<()>;
    /// Report step states; doubles as heartbeat. Returns whether the job
    /// was cancelled.
    async fn update_steps(&self, job_id: i64, steps: &[StepState]) -> anyhow::Result<Heartbeat>;
    async fn complete(&self, job_id: i64, result: &JobCompletion) -> anyhow::Result<()>;
    async fn upload_artifact(
        &self,
        job_id: i64,
        name: &str,
        zip: &Path,
        retention_days: Option<i64>,
    ) -> anyhow::Result<ArtifactInfo>;
    async fn list_artifacts(&self, job_id: i64) -> anyhow::Result<Vec<ArtifactInfo>>;
    async fn download_artifact(
        &self,
        job_id: i64,
        artifact_id: i64,
        dest: &Path,
    ) -> anyhow::Result<()>;
}
