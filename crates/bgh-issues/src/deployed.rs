//! `deployed` timeline events: when a deployment gets a `success` status,
//! every pull request of the repository whose head is the deployed commit
//! gets one `deployed` event per deployment (actor: the deployment's
//! creator, `commit_id`: the deployed SHA).

use std::sync::Arc;

use bgh_core::deployments::DeploymentRow;
use bgh_core::prelude::*;
use serde_json::json;

use crate::service;

pub async fn on_event(state: AppState, event: Arc<Event>) -> anyhow::Result<()> {
    let Event::DeploymentStatusCreated {
        repo_id,
        deployment_id,
        state: st,
        ..
    } = &*event
    else {
        return Ok(());
    };
    if st != "success" {
        return Ok(());
    }
    record(&state, *repo_id, *deployment_id)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
}

async fn record(state: &AppState, repo_id: i64, deployment_id: i64) -> ApiResult<()> {
    let Some(d) = DeploymentRow::find(&state.db, repo_id, deployment_id).await? else {
        return Ok(());
    };
    let mut tx = Tx::begin(state).await?;
    // Pull requests of this repository at the deployed commit that don't
    // have an event for this deployment yet.
    let pulls: Vec<db::Issue> = sqlx::query_as(&format!(
        "SELECT {} FROM issues i
          WHERE i.id IN (SELECT p.issue_id FROM pull_requests p
                          WHERE p.repo_id = $1 AND p.head_sha = $2)
            AND NOT EXISTS (SELECT 1 FROM issue_events e
                             WHERE e.issue_id = i.id AND e.event = 'deployed'
                               AND e.data->>'deployment_id' = $3::text)
          ORDER BY i.id",
        db::prefixed("i", db::Issue::COLUMNS)
    ))
    .bind(repo_id)
    .bind(&d.sha)
    .bind(d.id)
    .fetch_all(&mut *tx)
    .await?;
    if pulls.is_empty() || !bgh_core::events::claim_effect(&mut tx).await? {
        return Ok(());
    }
    for pr in &pulls {
        service::add_event(
            &mut tx,
            pr,
            d.creator_id,
            "deployed",
            Some(&d.sha),
            json!({ "deployment_id": d.id, "environment": d.environment }),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(())
}
