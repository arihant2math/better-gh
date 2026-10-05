//! Commit status, check run and check suite payloads.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use bgh_core::AppState;
use bgh_core::node_id::{self, NodeType};
use bgh_core::time::{Timestamp, ts};
use bgh_core::urls::Urls;
use bgh_git::{Commit, RepoStore};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use super::common::{self, RepoCtx, commit_node_id, envelope, glob_match};
use super::push::timestamp_with_offset;

/// App id GitHub uses for GitHub Actions.
pub const ACTIONS_APP_ID: i64 = 15368;

/// `node_id` of a GitHub App (`"03:App{id}"`).
fn app_node_id(id: i64) -> String {
    STANDARD.encode(format!("03:App{id}"))
}

/// The `app` object for a check suite's integration. `actions` (built-in
/// CI) renders as GitHub Actions.
pub fn app_json(urls: &Urls, ctx: &RepoCtx, slug: &str) -> Value {
    let (id, slug, name, description) = if slug == "actions" {
        (
            ACTIONS_APP_ID,
            "github-actions",
            "GitHub Actions",
            Some("Automate your workflow from idea to production"),
        )
    } else {
        (0, slug, slug, None)
    };
    json!({
        "id": id,
        "slug": slug,
        "node_id": app_node_id(id),
        "owner": common::user_json(urls, &ctx.owner),
        "name": name,
        "description": description,
        "external_url": urls.html("/features/actions"),
        "html_url": urls.html(&format!("/apps/{slug}")),
        "created_at": "2018-07-30T09:30:17Z",
        "updated_at": "2019-12-10T19:04:12Z",
        "permissions": {},
        "events": [],
    })
}

/// Open pull requests whose head is `sha`, minimal shape (one query).
pub async fn pull_requests_for_sha(
    state: &AppState,
    ctx: &RepoCtx,
    sha: &str,
) -> anyhow::Result<Vec<Value>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        id: i64,
        number: i64,
        head_ref: String,
        head_sha: String,
        base_ref: String,
        base_sha: String,
        head_repo_id: Option<i64>,
        head_repo_name: Option<String>,
        head_owner: Option<String>,
    }
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT i.id, i.number, p.head_ref, p.head_sha, p.base_ref, p.base_sha,
                hr.id AS head_repo_id, hr.name AS head_repo_name, hu.login AS head_owner
         FROM pull_requests p
         JOIN issues i ON i.id = p.issue_id
         LEFT JOIN repositories hr ON hr.id = p.head_repo_id
         LEFT JOIN users hu ON hu.id = hr.owner_id
         WHERE p.repo_id = $1 AND p.head_sha = $2 AND i.state = 'open'
         ORDER BY i.number",
    )
    .bind(ctx.repo.id)
    .bind(sha)
    .fetch_all(&state.db)
    .await?;
    let urls = &state.urls;
    let base_repo = json!({
        "id": ctx.repo.id,
        "url": ctx.api_url(urls),
        "name": ctx.name(),
    });
    Ok(rows
        .into_iter()
        .map(|p| {
            let head_repo = match (p.head_repo_id, &p.head_repo_name, &p.head_owner) {
                (Some(id), Some(name), Some(owner)) => json!({
                    "id": id,
                    "url": urls.repo(owner, name),
                    "name": name,
                }),
                _ => Value::Null,
            };
            json!({
                "url": urls.pull(ctx.owner_login(), ctx.name(), p.number),
                "id": p.id,
                "number": p.number,
                "head": { "ref": p.head_ref, "sha": p.head_sha, "repo": head_repo },
                "base": { "ref": p.base_ref, "sha": p.base_sha, "repo": base_repo },
            })
        })
        .collect())
}

/// Read one commit (None if unreadable).
async fn read_commit(state: &AppState, repo_id: i64, sha: &str) -> Option<Commit> {
    if !bgh_git::is_sha(sha) {
        return None;
    }
    let sha = sha.to_string();
    RepoStore::from_config(&state.config)
        .read(repo_id, move |r| r.commit(&sha))
        .await
        .ok()
}

/// `head_commit` of a check suite (simple-commit).
fn simple_commit(c: &Commit) -> Value {
    json!({
        "id": c.sha,
        "tree_id": c.tree,
        "message": c.message.trim_end_matches('\n'),
        "timestamp": timestamp_with_offset(c.committer.when, c.committer.offset_minutes),
        "author": { "name": c.author.name, "email": c.author.email },
        "committer": { "name": c.committer.name, "email": c.committer.email },
    })
}

