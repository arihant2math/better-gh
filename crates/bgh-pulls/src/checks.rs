//! Checks API: check runs (create / update / get / list / annotations /
//! rerequest) and check suites (create / get / list / rerequest /
//! preferences). Runs created with a GitHub App's credentials (installation
//! or user-to-server tokens) belong to that app's suite (`check_suites.app_id`,
//! P46); bgh-actions creates its own suites (`app_slug = 'actions'`, shown
//! as GitHub Actions, app id [`ACTIONS_APP_ID`]). Runs created by users
//! with ordinary tokens have no app (`app: null`, internal slug `api`).

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

/// App id of the built-in Actions app (GitHub's id for GitHub Actions; real
/// app ids are above it).
pub const ACTIONS_APP_ID: i64 = 15368;

/// The integration a caller creates check runs and suites as:
/// `(app_slug, app_id)`.
pub async fn caller_app(state: &AppState, auth: &AuthContext) -> ApiResult<(String, Option<i64>)> {
    let app: Option<(i64, String)> = if bgh_core::apps::installation_id(auth).is_some() {
        sqlx::query_as("SELECT id, slug FROM github_apps WHERE bot_user_id = $1")
            .bind(auth.user.id)
            .fetch_optional(&state.db)
            .await?
    } else if let Some(id) = bgh_core::apps::user_to_server_app_id(auth) {
        sqlx::query_as("SELECT id, slug FROM github_apps WHERE id = $1")
            .bind(id)
            .fetch_optional(&state.db)
            .await?
    } else {
        None
    };
    Ok(match app {
        Some((id, slug)) => (slug, Some(id)),
        None => (API_APP.to_string(), None),
    })
}

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

const RUN_COLUMNS: &str = "id, check_suite_id, repo_id, head_sha, name, status, conclusion, \
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
    pub app_id: Option<i64>,
    pub status: String,
    pub conclusion: Option<String>,
    pub rerequestable: bool,
    pub latest_check_runs_count: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

const SUITE_COLUMNS: &str = "id, repo_id, head_sha, head_branch, before_sha, after_sha, app_slug, \
    app_id, status, conclusion, rerequestable, latest_check_runs_count, created_at, updated_at";

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

/// `integration` (a GitHub App) of a check run or suite.
pub type AppJson = bgh_core::apps::Integration;

