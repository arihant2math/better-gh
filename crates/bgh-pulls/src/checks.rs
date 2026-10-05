//! Checks API: check runs (create / update / get / list / annotations /
//! rerequest) and check suites (create / get / list / rerequest /
//! preferences). Runs created through the REST API by users belong to the
//! `api` app; bgh-actions creates runs via [`create_run`] with its own app
//! slug (`actions`).

use std::collections::HashMap;

use axum::extract::State;
use axum::http::StatusCode;
use bgh_core::models::api::{MinimalRepository, SimpleUser};
use bgh_core::node_id::{self, NodeType};
use bgh_core::prelude::*;
use bgh_core::time::ts;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::git;
use crate::jobs::ChecksChanged;
use crate::model::PULL_FROM;

pub const API_APP: &str = "api";

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct RunRow {
    pub id: i64,
    pub check_suite_id: i64,
    pub repo_id: i64,
    pub head_sha: String,
    pub name: String,
    pub status: String,
    pub conclusion: Option<String>,
    pub external_id: Option<String>,
    pub details_url: Option<String>,
    pub output: Value,
    pub actions: Value,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub(crate) const RUN_COLUMNS: &str = "id, check_suite_id, repo_id, head_sha, name, status, conclusion, \
    external_id, details_url, output, actions, started_at, completed_at, created_at, updated_at";

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SuiteRow {
    pub id: i64,
    pub repo_id: i64,
    pub head_sha: String,
    pub head_branch: Option<String>,
    pub before_sha: Option<String>,
    pub after_sha: Option<String>,
    pub app_slug: String,
    pub status: String,
    pub conclusion: Option<String>,
    pub rerequestable: bool,
    pub latest_check_runs_count: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub(crate) const SUITE_COLUMNS: &str = "id, repo_id, head_sha, head_branch, before_sha, after_sha, app_slug, \
    status, conclusion, rerequestable, latest_check_runs_count, created_at, updated_at";

#[derive(Debug, Clone, sqlx::FromRow)]
struct AnnotationRow {
    path: String,
    start_line: i32,
    end_line: i32,
    start_column: Option<i32>,
    end_column: Option<i32>,
    annotation_level: String,
    title: Option<String>,
    message: String,
    raw_details: Option<String>,
    blob_href: String,
}

// ---------------------------------------------------------------------------
// JSON shapes
// ---------------------------------------------------------------------------

/// `integration` (a GitHub App); synthesized per app slug.
#[derive(Debug, Clone, Serialize)]
pub struct AppJson {
    pub id: i64,
    pub slug: String,
    pub node_id: String,
    pub owner: SimpleUser,
    pub name: String,
    pub description: String,
    pub external_url: String,
    pub html_url: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub permissions: Value,
    pub events: Vec<String>,
}

fn app_json(state: &AppState, slug: &str, owner: &db::User) -> AppJson {
    let (id, name, desc) = match slug {
        "actions" => (
            1,
            "Better GitHub Actions",
            "Built-in continuous integration",
        ),
        _ => (
            2,
            "Better GitHub API",
            "Checks created through the REST API",
        ),
    };
    let epoch = Timestamp::from(DateTime::<Utc>::UNIX_EPOCH);
    AppJson {
        id,
        slug: slug.to_string(),
        node_id: node_id::encode_str(NodeType::Bot, slug),
        owner: SimpleUser::new(&state.urls, owner),
        name: name.to_string(),
        description: desc.to_string(),
        external_url: state.urls.html("/"),
        html_url: state.urls.html(&format!("/apps/{slug}")),
        created_at: epoch,
        updated_at: epoch,
        permissions: json!({"checks": "write", "metadata": "read", "statuses": "write"}),
        events: vec!["check_run".into(), "check_suite".into()],
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RepoRef {
    pub id: i64,
    pub url: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PrBranch {
    #[serde(rename = "ref")]
    pub ref_: String,
    pub sha: String,
    pub repo: RepoRef,
}

/// `pull-request-minimal`.
#[derive(Debug, Clone, Serialize)]
pub struct PrMinimal {
    pub id: i64,
    pub number: i64,
    pub url: String,
    pub head: PrBranch,
    pub base: PrBranch,
}

#[derive(Debug, Clone, Serialize)]
pub struct Output {
    pub title: Option<String>,
    pub summary: Option<String>,
    pub text: Option<String>,
    pub annotations_count: i64,
    pub annotations_url: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SuiteRef {
    pub id: i64,
}

/// `check-run`.
#[derive(Debug, Clone, Serialize)]
pub struct CheckRunJson {
    pub id: i64,
    pub head_sha: String,
    pub node_id: String,
    pub external_id: Option<String>,
    pub url: String,
    pub html_url: String,
    pub details_url: Option<String>,
    pub status: String,
    pub conclusion: Option<String>,
    pub started_at: Option<Timestamp>,
    pub completed_at: Option<Timestamp>,
    pub output: Output,
    pub name: String,
    pub check_suite: SuiteRef,
    pub app: AppJson,
    pub pull_requests: Vec<PrMinimal>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GitPerson {
    pub name: String,
    pub email: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct HeadCommit {
    pub id: String,
    pub tree_id: String,
    pub message: String,
    pub timestamp: Timestamp,
    pub author: Option<GitPerson>,
    pub committer: Option<GitPerson>,
}

/// `check-suite`.
#[derive(Debug, Clone, Serialize)]
pub struct CheckSuiteJson {
    pub id: i64,
    pub node_id: String,
    pub head_branch: Option<String>,
    pub head_sha: String,
    pub status: String,
    pub conclusion: Option<String>,
    pub url: String,
    pub before: Option<String>,
    pub after: Option<String>,
    pub pull_requests: Vec<PrMinimal>,
    pub app: AppJson,
    pub repository: MinimalRepository,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub head_commit: Option<HeadCommit>,
    pub latest_check_runs_count: i64,
    pub check_runs_url: String,
    pub rerequestable: bool,
    pub runs_rerequestable: bool,
}

/// Open PRs whose head is one of `shas` in the repository, by SHA.
async fn prs_for_shas(
    state: &AppState,
    access: &RepoAccess,
    shas: &[String],
) -> ApiResult<HashMap<String, Vec<PrMinimal>>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        id: i64,
        number: i64,
        head_sha: String,
        head_ref: String,
        head_repo_id: Option<i64>,
        base_ref: String,
        base_sha: String,
    }
    let rows: Vec<Row> = sqlx::query_as(&format!(
        "SELECT i.id, i.number, p.head_sha, p.head_ref, p.head_repo_id, p.base_ref, p.base_sha
           FROM {PULL_FROM}
          WHERE i.repo_id = $1 AND i.state = 'open' AND p.head_sha = ANY($2)
          ORDER BY i.number"
    ))
    .bind(access.repo.id)
    .bind(shas)
    .fetch_all(&state.db)
    .await?;
    let head_repo_ids: Vec<i64> = rows.iter().filter_map(|r| r.head_repo_id).collect();
    let head_repos: HashMap<i64, (String, String)> = if head_repo_ids.is_empty() {
        HashMap::new()
    } else {
        sqlx::query_as::<_, (i64, String, String)>(
            "SELECT r.id, u.login, r.name FROM repositories r JOIN users u ON u.id = r.owner_id
              WHERE r.id = ANY($1)",
        )
        .bind(&head_repo_ids)
        .fetch_all(&state.db)
        .await?
        .into_iter()
        .map(|(id, o, n)| (id, (o, n)))
        .collect()
    };
    let o = access.owner.login.as_str();
    let n = access.repo.name.as_str();
    let base_repo = RepoRef {
        id: access.repo.id,
        url: state.urls.repo(o, n),
        name: n.to_string(),
    };
    let mut out: HashMap<String, Vec<PrMinimal>> = HashMap::new();
    for r in rows {
        let head_repo = r
            .head_repo_id
            .and_then(|id| head_repos.get(&id).map(|(ho, hn)| (id, ho, hn)))
            .map(|(id, ho, hn)| RepoRef {
                id,
                url: state.urls.repo(ho, hn),
                name: hn.clone(),
            })
            .unwrap_or_else(|| base_repo.clone());
        out.entry(r.head_sha.clone()).or_default().push(PrMinimal {
            id: r.id,
            number: r.number,
            url: state.urls.pull(o, n, r.number),
            head: PrBranch {
                ref_: r.head_ref,
                sha: r.head_sha,
                repo: head_repo,
            },
            base: PrBranch {
                ref_: r.base_ref,
                sha: r.base_sha,
                repo: base_repo.clone(),
            },
        });
    }
    Ok(out)
}

pub async fn render_runs(
    state: &AppState,
    access: &RepoAccess,
    runs: &[RunRow],
) -> ApiResult<Vec<CheckRunJson>> {
    if runs.is_empty() {
        return Ok(vec![]);
    }
    let ids: Vec<i64> = runs.iter().map(|r| r.id).collect();
    let counts: HashMap<i64, i64> = sqlx::query_as::<_, (i64, i64)>(
        "SELECT check_run_id, count(*) FROM check_run_annotations
          WHERE check_run_id = ANY($1) GROUP BY check_run_id",
    )
    .bind(&ids)
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .collect();
    let suite_ids: Vec<i64> = runs.iter().map(|r| r.check_suite_id).collect();
    let slugs: HashMap<i64, String> = sqlx::query_as::<_, (i64, String)>(
        "SELECT id, app_slug FROM check_suites WHERE id = ANY($1)",
    )
    .bind(&suite_ids)
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .collect();
    let shas: Vec<String> = runs.iter().map(|r| r.head_sha.clone()).collect();
    let prs = prs_for_shas(state, access, &shas).await?;
    let o = access.owner.login.as_str();
    let n = access.repo.name.as_str();
    Ok(runs
        .iter()
        .map(|r| {
            let url = state
                .urls
                .api(&format!("/repos/{o}/{n}/check-runs/{}", r.id));
            let s = |k: &str| r.output.get(k).and_then(Value::as_str).map(str::to_string);
            CheckRunJson {
                id: r.id,
                head_sha: r.head_sha.clone(),
                node_id: node_id::encode(NodeType::CheckRun, r.id),
                external_id: r.external_id.clone(),
                html_url: state.urls.html(&format!("/{o}/{n}/runs/{}", r.id)),
                details_url: r.details_url.clone(),
                status: r.status.clone(),
                conclusion: r.conclusion.clone(),
                started_at: ts(r.started_at),
                completed_at: ts(r.completed_at),
                output: Output {
                    title: s("title"),
                    summary: s("summary"),
                    text: s("text"),
                    annotations_count: counts.get(&r.id).copied().unwrap_or(0),
                    annotations_url: format!("{url}/annotations"),
                },
                url,
                name: r.name.clone(),
                check_suite: SuiteRef {
                    id: r.check_suite_id,
                },
                app: app_json(
                    state,
                    slugs
                        .get(&r.check_suite_id)
                        .map(String::as_str)
                        .unwrap_or(API_APP),
                    &access.owner,
                ),
                pull_requests: prs.get(&r.head_sha).cloned().unwrap_or_default(),
            }
        })
        .collect())
}

pub async fn render_suites(
    state: &AppState,
    access: &RepoAccess,
    suites: &[SuiteRow],
) -> ApiResult<Vec<CheckSuiteJson>> {
    if suites.is_empty() {
        return Ok(vec![]);
    }
    let shas: Vec<String> = suites.iter().map(|s| s.head_sha.clone()).collect();
    let prs = prs_for_shas(state, access, &shas).await?;
    let store = git::store(state);
    let lookup = shas.clone();
    let commits: HashMap<String, bgh_git::Commit> = store
        .read(access.repo.id, move |r| {
            Ok(lookup
                .iter()
                .filter_map(|s| r.commit(s).ok().map(|c| (s.clone(), c)))
                .collect())
        })
        .await
        .unwrap_or_default();
    let o = access.owner.login.as_str();
    let n = access.repo.name.as_str();
    Ok(suites
        .iter()
        .map(|s| {
            let url = state
                .urls
                .api(&format!("/repos/{o}/{n}/check-suites/{}", s.id));
            CheckSuiteJson {
                id: s.id,
                node_id: node_id::encode(NodeType::CheckSuite, s.id),
                head_branch: s.head_branch.clone(),
                head_sha: s.head_sha.clone(),
                status: s.status.clone(),
                conclusion: s.conclusion.clone(),
                check_runs_url: format!("{url}/check-runs"),
                url,
                before: s.before_sha.clone(),
                after: s.after_sha.clone(),
                pull_requests: prs.get(&s.head_sha).cloned().unwrap_or_default(),
                app: app_json(state, &s.app_slug, &access.owner),
                repository: MinimalRepository::new(
                    &state.urls,
                    &access.repo,
                    &access.owner,
                    access.api_permission(),
                ),
                created_at: s.created_at.into(),
                updated_at: s.updated_at.into(),
                head_commit: commits.get(&s.head_sha).map(|c| HeadCommit {
                    id: c.sha.clone(),
                    tree_id: c.tree.clone(),
                    message: c.message.trim_end().to_string(),
                    timestamp: c.committer.when.into(),
                    author: Some(GitPerson {
                        name: c.author.name.clone(),
                        email: c.author.email.clone(),
                    }),
                    committer: Some(GitPerson {
                        name: c.committer.name.clone(),
                        email: c.committer.email.clone(),
                    }),
                }),
                latest_check_runs_count: s.latest_check_runs_count,
                rerequestable: s.rerequestable,
                runs_rerequestable: true,
            }
        })
        .collect())
}

pub(crate) fn run_sync_json(r: &RunRow) -> Value {
    json!({
        "id": r.id, "repoId": r.repo_id, "checkSuiteId": r.check_suite_id,
        "headSha": r.head_sha, "name": r.name, "status": r.status,
        "conclusion": r.conclusion, "detailsUrl": r.details_url,
        "title": r.output.get("title"), "startedAt": ts(r.started_at),
        "completedAt": ts(r.completed_at),
    })
}

pub(crate) fn suite_sync_json(s: &SuiteRow) -> Value {
    json!({
        "id": s.id, "repoId": s.repo_id, "headSha": s.head_sha,
        "headBranch": s.head_branch, "appSlug": s.app_slug, "status": s.status,
        "conclusion": s.conclusion, "latestCheckRunsCount": s.latest_check_runs_count,
    })
}

// ---------------------------------------------------------------------------
// Service functions
// ---------------------------------------------------------------------------

const STATUSES: [&str; 6] = [
    "queued",
    "in_progress",
    "completed",
    "waiting",
    "requested",
    "pending",
];
const CONCLUSIONS: [&str; 8] = [
    "success",
    "failure",
    "neutral",
    "cancelled",
    "skipped",
    "timed_out",
    "action_required",
    "stale",
];

/// Branch whose tip is `sha` (PR head first, then any branch).
async fn branch_for_sha(state: &AppState, repo_id: i64, sha: &str) -> ApiResult<Option<String>> {
    let pr: Option<String> = sqlx::query_scalar(
        "SELECT head_ref FROM pull_requests WHERE head_repo_id = $1 AND head_sha = $2 LIMIT 1",
    )
    .bind(repo_id)
    .bind(sha)
    .fetch_optional(&state.db)
    .await?;
    if pr.is_some() {
        return Ok(pr);
    }
    let sha = sha.to_string();
    Ok(git::store(state)
        .read(repo_id, move |r| {
            Ok(r.branches()?
                .into_iter()
                .find(|b| b.peeled == sha)
                .map(|b| b.short_name().to_string()))
        })
        .await
        .unwrap_or(None))
}

/// Find or create the suite of `app_slug` for `head_sha`.
pub async fn ensure_suite(
    tx: &mut Tx,
    state: &AppState,
    repo_id: i64,
    head_sha: &str,
    app_slug: &str,
) -> ApiResult<(SuiteRow, bool)> {
    let existing: Option<SuiteRow> = sqlx::query_as(&format!(
        "SELECT {SUITE_COLUMNS} FROM check_suites
          WHERE repo_id = $1 AND head_sha = $2 AND app_slug = $3 ORDER BY id DESC LIMIT 1"
    ))
    .bind(repo_id)
    .bind(head_sha)
    .bind(app_slug)
    .fetch_optional(&mut **tx)
    .await?;
    if let Some(s) = existing {
        return Ok((s, false));
    }
    let branch = branch_for_sha(state, repo_id, head_sha).await?;
    let s: SuiteRow = sqlx::query_as(&format!(
        "INSERT INTO check_suites (repo_id, head_sha, head_branch, after_sha, app_slug, status)
         VALUES ($1, $2, $3, $2, $4, 'queued') RETURNING {SUITE_COLUMNS}"
    ))
    .bind(repo_id)
    .bind(head_sha)
    .bind(branch)
    .bind(app_slug)
    .fetch_one(&mut **tx)
    .await?;
    tx.sync(
        &bgh_core::sync::repo_scope(repo_id),
        "checkSuite",
        s.id,
        SyncAction::Insert,
        &suite_sync_json(&s),
    )
    .await?;
    Ok((s, true))
}

/// Recompute a suite's status/conclusion from its latest runs.
async fn recompute_suite(tx: &mut Tx, suite_id: i64) -> ApiResult<SuiteRow> {
    let runs: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT DISTINCT ON (name) status, conclusion FROM check_runs
          WHERE check_suite_id = $1 ORDER BY name, id DESC",
    )
    .bind(suite_id)
    .fetch_all(&mut **tx)
    .await?;
    let (status, conclusion) = if runs.is_empty() {
        ("queued", None)
    } else if runs.iter().all(|(s, _)| s == "completed") {
        let order = [
            "action_required",
            "failure",
            "timed_out",
            "cancelled",
            "stale",
            "neutral",
            "success",
            "skipped",
        ];
        let c = order
            .iter()
            .find(|c| runs.iter().any(|(_, rc)| rc.as_deref() == Some(**c)))
            .copied()
            .unwrap_or("success");
        // all skipped/neutral with some success → success
        let c = if c == "neutral" || c == "skipped" {
            if runs.iter().any(|(_, rc)| rc.as_deref() == Some("success")) {
                "success"
            } else {
                c
            }
        } else {
            c
        };
        ("completed", Some(c))
    } else if runs.iter().any(|(s, _)| s == "in_progress") {
        ("in_progress", None)
    } else {
        ("queued", None)
    };
    let s: SuiteRow = sqlx::query_as(&format!(
        "UPDATE check_suites SET status = $2, conclusion = $3, latest_check_runs_count = $4,
                updated_at = now() WHERE id = $1 RETURNING {SUITE_COLUMNS}"
    ))
    .bind(suite_id)
    .bind(status)
    .bind(conclusion)
    .bind(runs.len() as i64)
    .fetch_one(&mut **tx)
    .await?;
    Ok(s)
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct AnnotationInput {
    pub path: Option<String>,
    pub start_line: Option<i32>,
    pub end_line: Option<i32>,
    pub start_column: Option<i32>,
    pub end_column: Option<i32>,
    pub annotation_level: Option<String>,
    pub message: Option<String>,
    pub title: Option<String>,
    pub raw_details: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct OutputInput {
    pub title: Option<String>,
    pub summary: Option<String>,
    pub text: Option<String>,
    #[serde(default)]
    pub annotations: Vec<AnnotationInput>,
    #[serde(default)]
    pub images: Vec<Value>,
}

/// Parameters of `POST/PATCH /check-runs`.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct RunInput {
    pub name: Option<String>,
    pub head_sha: Option<String>,
    pub details_url: Option<String>,
    pub external_id: Option<String>,
    pub status: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub conclusion: Option<String>,
    pub completed_at: Option<DateTime<Utc>>,
    pub output: Option<OutputInput>,
    pub actions: Option<Vec<Value>>,
}

fn invalid(field: &str) -> ApiError {
    ApiError::invalid_field(FieldError::invalid("CheckRun", field))
}

fn validate_output(o: &OutputInput) -> ApiResult<()> {
    if o.annotations.len() > 50 {
        return Err(ApiError::unprocessable(
            "Only 50 annotations can be created per request.",
        ));
    }
    for a in &o.annotations {
        if a.path.as_deref().is_none_or(str::is_empty)
            || a.start_line.is_none()
            || a.end_line.is_none()
            || a.message.as_deref().is_none_or(str::is_empty)
            || !matches!(
                a.annotation_level.as_deref(),
                Some("notice" | "warning" | "failure")
            )
        {
            return Err(invalid("output.annotations"));
        }
    }
    Ok(())
}

async fn insert_annotations(
    tx: &mut Tx,
    state: &AppState,
    access_owner: &str,
    repo_name: &str,
    run: &RunRow,
    anns: &[AnnotationInput],
) -> ApiResult<()> {
    for a in anns {
        let path = a.path.clone().unwrap_or_default();
        let blob_href = state.urls.html(&format!(
            "/{access_owner}/{repo_name}/blob/{}/{}",
            run.head_sha,
            bgh_core::urls::encode_path(&path)
        ));
        sqlx::query(
            "INSERT INTO check_run_annotations (check_run_id, path, start_line, end_line,
                    start_column, end_column, annotation_level, title, message, raw_details, blob_href)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        )
        .bind(run.id)
        .bind(&path)
        .bind(a.start_line)
        .bind(a.end_line)
        .bind(a.start_column)
        .bind(a.end_column)
        .bind(a.annotation_level.as_deref())
        .bind(a.title.as_deref())
        .bind(a.message.as_deref())
        .bind(a.raw_details.as_deref())
        .bind(&blob_href)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

fn output_json(o: &OutputInput, prev: Option<&Value>) -> Value {
    let mut v = prev.cloned().unwrap_or_else(|| json!({}));
    if let Some(t) = &o.title {
        v["title"] = json!(t);
    }
    if let Some(s) = &o.summary {
        v["summary"] = json!(s);
    }
    if let Some(t) = &o.text {
        v["text"] = json!(t);
    }
    if !o.images.is_empty() {
        v["images"] = json!(o.images);
    }
    v
}

/// Create a check run (service entry point; also used by bgh-actions).
pub async fn create_run(
    state: &AppState,
    access: &RepoAccess,
    app_slug: &str,
    actor_id: Option<i64>,
    input: &RunInput,
) -> ApiResult<RunRow> {
    let name = input
        .name
        .as_deref()
        .filter(|n| !n.is_empty())
        .ok_or_else(|| ApiError::invalid_field(FieldError::missing_field("CheckRun", "name")))?;
    let head_sha = input.head_sha.as_deref().ok_or_else(|| {
        ApiError::invalid_field(FieldError::missing_field("CheckRun", "head_sha"))
    })?;
    let head_sha = git::resolve_commit(&git::store(state), access.repo.id, head_sha)
        .await?
        .filter(|s| s == head_sha || !bgh_git::is_sha(head_sha))
        .ok_or_else(|| {
            ApiError::unprocessable("No commit found for SHA: ".to_string() + head_sha)
        })?;
    let mut status = input.status.clone();
    if let Some(s) = &status
        && !STATUSES.contains(&s.as_str())
    {
        return Err(invalid("status"));
    }
    if let Some(c) = &input.conclusion {
        if !CONCLUSIONS.contains(&c.as_str()) {
            return Err(invalid("conclusion"));
        }
        status = Some("completed".into());
    }
    let status = status.unwrap_or_else(|| "queued".into());
    if status == "completed" && input.conclusion.is_none() {
        return Err(ApiError::invalid_field(FieldError::missing_field(
            "CheckRun",
            "conclusion",
        )));
    }
    if let Some(o) = &input.output {
        validate_output(o)?;
    }
    let completed_at = if status == "completed" {
        Some(input.completed_at.unwrap_or_else(Utc::now))
    } else {
        None
    };
    let started_at = input.started_at.or(Some(Utc::now()));
    let output = input
        .output
        .as_ref()
        .map(|o| output_json(o, None))
        .unwrap_or_else(|| json!({}));

    let mut tx = Tx::begin(state).await?;
    let (suite, created) =
        ensure_suite(&mut tx, state, access.repo.id, &head_sha, app_slug).await?;
    let run: RunRow = sqlx::query_as(&format!(
        "INSERT INTO check_runs (check_suite_id, repo_id, head_sha, name, status, conclusion,
                external_id, details_url, output, actions, started_at, completed_at, creator_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13) RETURNING {RUN_COLUMNS}"
    ))
    .bind(suite.id)
    .bind(access.repo.id)
    .bind(&head_sha)
    .bind(name)
    .bind(&status)
    .bind(input.conclusion.as_deref())
    .bind(input.external_id.as_deref())
    .bind(input.details_url.as_deref())
    .bind(&output)
    .bind(json!(input.actions.clone().unwrap_or_default()))
    .bind(started_at)
    .bind(completed_at)
    .bind(actor_id)
    .fetch_one(&mut *tx)
    .await?;
    if let Some(o) = &input.output {
        insert_annotations(
            &mut tx,
            state,
            &access.owner.login,
            &access.repo.name,
            &run,
            &o.annotations,
        )
        .await?;
    }
    let suite_after = recompute_suite(&mut tx, suite.id).await?;
    let scope = access.scope();
    tx.sync(
        &scope,
        "checkRun",
        run.id,
        SyncAction::Insert,
        &run_sync_json(&run),
    )
    .await?;
    tx.sync(
        &scope,
        "checkSuite",
        suite_after.id,
        SyncAction::Update,
        &suite_sync_json(&suite_after),
    )
    .await?;
    tx.enqueue(&ChecksChanged {
        repo_id: access.repo.id,
        sha: head_sha.clone(),
    })
    .await?;
    if created {
        tx.emit(Event::CheckSuiteRequested {
            repo_id: access.repo.id,
            check_suite_id: suite.id,
            actor_id,
        });
    }
    tx.emit(Event::CheckRunCreated {
        repo_id: access.repo.id,
        check_run_id: run.id,
        actor_id,
    });
    if run.status == "completed" {
        tx.emit(Event::CheckRunCompleted {
            repo_id: access.repo.id,
            check_run_id: run.id,
            actor_id,
        });
    }
    if suite_after.status == "completed" && suite.status != "completed" {
        tx.emit(Event::CheckSuiteCompleted {
            repo_id: access.repo.id,
            check_suite_id: suite.id,
        });
    }
    tx.commit().await?;
    Ok(run)
}

async fn find_run(state: &AppState, repo_id: i64, id: i64) -> ApiResult<RunRow> {
    sqlx::query_as(&format!(
        "SELECT {RUN_COLUMNS} FROM check_runs WHERE id = $1 AND repo_id = $2"
    ))
    .bind(id)
    .bind(repo_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)
}

async fn find_suite(state: &AppState, repo_id: i64, id: i64) -> ApiResult<SuiteRow> {
    sqlx::query_as(&format!(
        "SELECT {SUITE_COLUMNS} FROM check_suites WHERE id = $1 AND repo_id = $2"
    ))
    .bind(id)
    .bind(repo_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)
}

/// Update a check run (service entry point; also used by bgh-actions).
pub async fn update_run(
    state: &AppState,
    access: &RepoAccess,
    id: i64,
    actor_id: Option<i64>,
    input: &RunInput,
) -> ApiResult<RunRow> {
    let cur = find_run(state, access.repo.id, id).await?;
    let mut status = input.status.clone().unwrap_or_else(|| cur.status.clone());
    if !STATUSES.contains(&status.as_str()) {
        return Err(invalid("status"));
    }
    let mut conclusion = cur.conclusion.clone();
    if let Some(c) = &input.conclusion {
        if !CONCLUSIONS.contains(&c.as_str()) {
            return Err(invalid("conclusion"));
        }
        conclusion = Some(c.clone());
        status = "completed".into();
    }
    if status != "completed" {
        conclusion = None;
    } else if conclusion.is_none() {
        return Err(ApiError::invalid_field(FieldError::missing_field(
            "CheckRun",
            "conclusion",
        )));
    }
    if let Some(o) = &input.output {
        validate_output(o)?;
    }
    let completed_at = if status == "completed" {
        input.completed_at.or(cur.completed_at).or(Some(Utc::now()))
    } else {
        None
    };
    let output = match &input.output {
        Some(o) => output_json(o, Some(&cur.output)),
        None => cur.output.clone(),
    };
    let mut tx = Tx::begin(state).await?;
    let run: RunRow = sqlx::query_as(&format!(
        "UPDATE check_runs SET name = coalesce($2, name), details_url = coalesce($3, details_url),
                external_id = coalesce($4, external_id), status = $5, conclusion = $6,
                started_at = coalesce($7, started_at), completed_at = $8, output = $9,
                actions = coalesce($10, actions), updated_at = now()
          WHERE id = $1 RETURNING {RUN_COLUMNS}"
    ))
    .bind(id)
    .bind(input.name.as_deref())
    .bind(input.details_url.as_deref())
    .bind(input.external_id.as_deref())
    .bind(&status)
    .bind(conclusion.as_deref())
    .bind(input.started_at)
    .bind(completed_at)
    .bind(&output)
    .bind(input.actions.as_ref().map(|a| json!(a)))
    .fetch_one(&mut *tx)
    .await?;
    if let Some(o) = &input.output {
        insert_annotations(
            &mut tx,
            state,
            &access.owner.login,
            &access.repo.name,
            &run,
            &o.annotations,
        )
        .await?;
    }
    let before: String = sqlx::query_scalar("SELECT status FROM check_suites WHERE id = $1")
        .bind(run.check_suite_id)
        .fetch_one(&mut *tx)
        .await?;
    let suite = recompute_suite(&mut tx, run.check_suite_id).await?;
    let scope = access.scope();
    tx.sync(
        &scope,
        "checkRun",
        run.id,
        SyncAction::Update,
        &run_sync_json(&run),
    )
    .await?;
    tx.sync(
        &scope,
        "checkSuite",
        suite.id,
        SyncAction::Update,
        &suite_sync_json(&suite),
    )
    .await?;
    tx.enqueue(&ChecksChanged {
        repo_id: access.repo.id,
        sha: run.head_sha.clone(),
    })
    .await?;
    if run.status == "completed" && cur.status != "completed" {
        tx.emit(Event::CheckRunCompleted {
            repo_id: access.repo.id,
            check_run_id: run.id,
            actor_id,
        });
    }
    if suite.status == "completed" && before != "completed" {
        tx.emit(Event::CheckSuiteCompleted {
            repo_id: access.repo.id,
            check_suite_id: suite.id,
        });
    }
    tx.commit().await?;
    Ok(run)
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

pub async fn create(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo)): Path<(String, String)>,
    Json(input): Json<RunInput>,
) -> ApiResult<(StatusCode, axum::Json<CheckRunJson>)> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    access.require(Permission::Write)?;
    access.require_not_archived()?;
    let run = create_run(&state, &access, API_APP, Some(auth.user.id), &input).await?;
    Ok((
        StatusCode::CREATED,
        axum::Json(render_runs(&state, &access, &[run]).await?.remove(0)),
    ))
}

pub async fn update(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, id)): Path<(String, String, i64)>,
    Json(input): Json<RunInput>,
) -> ApiResult<axum::Json<CheckRunJson>> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    access.require(Permission::Write)?;
    access.require_not_archived()?;
    let run = update_run(&state, &access, id, Some(auth.user.id), &input).await?;
    Ok(axum::Json(
        render_runs(&state, &access, &[run]).await?.remove(0),
    ))
}

pub async fn get_run(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, id)): Path<(String, String, i64)>,
) -> ApiResult<axum::Json<CheckRunJson>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let run = find_run(&state, access.repo.id, id).await?;
    Ok(axum::Json(
        render_runs(&state, &access, &[run]).await?.remove(0),
    ))
}

