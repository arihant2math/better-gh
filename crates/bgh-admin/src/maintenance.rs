//! Repository maintenance run by background jobs: `git gc`, `git fsck`,
//! `git repack`, size and language recalculation, forced pruning and fork
//! dissociation. Object maintenance is fork-network aware
//! (`bgh_repos::maintenance::run_task`): repositories other forks borrow
//! objects from are never pruned, and nothing is pruned before the grace
//! period (`git_maintenance.prune_grace_days`) except by the forced `prune`.
//!
//! * `POST /_bgh/admin/repos/{owner}/{repo}/maintenance` `{"operation",
//!   "force"}` → 202 (`prune` needs `"force": true` and a repository no
//!   other repository borrows from; 422 otherwise)
//! * `GET  /_bgh/admin/repos/{owner}/{repo}/maintenance` → recent runs
//! * `POST /_bgh/admin/repos/{owner}/{repo}/detach` → 202: leave the fork
//!   network (clear parent/source, then a `dissociate` run)
//! * `POST /_bgh/admin/maintenance` `{"operation"}` → 202, every repository
//!   (not `prune` / `dissociate`)
//! * `GET  /_bgh/admin/git-maintenance` → schedule settings + status counts
//! * `GET  /_bgh/admin/git-maintenance/repos?status=` → per-repo status
//! * `POST /_bgh/admin/git-maintenance/run` → 202: one scheduler pass now

use std::collections::HashMap;
use std::path::Path as FsPath;
use std::process::Stdio;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use bgh_core::audit::Target;
use bgh_core::jobs::JobPayload;
use bgh_core::prelude::*;
use bgh_core::time::ts;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::FromRow;
use tokio::process::Command;

use bgh_git::maintenance::Task;

use crate::common::{self, log, repo_target};

pub const OPERATIONS: &[&str] = &[
    "gc",
    "fsck",
    "repack",
    "recalculate_size",
    "recalculate_languages",
    "prune",
    "dissociate",
];

/// Operations that `POST /_bgh/admin/maintenance` may not run everywhere.
const SINGLE_REPO_ONLY: &[&str] = &["prune", "dissociate"];

/// Job: execute one `repo_maintenance_runs` row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoMaintenance {
    pub run_id: i64,
}

impl JobPayload for RepoMaintenance {
    const KIND: &'static str = "admin.repo_maintenance";
    const MAX_ATTEMPTS: i32 = 3;
}

#[derive(Debug, FromRow)]
struct RunRow {
    id: i64,
    repo_id: i64,
    operation: String,
    status: String,
    output: Option<String>,
    requested_by_id: Option<i64>,
    created_at: DateTime<Utc>,
    started_at: Option<DateTime<Utc>>,
    finished_at: Option<DateTime<Utc>>,
}

const RUN_COLUMNS: &str =
    "id, repo_id, operation, status, output, requested_by_id, created_at, started_at, finished_at";

#[derive(Debug, Serialize)]
pub struct RunJson {
    pub id: i64,
    pub repository_id: i64,
    pub operation: String,
    pub status: String,
    pub output: Option<String>,
    pub requested_by_id: Option<i64>,
    pub created_at: Timestamp,
    pub started_at: Option<Timestamp>,
    pub finished_at: Option<Timestamp>,
}

