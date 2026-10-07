//! GitHub REST shapes of actions resources (workflows, runs, jobs,
//! artifacts, runners, secrets, variables, environments). List renderers
//! batch-load everything they reference (no N+1).

use std::collections::HashMap;

use bgh_core::AppState;
use bgh_core::error::ApiResult;
use bgh_core::models::api::{MinimalRepository, SimpleUser};
use bgh_core::models::db;
use bgh_core::node_id::{self, NodeType};
use bgh_core::perms::RepoAccess;
use bgh_core::time::{Timestamp, ts};
use serde_json::{Value, json};

use crate::models::{
    ArtifactRow, EnvironmentRow, JobRow, RunRow, RunnerRow, SecretRow, VariableRow, WorkflowRow,
};

fn repo_api(state: &AppState, a: &RepoAccess) -> String {
    state.urls.repo(&a.owner.login, &a.repo.name)
}

fn repo_html(state: &AppState, a: &RepoAccess) -> String {
    state.urls.repo_html(&a.owner.login, &a.repo.name)
}

pub fn workflow_json(state: &AppState, a: &RepoAccess, w: &WorkflowRow) -> Value {
    let html = repo_html(state, a);
    json!({
        "id": w.id,
        "node_id": node_id::encode(NodeType::Workflow, w.id),
        "name": w.name,
        "path": w.path,
        "state": w.state,
        "created_at": Timestamp(w.created_at),
        "updated_at": Timestamp(w.updated_at),
        "url": format!("{}/actions/workflows/{}", repo_api(state, a), w.id),
        "html_url": format!("{html}/blob/{}/{}", a.repo.default_branch, w.path),
        "badge_url": format!("{html}/workflows/{}/badge.svg", bgh_core::urls::encode_segment(&w.name)),
    })
}

/// Batch renderer for workflow runs of one repository.
pub async fn runs_json(state: &AppState, a: &RepoAccess, runs: &[RunRow]) -> ApiResult<Vec<Value>> {
    if runs.is_empty() {
        return Ok(vec![]);
    }
    let mut conn = state.db.acquire().await?;
    runs_json_conn(state, &mut conn, a, runs).await
}

