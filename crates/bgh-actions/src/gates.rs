//! Environment protection rules (P20): wait timers, required reviewers,
//! deployment branch policies, and the deployments Actions jobs create.
//!
//! * [`job_gate`] decides, when the engine materializes a job naming an
//!   environment, whether the job may run at all (branch policy), must wait
//!   ([`Gate::Wait`]: status `waiting`, one `actions_job_gates` row), or
//!   is queued at once.
//! * [`release_ready`] queues waiting jobs whose timer elapsed and whose
//!   reviews (if any) approved them; the maintenance loop calls it, and so
//!   does [`review`] after an approval.
//! * [`review`] is `POST /actions/runs/{id}/pending_deployments`.
//! * [`job_started`] / [`job_finished`] write the job's deployment statuses
//!   (`in_progress` → `success` / `failure` / `error`), with
//!   `environment.url` evaluated when the job completes.
//!
//! Secrets of an environment are only decrypted when a runner claims the
//! job (`server::prepare_spec`), and runners only claim `queued` jobs, so a
//! waiting job never sees them.

use std::collections::{HashMap, HashSet};

use bgh_core::AppState;
use bgh_core::deployments::DeploymentRow;
use bgh_core::events::Event;
use bgh_core::models::{api, db};
use bgh_core::node_id::{self, NodeType};
use bgh_core::prelude::{ApiError, ApiResult, FieldError, SyncAction, Tx};
use bgh_core::time::Timestamp;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::{FromRow, PgConnection};

use crate::deployments::{NewDeployment, NewStatus, create_deployment, create_status};
use crate::engine::{self, StoredJob};
use crate::expr::{self, MapContext};
use crate::models::{EnvironmentRow, JobRow, RunRow};

/// Most required reviewers an environment may have (GitHub's limit).
pub const MAX_REVIEWERS: usize = 6;
/// Longest wait timer in minutes (30 days, GitHub's limit).
pub const MAX_WAIT_TIMER: i64 = 43_200;

/// One required reviewer: a user or a team.
#[derive(Debug, Clone, Copy, PartialEq, Eq, FromRow)]
pub struct Reviewer {
    pub user_id: Option<i64>,
    pub team_id: Option<i64>,
}

/// A custom deployment branch or tag policy.
#[derive(Debug, Clone, FromRow)]
pub struct BranchPolicyRow {
    pub id: i64,
    pub environment_id: i64,
    pub name: String,
    #[sqlx(rename = "type")]
    pub kind: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl BranchPolicyRow {
    pub const COLUMNS: &'static str = "id, environment_id, name, type, created_at, updated_at";
}

/// GitHub `deployment-branch-policy` JSON.
pub fn branch_policy_json(p: &BranchPolicyRow) -> Value {
    json!({
        "id": p.id,
        "node_id": node_id::encode(NodeType::DeploymentBranchPolicy, p.id),
        "name": p.name,
        "type": p.kind,
    })
}

/// Reviewers of each of `env_ids` (in configured order).
pub async fn reviewers_for(
    conn: &mut PgConnection,
    env_ids: &[i64],
) -> sqlx::Result<HashMap<i64, Vec<Reviewer>>> {
    #[derive(FromRow)]
    struct Row {
        environment_id: i64,
        user_id: Option<i64>,
        team_id: Option<i64>,
    }
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT environment_id, user_id, team_id FROM actions_environment_reviewers
          WHERE environment_id = ANY($1) ORDER BY environment_id, position",
    )
    .bind(env_ids)
    .fetch_all(conn)
    .await?;
    let mut out: HashMap<i64, Vec<Reviewer>> = HashMap::new();
    for r in rows {
        out.entry(r.environment_id).or_default().push(Reviewer {
            user_id: r.user_id,
            team_id: r.team_id,
        });
    }
    Ok(out)
}