#[derive(Debug, Serialize)]
pub struct AnnotationJson {
    pub path: String,
    pub start_line: i32,
    pub end_line: i32,
    pub start_column: Option<i32>,
    pub end_column: Option<i32>,
    pub annotation_level: Option<String>,
    pub title: Option<String>,
    pub message: Option<String>,
    pub raw_details: Option<String>,
    pub blob_href: String,
}

pub async fn annotations(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo, id)): Path<(String, String, i64)>,
) -> ApiResult<Page<AnnotationJson>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    find_run(&state, access.repo.id, id).await?;
    let rows: Vec<AnnotationRow> = sqlx::query_as(
        "SELECT path, start_line, end_line, start_column, end_column, annotation_level, title,
                message, raw_details, blob_href
           FROM check_run_annotations WHERE check_run_id = $1 ORDER BY id LIMIT $2 OFFSET $3",
    )
    .bind(id)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    Ok(p.page(rows).map(|a| AnnotationJson {
        path: a.path,
        start_line: a.start_line,
        end_line: a.end_line,
        start_column: a.start_column,
        end_column: a.end_column,
        annotation_level: Some(a.annotation_level),
        title: a.title,
        message: Some(a.message),
        raw_details: a.raw_details,
        blob_href: a.blob_href,
    }))
}