/// [`runs_json`] on a given connection (e.g. inside the transaction that
/// changed the run, for event payloads).
pub async fn runs_json_conn(
    state: &AppState,
    conn: &mut sqlx::PgConnection,
    a: &RepoAccess,
    runs: &[RunRow],
) -> ApiResult<Vec<Value>> {
    let mut user_ids: Vec<i64> = runs
        .iter()
        .flat_map(|r| [r.actor_id, r.triggering_actor_id])
        .flatten()
        .collect();
    user_ids.sort_unstable();
    user_ids.dedup();
    let users: HashMap<i64, db::User> = db::User::find_many(&mut *conn, &user_ids)
        .await?
        .into_iter()
        .map(|u| (u.id, u))
        .collect();
    let wf_ids: Vec<i64> = runs.iter().map(|r| r.workflow_id).collect();
    let paths: HashMap<i64, String> = sqlx::query_as::<_, (i64, String)>(
        "SELECT id, path FROM actions_workflows WHERE id = ANY($1)",
    )
    .bind(&wf_ids)
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .collect();

    // Head repositories (forks) other than the base.
    let mut head_ids: Vec<i64> = runs
        .iter()
        .filter_map(|r| r.head_repo_id)
        .filter(|id| *id != a.repo.id)
        .collect();
    head_ids.sort_unstable();
    head_ids.dedup();
    let mut head_repos: HashMap<i64, Value> = HashMap::new();
    if !head_ids.is_empty() {
        let rows: Vec<db::Repository> = sqlx::query_as(&format!(
            "SELECT {} FROM repositories WHERE id = ANY($1)",
            db::Repository::COLUMNS
        ))
        .bind(&head_ids)
        .fetch_all(&mut *conn)
        .await?;
        let owner_ids: Vec<i64> = rows.iter().map(|r| r.owner_id).collect();
        let owners: HashMap<i64, db::User> = db::User::find_many(&mut *conn, &owner_ids)
            .await?
            .into_iter()
            .map(|u| (u.id, u))
            .collect();
        for r in rows {
            if let Some(o) = owners.get(&r.owner_id) {
                head_repos.insert(
                    r.id,
                    serde_json::to_value(MinimalRepository::new(&state.urls, &r, o, None))?,
                );
            }
        }
    }
    let base_repo =
        serde_json::to_value(MinimalRepository::new(&state.urls, &a.repo, &a.owner, None))?;

    // Head commits, one git session.
    let mut shas: Vec<(i64, String)> = runs
        .iter()
        .map(|r| (r.head_repo_id.unwrap_or(r.repo_id), r.head_sha.clone()))
        .collect();
    shas.sort();
    shas.dedup();
    let mut commits: HashMap<String, Value> = HashMap::new();
    let store = crate::trigger::store(state);
    let mut by_repo: HashMap<i64, Vec<String>> = HashMap::new();
    for (rid, sha) in shas {
        by_repo.entry(rid).or_default().push(sha);
    }
    for (rid, list) in by_repo {
        let found = store
            .read(rid, move |g| {
                Ok(list
                    .iter()
                    .filter_map(|s| g.commit(s).ok())
                    .collect::<Vec<_>>())
            })
            .await
            .unwrap_or_default();
        for c in found {
            commits.insert(
                c.sha.clone(),
                json!({
                    "id": c.sha,
                    "tree_id": c.tree,
                    "message": c.message.trim_end(),
                    "timestamp": Timestamp(c.committer.when),
                    "author": {"name": c.author.name, "email": c.author.email},
                    "committer": {"name": c.committer.name, "email": c.committer.email},
                }),
            );
        }
    }

    // Pull requests.
    let pr_ids: Vec<i64> = runs
        .iter()
        .flat_map(|r| r.pull_request_ids.iter().copied())
        .collect();
    let mut prs: HashMap<i64, Value> = HashMap::new();
    if !pr_ids.is_empty() {
        #[derive(sqlx::FromRow)]
        struct PrRow {
            id: i64,
            number: i64,
            head_ref: String,
            head_sha: String,
            base_ref: String,
            base_sha: String,
            head_repo_id: Option<i64>,
        }
        let rows: Vec<PrRow> = sqlx::query_as(
            "SELECT i.id, i.number, p.head_ref, p.head_sha, p.base_ref, p.base_sha, p.head_repo_id
               FROM issues i JOIN pull_requests p ON p.issue_id = i.id WHERE i.id = ANY($1)",
        )
        .bind(&pr_ids)
        .fetch_all(&mut *conn)
        .await?;
        let api = repo_api(state, a);
        let repo_ref = json!({"id": a.repo.id, "url": api, "name": a.repo.name});
        for p in rows {
            let head_repo = match p.head_repo_id {
                Some(id) if id == a.repo.id => repo_ref.clone(),
                Some(id) => head_repos
                    .get(&id)
                    .map(|r| json!({"id": id, "url": r["url"], "name": r["name"]}))
                    .unwrap_or(Value::Null),
                None => Value::Null,
            };
            prs.insert(
                p.id,
                json!({
                    "url": format!("{api}/pulls/{}", p.number),
                    "id": p.id,
                    "number": p.number,
                    "head": {"ref": p.head_ref, "sha": p.head_sha, "repo": head_repo},
                    "base": {"ref": p.base_ref, "sha": p.base_sha, "repo": repo_ref},
                }),
            );
        }
    }

    let api = repo_api(state, a);
    let html = repo_html(state, a);
    let user = |id: Option<i64>| {
        id.and_then(|id| users.get(&id))
            .map(|u| serde_json::to_value(SimpleUser::new(&state.urls, u)).unwrap_or_default())
            .unwrap_or(Value::Null)
    };
    Ok(runs
        .iter()
        .map(|r| {
            let run_api = format!("{api}/actions/runs/{}", r.id);
            let head_repository = match r.head_repo_id {
                Some(id) if id != a.repo.id => head_repos.get(&id).cloned().unwrap_or(Value::Null),
                _ => base_repo.clone(),
            };
            json!({
                "id": r.id,
                "name": r.name,
                "node_id": node_id::encode(NodeType::WorkflowRun, r.id),
                "head_branch": r.head_branch,
                "head_sha": r.head_sha,
                "path": paths.get(&r.workflow_id).cloned().unwrap_or_default(),
                "display_title": r.display_title,
                "run_number": r.run_number,
                "event": r.event,
                "status": r.status,
                "conclusion": r.conclusion,
                "workflow_id": r.workflow_id,
                "check_suite_id": r.check_suite_id,
                "check_suite_node_id": r.check_suite_id.map(|id| node_id::encode(NodeType::CheckSuite, id)),
                "url": run_api,
                "html_url": format!("{html}/actions/runs/{}", r.id),
                "pull_requests": r.pull_request_ids.iter().filter_map(|id| prs.get(id).cloned()).collect::<Vec<_>>(),
                "created_at": Timestamp(r.created_at),
                "updated_at": Timestamp(r.updated_at),
                "actor": user(r.actor_id.or(r.triggering_actor_id)),
                "run_attempt": r.run_attempt,
                "referenced_workflows": [],
                "run_started_at": Timestamp(r.run_started_at.unwrap_or(r.created_at)),
                "triggering_actor": user(r.triggering_actor_id.or(r.actor_id)),
                "jobs_url": format!("{run_api}/jobs"),
                "logs_url": format!("{run_api}/logs"),
                "check_suite_url": r.check_suite_id.map(|id| format!("{api}/check-suites/{id}")),
                "artifacts_url": format!("{run_api}/artifacts"),
                "cancel_url": format!("{run_api}/cancel"),
                "rerun_url": format!("{run_api}/rerun"),
                "previous_attempt_url": (r.run_attempt > 1).then(|| format!("{run_api}/attempts/{}", r.run_attempt - 1)),
                "workflow_url": format!("{api}/actions/workflows/{}", r.workflow_id),
                "head_commit": commits.get(&r.head_sha).cloned().unwrap_or(Value::Null),
                "repository": base_repo,
                "head_repository": head_repository,
            })
        })
        .collect())
}