/// `[{type, reviewer}]` for reviewers (users and teams batch-loaded).
pub async fn reviewers_json(
    state: &AppState,
    conn: &mut PgConnection,
    reviewers: &[Reviewer],
) -> sqlx::Result<Vec<Value>> {
    let user_ids: Vec<i64> = reviewers.iter().filter_map(|r| r.user_id).collect();
    let team_ids: Vec<i64> = reviewers.iter().filter_map(|r| r.team_id).collect();
    let users = if user_ids.is_empty() {
        vec![]
    } else {
        db::User::find_many(&mut *conn, &user_ids).await?
    };
    let teams: Vec<(db::Team, String)> = if team_ids.is_empty() {
        vec![]
    } else {
        #[derive(FromRow)]
        struct T {
            #[sqlx(flatten)]
            team: db::Team,
            org_login: String,
        }
        let rows: Vec<T> = sqlx::query_as(&format!(
            "SELECT {}, o.login AS org_login FROM teams t JOIN users o ON o.id = t.org_id
              WHERE t.id = ANY($1)",
            db::prefixed("t", db::Team::COLUMNS)
        ))
        .bind(&team_ids)
        .fetch_all(&mut *conn)
        .await?;
        rows.into_iter().map(|r| (r.team, r.org_login)).collect()
    };
    Ok(reviewers
        .iter()
        .filter_map(|r| {
            match (r.user_id, r.team_id) {
            (Some(u), _) => users.iter().find(|x| x.id == u).map(|u| {
                json!({"type": "User", "reviewer": api::SimpleUser::new(&state.urls, u)})
            }),
            (_, Some(t)) => teams.iter().find(|(x, _)| x.id == t).map(|(t, org)| {
                json!({"type": "Team", "reviewer": api::TeamSimple::new(&state.urls, org, t)})
            }),
            _ => None,
        }
        })
        .collect())
}

/// Users a reviewer list resolves to (team reviewers expanded).
pub async fn reviewer_user_ids(
    conn: &mut PgConnection,
    reviewers: &[Reviewer],
) -> sqlx::Result<Vec<i64>> {
    let mut ids: Vec<i64> = reviewers.iter().filter_map(|r| r.user_id).collect();
    let team_ids: Vec<i64> = reviewers.iter().filter_map(|r| r.team_id).collect();
    if !team_ids.is_empty() {
        let members: Vec<i64> =
            sqlx::query_scalar("SELECT DISTINCT user_id FROM team_members WHERE team_id = ANY($1)")
                .bind(&team_ids)
                .fetch_all(conn)
                .await?;
        ids.extend(members);
    }
    let mut seen = HashSet::new();
    ids.retain(|id| seen.insert(*id));
    Ok(ids)
}

/// An environment with its protection rules.
#[derive(Debug, Clone)]
pub struct Protection {
    pub env: EnvironmentRow,
    pub reviewers: Vec<Reviewer>,
    pub policies: Vec<BranchPolicyRow>,
}

impl Protection {
    pub async fn load(conn: &mut PgConnection, env_id: i64) -> sqlx::Result<Option<Self>> {
        let env: Option<EnvironmentRow> = sqlx::query_as(&format!(
            "SELECT {} FROM actions_environments WHERE id = $1",
            EnvironmentRow::COLUMNS
        ))
        .bind(env_id)
        .fetch_optional(&mut *conn)
        .await?;
        let Some(env) = env else { return Ok(None) };
        let reviewers = reviewers_for(&mut *conn, &[env.id])
            .await?
            .remove(&env.id)
            .unwrap_or_default();
        let policies = if env.branch_policy.as_deref() == Some("custom") {
            sqlx::query_as(&format!(
                "SELECT {} FROM actions_environment_branch_policies
                  WHERE environment_id = $1 ORDER BY id",
                BranchPolicyRow::COLUMNS
            ))
            .bind(env.id)
            .fetch_all(&mut *conn)
            .await?
        } else {
            vec![]
        };
        Ok(Some(Self {
            env,
            reviewers,
            policies,
        }))
    }

    /// Whether a job must wait (timer or reviewers).
    pub fn gated(&self) -> bool {
        self.env.wait_timer > 0 || !self.reviewers.is_empty()
    }

