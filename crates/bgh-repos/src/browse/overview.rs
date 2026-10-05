//! Code-tab helpers for the web client (package F2):
//!
//! * `GET /_bgh/repos/{o}/{r}/branch-list`: every branch with its last
//!   commit, ahead/behind counts against the default branch, protection and
//!   the newest pull request from it (branches page).
//! * `GET /_bgh/repos/{o}/{r}/files[/{ref}]`: all file paths of a commit
//!   (fuzzy file finder), cached by commit SHA.
//! * `GET /_bgh/repos/{o}/{r}/commit-status?sha=…&sha=…`: CI rollup per
//!   commit from commit statuses and check runs (commit lists).

use std::collections::{BTreeMap, HashMap};

use axum::extract::{RawQuery, State};
use axum::http::HeaderMap;
use axum::response::Response;
use bgh_core::prelude::*;
use futures::{StreamExt, stream};
use serde::Serialize;

use super::{
    CACHE_VERSION, CachedCommit, CommitSummary, Target, cache_get, cache_put, json_response,
    precheck, resolve, summarize,
};
use crate::protection::RepoRules;

/// Branches listed by `branch-list` (newest first beyond this are dropped).
const MAX_BRANCHES: usize = 1000;
/// Paths returned by `files` before `truncated` is set.
const MAX_FILES: usize = 100_000;
/// Commits per `commit-status` request.
const MAX_STATUS_SHAS: usize = 100;

// ----- branch-list -----------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct BranchPull {
    pub number: i64,
    pub state: String,
    pub merged: bool,
    pub draft: bool,
    pub title: String,
}

#[derive(Debug, Serialize)]
pub struct BranchOverview {
    pub name: String,
    pub commit: CommitSummary,
    pub ahead: u64,
    pub behind: u64,
    pub protected: bool,
    pub pull: Option<BranchPull>,
}

#[derive(Debug, Serialize)]
pub struct BranchList {
    pub default_branch: String,
    pub branches: Vec<BranchOverview>,
}

#[derive(sqlx::FromRow)]
struct PullRow {
    head_ref: String,
    number: i64,
    state: String,
    merged: bool,
    draft: bool,
    title: String,
}