pub async fn rerequest_run(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, id)): Path<(String, String, i64)>,
) -> ApiResult<(StatusCode, axum::Json<Value>)> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    access.require(Permission::Write)?;
    let cur = find_run(&state, access.repo.id, id).await?;
    if cur.status != "completed" {
        return Err(ApiError::forbidden(
            "This check run is not rerequestable because it is not completed",
        ));
    }
    let mut tx = Tx::begin(&state).await?;
    let run: RunRow = sqlx::query_as(&format!(
        "UPDATE check_runs SET status = 'queued', conclusion = NULL, completed_at = NULL,
                started_at = NULL, updated_at = now() WHERE id = $1 RETURNING {RUN_COLUMNS}"
    ))
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM check_run_annotations WHERE check_run_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    let suite = recompute_suite(&mut tx, run.check_suite_id).await?;
    tx.sync(
        &access.scope(),
        "checkRun",
        id,
        SyncAction::Update,
        &run_sync_json(&run),
    )
    .await?;
    tx.sync(
        &access.scope(),
        "checkSuite",
        suite.id,
        SyncAction::Update,
        &suite_sync_json(&suite),
    )
    .await?;
    tx.enqueue(&ChecksChanged {
        repo_id: access.repo.id,
        sha: run.head_sha.clone(),
    })
    .await?;
    tx.emit(Event::CheckRunRerequested {
        repo_id: access.repo.id,
        check_run_id: id,
        actor_id: auth.user.id,
    });
    tx.commit().await?;
    Ok((StatusCode::CREATED, axum::Json(json!({}))))
}