    /// Whether `user_id` is a required reviewer (directly or via a team).
    pub async fn is_reviewer(&self, conn: &mut PgConnection, user_id: i64) -> sqlx::Result<bool> {
        if self.reviewers.iter().any(|r| r.user_id == Some(user_id)) {
            return Ok(true);
        }
        let teams: Vec<i64> = self.reviewers.iter().filter_map(|r| r.team_id).collect();
        if teams.is_empty() {
            return Ok(false);
        }
        sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM team_members WHERE team_id = ANY($1) AND user_id = $2)",
        )
        .bind(&teams)
        .bind(user_id)
        .fetch_one(conn)
        .await
    }
}

/// What a run deploys from: a branch or a tag (by name).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeployRef {
    Branch(String),
    Tag(String),
}

impl DeployRef {
    /// From a run's `GITHUB_REF` and head branch (pull request runs deploy
    /// from their head branch).
    pub fn of_run(run: &RunRow) -> Option<Self> {
        if let Some(tag) = run.git_ref.strip_prefix("refs/tags/") {
            return Some(Self::Tag(tag.to_string()));
        }
        if let Some(b) = run.git_ref.strip_prefix("refs/heads/") {
            return Some(Self::Branch(b.to_string()));
        }
        run.head_branch
            .clone()
            .filter(|b| !b.is_empty())
            .map(Self::Branch)
    }

    /// GitHub's error when the environment's branch policy rejects it.
    pub fn not_allowed(&self, env: &str) -> String {
        match self {
            Self::Branch(b) => format!(
                "Branch \"{b}\" is not allowed to deploy to {env} due to environment protection rules."
            ),
            Self::Tag(t) => format!(
                "Tag \"{t}\" is not allowed to deploy to {env} due to environment protection rules."
            ),
        }
    }
}

/// Whether `r` may deploy to the environment (`deployment_branch_policy`).
pub async fn ref_allowed(
    state: &AppState,
    repo: &db::Repository,
    p: &Protection,
    r: Option<&DeployRef>,
) -> anyhow::Result<bool> {
    let Some(policy) = p.env.branch_policy.as_deref() else {
        return Ok(true);
    };
    let Some(r) = r else { return Ok(false) };
    Ok(match (policy, r) {
        ("protected", DeployRef::Branch(b)) => {
            let rules = bgh_repos::protection::RepoRules::load(&state.db, repo)
                .await
                .map_err(|e| anyhow::anyhow!("loading branch protection: {e}"))?;
            rules.protection_for(b).is_some()
                || rules
                    .rulesets_for(&format!("refs/heads/{b}"))
                    .next()
                    .is_some()
        }
        ("protected", DeployRef::Tag(_)) => false,
        (_, DeployRef::Branch(name)) => p
            .policies
            .iter()
            .any(|x| x.kind == "branch" && bgh_repos::protection::pattern_matches(&x.name, name)),
        (_, DeployRef::Tag(name)) => p
            .policies
            .iter()
            .any(|x| x.kind == "tag" && bgh_repos::protection::pattern_matches(&x.name, name)),
    })
}

/// What happens to a job naming an environment.
#[derive(Debug, Clone)]
pub enum Gate {
    /// Queue it now.
    Open { env_id: i64 },
    /// The branch policy forbids the run's ref: fail with this message.
    Blocked { message: String },
    /// Wait for the timer and/or reviewers.
    Wait {
        env_id: i64,
        wait_timer: i32,
        needs_review: bool,
    },
}

