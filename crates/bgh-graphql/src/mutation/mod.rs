//! The `Mutation` root.
//!
//! Every mutation calls the owning domain crate's handler (the same code
//! path as the REST endpoint), so validation, permissions, sync records,
//! events and jobs stay in one place. GraphQL node ids are translated to
//! the REST path parameters / bodies, and the result is re-read for the
//! payload.

mod issues;
mod pulls;
mod repos;

use std::sync::Arc;

use async_graphql::{Context, ID, MergedObject};
use axum::body::to_bytes;
use axum::response::IntoResponse;
use bgh_core::auth::AuthContext;
use bgh_core::node_id::{self, NodeType};
use bgh_core::prelude::*;
use serde_json::Value;

use crate::ctx::{GResult, api_err, err, gql, not_found};
use crate::loaders::{Loaders, RepoRow, one};
use crate::model::Repository;

/// Marker inserted for GET requests: mutations are refused.
pub struct ReadOnly;

/// The caller of a mutation (authenticated, not over GET).
pub fn guard<'a>(ctx: &Context<'a>) -> GResult<&'a AuthContext> {
    if ctx.data_opt::<ReadOnly>().is_some() {
        return Err(err("FORBIDDEN", "Mutations are not allowed over GET."));
    }
    gql(ctx).require_auth()
}

#[derive(MergedObject, Default)]
pub struct Mutation(
    issues::IssueMutations,
    pulls::PullMutations,
    repos::RepoMutations,
);

// ---------------------------------------------------------------------------
// Helpers shared by the mutation modules
// ---------------------------------------------------------------------------

/// Turn a handler result into JSON (any `IntoResponse` with a JSON body).
pub async fn into_json<T: IntoResponse>(r: ApiResult<T>) -> GResult<Value> {
    let resp = r.map_err(api_err)?.into_response();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), 64 * 1024 * 1024)
        .await
        .map_err(|e| api_err(ApiError::internal(anyhow::anyhow!("reading body: {e}"))))?;
    if !status.is_success() {
        let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        let msg = v["message"]
            .as_str()
            .unwrap_or("request failed")
            .to_string();
        return Err(err("UNPROCESSABLE", msg));
    }
    if bytes.is_empty() {
        return Ok(Value::Null);
    }
    Ok(serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

/// Deserialize a domain request body from JSON.
pub fn body<T: serde::de::DeserializeOwned>(v: Value) -> GResult<T> {
    serde_json::from_value(v).map_err(|e| err("UNPROCESSABLE", e.to_string()))
}

pub fn decode(id: &ID, expected: &[NodeType], what: &str) -> GResult<i64> {
    match node_id::decode(&id.0) {
        Some((ty, n)) if expected.contains(&ty) => Ok(n),
        _ => Err(not_found(format!(
            "Could not resolve to {what} with the global id of '{}'.",
            id.0
        ))),
    }
}

/// Load a repository the viewer can read.
pub async fn repo_by_id(ctx: &Context<'_>, id: i64) -> GResult<Arc<RepoRow>> {
    let l = ctx.data_unchecked::<Loaders>();
    match one(&l.repos, id).await? {
        Some(r) if r.readable() => Ok(r),
        _ => Err(not_found(
            "Could not resolve to a Repository with the given id.",
        )),
    }
}

pub async fn repo_by_node(ctx: &Context<'_>, id: &ID) -> GResult<Arc<RepoRow>> {
    let rid = decode(id, &[NodeType::Repository], "a Repository")?;
    repo_by_id(ctx, rid).await
}

/// An issue or PR (by node id) plus its readable repository.
pub async fn issue_by_node(ctx: &Context<'_>, id: &ID) -> GResult<(db::Issue, Arc<RepoRow>)> {
    let iid = decode(id, &[NodeType::Issue, NodeType::PullRequest], "an Issue")?;
    let l = ctx.data_unchecked::<Loaders>();
    let issue = one(&l.issues, iid).await?.ok_or_else(|| {
        not_found(format!(
            "Could not resolve to a node with the global id of '{}'.",
            id.0
        ))
    })?;
    let repo = repo_by_id(ctx, issue.repo_id).await?;
    Ok(((*issue).clone(), repo))
}

pub fn owner_repo(r: &RepoRow) -> (String, String) {
    (r.owner.login.clone(), r.repo.name.clone())
}

/// Fresh issue row by id (after a write).
pub async fn reload_issue(ctx: &Context<'_>, id: i64) -> GResult<db::Issue> {
    sqlx::query_as::<_, db::Issue>(&format!(
        "SELECT {} FROM issues WHERE id = $1",
        db::Issue::COLUMNS
    ))
    .bind(id)
    .fetch_optional(&gql(ctx).state.db)
    .await
    .map_err(|e| api_err(e.into()))?
    .ok_or_else(|| not_found("issue not found"))
}

/// Fresh repository object (after a write).
pub async fn reload_repo(ctx: &Context<'_>, id: i64) -> GResult<Repository> {
    let g = gql(ctx);
    let repo = db::Repository::find(&g.state.db, id)
        .await
        .map_err(|e| api_err(e.into()))?
        .ok_or_else(|| not_found("repository not found"))?;
    let rows = crate::loaders::RepoLoader::rows(&g.state, g.auth.as_ref(), vec![repo])
        .await
        .map_err(api_err)?;
    rows.into_iter()
        .next()
        .map(|r| Repository(Arc::new(r)))
        .ok_or_else(|| not_found("repository not found"))
}

/// Logins for user node ids (unknown ids are an error, like GitHub).
pub async fn logins(ctx: &Context<'_>, ids: &[ID]) -> GResult<Vec<String>> {
    let mut out = Vec::with_capacity(ids.len());
    let l = ctx.data_unchecked::<Loaders>();
    for id in ids {
        let uid = decode(id, &[NodeType::User, NodeType::Bot], "a User")?;
        let u = one(&l.users, uid).await?.ok_or_else(|| {
            not_found(format!(
                "Could not resolve to a User with the global id of '{}'.",
                id.0
            ))
        })?;
        out.push(u.login.clone());
    }
    Ok(out)
}

/// Label names for label node ids within a repository.
pub async fn label_names(ctx: &Context<'_>, repo_id: i64, ids: &[ID]) -> GResult<Vec<String>> {
    let mut nums = Vec::with_capacity(ids.len());
    for id in ids {
        nums.push(decode(id, &[NodeType::Label], "a Label")?);
    }
    let rows: Vec<(i64, String)> =
        sqlx::query_as("SELECT id, name FROM labels WHERE id = ANY($1) AND repo_id = $2")
            .bind(&nums)
            .bind(repo_id)
            .fetch_all(&gql(ctx).state.db)
            .await
            .map_err(|e| api_err(e.into()))?;
    nums.iter()
        .map(|n| {
            rows.iter()
                .find(|r| r.0 == *n)
                .map(|r| r.1.clone())
                .ok_or_else(|| not_found("Could not resolve to a Label with the given id."))
        })
        .collect()
}

/// Milestone number for a milestone node id within a repository.
pub async fn milestone_number(ctx: &Context<'_>, repo_id: i64, id: &ID) -> GResult<i64> {
    let mid = decode(id, &[NodeType::Milestone], "a Milestone")?;
    let n: Option<i64> =
        sqlx::query_scalar("SELECT number FROM milestones WHERE id = $1 AND repo_id = $2")
            .bind(mid)
            .bind(repo_id)
            .fetch_optional(&gql(ctx).state.db)
            .await
            .map_err(|e| api_err(e.into()))?;
    n.ok_or_else(|| not_found("Could not resolve to a Milestone with the given id."))
}