/// The built-in Actions app.
fn actions_app(state: &AppState, owner: &db::User) -> AppJson {
    let epoch = Timestamp::from(DateTime::<Utc>::UNIX_EPOCH);
    let perms = [
        ("checks", "write"),
        ("metadata", "read"),
        ("statuses", "write"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    AppJson {
        id: ACTIONS_APP_ID,
        slug: "github-actions".into(),
        node_id: node_id::encode(NodeType::Integration, ACTIONS_APP_ID),
        client_id: "Iv1.github-actions".into(),
        owner: SimpleUser::new(&state.urls, owner),
        name: "GitHub Actions".into(),
        description: Some("Automate your workflow from idea to production".into()),
        external_url: state.urls.html("/features/actions"),
        html_url: state.urls.html("/apps/github-actions"),
        created_at: epoch,
        updated_at: epoch,
        permissions: perms,
        events: vec!["check_run".into(), "check_suite".into()],
        installations_count: None,
    }
}

/// `app` objects of check suites `(app_slug, app_id)`, keyed by suite id
/// (real apps batch-loaded; `None` for runs created by users).
async fn suite_apps(
    state: &AppState,
    owner: &db::User,
    suites: &[(i64, String, Option<i64>)],
) -> ApiResult<HashMap<i64, Option<AppJson>>> {
    let mut ids: Vec<i64> = suites.iter().filter_map(|s| s.2).collect();
    ids.sort_unstable();
    ids.dedup();
    let apps: Vec<bgh_core::apps::AppRow> = if ids.is_empty() {
        Vec::new()
    } else {
        sqlx::query_as(&format!(
            "SELECT {} FROM github_apps WHERE id = ANY($1)",
            bgh_core::apps::AppRow::COLUMNS
        ))
        .bind(&ids)
        .fetch_all(&state.db)
        .await?
    };
    let owners = bgh_core::views::users_by_id(state, apps.iter().map(|a| Some(a.owner_id))).await?;
    let apps: HashMap<i64, AppJson> = apps
        .iter()
        .filter_map(|a| {
            Some((
                a.id,
                AppJson::new(&state.urls, a, owners.get(&a.owner_id)?, None),
            ))
        })
        .collect();
    Ok(suites
        .iter()
        .map(|(id, slug, app_id)| {
            let app = match app_id {
                Some(a) => apps.get(a).cloned(),
                None if slug == "actions" => Some(actions_app(state, owner)),
                None => None,
            };
            (*id, app)
        })
        .collect())
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
    pub app: Option<AppJson>,
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
    pub app: Option<AppJson>,
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
    let suites: Vec<(i64, String, Option<i64>)> =
        sqlx::query_as("SELECT id, app_slug, app_id FROM check_suites WHERE id = ANY($1)")
            .bind(&suite_ids)
            .fetch_all(&state.db)
            .await?;
    let apps = suite_apps(state, &access.owner, &suites).await?;
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
                app: apps.get(&r.check_suite_id).cloned().flatten(),
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
    let keys: Vec<(i64, String, Option<i64>)> = suites
        .iter()
        .map(|s| (s.id, s.app_slug.clone(), s.app_id))
        .collect();
    let apps = suite_apps(state, &access.owner, &keys).await?;
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
                app: apps.get(&s.id).cloned().flatten(),
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

/// Find or create the suite of the app `(app_slug, app_id)` for
/// `head_sha` (one suite per app and commit).
pub async fn ensure_suite(
    tx: &mut Tx,
    state: &AppState,
    repo_id: i64,
    head_sha: &str,
    app: (&str, Option<i64>),
) -> ApiResult<(SuiteRow, bool)> {
    let (app_slug, app_id) = app;
    let existing: Option<SuiteRow> = sqlx::query_as(&format!(
        "SELECT {SUITE_COLUMNS} FROM check_suites
          WHERE repo_id = $1 AND head_sha = $2
            AND (CASE WHEN $4::bigint IS NULL THEN app_id IS NULL AND app_slug = $3
                      ELSE app_id = $4 END)
          ORDER BY id DESC LIMIT 1"
    ))
    .bind(repo_id)
    .bind(head_sha)
    .bind(app_slug)
    .bind(app_id)
    .fetch_optional(&mut **tx)
    .await?;
    if let Some(s) = existing {
        return Ok((s, false));
    }
    let branch = branch_for_sha(state, repo_id, head_sha).await?;
    let s: SuiteRow = sqlx::query_as(&format!(
        "INSERT INTO check_suites (repo_id, head_sha, head_branch, after_sha, app_slug, app_id,
                status)
         VALUES ($1, $2, $3, $2, $4, $5, 'queued') RETURNING {SUITE_COLUMNS}"
    ))
    .bind(repo_id)
    .bind(head_sha)
    .bind(branch)
    .bind(app_slug)
    .bind(app_id)
    .fetch_one(&mut **tx)
    .await?;
    tx.sync_model(SyncModel::CheckSuite, s.id, SyncAction::Insert)
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
    app: (&str, Option<i64>),
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
    let (suite, created) = ensure_suite(&mut tx, state, access.repo.id, &head_sha, app).await?;
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
    tx.sync_model(SyncModel::CheckRun, run.id, SyncAction::Insert)
        .await?;
    tx.sync_model(SyncModel::CheckSuite, suite_after.id, SyncAction::Update)
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
    tx.sync_model(SyncModel::CheckRun, run.id, SyncAction::Update)
        .await?;
    tx.sync_model(SyncModel::CheckSuite, suite.id, SyncAction::Update)
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
    let (slug, app_id) = caller_app(&state, &auth).await?;
    let run = create_run(&state, &access, (&slug, app_id), Some(auth.user.id), &input).await?;
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
    tx.sync_model(SyncModel::CheckRun, id, SyncAction::Update)
        .await?;
    tx.sync_model(SyncModel::CheckSuite, suite.id, SyncAction::Update)
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
pub struct RequestedActionBody {
    pub identifier: Option<String>,
}

/// `POST /_bgh/repos/{owner}/{repo}/check-runs/{id}/requested-action`
/// `{"identifier"}`: a user clicked one of the run's `actions` buttons
/// (GitHub's UI-only flow) → `check_run` `requested_action` webhook for
/// the integration to act on. Write access; the identifier must be one of
/// the run's actions.
pub async fn request_action(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, id)): Path<(String, String, i64)>,
    Json(body): Json<RequestedActionBody>,
) -> ApiResult<StatusCode> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    access.require(Permission::Write)?;
    let run = find_run(&state, access.repo.id, id).await?;
    let identifier = body.identifier.unwrap_or_default();
    let known = run
        .actions
        .as_array()
        .into_iter()
        .flatten()
        .any(|a| a["identifier"].as_str() == Some(identifier.as_str()));
    if !known {
        return Err(ApiError::invalid_field(FieldError::invalid(
            "CheckRun",
            "identifier",
        )));
    }
    let mut tx = Tx::begin(&state).await?;
    tx.emit(Event::CheckRunActionRequested {
        repo_id: access.repo.id,
        check_run_id: run.id,
        actor_id: auth.user.id,
        identifier,
    });
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
pub struct RunListQuery {
    pub check_name: Option<String>,
    pub status: Option<String>,
    pub filter: Option<String>,
    pub app_id: Option<i64>,
}

/// A wrapped list body (`{"total_count": n, ...}`) sent with the
/// pagination `Link` header.
#[derive(Debug)]
pub struct Linked<T> {
    pub body: T,
    pub link: Option<String>,
}

impl<T> Linked<T> {
    pub fn new(p: &Pagination, total: i64, page_len: usize, body: T) -> Self {
        let has_next = p.offset() + (page_len as i64) < total;
        Self {
            body,
            link: p.link_header(has_next, Some(total)),
        }
    }
}

impl<T: Serialize> axum::response::IntoResponse for Linked<T> {
    fn into_response(self) -> axum::response::Response {
        let mut resp = axum::Json(self.body).into_response();
        if let Some(link) = self.link
            && let Ok(v) = axum::http::HeaderValue::from_str(&link)
        {
            resp.headers_mut().insert(axum::http::header::LINK, v);
        }
        resp
    }
}

#[derive(Debug, Serialize)]
pub struct RunList {
    pub total_count: i64,
    pub check_runs: Vec<CheckRunJson>,
}

/// `app.id` of a check suite's integration; the expected source of
/// required checks (`app_id` / `integration_id`): the real app, the
/// built-in Actions app, or 0 (no app: runs created by users).
pub fn suite_app_id(slug: &str, app_id: Option<i64>) -> i64 {
    match app_id {
        Some(id) => id,
        None if slug == "actions" => ACTIONS_APP_ID,
        None => 0,
    }
}

/// SQL for a suite's app id (alias `s`), see [`suite_app_id`].
const SUITE_APP_ID_SQL: &str =
    "coalesce(s.app_id, CASE WHEN s.app_slug = 'actions' THEN 15368 ELSE 0 END)";

async fn list_runs_where(
    state: &AppState,
    access: &RepoAccess,
    p: &Pagination,
    q: &RunListQuery,
    sha: Option<&str>,
    suite_id: Option<i64>,
) -> ApiResult<Linked<RunList>> {
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
    let app = q.app_id;
    let base = format!(
        "SELECT {} FROM check_runs r JOIN check_suites s ON s.id = r.check_suite_id
          WHERE r.repo_id = $1 AND ($2::text IS NULL OR r.head_sha = $2)
            AND ($3::bigint IS NULL OR r.check_suite_id = $3)
            AND ($4::text IS NULL OR r.name = $4)
            AND ($5::text IS NULL OR r.status = $5)
            AND ($6::bigint IS NULL OR {SUITE_APP_ID_SQL} = $6)",
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
    let len = rows.len();
    Ok(Linked::new(
        p,
        count,
        len,
        RunList {
            total_count: count,
            check_runs: render_runs(state, access, &rows).await?,
        },
    ))
}

pub async fn list_for_ref(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo, rev)): Path<(String, String, String)>,
    Query(q): Query<RunListQuery>,
) -> ApiResult<Linked<RunList>> {
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
) -> ApiResult<Linked<RunList>> {
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
) -> ApiResult<Linked<SuiteList>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let sha = git::resolve_commit(&git::store(&state), access.repo.id, &rev)
        .await?
        .ok_or(ApiError::NotFound)?;
    let app = q.app_id;
    let filter = format!(
        "repo_id = $1 AND head_sha = $2 AND ($3::bigint IS NULL OR {} = $3)
        AND ($4::text IS NULL OR EXISTS (SELECT 1 FROM check_runs r
                WHERE r.check_suite_id = check_suites.id AND r.name = $4))",
        SUITE_APP_ID_SQL.replace("s.", "check_suites.")
    );
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
    let len = rows.len();
    Ok(Linked::new(
        &p,
        total,
        len,
        SuiteList {
            total_count: total,
            check_suites: render_suites(&state, &access, &rows).await?,
        },
    ))
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
    let (slug, app_id) = caller_app(&state, &auth).await?;
    let (suite, created) =
        ensure_suite(&mut tx, &state, access.repo.id, &sha, (&slug, app_id)).await?;
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
    tx.sync_model(SyncModel::CheckSuite, id, SyncAction::Update)
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