/// Decide the gate of a job naming environment `name` (created when
/// missing, like GitHub).
pub async fn job_gate(
    state: &AppState,
    tx: &mut Tx,
    run: &RunRow,
    repo: &db::Repository,
    name: &str,
) -> anyhow::Result<Gate> {
    let env_id = crate::deployments::ensure_environment(tx, run.repo_id, name)
        .await
        .map_err(|e| anyhow::anyhow!("creating environment: {e}"))?;
    let Some(p) = Protection::load(tx, env_id).await? else {
        return Ok(Gate::Open { env_id });
    };
    let r = DeployRef::of_run(run);
    if !ref_allowed(state, repo, &p, r.as_ref()).await? {
        let message = match &r {
            Some(r) => r.not_allowed(&p.env.name),
            None => format!(
                "The ref \"{}\" is not allowed to deploy to {} due to environment protection rules.",
                run.git_ref, p.env.name
            ),
        };
        return Ok(Gate::Blocked { message });
    }
    if p.gated() {
        return Ok(Gate::Wait {
            env_id,
            wait_timer: p.env.wait_timer,
            needs_review: !p.reviewers.is_empty(),
        });
    }
    Ok(Gate::Open { env_id })
}

fn job_html(state: &AppState, owner: &str, repo: &str, job: &JobRow) -> String {
    format!(
        "{}/actions/runs/{}/job/{}",
        state.urls.repo_html(owner, repo),
        job.run_id,
        job.id
    )
}

/// After the engine inserted a gated or open job: record the gate, create
/// the job's deployment (status `queued`), and request reviews.
#[allow(clippy::too_many_arguments)]
pub async fn job_created(
    state: &AppState,
    tx: &mut Tx,
    run: &RunRow,
    repo: &db::Repository,
    owner: &db::User,
    job: &JobRow,
    environment: &str,
    gate: &Gate,
) -> anyhow::Result<()> {
    let bot = bgh_core::bots::ensure_actions_bot(tx).await?;
    let git_ref = match DeployRef::of_run(run) {
        Some(DeployRef::Branch(b) | DeployRef::Tag(b)) => b,
        None => run.head_sha.clone(),
    };
    let d = create_deployment(
        tx,
        run.repo_id,
        Some(bot),
        NewDeployment {
            git_ref,
            sha: run.head_sha.clone(),
            task: "deploy".into(),
            payload: json!({}),
            environment: environment.to_string(),
            description: None,
            transient_environment: false,
            production_environment: environment.eq_ignore_ascii_case("production"),
            run_id: Some(run.id),
            job_id: Some(job.id),
        },
    )
    .await
    .map_err(|e| anyhow::anyhow!("creating deployment: {e}"))?;
    let url = job_html(state, &owner.login, &repo.name, job);
    create_status(
        tx,
        run.repo_id,
        d.id,
        Some(bot),
        NewStatus {
            state: "queued".into(),
            description: String::new(),
            environment: None,
            target_url: url.clone(),
            log_url: url,
            environment_url: String::new(),
            auto_inactive: false,
        },
    )
    .await
    .map_err(|e| anyhow::anyhow!("creating deployment status: {e}"))?;

    let Gate::Wait {
        env_id,
        wait_timer,
        needs_review,
    } = *gate
    else {
        return Ok(());
    };
    // Another waiting job of this run already asked for this environment's
    // review (matrix jobs): one request per run and environment.
    let already: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM actions_job_gates g JOIN actions_jobs j ON j.id = g.job_id
                         WHERE g.run_id = $1 AND g.environment_id = $2 AND j.status = 'waiting'
                           AND g.review_state IS NULL)",
    )
    .bind(run.id)
    .bind(env_id)
    .fetch_one(&mut **tx)
    .await?;
    sqlx::query(
        "INSERT INTO actions_job_gates (job_id, run_id, environment_id, wait_timer, wait_until,
                                        needs_review)
         VALUES ($1, $2, $3, $4, CASE WHEN $4 > 0 THEN now() + make_interval(mins => $4) END, $5)",
    )
    .bind(job.id)
    .bind(run.id)
    .bind(env_id)
    .bind(wait_timer)
    .bind(needs_review)
    .execute(&mut **tx)
    .await?;
    if needs_review && !already {
        let ev = review_event(
            state,
            tx,
            run,
            env_id,
            "requested",
            run.triggering_actor_id.or(run.actor_id),
            None,
            &[job.id],
        )
        .await?;
        tx.emit(ev);
    }
    Ok(())
}

