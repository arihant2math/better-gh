//! `POST /repos/{owner}/{repo}/releases/generate-notes`: GitHub-style
//! release notes from the pull requests merged since the previous release.

use std::collections::{HashMap, HashSet};

use axum::extract::State;
use bgh_core::prelude::*;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::git;

/// Commits considered when collecting merged pull requests.
const MAX_COMMITS: usize = 10_000;

#[derive(Debug, Default, Deserialize)]
pub struct NotesBody {
    pub tag_name: Option<String>,
    pub target_commitish: Option<String>,
    pub previous_tag_name: Option<String>,
    pub configuration_file_path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Notes {
    pub name: String,
    pub body: String,
}

#[derive(sqlx::FromRow)]
struct MergedPull {
    issue_id: i64,
    number: i64,
    title: String,
    author_id: Option<i64>,
    merged_at: Option<DateTime<Utc>>,
}

/// The tag of the release preceding `tag` (newest published release with a
/// different tag that still exists in git).
async fn previous_tag(state: &AppState, repo_id: i64, tag: &str) -> ApiResult<Option<String>> {
    let tags: Vec<String> = sqlx::query_scalar(
        "SELECT tag_name FROM releases
          WHERE repo_id = $1 AND NOT draft AND tag_name <> $2
            AND created_at <= coalesce(
                (SELECT max(created_at) FROM releases WHERE repo_id = $1 AND tag_name = $2 AND NOT draft),
                now())
          ORDER BY created_at DESC, id DESC LIMIT 20",
    )
    .bind(repo_id)
    .bind(tag)
    .fetch_all(&state.db)
    .await?;
    for t in tags {
        if git::tag_commit(state, repo_id, &t).await?.is_some() {
            return Ok(Some(t));
        }
    }
    Ok(None)
}

/// Generate notes for `tag` (its commit if it exists, else `target`).
pub async fn generate(
    state: &AppState,
    access: &RepoAccess,
    tag: &str,
    target: Option<&str>,
    previous: Option<&str>,
) -> ApiResult<Notes> {
    let repo = &access.repo;
    let head = match git::tag_commit(state, repo.id, tag).await? {
        Some(sha) => Some(sha),
        None => {
            let target = target
                .filter(|t| !t.is_empty())
                .unwrap_or(&repo.default_branch);
            git::resolve_commit(state, repo.id, target).await?
        }
    };
    let previous = match previous {
        Some(p) => {
            if git::tag_commit(state, repo.id, p).await?.is_none() {
                return Err(ApiError::invalid_field(FieldError::invalid(
                    "Release",
                    "previous_tag_name",
                )));
            }
            Some(p.to_string())
        }
        None => previous_tag(state, repo.id, tag).await?,
    };

    let commits: Vec<String> = match &head {
        Some(head) => {
            let head = head.clone();
            let exclude = previous.as_ref().map(|p| format!("refs/tags/{p}"));
            git::store(state)
                .read(repo.id, move |r| {
                    let ex: Vec<&str> = exclude.iter().map(String::as_str).collect();
                    r.rev_list(&head, &ex, MAX_COMMITS)
                })
                .await?
        }
        None => vec![],
    };

    let pulls: Vec<MergedPull> = if commits.is_empty() {
        vec![]
    } else {
        sqlx::query_as(
            "SELECT i.id AS issue_id, i.number, i.title, i.author_id, p.merged_at
               FROM pull_requests p JOIN issues i ON i.id = p.issue_id
              WHERE p.repo_id = $1 AND p.merged
                AND (p.merge_commit_sha = ANY($2) OR p.head_sha = ANY($2))
              ORDER BY p.merged_at, i.number",
        )
        .bind(repo.id)
        .bind(&commits)
        .fetch_all(&state.db)
        .await?
    };

    // First-time contributors: authors with no merged PR before this range.
    let authors: Vec<i64> = {
        let mut a: Vec<i64> = pulls.iter().filter_map(|p| p.author_id).collect();
        a.sort_unstable();
        a.dedup();
        a
    };
    let in_range: Vec<i64> = pulls.iter().map(|p| p.issue_id).collect();
    let returning: HashSet<i64> = if authors.is_empty() {
        HashSet::new()
    } else {
        sqlx::query_scalar::<_, i64>(
            "SELECT DISTINCT i.author_id FROM pull_requests p JOIN issues i ON i.id = p.issue_id
              WHERE p.repo_id = $1 AND p.merged AND i.author_id = ANY($2)
                AND NOT (i.id = ANY($3))
                AND p.merged_at < coalesce($4, now())",
        )
        .bind(repo.id)
        .bind(&authors)
        .bind(&in_range)
        .bind(pulls.iter().filter_map(|p| p.merged_at).max())
        .fetch_all(&state.db)
        .await?
        .into_iter()
        .collect()
    };
    let users = bgh_core::views::users_by_id(state, authors.iter().map(|a| Some(*a))).await?;
    let login = |id: Option<i64>| {
        id.and_then(|id| users.get(&id))
            .map(|u| u.login.clone())
            .unwrap_or_else(|| bgh_core::models::api::GHOST_LOGIN.to_string())
    };

    let owner = &access.owner.login;
    let name = &repo.name;
    let mut body = String::new();
    if !pulls.is_empty() {
        body.push_str("## What's Changed\n");
        for p in &pulls {
            body.push_str(&format!(
                "* {} by @{} in {}\n",
                p.title.trim(),
                login(p.author_id),
                state.urls.pull_html(owner, name, p.number)
            ));
        }
        let mut first: HashMap<i64, &MergedPull> = HashMap::new();
        for p in &pulls {
            if let Some(a) = p.author_id
                && !returning.contains(&a)
            {
                first.entry(a).or_insert(p);
            }
        }
        if !first.is_empty() {
            let mut list: Vec<&&MergedPull> = first.values().collect();
            list.sort_by_key(|p| (p.merged_at, p.number));
            body.push_str("\n## New Contributors\n");
            for p in list {
                body.push_str(&format!(
                    "* @{} made their first contribution in {}\n",
                    login(p.author_id),
                    state.urls.pull_html(owner, name, p.number)
                ));
            }
        }
        body.push('\n');
    }
    match &previous {
        Some(prev) => body.push_str(&format!(
            "**Full Changelog**: {}\n",
            state
                .urls
                .html(&format!("/{owner}/{name}/compare/{prev}...{tag}"))
        )),
        None => body.push_str(&format!(
            "**Full Changelog**: {}\n",
            state.urls.html(&format!("/{owner}/{name}/commits/{tag}"))
        )),
    }
    Ok(Notes {
        name: tag.to_string(),
        body,
    })
}

/// `POST /repos/{owner}/{repo}/releases/generate-notes`
pub async fn generate_notes(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo)): Path<(String, String)>,
    Json(body): Json<NotesBody>,
) -> ApiResult<Json<Notes>> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    access.require(Permission::Write)?;
    let tag = body
        .tag_name
        .filter(|t| !t.is_empty())
        .ok_or_else(|| ApiError::invalid_field(FieldError::missing_field("Release", "tag_name")))?;
    git::validate_tag_name(&tag)?;
    let _ = body.configuration_file_path;
    Ok(Json(
        generate(
            &state,
            &access,
            &tag,
            body.target_commitish.as_deref(),
            body.previous_tag_name.as_deref().filter(|p| !p.is_empty()),
        )
        .await?,
    ))
}