impl From<RunRow> for RunJson {
    fn from(r: RunRow) -> Self {
        Self {
            id: r.id,
            repository_id: r.repo_id,
            operation: r.operation,
            status: r.status,
            output: r.output,
            requested_by_id: r.requested_by_id,
            created_at: r.created_at.into(),
            started_at: ts(r.started_at),
            finished_at: ts(r.finished_at),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct OperationBody {
    #[serde(default)]
    pub operation: String,
    /// Required (`true`) for `prune`.
    #[serde(default)]
    pub force: bool,
}

fn op_error(message: impl Into<String>) -> ApiError {
    ApiError::invalid_field(FieldError::custom(
        "Maintenance",
        "operation",
        message.into(),
    ))
}

fn check_operation(op: &str) -> ApiResult<()> {
    if OPERATIONS.contains(&op) {
        Ok(())
    } else {
        Err(ApiError::invalid_field(FieldError::custom(
            "Maintenance",
            "operation",
            format!("operation must be one of {}", OPERATIONS.join(", ")),
        )))
    }
}

/// `POST /_bgh/admin/repos/{owner}/{repo}/maintenance` → 202 run.
pub async fn schedule(
    State(state): State<AppState>,
    auth: RequireSiteAdmin,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
    Json(body): Json<OperationBody>,
) -> ApiResult<(StatusCode, Json<RunJson>)> {
    check_operation(&body.operation)?;
    let (owner, repo) = common::repo(&state, &owner, &name).await?;
    if body.operation == "prune" {
        if !body.force {
            return Err(op_error(
                "prune removes unreachable objects immediately; pass \"force\": true",
            ));
        }
        let role = bgh_repos::maintenance::network_role(&state, repo.id)
            .await
            .map_err(ApiError::internal)?;
        if role.has_dependents {
            return Err(op_error(
                "other repositories borrow objects from this one; prune is not allowed",
            ));
        }
    }
    let run = insert_run(
        &state,
        &auth,
        &headers,
        &owner,
        &repo,
        &body.operation,
        body.force,
    )
    .await?;
    Ok((StatusCode::ACCEPTED, Json(run.into())))
}

/// Insert a run row, enqueue its job and audit it (one transaction).
async fn insert_run(
    state: &AppState,
    auth: &RequireSiteAdmin,
    headers: &HeaderMap,
    owner: &db::User,
    repo: &db::Repository,
    operation: &str,
    force: bool,
) -> ApiResult<RunRow> {
    let mut tx = Tx::begin(state).await?;
    let run: RunRow = sqlx::query_as(&format!(
        "INSERT INTO repo_maintenance_runs (repo_id, operation, requested_by_id)
         VALUES ($1, $2, $3) RETURNING {RUN_COLUMNS}"
    ))
    .bind(repo.id)
    .bind(operation)
    .bind(auth.user.id)
    .fetch_one(&mut *tx)
    .await?;
    tx.enqueue(&RepoMaintenance { run_id: run.id }).await?;
    let mut data = json!({ "operation": operation, "run_id": run.id, "repo": format!("{}/{}", owner.login, repo.name) });
    if force {
        data["force"] = json!(true);
    }
    log(
        &mut tx,
        auth,
        headers,
        "repo.maintenance",
        repo_target(owner, repo.id),
        data,
    )
    .await?;
    tx.commit().await?;
    Ok(run)
}

/// `POST /_bgh/admin/repos/{owner}/{repo}/detach` → 202 run: the
/// repository leaves its fork network. Its parent/source links are cleared
/// (its own forks get it as their new source) and a `dissociate` run makes
/// its storage self-contained.
pub async fn detach(
    State(state): State<AppState>,
    auth: RequireSiteAdmin,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
) -> ApiResult<(StatusCode, Json<RunJson>)> {
    let (owner, repo) = common::repo(&state, &owner, &name).await?;
    let store = bgh_repos::store(&state);
    let dir = store.path(repo.id);
    let borrows = tokio::task::spawn_blocking(move || bgh_git::maintenance::has_alternates(&dir))
        .await
        .map_err(ApiError::internal)?;
    if repo.parent_id.is_none() && repo.source_id.is_none() && !borrows {
        return Err(ApiError::invalid_field(FieldError::custom(
            "Repository",
            "parent",
            "repository is not part of a fork network",
        )));
    }
    let mut tx = Tx::begin(&state).await?;
    sqlx::query("UPDATE repositories SET fork = false, parent_id = NULL, source_id = NULL, updated_at = now() WHERE id = $1")
        .bind(repo.id)
        .execute(&mut *tx)
        .await?;
    if let Some(parent) = repo.parent_id {
        sqlx::query(
            "UPDATE repositories SET forks_count = greatest(forks_count - 1, 0) WHERE id = $1",
        )
        .bind(parent)
        .execute(&mut *tx)
        .await?;
    }
    // The detached repository becomes the root of its own subtree.
    let moved: Vec<i64> = sqlx::query_scalar(
        "WITH RECURSIVE sub AS (
             SELECT id FROM repositories WHERE parent_id = $1
             UNION SELECT r.id FROM repositories r JOIN sub ON r.parent_id = sub.id
         )
         UPDATE repositories SET source_id = $1 WHERE id IN (SELECT id FROM sub) RETURNING id",
    )
    .bind(repo.id)
    .fetch_all(&mut *tx)
    .await?;
    for id in std::iter::once(repo.id).chain(repo.parent_id).chain(moved) {
        tx.sync_model(SyncModel::Repo, id, SyncAction::Update)
            .await?;
    }
    log(
        &mut tx,
        &auth,
        &headers,
        "repo.detach_fork_network",
        repo_target(&owner, repo.id),
        json!({ "repo": format!("{}/{}", owner.login, repo.name), "parent_id": repo.parent_id }),
    )
    .await?;
    tx.commit().await?;
    let run = insert_run(&state, &auth, &headers, &owner, &repo, "dissociate", false).await?;
    Ok((StatusCode::ACCEPTED, Json(run.into())))
}

/// `GET /_bgh/admin/repos/{owner}/{repo}/maintenance` → runs, newest first.
pub async fn list_runs(
    State(state): State<AppState>,
    _auth: RequireSiteAdmin,
    p: Pagination,
    Path((owner, name)): Path<(String, String)>,
) -> ApiResult<Page<RunJson>> {
    let (_, repo) = common::repo(&state, &owner, &name).await?;
    let rows: Vec<RunRow> = sqlx::query_as(&format!(
        "SELECT {RUN_COLUMNS} FROM repo_maintenance_runs WHERE repo_id = $1
          ORDER BY id DESC LIMIT $2 OFFSET $3"
    ))
    .bind(repo.id)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    Ok(p.page(rows).map(RunJson::from))
}

/// `POST /_bgh/admin/maintenance` → 202 `{"operation", "scheduled": n}`:
/// one run (and job) per repository, set-based.
pub async fn schedule_all(
    State(state): State<AppState>,
    auth: RequireSiteAdmin,
    headers: HeaderMap,
    Json(body): Json<OperationBody>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    check_operation(&body.operation)?;
    if SINGLE_REPO_ONLY.contains(&body.operation.as_str()) {
        return Err(op_error(format!(
            "{} can only run on a single repository",
            body.operation
        )));
    }
    let mut tx = Tx::begin(&state).await?;
    let n: i64 = sqlx::query_scalar(
        "WITH runs AS (
            INSERT INTO repo_maintenance_runs (repo_id, operation, requested_by_id)
            SELECT id, $1, $2 FROM repositories ORDER BY id
            RETURNING id
         ), queued AS (
            INSERT INTO jobs (kind, payload, max_attempts)
            SELECT $3, jsonb_build_object('run_id', id), $4 FROM runs
            RETURNING id
         )
         SELECT count(*) FROM queued",
    )
    .bind(&body.operation)
    .bind(auth.user.id)
    .bind(RepoMaintenance::KIND)
    .bind(RepoMaintenance::MAX_ATTEMPTS)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query("SELECT pg_notify($1, $2)")
        .bind(bgh_core::jobs::NOTIFY_CHANNEL)
        .bind(RepoMaintenance::KIND)
        .execute(&mut *tx)
        .await?;
    log(
        &mut tx,
        &auth,
        &headers,
        "business.repo_maintenance",
        Target::Site,
        json!({ "operation": body.operation, "repositories": n }),
    )
    .await?;
    tx.commit().await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({ "operation": body.operation, "scheduled": n })),
    ))
}