/// `workflow_job_run` object of `deployment_review` payloads.
fn job_run_json(state: &AppState, owner: &str, repo: &str, job: &JobRow, env: &str) -> Value {
    json!({
        "id": job.id,
        "name": job.name,
        "status": job.status,
        "conclusion": job.conclusion,
        "environment": env,
        "html_url": job_html(state, owner, repo, job),
        "created_at": Timestamp(job.created_at),
        "updated_at": Timestamp(job.updated_at),
    })
}

/// Build an [`Event::DeploymentReview`] for environment `env_id` of `run`.
#[allow(clippy::too_many_arguments)]
async fn review_event(
    state: &AppState,
    tx: &mut Tx,
    run: &RunRow,
    env_id: i64,
    action: &str,
    actor_id: Option<i64>,
    comment: Option<&str>,
    job_ids: &[i64],
) -> anyhow::Result<Event> {
    let p = Protection::load(tx, env_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("environment deleted"))?;
    let repo = db::Repository::find(&mut **tx, run.repo_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("repository deleted"))?;
    let owner = db::User::find(&mut **tx, repo.owner_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("owner deleted"))?;
    let reviewers = reviewers_json(state, tx, &p.reviewers).await?;
    let reviewer_ids = if action == "requested" {
        reviewer_user_ids(tx, &p.reviewers).await?
    } else {
        vec![]
    };
    let jobs: Vec<JobRow> = sqlx::query_as(&format!(
        "SELECT {} FROM actions_jobs WHERE id = ANY($1) ORDER BY id",
        JobRow::COLUMNS
    ))
    .bind(job_ids)
    .fetch_all(&mut **tx)
    .await?;
    let job_runs: Vec<Value> = jobs
        .iter()
        .map(|j| job_run_json(state, &owner.login, &repo.name, j, &p.env.name))
        .collect();
    let since: Option<DateTime<Utc>> = sqlx::query_scalar(
        "SELECT min(created_at) FROM actions_job_gates WHERE run_id = $1 AND environment_id = $2",
    )
    .bind(run.id)
    .bind(env_id)
    .fetch_one(&mut **tx)
    .await?;
    let workflow_run = engine::run_event(state, tx, run, "requested")
        .await
        .ok()
        .and_then(|e| match e {
            Event::WorkflowRunUpdated { workflow_run, .. } => Some(workflow_run),
            _ => None,
        })
        .unwrap_or(Value::Null);
    let actor = match actor_id {
        Some(id) => db::User::find(&mut **tx, id).await?,
        None => None,
    };
    let actor_json = actor
        .as_ref()
        .map(|u| json!(api::SimpleUser::new(&state.urls, u)))
        .unwrap_or(Value::Null);
    let mut payload = json!({
        "environment": p.env.name,
        "reviewers": reviewers,
        "since": since.map(Timestamp),
        "workflow_job_run": job_runs.first().cloned().unwrap_or(Value::Null),
        "workflow_run": workflow_run,
    });
    if action == "requested" {
        payload["requestor"] = actor_json;
    } else {
        payload["approver"] = actor_json;
        payload["comment"] = json!(comment.unwrap_or_default());
        payload["workflow_job_runs"] = json!(job_runs);
    }
    Ok(Event::DeploymentReview {
        repo_id: run.repo_id,
        run_id: run.id,
        action: action.to_string(),
        actor_id,
        reviewer_ids,
        payload,
    })
}

