//! Core rows referenced by project snapshots (users, repositories, issues,
//! labels, milestones), built by the shared sync shape loaders
//! (`bgh_core::sync::shapes`) so they are identical to bootstrap rows.

use std::collections::HashSet;

use bgh_core::perms::{self, Permission};
use bgh_core::prelude::*;
use bgh_core::sync::shapes::{self, Filter, Model, Opts};
use serde_json::Value;

async fn load(state: &AppState, model: Model, filter: Filter<'_>) -> ApiResult<Vec<Value>> {
    let mut conn = state.db.acquire().await?;
    Ok(shapes::load(&mut conn, model, filter, Opts::default())
        .await?
        .into_iter()
        .map(|r| r.data)
        .collect())
}

/// Users by id as compact rows (organizations are skipped).
pub async fn users_json(
    state: &AppState,
    ids: impl IntoIterator<Item = Option<i64>>,
) -> ApiResult<Vec<Value>> {
    let mut ids: Vec<i64> = ids.into_iter().flatten().collect();
    ids.sort_unstable();
    ids.dedup();
    if ids.is_empty() {
        return Ok(vec![]);
    }
    let orgs: Vec<i64> =
        sqlx::query_scalar("SELECT id FROM users WHERE id = ANY($1) AND type = 'Organization'")
            .bind(&ids)
            .fetch_all(&state.db)
            .await?;
    ids.retain(|id| !orgs.contains(id));
    load(state, Model::User, Filter::Ids(&ids)).await
}

/// Core rows referenced by project items, restricted to repositories the
/// caller can read.
#[derive(Debug, Default)]
pub struct Refs {
    pub issues: Vec<Value>,
    pub repos: Vec<Value>,
    pub labels: Vec<Value>,
    pub milestones: Vec<Value>,
    pub user_ids: Vec<i64>,
}

/// Repositories readable by `auth` among `repo_ids`.
pub async fn readable_repos(
    state: &AppState,
    auth: Option<&AuthContext>,
    repo_ids: &[i64],
) -> ApiResult<Vec<db::Repository>> {
    if repo_ids.is_empty() {
        return Ok(vec![]);
    }
    let repos: Vec<db::Repository> = sqlx::query_as(&format!(
        "SELECT {} FROM repositories WHERE id = ANY($1) ORDER BY id",
        db::Repository::COLUMNS
    ))
    .bind(repo_ids)
    .fetch_all(&state.db)
    .await?;
    let raw = perms::repo_permissions(&state.db, auth.map(|a| a.user.id), &repos).await?;
    Ok(repos
        .into_iter()
        .filter(|r| {
            let p = raw.get(&r.id).copied().unwrap_or(Permission::None);
            perms::effective(auth, r, p) >= Permission::Read
        })
        .collect())
}

pub async fn load_refs(
    state: &AppState,
    auth: Option<&AuthContext>,
    issue_ids: &[i64],
) -> ApiResult<Refs> {
    if issue_ids.is_empty() {
        return Ok(Refs::default());
    }
    let issue_repos: Vec<(i64, i64)> =
        sqlx::query_as("SELECT id, repo_id FROM issues WHERE id = ANY($1)")
            .bind(issue_ids)
            .fetch_all(&state.db)
            .await?;
    let mut repo_ids: Vec<i64> = issue_repos.iter().map(|(_, r)| *r).collect();
    repo_ids.sort_unstable();
    repo_ids.dedup();
    let readable: HashSet<i64> = readable_repos(state, auth, &repo_ids)
        .await?
        .iter()
        .map(|r| r.id)
        .collect();
    let repo_ids: Vec<i64> = repo_ids
        .into_iter()
        .filter(|r| readable.contains(r))
        .collect();
    let visible: Vec<i64> = issue_repos
        .iter()
        .filter(|(_, r)| readable.contains(r))
        .map(|(i, _)| *i)
        .collect();

    let issues = load(state, Model::Issue, Filter::Ids(&visible)).await?;
    let mut users = std::collections::BTreeSet::new();
    for i in &issues {
        shapes::referenced_users(Model::Issue.name(), i, &mut users);
    }
    Ok(Refs {
        issues,
        repos: load(state, Model::Repo, Filter::Ids(&repo_ids)).await?,
        labels: load(state, Model::Label, Filter::Repos(&repo_ids)).await?,
        milestones: load(state, Model::Milestone, Filter::Repos(&repo_ids)).await?,
        user_ids: users.into_iter().collect(),
    })
}
