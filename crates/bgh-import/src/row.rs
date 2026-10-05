//! `imports` rows, options and their JSON.

use bgh_core::prelude::*;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Steps in run order. `git` and `settings` run first so later steps can
/// rely on the repository and its tags.
pub const STEPS: &[&str] = &[
    "git",
    "settings",
    "labels",
    "milestones",
    "issues",
    "comments",
    "events",
    "releases",
    "teams",
    "finish",
];

/// What to import (all on by default except `teams`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Options {
    #[serde(default = "yes")]
    pub git: bool,
    #[serde(default = "yes")]
    pub settings: bool,
    #[serde(default = "yes")]
    pub labels: bool,
    #[serde(default = "yes")]
    pub milestones: bool,
    #[serde(default = "yes")]
    pub issues: bool,
    #[serde(default = "yes")]
    pub releases: bool,
    /// Org teams and their repository permissions (organization targets).
    #[serde(default)]
    pub teams: bool,
    #[serde(default)]
    pub include_lfs: bool,
    /// Source login → local login, consulted after verified-email matching.
    #[serde(default)]
    pub user_map: BTreeMap<String, String>,
}

fn yes() -> bool {
    true
}

impl Default for Options {
    fn default() -> Self {
        serde_json::from_value(json!({})).expect("defaults")
    }
}