#[derive(Debug, Deserialize)]
pub struct RunListQuery {
    pub check_name: Option<String>,
    pub status: Option<String>,
    pub filter: Option<String>,
    pub app_id: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct RunList {
    pub total_count: i64,
    pub check_runs: Vec<CheckRunJson>,
}

fn app_slug_for_id(id: i64) -> &'static str {
    if id == 1 { "actions" } else { API_APP }
}

async fn list_runs_where(
    state: &AppState,
    access: &RepoAccess,
    p: &Pagination,
    q: &RunListQuery,
    sha: Option<&str>,
    suite_id: Option<i64>,
) -> ApiResult<axum::Json<RunList>> {
    let latest = match q.filter.as_deref().unwrap_or("latest") {
        "latest" => true,
        "all" => false,
        _ => return Err(invalid("filter")),
    };
    if let Some(s) = &q.status
        && !matches!(s.as_str(), "queued" | "in_progress" | "completed")
    {
        return Err(invalid("status"));
    }
    let app = q.app_id.map(app_slug_for_id);
    let base = format!(
        "SELECT {} FROM check_runs r JOIN check_suites s ON s.id = r.check_suite_id
          WHERE r.repo_id = $1 AND ($2::text IS NULL OR r.head_sha = $2)
            AND ($3::bigint IS NULL OR r.check_suite_id = $3)
            AND ($4::text IS NULL OR r.name = $4)
            AND ($5::text IS NULL OR r.status = $5)
            AND ($6::text IS NULL OR s.app_slug = $6)",
        db::prefixed("r", RUN_COLUMNS)
    );
    let inner = if latest {
        format!(
            "SELECT DISTINCT ON (x.name, x.check_suite_id) * FROM ({base}) x
              ORDER BY x.name, x.check_suite_id, x.id DESC"
        )
    } else {
        base
    };
    let count: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM ({inner}) c"))
        .bind(access.repo.id)
        .bind(sha)
        .bind(suite_id)
        .bind(q.check_name.as_deref())
        .bind(q.status.as_deref())
        .bind(app)
        .fetch_one(&state.db)
        .await?;
    let rows: Vec<RunRow> = sqlx::query_as(&format!(
        "SELECT {RUN_COLUMNS} FROM ({inner}) c ORDER BY id DESC LIMIT $7 OFFSET $8"
    ))
    .bind(access.repo.id)
    .bind(sha)
    .bind(suite_id)
    .bind(q.check_name.as_deref())
    .bind(q.status.as_deref())
    .bind(app)
    .bind(p.limit())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    Ok(axum::Json(RunList {
        total_count: count,
        check_runs: render_runs(state, access, &rows).await?,
    }))
}