pub fn job_json(state: &AppState, a: &RepoAccess, j: &JobRow, workflow_name: &str) -> Value {
    let api = repo_api(state, a);
    let html = repo_html(state, a);
    let steps: Vec<Value> = j
        .steps
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|s| {
            json!({
                "name": s["name"],
                "status": s["status"],
                "conclusion": s["conclusion"],
                "number": s["number"],
                "started_at": s["started_at"].as_str().and_then(github_ts),
                "completed_at": s["completed_at"].as_str().and_then(github_ts),
            })
        })
        .collect();
    json!({
        "id": j.id,
        "run_id": j.run_id,
        "workflow_name": workflow_name,
        "head_branch": j.head_branch,
        "run_url": format!("{api}/actions/runs/{}", j.run_id),
        "run_attempt": j.run_attempt,
        "node_id": node_id::encode(NodeType::CheckRun, j.check_run_id.unwrap_or(j.id)),
        "head_sha": j.head_sha,
        "url": format!("{api}/actions/jobs/{}", j.id),
        "html_url": format!("{html}/actions/runs/{}/job/{}", j.run_id, j.id),
        "status": j.status,
        "conclusion": j.conclusion,
        "created_at": Timestamp(j.created_at),
        "started_at": Timestamp(j.started_at.unwrap_or(j.created_at)),
        "completed_at": ts(j.completed_at),
        "name": j.name,
        "steps": steps,
        "check_run_url": j.check_run_id.map(|id| format!("{api}/check-runs/{id}")),
        "labels": j.labels,
        "runner_id": j.runner_id,
        "runner_name": j.runner_name,
        "runner_group_id": j.runner_id.map(|_| 1),
        "runner_group_name": j.runner_id.map(|_| "Default"),
    })
}

/// Normalize a stored RFC 3339 timestamp to GitHub's second precision.
fn github_ts(s: &str) -> Option<Value> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| json!(Timestamp(d.with_timezone(&chrono::Utc))))
}

pub fn artifact_json(
    state: &AppState,
    a: &RepoAccess,
    art: &ArtifactRow,
    run: Option<&RunRow>,
) -> Value {
    let api = repo_api(state, a);
    json!({
        "id": art.id,
        "node_id": node_id::encode(NodeType::Artifact, art.id),
        "name": art.name,
        "size_in_bytes": art.size_in_bytes,
        "url": format!("{api}/actions/artifacts/{}", art.id),
        "archive_download_url": format!("{api}/actions/artifacts/{}/zip", art.id),
        "expired": art.expired,
        "digest": art.digest,
        "created_at": Timestamp(art.created_at),
        "expires_at": Timestamp(art.expires_at),
        "updated_at": Timestamp(art.updated_at),
        "workflow_run": run.map(|r| json!({
            "id": r.id,
            "repository_id": r.repo_id,
            "head_repository_id": r.head_repo_id.unwrap_or(r.repo_id),
            "head_branch": r.head_branch,
            "head_sha": r.head_sha,
        })),
    })
}