/// Run git in `dir` with an isolated config; returns (success, output).
async fn git(state: &AppState, dir: &FsPath, args: &[&str]) -> anyhow::Result<(bool, String)> {
    let out = Command::new(&state.config.git_bin)
        .arg("--git-dir")
        .arg(dir)
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    Ok((out.status.success(), text.trim().to_string()))
}

/// Language by file extension (a small linguist subset).
fn language(path: &str) -> Option<&'static str> {
    let name = path.rsplit('/').next().unwrap_or(path);
    if name == "Dockerfile" {
        return Some("Dockerfile");
    }
    if name == "Makefile" {
        return Some("Makefile");
    }
    let ext = name.rsplit_once('.')?.1.to_ascii_lowercase();
    Some(match ext.as_str() {
        "rs" => "Rust",
        "go" => "Go",
        "py" => "Python",
        "rb" => "Ruby",
        "js" | "mjs" | "cjs" | "jsx" => "JavaScript",
        "ts" | "tsx" | "mts" | "cts" => "TypeScript",
        "java" => "Java",
        "kt" | "kts" => "Kotlin",
        "swift" => "Swift",
        "c" | "h" => "C",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" => "C++",
        "cs" => "C#",
        "m" => "Objective-C",
        "php" => "PHP",
        "scala" => "Scala",
        "hs" => "Haskell",
        "ex" | "exs" => "Elixir",
        "erl" => "Erlang",
        "clj" | "cljs" => "Clojure",
        "lua" => "Lua",
        "pl" | "pm" => "Perl",
        "r" => "R",
        "dart" => "Dart",
        "zig" => "Zig",
        "nim" => "Nim",
        "ml" | "mli" => "OCaml",
        "fs" | "fsx" => "F#",
        "sh" | "bash" | "zsh" => "Shell",
        "ps1" => "PowerShell",
        "html" | "htm" => "HTML",
        "css" => "CSS",
        "scss" => "SCSS",
        "vue" => "Vue",
        "svelte" => "Svelte",
        "sql" => "SQL",
        "tex" => "TeX",
        "vim" => "Vim Script",
        "nix" => "Nix",
        "tf" => "HCL",
        _ => return None,
    })
}