pub async fn branch_list(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo)): Path<(String, String)>,
    req: HeaderMap,
) -> ApiResult<Response> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let repo_id = access.repo.id;
    let default_branch = access.repo.default_branch.clone();
    let store = crate::store(&state);
    let mut branches: Vec<(String, CachedCommit)> = store
        .read(repo_id, |r| {
            let mut out = Vec::new();
            for b in r.branches()? {
                let c = r.commit(&b.peeled)?;
                out.push((b.short_name().to_string(), CachedCommit::from(&c)));
            }
            Ok(out)
        })
        .await?;
    branches.sort_by(|a, b| {
        (b.0 == default_branch)
            .cmp(&(a.0 == default_branch))
            .then(b.1.committer_date.cmp(&a.1.committer_date))
    });
    branches.truncate(MAX_BRANCHES);

    // Ahead/behind vs the default branch: one `rev-list --count` per
    // branch, cached by the (default tip, branch tip) pair.
    let default_sha = branches
        .iter()
        .find(|(n, _)| *n == default_branch)
        .map(|(_, c)| c.sha.clone());
    let cli = store.cli(repo_id)?;
    let counts: HashMap<String, (u64, u64)> = match &default_sha {
        None => HashMap::new(),
        Some(base) => {
            let jobs: Vec<(String, String)> = branches
                .iter()
                .map(|(n, c)| (n.clone(), c.sha.clone()))
                .collect();
            let cli = std::sync::Arc::new(cli);
            stream::iter(jobs)
                .map(|(name, sha)| {
                    let (state, cli, base) = (state.clone(), cli.clone(), base.clone());
                    async move {
                        if sha == base {
                            return (name, (0, 0));
                        }
                        let key = format!("ab:{CACHE_VERSION}:{repo_id}:{base}:{sha}");
                        if let Some(v) = cache_get::<(u64, u64)>(&state, &key).await {
                            return (name, v);
                        }
                        let v = cli.ahead_behind(&base, &sha).await.unwrap_or((0, 0));
                        cache_put(&state, &key, &v).await;
                        (name, v)
                    }
                })
                .buffer_unordered(8)
                .collect()
                .await
        }
    };

    let names: Vec<String> = branches.iter().map(|(n, _)| n.clone()).collect();
    let pulls: Vec<PullRow> = sqlx::query_as(
        "SELECT DISTINCT ON (pr.head_ref) pr.head_ref, i.number, i.state, pr.merged, pr.draft, i.title
           FROM pull_requests pr JOIN issues i ON i.id = pr.issue_id
          WHERE pr.repo_id = $1 AND pr.head_repo_id = $1 AND pr.head_ref = ANY($2)
          ORDER BY pr.head_ref, (i.state = 'open') DESC, i.number DESC",
    )
    .bind(repo_id)
    .bind(&names)
    .fetch_all(&state.db)
    .await?;
    let mut pulls: HashMap<String, PullRow> =
        pulls.into_iter().map(|p| (p.head_ref.clone(), p)).collect();

    let rules = RepoRules::load(&state.db, &access.repo).await?;
    let commits: Vec<&CachedCommit> = branches.iter().map(|(_, c)| c).collect();
    let summaries = summarize(&state, &commits).await?;
    let body = BranchList {
        default_branch: default_branch.clone(),
        branches: branches
            .iter()
            .filter_map(|(name, c)| {
                let (ahead, behind) = counts.get(name).copied().unwrap_or((0, 0));
                let refname = format!("refs/heads/{name}");
                Some(BranchOverview {
                    protected: rules.protection_for(name).is_some()
                        || rules.rulesets_for(&refname).next().is_some(),
                    pull: pulls.remove(name).map(|p| BranchPull {
                        number: p.number,
                        state: p.state,
                        merged: p.merged,
                        draft: p.draft,
                        title: p.title,
                    }),
                    commit: summaries.get(&c.sha).cloned()?,
                    name: name.clone(),
                    ahead,
                    behind,
                })
            })
            .collect(),
    };
    let t = Target {
        access,
        refname: String::new(),
        commit: String::new(),
        path: String::new(),
    };
    json_response(&req, &t, "", &body)
}

// ----- files -------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct FileList {
    pub commit: String,
    pub paths: Vec<String>,
    pub truncated: bool,
}

pub async fn files_root(
    state: State<AppState>,
    auth: MaybeUser,
    Path((owner, repo)): Path<(String, String)>,
    req: HeaderMap,
) -> ApiResult<Response> {
    let t = resolve(&state, auth.as_ref(), &owner, &repo, None).await?;
    files_of(&state, t, req).await
}

pub async fn files(
    state: State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, spec)): Path<(String, String, String)>,
    req: HeaderMap,
) -> ApiResult<Response> {
    let t = resolve(&state, auth.as_ref(), &owner, &repo, Some(&spec)).await?;
    files_of(&state, t, req).await
}

async fn files_of(state: &AppState, t: Target, req: HeaderMap) -> ApiResult<Response> {
    let key = format!("files:{}", t.commit);
    if let Some(r) = precheck(&req, &t, &key) {
        return Ok(r);
    }
    let cache_key = format!("files:{CACHE_VERSION}:{}:{}", t.access.repo.id, t.commit);
    let (paths, truncated) = match cache_get::<(Vec<String>, bool)>(state, &cache_key).await {
        Some(v) => v,
        None => {
            let cli = t.store(state).cli(t.access.repo.id)?;
            let out = cli
                .run(
                    &["ls-tree", "-r", "-z", "--name-only", &t.commit],
                    &[],
                    None,
                )
                .await?;
            let mut paths: Vec<String> = out
                .split(|&b| b == 0)
                .filter(|p| !p.is_empty())
                .take(MAX_FILES + 1)
                .map(|p| String::from_utf8_lossy(p).into_owned())
                .collect();
            let truncated = paths.len() > MAX_FILES;
            paths.truncate(MAX_FILES);
            let v = (paths, truncated);
            cache_put(state, &cache_key, &v).await;
            v
        }
    };
    let body = FileList {
        commit: t.commit.clone(),
        paths,
        truncated,
    };
    json_response(&req, &t, &key, &body)
}