/// Queue waiting jobs whose gate is satisfied (timer elapsed, reviews
/// approved). `run_id`: only that run. Returns the number released.
pub async fn release_ready(state: &AppState, run_id: Option<i64>) -> anyhow::Result<usize> {
    // Gates of jobs that finished without starting (cancelled, rejected).
    sqlx::query(
        "UPDATE actions_job_gates g SET released_at = now()
           FROM actions_jobs j
          WHERE j.id = g.job_id AND g.released_at IS NULL AND j.status <> 'waiting'
            AND ($1::bigint IS NULL OR g.run_id = $1)",
    )
    .bind(run_id)
    .execute(&state.db)
    .await?;
    let due: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT g.job_id, g.run_id FROM actions_job_gates g
          WHERE g.released_at IS NULL
            AND (g.wait_until IS NULL OR g.wait_until <= now())
            AND (NOT g.needs_review OR g.review_state = 'approved')
            AND ($1::bigint IS NULL OR g.run_id = $1)
          ORDER BY g.job_id",
    )
    .bind(run_id)
    .fetch_all(&state.db)
    .await?;
    let mut runs = Vec::new();
    for (job_id, run_id) in &due {
        let mut tx = Tx::begin(state).await?;
        let released: Option<i64> = sqlx::query_scalar(
            "UPDATE actions_job_gates SET released_at = now()
              WHERE job_id = $1 AND released_at IS NULL RETURNING job_id",
        )
        .bind(job_id)
        .fetch_optional(&mut *tx)
        .await?;
        if released.is_none() {
            continue;
        }
        let row: Option<JobRow> = sqlx::query_as(&format!(
            "UPDATE actions_jobs SET status = 'queued', updated_at = now()
              WHERE id = $1 AND status = 'waiting' RETURNING {}",
            JobRow::COLUMNS
        ))
        .bind(job_id)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(row) = row {
            engine::sync_job(&mut tx, &row, SyncAction::Update).await?;
            let ev = engine::job_event(&mut tx, &row, "queued").await?;
            tx.emit(ev);
        }
        tx.commit().await?;
        if !runs.contains(run_id) {
            runs.push(*run_id);
        }
    }
    for run_id in runs {
        engine::advance_run(state, run_id).await?;
    }
    Ok(due.len())
}

