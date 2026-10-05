//! Deployment service: creating deployments and deployment statuses
//! inside a caller's [`Tx`]. The REST API (`api::deployments`) and, from
//! P20 on, the Actions engine (jobs with `environment:`) both go through
//! these functions, so environments are auto-created, `auto_inactive` is
//! applied and the `deployment` / `deployment_status` events are emitted
//! the same way everywhere.
//!
//! Extension points for P20 (environment protection rules):
//! * [`ensure_environment`] is the single place that creates
//!   `actions_environments` rows for deployments;
//! * [`NewDeployment::run_id`] / [`NewDeployment::job_id`] link a
//!   deployment to the job that created it (`deployments.run_id/job_id`);
//! * [`create_status`] is what a job calls for `in_progress` →
//!   `success`/`failure` (pass `environment_url` evaluated from the job's
//!   `environment.url`).

use bgh_core::deployments::{DeploymentRow, DeploymentStatusRow};
use bgh_core::prelude::*;
use serde_json::Value;

/// A deployment to create (ref already resolved to `sha`).
#[derive(Debug, Clone)]
pub struct NewDeployment {
    pub git_ref: String,
    pub sha: String,
    pub task: String,
    pub payload: Value,
    pub environment: String,
    pub description: Option<String>,
    pub transient_environment: bool,
    pub production_environment: bool,
    pub run_id: Option<i64>,
    pub job_id: Option<i64>,
}

/// A deployment status to create.
#[derive(Debug, Clone)]
pub struct NewStatus {
    pub state: String,
    pub description: String,
    /// Moves the deployment to another environment when set.
    pub environment: Option<String>,
    pub target_url: String,
    pub log_url: String,
    pub environment_url: String,
    /// On `success`, mark earlier successful deployments to the same
    /// environment `inactive`.
    pub auto_inactive: bool,
}

/// Valid environment name (same rule as `PUT /environments/{name}`).
pub fn valid_environment(name: &str) -> bool {
    !name.trim().is_empty() && name.len() <= 255 && !name.contains('/')
}

/// The id of `repo_id`'s environment `name`, created when missing.
pub async fn ensure_environment(tx: &mut Tx, repo_id: i64, name: &str) -> ApiResult<i64> {
    if let Some(id) = sqlx::query_scalar(
        "SELECT id FROM actions_environments WHERE repo_id = $1 AND lower(name) = lower($2)",
    )
    .bind(repo_id)
    .bind(name)
    .fetch_optional(&mut **tx)
    .await?
    {
        return Ok(id);
    }
    let id = sqlx::query_scalar(
        "INSERT INTO actions_environments (repo_id, name) VALUES ($1, $2)
         ON CONFLICT (repo_id, lower(name)) DO UPDATE SET updated_at = actions_environments.updated_at
         RETURNING id",
    )
    .bind(repo_id)
    .bind(name)
    .fetch_one(&mut **tx)
    .await?;
    Ok(id)
}

/// Insert a deployment and emit [`Event::DeploymentCreated`].
pub async fn create_deployment(
    tx: &mut Tx,
    repo_id: i64,
    creator_id: Option<i64>,
    d: NewDeployment,
) -> ApiResult<DeploymentRow> {
    let env_id = ensure_environment(tx, repo_id, &d.environment).await?;
    let row: DeploymentRow = sqlx::query_as(&format!(
        "INSERT INTO deployments (repo_id, environment_id, environment, original_environment, sha,
                                  ref, task, payload, description, creator_id,
                                  transient_environment, production_environment, run_id, job_id)
         VALUES ($1, $2, $3, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
         RETURNING {}",
        DeploymentRow::COLUMNS
    ))
    .bind(repo_id)
    .bind(env_id)
    .bind(&d.environment)
    .bind(&d.sha)
    .bind(&d.git_ref)
    .bind(&d.task)
    .bind(&d.payload)
    .bind(&d.description)
    .bind(creator_id)
    .bind(d.transient_environment)
    .bind(d.production_environment)
    .bind(d.run_id)
    .bind(d.job_id)
    .fetch_one(&mut **tx)
    .await?;
    tx.emit(Event::DeploymentCreated {
        repo_id,
        deployment_id: row.id,
        actor_id: creator_id,
    });
    Ok(row)
}

