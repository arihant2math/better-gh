//! Repository JSON rendering (REST and sync shapes).

use bgh_core::models::api::{Repository, RepositoryExtras};
use bgh_core::perms::RepoAccess;
use bgh_core::prelude::*;
use bgh_core::views;
use serde_json::{Value, json};

/// Full repository JSON (`GET /repos/{owner}/{repo}`, create responses).
pub async fn full_repo(
    state: &AppState,
    auth: Option<&AuthContext>,
    access: &RepoAccess,
) -> ApiResult<Repository> {
    let repo = &access.repo;
    let mut related = Vec::new();
    if let Some(id) = repo.parent_id {
        related.push(id);
    }
    if let Some(id) = repo.source_id.filter(|s| Some(*s) != repo.parent_id) {
        related.push(id);
    }
    if let Some(id) = repo.template_repository_id {
        related.push(id);
    }
    let mut parent = None;
    let mut source = None;
    let mut template_repository = None;
    let mut network_count = repo.forks_count;
    if !related.is_empty() {
        let rows: Vec<db::Repository> = sqlx::query_as(&format!(
            "SELECT {} FROM repositories WHERE id = ANY($1)",
            db::Repository::COLUMNS
        ))
        .bind(&related)
        .fetch_all(&state.db)
        .await?;
        if let Some(src) = rows.iter().find(|r| Some(r.id) == repo.source_id) {
            network_count = src.forks_count;
        }
        for m in views::minimal_repos(state, auth, rows).await? {
            if Some(m.id) == repo.parent_id {
                parent = Some(m.clone());
            }
            if Some(m.id) == repo.template_repository_id {
                template_repository = Some(m.clone());
            }
            if Some(m.id) == repo.source_id {
                source = Some(m);
            }
        }
        if source.is_none() && repo.fork {
            source = parent.clone();
        }
    }
    let mut full = Repository::new(
        &state.urls,
        repo,
        &access.owner,
        access.api_permission(),
        RepositoryExtras {
            subscribers_count: repo.watchers_count,
            network_count,
            parent,
            source,
            template_repository,
        },
    );
    if access.authenticated && access.permission >= Permission::Admin {
        full.security_and_analysis =
            Some(bgh_security::settings::security_and_analysis(state, repo.id).await?);
    }
    Ok(full)
}

/// Legacy compact shape. Sync payloads must come from
/// `bgh_core::sync::shapes` instead: `tx.sync_model(SyncModel::Repo, id, action)`.
pub fn repo_sync_json(repo: &db::Repository, owner_login: &str) -> Value {
    json!({
        "id": repo.id,
        "owner": owner_login,
        "owner_id": repo.owner_id,
        "name": repo.name,
        "description": repo.description,
        "visibility": repo.visibility,
        "fork": repo.fork,
        "archived": repo.archived,
        "default_branch": repo.default_branch,
        "has_issues": repo.has_issues,
        "stargazers_count": repo.stargazers_count,
        "forks_count": repo.forks_count,
        "open_issues_count": repo.open_issues_count,
        "pushed_at": repo.pushed_at.map(Timestamp::from),
        "updated_at": Timestamp::from(repo.updated_at),
    })
}