pub async fn list_for_ref(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo, rev)): Path<(String, String, String)>,
    Query(q): Query<RunListQuery>,
) -> ApiResult<axum::Json<RunList>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let sha = git::resolve_commit(&git::store(&state), access.repo.id, &rev)
        .await?
        .ok_or(ApiError::NotFound)?;
    list_runs_where(&state, &access, &p, &q, Some(&sha), None).await
}

pub async fn list_for_suite(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo, id)): Path<(String, String, i64)>,
    Query(q): Query<RunListQuery>,
) -> ApiResult<axum::Json<RunList>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    find_suite(&state, access.repo.id, id).await?;
    list_runs_where(&state, &access, &p, &q, None, Some(id)).await
}

#[derive(Debug, Deserialize)]
pub struct SuiteListQuery {
    pub app_id: Option<i64>,
    pub check_name: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SuiteList {
    pub total_count: i64,
    pub check_suites: Vec<CheckSuiteJson>,
}

pub async fn suites_for_ref(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo, rev)): Path<(String, String, String)>,
    Query(q): Query<SuiteListQuery>,
) -> ApiResult<axum::Json<SuiteList>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let sha = git::resolve_commit(&git::store(&state), access.repo.id, &rev)
        .await?
        .ok_or(ApiError::NotFound)?;
    let app = q.app_id.map(app_slug_for_id);
    let filter = "repo_id = $1 AND head_sha = $2 AND ($3::text IS NULL OR app_slug = $3)
        AND ($4::text IS NULL OR EXISTS (SELECT 1 FROM check_runs r
                WHERE r.check_suite_id = check_suites.id AND r.name = $4))";
    let total: i64 =
        sqlx::query_scalar(&format!("SELECT count(*) FROM check_suites WHERE {filter}"))
            .bind(access.repo.id)
            .bind(&sha)
            .bind(app)
            .bind(q.check_name.as_deref())
            .fetch_one(&state.db)
            .await?;
    let rows: Vec<SuiteRow> = sqlx::query_as(&format!(
        "SELECT {SUITE_COLUMNS} FROM check_suites WHERE {filter} ORDER BY id LIMIT $5 OFFSET $6"
    ))
    .bind(access.repo.id)
    .bind(&sha)
    .bind(app)
    .bind(q.check_name.as_deref())
    .bind(p.limit())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    Ok(axum::Json(SuiteList {
        total_count: total,
        check_suites: render_suites(&state, &access, &rows).await?,
    }))
}