// ----- commit-status -------------------------------------------------------------

#[derive(Debug, Default, Serialize)]
pub struct Rollup {
    /// `success` | `failure` | `pending`
    pub state: &'static str,
    pub total: u32,
    pub success: u32,
    pub failure: u32,
    pub pending: u32,
}

#[derive(Debug, Serialize)]
pub struct Statuses {
    pub statuses: BTreeMap<String, Rollup>,
}

pub async fn commit_status(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo)): Path<(String, String)>,
    RawQuery(query): RawQuery,
    req: HeaderMap,
) -> ApiResult<Response> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let mut shas: Vec<String> = query
        .unwrap_or_default()
        .split('&')
        .filter_map(|kv| kv.strip_prefix("sha="))
        .flat_map(|v| {
            v.replace("%2C", ",")
                .replace("%2c", ",")
                .split(',')
                .map(str::to_ascii_lowercase)
                .collect::<Vec<_>>()
        })
        .filter(|s| bgh_git::is_sha(s))
        .collect();
    shas.sort();
    shas.dedup();
    shas.truncate(MAX_STATUS_SHAS);

    let statuses: Vec<(String, String)> = sqlx::query_as(
        "SELECT DISTINCT ON (sha, context) sha, state
           FROM commit_statuses WHERE repo_id = $1 AND sha = ANY($2)
          ORDER BY sha, context, id DESC",
    )
    .bind(access.repo.id)
    .bind(&shas)
    .fetch_all(&state.db)
    .await?;
    let runs: Vec<(String, String, Option<String>)> = sqlx::query_as(
        "SELECT DISTINCT ON (head_sha, name) head_sha, status, conclusion
           FROM check_runs WHERE repo_id = $1 AND head_sha = ANY($2)
          ORDER BY head_sha, name, id DESC",
    )
    .bind(access.repo.id)
    .bind(&shas)
    .fetch_all(&state.db)
    .await?;

    let mut out: BTreeMap<String, Rollup> = BTreeMap::new();
    let mut add = |sha: String, outcome: Outcome| {
        let r = out.entry(sha).or_default();
        r.total += 1;
        match outcome {
            Outcome::Success => r.success += 1,
            Outcome::Failure => r.failure += 1,
            Outcome::Pending => r.pending += 1,
        }
    };
    for (sha, s) in statuses {
        add(
            sha,
            match s.as_str() {
                "success" => Outcome::Success,
                "pending" => Outcome::Pending,
                _ => Outcome::Failure,
            },
        );
    }
    for (sha, status, conclusion) in runs {
        add(
            sha,
            match (status.as_str(), conclusion.as_deref()) {
                ("completed", Some("success" | "neutral" | "skipped")) => Outcome::Success,
                ("completed", _) => Outcome::Failure,
                _ => Outcome::Pending,
            },
        );
    }
    for r in out.values_mut() {
        r.state = if r.failure > 0 {
            "failure"
        } else if r.pending > 0 {
            "pending"
        } else {
            "success"
        };
    }
    let t = Target {
        access,
        refname: String::new(),
        commit: String::new(),
        path: String::new(),
    };
    json_response(&req, &t, "", &Statuses { statuses: out })
}

enum Outcome {
    Success,
    Failure,
    Pending,
}