pub fn runner_json(r: &RunnerRow) -> Value {
    let mut labels: Vec<Value> = Vec::new();
    let mut id = 1;
    for l in &r.system_labels {
        labels.push(json!({"id": id, "name": l, "type": "read-only"}));
        id += 1;
    }
    for l in &r.labels {
        labels.push(json!({"id": id, "name": l, "type": "custom"}));
        id += 1;
    }
    json!({
        "id": r.id,
        "name": r.name,
        "os": r.os,
        "status": if r.online() { "online" } else { "offline" },
        "busy": r.busy,
        "ephemeral": r.ephemeral,
        "runner_group_id": r.runner_group_id.unwrap_or(1),
        "labels": labels,
    })
}

pub fn labels_json(r: &RunnerRow) -> Value {
    let v = runner_json(r);
    json!({"total_count": v["labels"].as_array().map(Vec::len).unwrap_or(0), "labels": v["labels"]})
}

pub fn secret_json(s: &SecretRow, selected_url: Option<String>) -> Value {
    let mut v = json!({
        "name": s.name,
        "created_at": Timestamp(s.created_at),
        "updated_at": Timestamp(s.updated_at),
    });
    if let Some(vis) = &s.visibility {
        v["visibility"] = json!(vis);
        if vis == "selected" {
            v["selected_repositories_url"] = json!(selected_url);
        }
    }
    v
}

pub fn variable_json(s: &VariableRow, selected_url: Option<String>) -> Value {
    let mut v = json!({
        "name": s.name,
        "value": s.value,
        "created_at": Timestamp(s.created_at),
        "updated_at": Timestamp(s.updated_at),
    });
    if let Some(vis) = &s.visibility {
        v["visibility"] = json!(vis);
        if vis == "selected" {
            v["selected_repositories_url"] = json!(selected_url);
        }
    }
    v
}

/// GitHub `environment` JSON. `reviewers`: the rendered required reviewers
/// (`[{type, reviewer}]`, see `environments::reviewers_json`).
pub fn environment_json(
    state: &AppState,
    a: &RepoAccess,
    e: &EnvironmentRow,
    reviewers: &[Value],
) -> Value {
    let mut v = environment_ref_json(state, a, e);
    v["created_at"] = json!(Timestamp(e.created_at));
    v["updated_at"] = json!(Timestamp(e.updated_at));
    v["can_admins_bypass"] = json!(e.can_admins_bypass);
    // Rule ids are derived from the environment id (one rule per type).
    let rule = |n: i64, kind: &str| {
        let id = e.id * 10 + n;
        json!({
            "id": id,
            "node_id": node_id::encode(NodeType::EnvironmentProtectionRule, id),
            "type": kind,
        })
    };
    let mut rules = Vec::new();
    if e.wait_timer > 0 {
        let mut r = rule(1, "wait_timer");
        r["wait_timer"] = json!(e.wait_timer);
        rules.push(r);
    }
    if !reviewers.is_empty() {
        let mut r = rule(2, "required_reviewers");
        r["prevent_self_review"] = json!(e.prevent_self_review);
        r["reviewers"] = json!(reviewers);
        rules.push(r);
    }
    if e.branch_policy.is_some() {
        rules.push(rule(3, "branch_policy"));
    }
    v["protection_rules"] = json!(rules);
    v["deployment_branch_policy"] = match e.branch_policy.as_deref() {
        Some(p) => json!({
            "protected_branches": p == "protected",
            "custom_branch_policies": p == "custom",
        }),
        None => Value::Null,
    };
    v
}

/// The short environment object of `pending_deployments` and approvals:
/// `{id, node_id, name, url, html_url}`.
pub fn environment_ref_json(state: &AppState, a: &RepoAccess, e: &EnvironmentRow) -> Value {
    let api = repo_api(state, a);
    let html = repo_html(state, a);
    json!({
        "id": e.id,
        "node_id": node_id::encode(NodeType::Environment, e.id),
        "name": e.name,
        "url": format!("{api}/environments/{}", bgh_core::urls::encode_segment(&e.name)),
        "html_url": format!("{html}/deployments/activity_log?environments_filter={}", bgh_core::urls::encode_segment(&e.name)),
    })
}
