//! Bootstrap scope provider: project rows of `org:{id}` / `user:{id}`
//! scopes (see `bgh_core::sync::ScopeProvider`).

use bgh_core::sync::{ScopeProvider, ScopeRows};
use futures::FutureExt;
use futures::future::BoxFuture;
use sqlx::PgConnection;

use crate::model::*;

pub const PROVIDER: ScopeProvider = ScopeProvider {
    name: "projects",
    models: &[M_PROJECT, M_FIELD, M_VIEW, M_ITEM, M_WORKFLOW],
    load,
};

fn load<'c>(
    conn: &'c mut PgConnection,
    scope: &'c str,
    viewer: Option<i64>,
) -> BoxFuture<'c, Result<ScopeRows, sqlx::Error>> {
    async move { load_scope(conn, scope, viewer).await }.boxed()
}

/// `(owner id, is org)` of an owner scope.
fn parse_scope(scope: &str) -> Option<(i64, bool)> {
    let (kind, id) = scope.split_once(':')?;
    let id = id.parse().ok()?;
    match kind {
        "org" => Some((id, true)),
        "user" => Some((id, false)),
        _ => None,
    }
}

async fn load_scope(
    conn: &mut PgConnection,
    scope: &str,
    viewer: Option<i64>,
) -> Result<ScopeRows, sqlx::Error> {
    let Some((owner_id, is_org)) = parse_scope(scope) else {
        return Ok(ScopeRows::default());
    };
    // Owners, org members and site admins see every project; others only public ones.
    let full: bool = match viewer {
        Some(v) if v == owner_id => true,
        Some(v) => {
            sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM users WHERE id = $1 AND site_admin)
                     OR ($3 AND EXISTS (SELECT 1 FROM org_members WHERE org_id = $2 AND user_id = $1))",
            )
            .bind(v)
            .bind(owner_id)
            .bind(is_org)
            .fetch_one(&mut *conn)
            .await?
        }
        None => false,
    };
    let projects: Vec<ProjectRow> = sqlx::query_as(&format!(
        "{} WHERE p.owner_id = $1 AND ($2 OR p.public) ORDER BY p.id",
        ProjectRow::SELECT
    ))
    .bind(owner_id)
    .bind(full)
    .fetch_all(&mut *conn)
    .await?;
    if projects.is_empty() {
        return Ok(ScopeRows::default());
    }
    let ids: Vec<i64> = projects.iter().map(|p| p.id).collect();
    let fields: Vec<FieldRow> = sqlx::query_as(&format!(
        "SELECT {} FROM project_fields WHERE project_id = ANY($1) ORDER BY id",
        FieldRow::COLUMNS
    ))
    .bind(&ids)
    .fetch_all(&mut *conn)
    .await?;
    let views: Vec<ViewRow> = sqlx::query_as(&format!(
        "SELECT {} FROM project_views WHERE project_id = ANY($1) ORDER BY id",
        ViewRow::COLUMNS
    ))
    .bind(&ids)
    .fetch_all(&mut *conn)
    .await?;
    let items: Vec<ItemRow> = sqlx::query_as(&format!(
        "{} WHERE i.project_id = ANY($1) ORDER BY i.id",
        ItemRow::SELECT
    ))
    .bind(&ids)
    .fetch_all(&mut *conn)
    .await?;
    let workflows: Vec<WorkflowRow> = sqlx::query_as(&format!(
        "SELECT {} FROM project_workflows WHERE project_id = ANY($1) ORDER BY id",
        WorkflowRow::COLUMNS
    ))
    .bind(&ids)
    .fetch_all(&mut *conn)
    .await?;
    let mut user_ids: Vec<i64> = projects.iter().filter_map(|p| p.creator_id).collect();
    for i in &items {
        user_ids.extend(i.creator_id);
        user_ids.extend(&i.assignee_ids);
    }
    user_ids.sort_unstable();
    user_ids.dedup();
    Ok(ScopeRows {
        models: vec![
            (
                M_PROJECT,
                projects.iter().map(ProjectRow::sync_json).collect(),
            ),
            (M_FIELD, fields.iter().map(FieldRow::sync_json).collect()),
            (M_VIEW, views.iter().map(ViewRow::sync_json).collect()),
            (M_ITEM, items.iter().map(ItemRow::sync_json).collect()),
            (
                M_WORKFLOW,
                workflows.iter().map(WorkflowRow::sync_json).collect(),
            ),
        ],
        user_ids,
    })
}