/// Bytes per language at HEAD (`git ls-tree -r -l`).
async fn languages(state: &AppState, dir: &FsPath) -> anyhow::Result<Vec<(String, i64)>> {
    let (ok, out) = git(state, dir, &["ls-tree", "-r", "-l", "-z", "HEAD"]).await?;
    let mut by_lang: HashMap<&'static str, i64> = HashMap::new();
    if ok {
        for entry in out.split('\0') {
            // "<mode> blob <sha> <size>\t<path>"
            let Some((meta, path)) = entry.split_once('\t') else {
                continue;
            };
            let mut cols = meta.split_whitespace();
            if cols.nth(1) != Some("blob") {
                continue;
            }
            let size: i64 = cols.nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
            if path.starts_with("vendor/") || path.contains("node_modules/") {
                continue;
            }
            if let Some(lang) = language(path) {
                *by_lang.entry(lang).or_default() += size;
            }
        }
    }
    let mut v: Vec<(String, i64)> = by_lang
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    Ok(v)
}

async fn update_repo(
    state: &AppState,
    repo_id: i64,
    set: &str,
    value: Value,
) -> anyhow::Result<()> {
    let mut tx = Tx::begin(state).await?;
    let repo: Option<db::Repository> = sqlx::query_as(&format!(
        "UPDATE repositories SET {set} WHERE id = $1 RETURNING {}",
        db::Repository::COLUMNS
    ))
    .bind(repo_id)
    .bind(value)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(repo) = repo else { return Ok(()) };
    tx.sync_model(SyncModel::Repo, repo.id, SyncAction::Update)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    tx.commit().await?;
    Ok(())
}