pub async fn get_suite(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, id)): Path<(String, String, i64)>,
) -> ApiResult<axum::Json<CheckSuiteJson>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let s = find_suite(&state, access.repo.id, id).await?;
    Ok(axum::Json(
        render_suites(&state, &access, &[s]).await?.remove(0),
    ))
}

#[derive(Debug, Deserialize)]
pub struct CreateSuiteBody {
    pub head_sha: Option<String>,
}

pub async fn create_suite(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo)): Path<(String, String)>,
    Json(body): Json<CreateSuiteBody>,
) -> ApiResult<(StatusCode, axum::Json<CheckSuiteJson>)> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    access.require(Permission::Write)?;
    let head = body.head_sha.ok_or_else(|| {
        ApiError::invalid_field(FieldError::missing_field("CheckSuite", "head_sha"))
    })?;
    let sha = git::resolve_commit(&git::store(&state), access.repo.id, &head)
        .await?
        .ok_or_else(|| ApiError::unprocessable(format!("No commit found for SHA: {head}")))?;
    let mut tx = Tx::begin(&state).await?;
    let (suite, created) = ensure_suite(&mut tx, &state, access.repo.id, &sha, API_APP).await?;
    if created {
        tx.emit(Event::CheckSuiteRequested {
            repo_id: access.repo.id,
            check_suite_id: suite.id,
            actor_id: Some(auth.user.id),
        });
    }
    tx.commit().await?;
    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((
        status,
        axum::Json(render_suites(&state, &access, &[suite]).await?.remove(0)),
    ))
}

