//! `deployment` and `deployment_status` objects (shapes shared with the
//! REST API in `bgh_core::deployments`).

use bgh_core::AppState;
use bgh_core::deployments::{DeploymentRow, DeploymentStatusRow, deployment_json, status_json};
use serde_json::Value;

use super::common::{self, RepoCtx};

/// Webhook `deployment` object by id (`None` if deleted).
pub async fn deployment(
    state: &AppState,
    ctx: &RepoCtx,
    deployment_id: i64,
) -> anyhow::Result<Option<Value>> {
    let Some(d) = DeploymentRow::find(&state.db, ctx.repo.id, deployment_id).await? else {
        return Ok(None);
    };
    let users = common::users(state, [d.creator_id]).await?;
    Ok(Some(deployment_json(
        &state.urls,
        ctx.owner_login(),
        ctx.name(),
        &d,
        d.creator_id.and_then(|i| users.get(&i)),
    )))
}

/// Webhook `(deployment_status, deployment)` objects (`None` if deleted).
pub async fn deployment_status(
    state: &AppState,
    ctx: &RepoCtx,
    deployment_id: i64,
    status_id: i64,
) -> anyhow::Result<Option<(Value, Value)>> {
    let Some(d) = DeploymentRow::find(&state.db, ctx.repo.id, deployment_id).await? else {
        return Ok(None);
    };
    let Some(s) = DeploymentStatusRow::find(&state.db, d.id, status_id).await? else {
        return Ok(None);
    };
    let users = common::users(state, [d.creator_id, s.creator_id]).await?;
    let (o, r) = (ctx.owner_login(), ctx.name());
    Ok(Some((
        status_json(
            &state.urls,
            o,
            r,
            &s,
            s.creator_id.and_then(|i| users.get(&i)),
        ),
        deployment_json(
            &state.urls,
            o,
            r,
            &d,
            d.creator_id.and_then(|i| users.get(&i)),
        ),
    )))
}