// ---------------------------------------------------------------------------
// Commit statuses
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow)]
struct StatusRow {
    id: i64,
    sha: String,
    state: String,
    context: String,
    description: Option<String>,
    target_url: Option<String>,
    avatar_url: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

/// The `status` payload (no `action`). `None` if the status is gone.
pub async fn status_payload(
    state: &AppState,
    ctx: &RepoCtx,
    status_id: i64,
    sender: Value,
) -> anyhow::Result<Option<Value>> {
    let row: Option<StatusRow> = sqlx::query_as(
        "SELECT id, sha, state, context, description, target_url, avatar_url, created_at, updated_at
         FROM commit_statuses WHERE id = $1 AND repo_id = $2",
    )
    .bind(status_id)
    .bind(ctx.repo.id)
    .fetch_optional(&state.db)
    .await?;
    let Some(s) = row else { return Ok(None) };
    let urls = &state.urls;
    let repo_api = ctx.api_url(urls);

    // Commit object + branches whose head is the SHA, in one blocking read.
    let sha = s.sha.clone();
    let read = if bgh_git::is_sha(&sha) {
        RepoStore::from_config(&state.config)
            .read(ctx.repo.id, move |r| {
                let commit = r.commit(&sha).ok();
                let branches: Vec<String> = r
                    .branches()?
                    .into_iter()
                    .filter(|b| b.peeled == sha)
                    .map(|b| b.short_name().to_string())
                    .collect();
                Ok((commit, branches))
            })
            .await
            .unwrap_or((None, Vec::new()))
    } else {
        (None, Vec::new())
    };
    let (commit, branch_names) = read;
    let patterns: Vec<String> = if branch_names.is_empty() {
        Vec::new()
    } else {
        sqlx::query_scalar("SELECT pattern FROM branch_protections WHERE repo_id = $1")
            .bind(ctx.repo.id)
            .fetch_all(&state.db)
            .await?
    };
    let branches: Vec<Value> = branch_names
        .iter()
        .map(|b| {
            json!({
                "name": b,
                "commit": { "sha": s.sha, "url": urls.commit(ctx.owner_login(), ctx.name(), &s.sha) },
                "protected": patterns.iter().any(|p| glob_match(p, b)),
            })
        })
        .collect();

    let commit_api = urls.commit(ctx.owner_login(), ctx.name(), &s.sha);
    let commit_json = match &commit {
        Some(c) => {
            let by_email =
                common::users_by_email(state, &[c.author.email.clone(), c.committer.email.clone()])
                    .await?;
            let gh_user = |email: &str| {
                by_email
                    .get(&email.to_lowercase())
                    .map(|u| common::user_json(urls, u))
                    .unwrap_or(Value::Null)
            };
            json!({
                "sha": c.sha,
                "node_id": commit_node_id(ctx.repo.id, &c.sha),
                "commit": {
                    "author": {
                        "name": c.author.name,
                        "email": c.author.email,
                        "date": Timestamp(c.author.when),
                    },
                    "committer": {
                        "name": c.committer.name,
                        "email": c.committer.email,
                        "date": Timestamp(c.committer.when),
                    },
                    "message": c.message.trim_end_matches('\n'),
                    "tree": { "sha": c.tree, "url": format!("{repo_api}/git/trees/{}", c.tree) },
                    "url": format!("{repo_api}/git/commits/{}", c.sha),
                    "comment_count": 0,
                    "verification": {
                        "verified": false,
                        "reason": "unsigned",
                        "signature": null,
                        "payload": null,
                        "verified_at": null,
                    },
                },
                "url": commit_api,
                "html_url": urls.commit_html(ctx.owner_login(), ctx.name(), &c.sha),
                "comments_url": format!("{commit_api}/comments"),
                "author": gh_user(&c.author.email),
                "committer": gh_user(&c.committer.email),
                "parents": c.parents.iter().map(|p| json!({
                    "sha": p,
                    "url": urls.commit(ctx.owner_login(), ctx.name(), p),
                    "html_url": urls.commit_html(ctx.owner_login(), ctx.name(), p),
                })).collect::<Vec<_>>(),
            })
        }
        None => json!({
            "sha": s.sha,
            "node_id": commit_node_id(ctx.repo.id, &s.sha),
            "commit": null,
            "url": commit_api,
            "html_url": urls.commit_html(ctx.owner_login(), ctx.name(), &s.sha),
            "comments_url": format!("{commit_api}/comments"),
            "author": null,
            "committer": null,
            "parents": [],
        }),
    };

    Ok(Some(envelope(
        urls,
        ctx,
        None,
        vec![
            ("id", json!(s.id)),
            ("sha", json!(s.sha)),
            ("name", json!(ctx.full_name())),
            ("target_url", json!(s.target_url)),
            ("avatar_url", json!(s.avatar_url)),
            ("context", json!(s.context)),
            ("description", json!(s.description)),
            ("state", json!(s.state)),
            ("commit", commit_json),
            ("branches", Value::Array(branches)),
            ("created_at", json!(Timestamp(s.created_at))),
            ("updated_at", json!(Timestamp(s.updated_at))),
        ],
        sender,
    )))
}

// ---------------------------------------------------------------------------
// Check suites and runs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow)]
struct SuiteRow {
    id: i64,
    head_sha: String,
    head_branch: Option<String>,
    before_sha: Option<String>,
    after_sha: Option<String>,
    app_slug: String,
    status: String,
    conclusion: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

async fn suite_row(state: &AppState, ctx: &RepoCtx, id: i64) -> anyhow::Result<Option<SuiteRow>> {
    Ok(sqlx::query_as(
        "SELECT id, head_sha, head_branch, before_sha, after_sha, app_slug, status, conclusion,
                created_at, updated_at
         FROM check_suites WHERE id = $1 AND repo_id = $2",
    )
    .bind(id)
    .bind(ctx.repo.id)
    .fetch_optional(&state.db)
    .await?)
}

/// Fields shared by the full check suite and the one embedded in check runs.
fn suite_base(
    urls: &Urls,
    ctx: &RepoCtx,
    s: &SuiteRow,
    prs: &[Value],
    app: &Value,
) -> serde_json::Map<String, Value> {
    let v = json!({
        "id": s.id,
        "node_id": node_id::encode(NodeType::CheckSuite, s.id),
        "head_branch": s.head_branch,
        "head_sha": s.head_sha,
        "status": s.status,
        "conclusion": s.conclusion,
        "url": format!("{}/check-suites/{}", ctx.api_url(urls), s.id),
        "before": s.before_sha,
        "after": s.after_sha,
        "pull_requests": prs,
        "app": app,
        "created_at": Timestamp(s.created_at),
        "updated_at": Timestamp(s.updated_at),
    });
    match v {
        Value::Object(m) => m,
        _ => serde_json::Map::new(),
    }
}

/// Webhook `check_suite` object by id.
pub async fn check_suite(
    state: &AppState,
    ctx: &RepoCtx,
    suite_id: i64,
) -> anyhow::Result<Option<Value>> {
    let Some(s) = suite_row(state, ctx, suite_id).await? else {
        return Ok(None);
    };
    let urls = &state.urls;
    let prs = pull_requests_for_sha(state, ctx, &s.head_sha).await?;
    let app = app_json(urls, ctx, &s.app_slug);
    let runs: i64 = sqlx::query_scalar("SELECT count(*) FROM check_runs WHERE check_suite_id = $1")
        .bind(s.id)
        .fetch_one(&state.db)
        .await?;
    let head_commit = read_commit(state, ctx.repo.id, &s.head_sha)
        .await
        .map(|c| simple_commit(&c))
        .unwrap_or(Value::Null);
    let mut m = suite_base(urls, ctx, &s, &prs, &app);
    m.insert("rerequestable".into(), json!(true));
    m.insert("runs_rerequestable".into(), json!(true));
    m.insert("latest_check_runs_count".into(), json!(runs));
    m.insert(
        "check_runs_url".into(),
        json!(format!(
            "{}/check-suites/{}/check-runs",
            ctx.api_url(urls),
            s.id
        )),
    );
    m.insert("head_commit".into(), head_commit);
    Ok(Some(Value::Object(m)))
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct RunRow {
    id: i64,
    check_suite_id: i64,
    head_sha: String,
    name: String,
    status: String,
    conclusion: Option<String>,
    external_id: Option<String>,
    details_url: Option<String>,
    output: Value,
    started_at: Option<DateTime<Utc>>,
    completed_at: Option<DateTime<Utc>>,
}

/// Webhook `check_run` object by id.
pub async fn check_run(
    state: &AppState,
    ctx: &RepoCtx,
    run_id: i64,
) -> anyhow::Result<Option<Value>> {
    let row: Option<RunRow> = sqlx::query_as(
        "SELECT id, check_suite_id, head_sha, name, status, conclusion, external_id, details_url,
                output, started_at, completed_at
         FROM check_runs WHERE id = $1 AND repo_id = $2",
    )
    .bind(run_id)
    .bind(ctx.repo.id)
    .fetch_optional(&state.db)
    .await?;
    let Some(run) = row else { return Ok(None) };
    let Some(suite) = suite_row(state, ctx, run.check_suite_id).await? else {
        return Ok(None);
    };
    let urls = &state.urls;
    let repo_api = ctx.api_url(urls);
    let prs = pull_requests_for_sha(state, ctx, &run.head_sha).await?;
    let app = app_json(urls, ctx, &suite.app_slug);
    let out = |k: &str| run.output.get(k).cloned().unwrap_or(Value::Null);
    let annotations_url = format!("{repo_api}/check-runs/{}/annotations", run.id);
    Ok(Some(json!({
        "id": run.id,
        "name": run.name,
        "node_id": node_id::encode(NodeType::CheckRun, run.id),
        "head_sha": run.head_sha,
        "external_id": run.external_id,
        "url": format!("{repo_api}/check-runs/{}", run.id),
        "html_url": format!("{}/runs/{}", ctx.html_url(urls), run.id),
        "details_url": run.details_url,
        "status": run.status,
        "conclusion": run.conclusion,
        "started_at": ts(run.started_at),
        "completed_at": ts(run.completed_at),
        "output": {
            "title": out("title"),
            "summary": out("summary"),
            "text": out("text"),
            "annotations_count": run.output.get("annotations_count").cloned().unwrap_or(json!(0)),
            "annotations_url": annotations_url,
        },
        "check_suite": suite_base(urls, ctx, &suite, &prs, &app),
        "app": app,
        "pull_requests": prs,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actions_app_node_id() {
        // GitHub's real node id for the GitHub Actions app.
        assert_eq!(app_node_id(ACTIONS_APP_ID), "MDM6QXBwMTUzNjg=");
    }
}
