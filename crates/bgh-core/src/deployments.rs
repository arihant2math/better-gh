//! Deployments and deployment statuses (`deployments`,
//! `deployment_statuses`, migration 3100): row types and GitHub's REST
//! shapes, shared by the REST API (bgh-actions) and the `deployment` /
//! `deployment_status` webhook payloads (bgh-notify).

use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use crate::models::api::SimpleUser;
use crate::models::db;
use crate::node_id::{self, NodeType};
use crate::time::Timestamp;
use crate::urls::Urls;

/// Every deployment status state GitHub accepts.
pub const STATES: [&str; 7] = [
    "error",
    "failure",
    "inactive",
    "in_progress",
    "queued",
    "pending",
    "success",
];

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct DeploymentRow {
    pub id: i64,
    pub repo_id: i64,
    pub environment_id: Option<i64>,
    pub environment: String,
    pub original_environment: String,
    pub sha: String,
    #[sqlx(rename = "ref")]
    pub git_ref: String,
    pub task: String,
    pub payload: Value,
    pub description: Option<String>,
    pub creator_id: Option<i64>,
    pub transient_environment: bool,
    pub production_environment: bool,
    /// Latest status state (`None` before the first status).
    pub state: Option<String>,
    pub latest_status_id: Option<i64>,
    pub run_id: Option<i64>,
    pub job_id: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl DeploymentRow {
    pub const COLUMNS: &'static str = "id, repo_id, environment_id, environment, \
        original_environment, sha, ref, task, payload, description, creator_id, \
        transient_environment, production_environment, state, latest_status_id, run_id, \
        job_id, created_at, updated_at";

    pub async fn find(
        db: impl sqlx::PgExecutor<'_>,
        repo_id: i64,
        id: i64,
    ) -> Result<Option<Self>, sqlx::Error> {
        sqlx::query_as(&format!(
            "SELECT {} FROM deployments WHERE id = $1 AND repo_id = $2",
            Self::COLUMNS
        ))
        .bind(id)
        .bind(repo_id)
        .fetch_optional(db)
        .await
    }
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct DeploymentStatusRow {
    pub id: i64,
    pub deployment_id: i64,
    pub repo_id: i64,
    pub state: String,
    pub description: String,
    pub environment: String,
    pub target_url: String,
    pub log_url: String,
    pub environment_url: String,
    pub creator_id: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl DeploymentStatusRow {
    pub const COLUMNS: &'static str = "id, deployment_id, repo_id, state, description, \
        environment, target_url, log_url, environment_url, creator_id, created_at, updated_at";

    pub async fn find(
        db: impl sqlx::PgExecutor<'_>,
        deployment_id: i64,
        id: i64,
    ) -> Result<Option<Self>, sqlx::Error> {
        sqlx::query_as(&format!(
            "SELECT {} FROM deployment_statuses WHERE id = $1 AND deployment_id = $2",
            Self::COLUMNS
        ))
        .bind(id)
        .bind(deployment_id)
        .fetch_optional(db)
        .await
    }
}

fn user_json(urls: &Urls, u: Option<&db::User>) -> Value {
    serde_json::to_value(SimpleUser::or_ghost(urls, u)).unwrap_or(Value::Null)
}

/// `GET /repos/{o}/{r}/deployments/{id}` (GitHub's `deployment` schema).
pub fn deployment_json(
    urls: &Urls,
    owner: &str,
    repo: &str,
    d: &DeploymentRow,
    creator: Option<&db::User>,
) -> Value {
    let repo_url = urls.repo(owner, repo);
    let url = format!("{repo_url}/deployments/{}", d.id);
    json!({
        "url": url,
        "id": d.id,
        "node_id": node_id::encode(NodeType::Deployment, d.id),
        "sha": d.sha,
        "ref": d.git_ref,
        "task": d.task,
        "payload": d.payload,
        "original_environment": d.original_environment,
        "environment": d.environment,
        "description": d.description,
        "creator": user_json(urls, creator),
        "created_at": Timestamp(d.created_at),
        "updated_at": Timestamp(d.updated_at),
        "statuses_url": format!("{url}/statuses"),
        "repository_url": repo_url,
        "transient_environment": d.transient_environment,
        "production_environment": d.production_environment,
        "performed_via_github_app": null,
    })
}

/// `GET /repos/{o}/{r}/deployments/{id}/statuses/{status_id}`.
pub fn status_json(
    urls: &Urls,
    owner: &str,
    repo: &str,
    s: &DeploymentStatusRow,
    creator: Option<&db::User>,
) -> Value {
    let repo_url = urls.repo(owner, repo);
    let deployment_url = format!("{repo_url}/deployments/{}", s.deployment_id);
    json!({
        "url": format!("{deployment_url}/statuses/{}", s.id),
        "id": s.id,
        "node_id": node_id::encode(NodeType::DeploymentStatus, s.id),
        "state": s.state,
        "creator": user_json(urls, creator),
        "description": s.description,
        "environment": s.environment,
        "target_url": s.target_url,
        "created_at": Timestamp(s.created_at),
        "updated_at": Timestamp(s.updated_at),
        "deployment_url": deployment_url,
        "repository_url": repo_url,
        "environment_url": s.environment_url,
        "log_url": s.log_url,
        "performed_via_github_app": null,
    })
}

/// Latest deployment of a commit in one environment (merge box,
/// deployments page).
#[derive(Debug, Clone, serde::Serialize)]
pub struct EnvironmentDeployment {
    pub deployment_id: i64,
    pub environment: String,
    /// Latest status state (`None` before the first status).
    pub state: Option<String>,
    pub environment_url: Option<String>,
    pub log_url: Option<String>,
    pub production_environment: bool,
    pub transient_environment: bool,
    pub updated_at: Timestamp,
}

/// For each environment, the latest deployment of `sha` in `repo_id`.
pub async fn latest_for_sha(
    db: impl sqlx::PgExecutor<'_>,
    repo_id: i64,
    sha: &str,
) -> Result<Vec<EnvironmentDeployment>, sqlx::Error> {
    #[derive(sqlx::FromRow)]
    struct Row {
        id: i64,
        environment: String,
        state: Option<String>,
        environment_url: Option<String>,
        log_url: Option<String>,
        production_environment: bool,
        transient_environment: bool,
        updated_at: DateTime<Utc>,
    }
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT DISTINCT ON (lower(d.environment))
                d.id, d.environment, d.state, s.environment_url, s.log_url,
                d.production_environment, d.transient_environment, d.updated_at
           FROM deployments d
           LEFT JOIN deployment_statuses s ON s.id = d.latest_status_id
          WHERE d.repo_id = $1 AND d.sha = $2
          ORDER BY lower(d.environment), d.id DESC",
    )
    .bind(repo_id)
    .bind(sha)
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| EnvironmentDeployment {
            deployment_id: r.id,
            environment: r.environment,
            state: r.state,
            environment_url: r.environment_url.filter(|u| !u.is_empty()),
            log_url: r.log_url.filter(|u| !u.is_empty()),
            production_environment: r.production_environment,
            transient_environment: r.transient_environment,
            updated_at: Timestamp(r.updated_at),
        })
        .collect())
}