async fn execute(state: &AppState, run: &RunRow) -> anyhow::Result<(bool, String)> {
    let store = bgh_repos::store(state);
    if !store.exists(run.repo_id) {
        return Ok((false, "repository storage does not exist".into()));
    }
    let dir = store.path(run.repo_id);
    let object_task = match run.operation.as_str() {
        "gc" => Some(Task::Gc),
        "repack" => Some(Task::Repack),
        "prune" => Some(Task::PruneNow),
        _ => None,
    };
    if let Some(task) = object_task {
        return Ok(
            match bgh_repos::maintenance::run_task(state, run.repo_id, task).await {
                Ok(out) => (true, if out.is_empty() { "done".into() } else { out }),
                Err(e) => (false, format!("{e:#}")),
            },
        );
    }
    match run.operation.as_str() {
        "dissociate" => {
            bgh_repos::maintenance::dissociate(state, run.repo_id).await?;
            Ok((
                true,
                "repository is self-contained (fsck --connectivity-only passed)".into(),
            ))
        }
        "fsck" => {
            let (ok, out) = git(state, &dir, &["fsck", "--no-progress", "--no-dangling"]).await?;
            Ok((
                ok,
                if out.is_empty() {
                    "no problems found".into()
                } else {
                    out
                },
            ))
        }
        "recalculate_size" => {
            let kb = store.disk_size_kb(run.repo_id).await?;
            update_repo(
                state,
                run.repo_id,
                "size = $2::bigint, updated_at = now()",
                json!(kb),
            )
            .await?;
            Ok((true, json!({ "size_kb": kb }).to_string()))
        }
        "recalculate_languages" => {
            let langs = languages(state, &dir).await?;
            let primary = langs.first().map(|(l, _)| l.clone());
            update_repo(
                state,
                run.repo_id,
                "language = $2 #>> '{}', updated_at = now()",
                json!(primary),
            )
            .await?;
            let map: serde_json::Map<String, Value> =
                langs.into_iter().map(|(k, v)| (k, json!(v))).collect();
            Ok((true, Value::Object(map).to_string()))
        }
        other => Ok((false, format!("unknown operation {other:?}"))),
    }
}

/// Job handler for [`RepoMaintenance`].
pub async fn run(state: AppState, job: RepoMaintenance) -> anyhow::Result<()> {
    let run: Option<RunRow> = sqlx::query_as(&format!(
        "UPDATE repo_maintenance_runs SET status = 'running', started_at = now()
          WHERE id = $1 AND status IN ('queued', 'running') RETURNING {RUN_COLUMNS}"
    ))
    .bind(job.run_id)
    .fetch_optional(&state.db)
    .await?;
    let Some(run) = run else {
        return Ok(()); // repository deleted or already finished
    };
    let (ok, output) = match execute(&state, &run).await {
        Ok(r) => r,
        Err(e) => (false, format!("{e:#}")),
    };
    // Keep the stored output bounded.
    let output: String = output.chars().take(64 * 1024).collect();
    sqlx::query(
        "UPDATE repo_maintenance_runs SET status = $2, output = $3, finished_at = now() WHERE id = $1",
    )
    .bind(run.id)
    .bind(if ok { "succeeded" } else { "failed" })
    .bind(&output)
    .execute(&state.db)
    .await?;
    Ok(())
}

// ----- scheduled maintenance status --------------------------------------------

#[derive(Debug, FromRow)]
struct StatusRow {
    repo_id: i64,
    full_name: String,
    status: String,
    error: Option<String>,
    last_run_at: Option<DateTime<Utc>>,
    last_full_at: Option<DateTime<Utc>>,
    pack_count: i64,
    loose_count: i64,
    has_alternates: bool,
    has_dependents: bool,
}

#[derive(Debug, Serialize)]
pub struct StatusJson {
    pub repository_id: i64,
    pub full_name: String,
    pub status: String,
    pub error: Option<String>,
    pub last_run_at: Option<Timestamp>,
    pub last_full_at: Option<Timestamp>,
    pub pack_count: i64,
    pub loose_count: i64,
    pub has_alternates: bool,
    pub has_dependents: bool,
}

impl From<StatusRow> for StatusJson {
    fn from(r: StatusRow) -> Self {
        Self {
            repository_id: r.repo_id,
            full_name: r.full_name,
            status: r.status,
            error: r.error,
            last_run_at: ts(r.last_run_at),
            last_full_at: ts(r.last_full_at),
            pack_count: r.pack_count,
            loose_count: r.loose_count,
            has_alternates: r.has_alternates,
            has_dependents: r.has_dependents,
        }
    }
}

