//! `secret_scanning_alert` / `secret_scanning_alert_location` objects
//! (shape shared with the REST API in `bgh_core::secret_scanning`).

use bgh_core::AppState;
use bgh_core::secret_scanning::{self as ss, LocationRow};
use serde_json::Value;

use super::common::RepoCtx;

/// Webhook `alert` object by id (`None` if deleted). Like GitHub's
/// `secret-scanning-alert-webhook`, it never carries the secret itself.
pub async fn secret_scanning_alert(
    state: &AppState,
    ctx: &RepoCtx,
    alert_id: i64,
) -> anyhow::Result<Option<Value>> {
    let Some(row) = ss::AlertRow::find(&state.db, ctx.repo.id, alert_id).await? else {
        return Ok(None);
    };
    let mut out = ss::render(
        state,
        ctx.owner_login(),
        &ctx.repo.name,
        std::slice::from_ref(&row),
    )
    .await
    .map_err(|e| anyhow::anyhow!("rendering secret scanning alert {alert_id}: {e:?}"))?;
    let Some(mut alert) = out.pop() else {
        return Ok(None);
    };
    if let Some(o) = alert.as_object_mut() {
        o.remove("secret");
        o.remove("first_location_detected");
        o.remove("has_more_locations");
    }
    Ok(Some(alert))
}

/// Webhook `location` object by id.
pub async fn secret_scanning_location(
    state: &AppState,
    ctx: &RepoCtx,
    location_id: i64,
) -> anyhow::Result<Option<Value>> {
    let Some(l) = LocationRow::find(&state.db, location_id).await? else {
        return Ok(None);
    };
    Ok(Some(ss::location(
        state,
        ctx.owner_login(),
        &ctx.repo.name,
        &l,
    )))
}
