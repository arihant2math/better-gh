//! Batch loaders that turn DB rows into API models without N+1 queries.
//!
//! Use these whenever rendering lists (search results, event payloads,
//! `GET /user/repos`, ...).

use std::collections::HashMap;

use crate::auth::AuthContext;
use crate::error::ApiResult;
use crate::models::api::MinimalRepository;
use crate::models::db;
use crate::perms::{self, Permission};
use crate::state::AppState;

/// Load users by id into a map (one query; duplicates and `None`s ignored).
pub async fn users_by_id(
    state: &AppState,
    ids: impl IntoIterator<Item = Option<i64>>,
) -> ApiResult<HashMap<i64, db::User>> {
    let mut ids: Vec<i64> = ids.into_iter().flatten().collect();
    ids.sort_unstable();
    ids.dedup();
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    Ok(db::User::find_many(&state.db, &ids)
        .await?
        .into_iter()
        .map(|u| (u.id, u))
        .collect())
}

/// Render repositories for `auth`, in input order, dropping those the
/// caller can't read. Includes `permissions` for authenticated callers.
pub async fn minimal_repos(
    state: &AppState,
    auth: Option<&AuthContext>,
    repos: Vec<db::Repository>,
) -> ApiResult<Vec<MinimalRepository>> {
    if repos.is_empty() {
        return Ok(vec![]);
    }
    let owners = users_by_id(state, repos.iter().map(|r| Some(r.owner_id))).await?;
    let raw = perms::repo_permissions(&state.db, auth.map(|a| a.user.id), &repos).await?;
    let mut out = Vec::with_capacity(repos.len());
    for repo in &repos {
        let p = perms::effective(
            auth,
            repo,
            raw.get(&repo.id).copied().unwrap_or(Permission::None),
        );
        if p < Permission::Read {
            continue;
        }
        let Some(owner) = owners.get(&repo.owner_id) else {
            continue;
        };
        out.push(MinimalRepository::new(
            &state.urls,
            repo,
            owner,
            auth.is_some().then_some(p),
        ));
    }
    Ok(out)
}