pub async fn rerequest_suite(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, id)): Path<(String, String, i64)>,
) -> ApiResult<(StatusCode, axum::Json<Value>)> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    access.require(Permission::Write)?;
    let s = find_suite(&state, access.repo.id, id).await?;
    if !s.rerequestable {
        return Err(ApiError::forbidden("This check suite is not rerequestable"));
    }
    let mut tx = Tx::begin(&state).await?;
    sqlx::query(
        "UPDATE check_runs SET status = 'queued', conclusion = NULL, completed_at = NULL,
                started_at = NULL, updated_at = now() WHERE check_suite_id = $1",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    let suite = recompute_suite(&mut tx, id).await?;
    tx.sync(
        &access.scope(),
        "checkSuite",
        id,
        SyncAction::Update,
        &suite_sync_json(&suite),
    )
    .await?;
    tx.enqueue(&ChecksChanged {
        repo_id: access.repo.id,
        sha: suite.head_sha.clone(),
    })
    .await?;
    tx.emit(Event::CheckSuiteRerequested {
        repo_id: access.repo.id,
        check_suite_id: id,
        actor_id: auth.user.id,
    });
    tx.commit().await?;
    Ok((StatusCode::CREATED, axum::Json(json!({}))))
}

#[derive(Debug, Deserialize)]
pub struct PreferencesBody {
    #[serde(default)]
    pub auto_trigger_checks: Vec<Value>,
}

pub async fn preferences(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo)): Path<(String, String)>,
    Json(body): Json<PreferencesBody>,
) -> ApiResult<axum::Json<Value>> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    access.require(Permission::Admin)?;
    let repository = MinimalRepository::new(
        &state.urls,
        &access.repo,
        &access.owner,
        access.api_permission(),
    );
    Ok(axum::Json(json!({
        "preferences": {"auto_trigger_checks": body.auto_trigger_checks},
        "repository": repository,
    })))
}