/// A waiting job of a run with its gate.
#[derive(Debug, Clone, FromRow)]
pub struct PendingGate {
    pub job_id: i64,
    pub environment_id: i64,
    pub wait_timer: i32,
    pub needs_review: bool,
    pub review_state: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Gates of `run_id`'s jobs that are still waiting.
pub async fn pending_gates(conn: &mut PgConnection, run_id: i64) -> sqlx::Result<Vec<PendingGate>> {
    sqlx::query_as(
        "SELECT g.job_id, g.environment_id, g.wait_timer, g.needs_review, g.review_state,
                g.created_at
           FROM actions_job_gates g JOIN actions_jobs j ON j.id = g.job_id
          WHERE g.run_id = $1 AND j.status = 'waiting' AND g.released_at IS NULL
          ORDER BY g.environment_id, g.job_id",
    )
    .bind(run_id)
    .fetch_all(conn)
    .await
}

/// Whether `user` may approve or reject deployments to `p` in `run`
/// (`current_user_can_approve`): a required reviewer (not the run's
/// triggering actor when `prevent_self_review`), or a repository admin
/// when `can_admins_bypass`.
pub async fn can_approve(
    conn: &mut PgConnection,
    p: &Protection,
    run: &RunRow,
    user_id: i64,
    is_admin: bool,
) -> sqlx::Result<bool> {
    if is_admin && p.env.can_admins_bypass {
        return Ok(true);
    }
    if p.env.prevent_self_review && run.triggering_actor_id.or(run.actor_id) == Some(user_id) {
        return Ok(false);
    }
    p.is_reviewer(conn, user_id).await
}

/// Approve or reject the pending deployments of `run` to `env_ids` as
/// `user`. Returns the deployments of the reviewed jobs.
pub async fn review(
    state: &AppState,
    run: &RunRow,
    user: &db::User,
    is_admin: bool,
    env_ids: &[i64],
    approve: bool,
    comment: &str,
) -> ApiResult<Vec<DeploymentRow>> {
    let mut tx = Tx::begin(state).await?;
    // Serialize reviews of one run.
    sqlx::query("SELECT 1 FROM actions_runs WHERE id = $1 FOR UPDATE")
        .bind(run.id)
        .execute(&mut *tx)
        .await?;
    let gates: Vec<PendingGate> = pending_gates(&mut tx, run.id)
        .await?
        .into_iter()
        .filter(|g| g.needs_review && g.review_state.is_none())
        .collect();
    if gates.is_empty() {
        return Err(ApiError::unprocessable(
            "There are no pending deployment requests to approve or reject.",
        ));
    }
    let mut envs: Vec<Protection> = Vec::new();
    for id in env_ids {
        if envs.iter().any(|p| p.env.id == *id) {
            continue;
        }
        if !gates.iter().any(|g| g.environment_id == *id) {
            return Err(ApiError::invalid_field(FieldError::custom(
                "PendingDeployment",
                "environment_ids",
                format!("Environment {id} has no pending deployment request in this run"),
            )));
        }
        let p = Protection::load(&mut tx, *id)
            .await?
            .ok_or(ApiError::NotFound)?;
        if !can_approve(&mut tx, &p, run, user.id, is_admin).await? {
            return Err(ApiError::invalid_field(FieldError::custom(
                "PendingDeployment",
                "environment_ids",
                format!(
                    "{} is not allowed to review deployments to {}",
                    user.login, p.env.name
                ),
            )));
        }
        envs.push(p);
    }
    let state_str = if approve { "approved" } else { "rejected" };
    let ids: Vec<i64> = envs.iter().map(|p| p.env.id).collect();
    let names: Vec<String> = envs.iter().map(|p| p.env.name.clone()).collect();
    sqlx::query(
        "INSERT INTO actions_deployment_reviews (run_id, user_id, state, comment, environment_ids,
                                                 environment_names)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(run.id)
    .bind(user.id)
    .bind(state_str)
    .bind(comment)
    .bind(&ids)
    .bind(&names)
    .execute(&mut *tx)
    .await?;
    let job_ids: Vec<i64> = gates
        .iter()
        .filter(|g| ids.contains(&g.environment_id))
        .map(|g| g.job_id)
        .collect();
    sqlx::query("UPDATE actions_job_gates SET review_state = $2 WHERE job_id = ANY($1)")
        .bind(&job_ids)
        .bind(state_str)
        .execute(&mut *tx)
        .await?;
    if !approve {
        let rows: Vec<JobRow> = sqlx::query_as(&format!(
            "SELECT {} FROM actions_jobs WHERE id = ANY($1) AND status = 'waiting' FOR UPDATE",
            JobRow::COLUMNS
        ))
        .bind(&job_ids)
        .fetch_all(&mut *tx)
        .await?;
        for row in &rows {
            crate::server::finish_job_row(&mut tx, row, "failure", None, &[], None)
                .await
                .map_err(internal)?;
        }
    }
    for p in &envs {
        let mine: Vec<i64> = gates
            .iter()
            .filter(|g| g.environment_id == p.env.id)
            .map(|g| g.job_id)
            .collect();
        let ev = review_event(
            state,
            &mut tx,
            run,
            p.env.id,
            state_str,
            Some(user.id),
            Some(comment),
            &mine,
        )
        .await
        .map_err(internal)?;
        tx.emit(ev);
    }
    let deployments: Vec<DeploymentRow> = sqlx::query_as(&format!(
        "SELECT {} FROM deployments WHERE job_id = ANY($1) ORDER BY id",
        DeploymentRow::COLUMNS
    ))
    .bind(&job_ids)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    if !approve {
        for id in &job_ids {
            crate::logs::append(
                state,
                *id,
                1,
                &format!(
                    "##[error]The deployment was rejected by {}{}",
                    user.login,
                    if comment.is_empty() {
                        String::new()
                    } else {
                        format!(": {comment}")
                    }
                ),
            )
            .await
            .ok();
        }
    }
    release_ready(state, Some(run.id)).await.map_err(internal)?;
    engine::advance_run(state, run.id).await.map_err(internal)?;
    Ok(deployments)
}

fn internal(e: anyhow::Error) -> ApiError {
    ApiError::Internal(e)
}

/// The deployment a job created, if it is not finished yet.
async fn open_deployment(tx: &mut Tx, job_id: i64) -> sqlx::Result<Option<DeploymentRow>> {
    sqlx::query_as(&format!(
        "SELECT {} FROM deployments WHERE job_id = $1
            AND coalesce(state, 'queued') IN ('queued', 'pending', 'in_progress')
          ORDER BY id DESC LIMIT 1",
        DeploymentRow::COLUMNS
    ))
    .bind(job_id)
    .fetch_optional(&mut **tx)
    .await
}

async fn job_urls(tx: &mut Tx, job: &JobRow) -> anyhow::Result<String> {
    let state = tx.state().clone();
    let repo = db::Repository::find(&mut **tx, job.repo_id).await?;
    let owner = match &repo {
        Some(r) => db::User::find(&mut **tx, r.owner_id).await?,
        None => None,
    };
    Ok(match (repo, owner) {
        (Some(r), Some(o)) => job_html(&state, &o.login, &r.name, job),
        _ => String::new(),
    })
}

/// A runner claimed `job`: its deployment becomes `in_progress`.
pub async fn job_started(tx: &mut Tx, job: &JobRow) -> anyhow::Result<()> {
    let Some(d) = open_deployment(tx, job.id).await? else {
        return Ok(());
    };
    let bot = bgh_core::bots::ensure_actions_bot(tx).await?;
    let url = job_urls(tx, job).await?;
    create_status(
        tx,
        job.repo_id,
        d.id,
        Some(bot),
        NewStatus {
            state: "in_progress".into(),
            description: String::new(),
            environment: None,
            target_url: url.clone(),
            log_url: url,
            environment_url: String::new(),
            auto_inactive: false,
        },
    )
    .await
    .map_err(|e| anyhow::anyhow!("deployment status: {e}"))?;
    Ok(())
}

/// `environment.url` of a job, evaluated against what the server knows
/// at completion (github, needs, matrix, inputs, vars, strategy).
fn environment_url(stored: &StoredJob) -> String {
    let Some(raw) = stored.environment_url.as_deref() else {
        return String::new();
    };
    let s = &stored.spec;
    let ctx = MapContext::new()
        .with("github", s.github.clone())
        .with("needs", s.needs.clone())
        .with("matrix", s.matrix.clone())
        .with("inputs", s.inputs.clone())
        .with("vars", s.vars.clone())
        .with("strategy", s.strategy.clone())
        .with("steps", json!({}))
        .with("env", json!({}));
    expr::interpolate(raw, &ctx)
        .map(|u| u.trim().to_string())
        .unwrap_or_default()
}

/// `job` completed with `conclusion`: its deployment gets the final state
/// (`success`, `failure`, or `error` when cancelled) and, on success, the
/// evaluated `environment.url`.
pub async fn job_finished(tx: &mut Tx, job: &JobRow, conclusion: &str) -> anyhow::Result<()> {
    let Some(d) = open_deployment(tx, job.id).await? else {
        return Ok(());
    };
    let state = match conclusion {
        "success" => "success",
        "cancelled" | "skipped" => "error",
        _ => "failure",
    };
    let env_url = job
        .spec
        .clone()
        .and_then(|v| serde_json::from_value::<StoredJob>(v).ok())
        .map(|s| environment_url(&s))
        .unwrap_or_default();
    let bot = bgh_core::bots::ensure_actions_bot(tx).await?;
    let url = job_urls(tx, job).await?;
    create_status(
        tx,
        job.repo_id,
        d.id,
        Some(bot),
        NewStatus {
            state: state.into(),
            description: String::new(),
            environment: None,
            target_url: url.clone(),
            log_url: url,
            environment_url: if state == "success" {
                env_url
            } else {
                String::new()
            },
            auto_inactive: true,
        },
    )
    .await
    .map_err(|e| anyhow::anyhow!("deployment status: {e}"))?;
    Ok(())
}