impl Options {
    pub fn enabled(&self, step: &str) -> bool {
        match step {
            "git" => self.git,
            "settings" => self.settings,
            "labels" => self.labels,
            "milestones" => self.milestones,
            // Comments and events belong to the issues.
            "issues" | "comments" | "events" => self.issues,
            "releases" => self.releases,
            "teams" => self.teams,
            _ => true,
        }
    }
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ImportRow {
    pub id: i64,
    pub kind: String,
    pub api_url: String,
    pub source_repo: String,
    pub enc_token: Option<Vec<u8>>,
    pub owner_id: i64,
    pub repo_name: String,
    pub repo_id: Option<i64>,
    pub visibility: String,
    pub options: Value,
    pub status: String,
    pub step: String,
    pub cursor: Value,
    pub stats: Value,
    pub error: Option<String>,
    pub attempts: i32,
    pub resume_at: Option<DateTime<Utc>>,
    pub heartbeat_at: Option<DateTime<Utc>>,
    pub created_by: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

impl ImportRow {
    pub const COLUMNS: &'static str = "id, kind, api_url, source_repo, enc_token, owner_id, repo_name, \
        repo_id, visibility, options, status, step, cursor, stats, error, attempts, resume_at, \
        heartbeat_at, created_by, created_at, updated_at, completed_at";

    pub async fn find(db: impl sqlx::PgExecutor<'_>, id: i64) -> ApiResult<Option<Self>> {
        Ok(sqlx::query_as(&format!(
            "SELECT {} FROM imports WHERE id = $1",
            Self::COLUMNS
        ))
        .bind(id)
        .fetch_optional(db)
        .await?)
    }

    pub fn options(&self) -> Options {
        serde_json::from_value(self.options.clone()).unwrap_or_default()
    }

    /// Host of the source, the scope of user mappings (`github.com` for
    /// `api.github.com`).
    pub fn source_host(&self) -> String {
        source_host(&self.api_url)
    }

    /// Scope of this import's repository-level mappings.
    pub fn scope(&self) -> String {
        format!("import:{}", self.id)
    }

    pub fn is_active(&self) -> bool {
        matches!(self.status.as_str(), "queued" | "running" | "waiting")
    }
}

pub fn source_host(api_url: &str) -> String {
    let host = url::Url::parse(api_url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
        .unwrap_or_default();
    host.strip_prefix("api.")
        .map(str::to_string)
        .unwrap_or(host)
}

/// Web URL of the source repository (`https://github.com/o/r`, GHES:
/// strip `/api/v3`).
pub fn source_html_url(api_url: &str, source_repo: &str) -> String {
    let api = api_url.trim_end_matches('/');
    if let Ok(u) = url::Url::parse(api)
        && u.host_str() == Some("api.github.com")
    {
        return format!("https://github.com/{source_repo}");
    }
    format!("{}/{source_repo}", api.trim_end_matches("/api/v3"))
}

/// Private JSON (`/_bgh/metadata-imports/{id}`). Never includes the token.
pub fn to_json(
    state: &AppState,
    row: &ImportRow,
    owner: Option<&db::User>,
    repo: Option<&db::Repository>,
    git: Option<Value>,
) -> Value {
    let owner_login = owner.map(|o| o.login.clone());
    let repository = match (repo, owner) {
        (Some(r), Some(o)) => json!({
            "id": r.id,
            "name": r.name,
            "full_name": format!("{}/{}", o.login, r.name),
            "private": r.visibility != "public",
            "html_url": state.urls.html(&format!("/{}/{}", o.login, r.name)),
            "url": state.urls.api(&format!("/repos/{}/{}", o.login, r.name)),
        }),
        _ => Value::Null,
    };
    let options = row.options();
    let steps: Vec<Value> = STEPS
        .iter()
        .map(|s| {
            let idx = STEPS.iter().position(|x| x == s).unwrap_or(0);
            let cur = STEPS.iter().position(|x| *x == row.step).unwrap_or(0);
            let state = if !options.enabled(s) {
                "skipped"
            } else if row.status == "complete" || idx < cur {
                "done"
            } else if idx == cur && row.is_active() {
                "running"
            } else if idx == cur && row.status == "failed" {
                "failed"
            } else {
                "pending"
            };
            json!({"name": s, "state": state})
        })
        .collect();
    json!({
        "id": row.id,
        "kind": row.kind,
        "api_url": row.api_url,
        "source_repo": row.source_repo,
        "source_url": source_html_url(&row.api_url, &row.source_repo),
        "has_token": row.enc_token.is_some(),
        "owner": owner_login,
        "repo_name": row.repo_name,
        "visibility": row.visibility,
        "repository": repository,
        "options": {
            "git": options.git,
            "settings": options.settings,
            "labels": options.labels,
            "milestones": options.milestones,
            "issues": options.issues,
            "releases": options.releases,
            "teams": options.teams,
            "include_lfs": options.include_lfs,
            "user_map_entries": options.user_map.len(),
        },
        "status": row.status,
        "step": row.step,
        "steps": steps,
        "stats": row.stats,
        "git": git,
        "error": row.error,
        "attempts": row.attempts,
        "resume_at": row.resume_at.map(Timestamp::from),
        "created_at": Timestamp::from(row.created_at),
        "updated_at": Timestamp::from(row.updated_at),
        "completed_at": row.completed_at.map(Timestamp::from),
    })
}

/// Append a progress-log line.
pub async fn log(db: impl sqlx::PgExecutor<'_>, import_id: i64, level: &str, message: &str) {
    if let Err(e) =
        sqlx::query("INSERT INTO import_log (import_id, level, message) VALUES ($1, $2, $3)")
            .bind(import_id)
            .bind(level)
            .bind(message)
            .execute(db)
            .await
    {
        tracing::warn!(import_id, "writing import log: {e}");
    }
}

/// Increment a counter in `imports.stats` (inside the item's transaction,
/// so counts follow the mappings exactly).
pub async fn bump(
    conn: &mut sqlx::PgConnection,
    import_id: i64,
    key: &str,
    by: i64,
) -> ApiResult<()> {
    sqlx::query(
        "UPDATE imports SET stats = jsonb_set(stats, ARRAY[$2],
                to_jsonb(COALESCE((stats->>$2)::bigint, 0) + $3))
         WHERE id = $1",
    )
    .bind(import_id)
    .bind(key)
    .bind(by)
    .execute(conn)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_and_urls() {
        assert_eq!(source_host("https://api.github.com"), "github.com");
        assert_eq!(source_host("https://ghe.example/api/v3"), "ghe.example");
        assert_eq!(
            source_html_url("https://api.github.com/", "o/r"),
            "https://github.com/o/r"
        );
        assert_eq!(
            source_html_url("https://ghe.example/api/v3", "o/r"),
            "https://ghe.example/o/r"
        );
        let o = Options::default();
        assert!(o.git && o.issues && !o.teams && o.enabled("comments"));
    }
}