const STATUS_SELECT: &str = "SELECT m.repo_id, o.login || '/' || r.name AS full_name, m.status, m.error,
        m.last_run_at, m.last_full_at, m.pack_count, m.loose_count, m.has_alternates, m.has_dependents
   FROM repo_maintenance m
   JOIN repositories r ON r.id = m.repo_id
   JOIN users o ON o.id = r.owner_id";

/// Scheduled-maintenance status of one repository (admin repo detail).
pub async fn repo_status(state: &AppState, repo_id: i64) -> ApiResult<Option<StatusJson>> {
    let row: Option<StatusRow> = sqlx::query_as(&format!("{STATUS_SELECT} WHERE m.repo_id = $1"))
        .bind(repo_id)
        .fetch_optional(&state.db)
        .await?;
    Ok(row.map(Into::into))
}

/// `GET /_bgh/admin/git-maintenance` → `{settings, counts}`.
pub async fn overview(
    State(state): State<AppState>,
    _auth: RequireSiteAdmin,
) -> ApiResult<Json<Value>> {
    let s = bgh_core::settings::load_uncached(&state.config, &state.db).await?;
    let counts: (i64, i64, i64, i64, i64, Option<DateTime<Utc>>) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM repositories),
                count(*) FILTER (WHERE m.status = 'succeeded'),
                count(*) FILTER (WHERE m.status = 'failed'),
                count(*) FILTER (WHERE m.status = 'skipped'),
                count(*) FILTER (WHERE m.has_dependents),
                max(m.last_run_at)
           FROM repo_maintenance m",
    )
    .fetch_one(&state.db)
    .await?;
    Ok(Json(json!({
        "settings": s.git_maintenance,
        "repositories": counts.0,
        "succeeded": counts.1,
        "failed": counts.2,
        "skipped": counts.3,
        "never_run": (counts.0 - counts.1 - counts.2 - counts.3).max(0),
        "with_dependents": counts.4,
        "last_run_at": ts(counts.5),
    })))
}

#[derive(Debug, Default, Deserialize)]
pub struct StatusQuery {
    /// `succeeded` | `failed` | `skipped`
    pub status: Option<String>,
}

/// `GET /_bgh/admin/git-maintenance/repos?status=` → per-repository state,
/// failures first, then most recently run.
pub async fn list_status(
    State(state): State<AppState>,
    _auth: RequireSiteAdmin,
    p: Pagination,
    Query(q): Query<StatusQuery>,
) -> ApiResult<Page<StatusJson>> {
    let rows: Vec<StatusRow> = sqlx::query_as(&format!(
        "{STATUS_SELECT} WHERE ($1::text IS NULL OR m.status = $1)
          ORDER BY (m.status = 'failed') DESC, m.updated_at DESC, m.repo_id DESC
          LIMIT $2 OFFSET $3"
    ))
    .bind(q.status.as_deref())
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    Ok(p.page(rows).map(StatusJson::from))
}

/// `POST /_bgh/admin/git-maintenance/run` → 202: queue one scheduler pass
/// (including archive-cache pruning).
pub async fn run_now(
    State(state): State<AppState>,
    auth: RequireSiteAdmin,
    headers: HeaderMap,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let mut tx = Tx::begin(&state).await?;
    tx.enqueue(&bgh_repos::maintenance::RunPass {}).await?;
    log(
        &mut tx,
        &auth,
        &headers,
        "business.git_maintenance_run",
        Target::Site,
        json!({}),
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::ACCEPTED, Json(json!({ "queued": true }))))
}

#[cfg(test)]
mod tests {
    #[test]
    fn detects_languages() {
        assert_eq!(super::language("src/main.rs"), Some("Rust"));
        assert_eq!(super::language("web/App.TSX"), Some("TypeScript"));
        assert_eq!(super::language("Dockerfile"), Some("Dockerfile"));
        assert_eq!(super::language("README"), None);
    }
}
