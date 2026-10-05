//! Repository maintenance run by background jobs: `git gc`, `git fsck`,
//! `git repack`, size and language recalculation.
//!
//! * `POST /_bgh/admin/repos/{owner}/{repo}/maintenance` `{"operation"}` → 202
//! * `GET  /_bgh/admin/repos/{owner}/{repo}/maintenance` → recent runs
//! * `POST /_bgh/admin/maintenance` `{"operation"}` → 202, every repository

use std::collections::HashMap;
use std::path::Path as FsPath;
use std::process::Stdio;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use bgh_core::audit::Target;
use bgh_core::jobs::JobPayload;
use bgh_core::prelude::*;
use bgh_core::sync;
use bgh_core::time::ts;
use bgh_repos::json::repo_sync_json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::FromRow;
use tokio::process::Command;

use crate::common::{self, log, repo_target};

pub const OPERATIONS: &[&str] = &[
    "gc",
    "fsck",
    "repack",
    "recalculate_size",
    "recalculate_languages",
];

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
    let mut tx = Tx::begin(&state).await?;
    let run: RunRow = sqlx::query_as(&format!(
        "INSERT INTO repo_maintenance_runs (repo_id, operation, requested_by_id)
         VALUES ($1, $2, $3) RETURNING {RUN_COLUMNS}"
    ))
    .bind(repo.id)
    .bind(&body.operation)
    .bind(auth.user.id)
    .fetch_one(&mut *tx)
    .await?;
    tx.enqueue(&RepoMaintenance { run_id: run.id }).await?;
    log(
        &mut tx,
        &auth,
        &headers,
        "repo.maintenance",
        repo_target(&owner, repo.id),
        json!({ "operation": body.operation, "run_id": run.id, "repo": format!("{}/{}", owner.login, repo.name) }),
    )
    .await?;
    tx.commit().await?;
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
    let Some(owner) = db::User::find(&mut *tx, repo.owner_id).await? else {
        return Ok(());
    };
    tx.sync(
        &sync::repo_scope(repo.id),
        "repository",
        repo.id,
        SyncAction::Update,
        &repo_sync_json(&repo, &owner.login),
    )
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
    match run.operation.as_str() {
        "gc" => git(state, &dir, &["gc", "--quiet", "--prune=now"]).await,
        // -l: don't copy objects borrowed from a fork parent (alternates).
        "repack" => git(state, &dir, &["repack", "-a", "-d", "-l", "-q"]).await,
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