async fn insert_status(
    tx: &mut Tx,
    d: &DeploymentRow,
    creator_id: Option<i64>,
    s: &NewStatus,
    environment: &str,
) -> ApiResult<DeploymentStatusRow> {
    let row: DeploymentStatusRow = sqlx::query_as(&format!(
        "INSERT INTO deployment_statuses (deployment_id, repo_id, state, description, environment,
                                          target_url, log_url, environment_url, creator_id,
                                          created_at, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, clock_timestamp(), clock_timestamp())
         RETURNING {}",
        DeploymentStatusRow::COLUMNS
    ))
    .bind(d.id)
    .bind(d.repo_id)
    .bind(&s.state)
    .bind(&s.description)
    .bind(environment)
    .bind(&s.target_url)
    .bind(&s.log_url)
    .bind(&s.environment_url)
    .bind(creator_id)
    .fetch_one(&mut **tx)
    .await?;
    sqlx::query(
        "UPDATE deployments SET state = $2, latest_status_id = $3, updated_at = $4 WHERE id = $1",
    )
    .bind(d.id)
    .bind(&row.state)
    .bind(row.id)
    .bind(row.created_at)
    .execute(&mut **tx)
    .await?;
    tx.emit(Event::DeploymentStatusCreated {
        repo_id: d.repo_id,
        deployment_id: d.id,
        status_id: row.id,
        state: row.state.clone(),
        actor_id: creator_id,
    });
    Ok(row)
}

/// Insert a status for `deployment_id` (locks the deployment), move the
/// deployment to `s.environment` when given, apply `auto_inactive`, and
/// emit [`Event::DeploymentStatusCreated`] for every status written.
pub async fn create_status(
    tx: &mut Tx,
    repo_id: i64,
    deployment_id: i64,
    creator_id: Option<i64>,
    s: NewStatus,
) -> ApiResult<DeploymentStatusRow> {
    let mut d: DeploymentRow = sqlx::query_as(&format!(
        "SELECT {} FROM deployments WHERE id = $1 AND repo_id = $2 FOR UPDATE",
        DeploymentRow::COLUMNS
    ))
    .bind(deployment_id)
    .bind(repo_id)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(ApiError::NotFound)?;
    if let Some(env) = s.environment.as_deref()
        && env != d.environment
    {
        let env_id = ensure_environment(tx, repo_id, env).await?;
        sqlx::query("UPDATE deployments SET environment = $2, environment_id = $3 WHERE id = $1")
            .bind(d.id)
            .bind(env)
            .bind(env_id)
            .execute(&mut **tx)
            .await?;
        d.environment = env.to_string();
        d.environment_id = Some(env_id);
    }
    let environment = d.environment.clone();
    let row = insert_status(tx, &d, creator_id, &s, &environment).await?;

    if row.state == "success" && s.auto_inactive {
        let older: Vec<DeploymentRow> = sqlx::query_as(&format!(
            "SELECT {} FROM deployments
              WHERE repo_id = $1 AND lower(environment) = lower($2) AND id <> $3
                AND state = 'success'
              ORDER BY id FOR UPDATE",
            DeploymentRow::COLUMNS
        ))
        .bind(repo_id)
        .bind(&environment)
        .bind(d.id)
        .fetch_all(&mut **tx)
        .await?;
        let inactive = NewStatus {
            state: "inactive".into(),
            description: String::new(),
            environment: None,
            target_url: String::new(),
            log_url: String::new(),
            environment_url: String::new(),
            auto_inactive: false,
        };
        for o in &older {
            insert_status(tx, o, creator_id, &inactive, &o.environment).await?;
        }
    }
    Ok(row)
}
