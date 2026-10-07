//! Row structs and their compact sync shapes (camelCase, see
//! `docs/SYNC_PROTOCOL.md` §11).

use bgh_core::time::{Timestamp, ts};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::{FromRow, PgExecutor};

/// Model names recorded in `sync_actions`.
pub const M_PROJECT: &str = "project";
pub const M_FIELD: &str = "projectField";
pub const M_VIEW: &str = "projectView";
pub const M_ITEM: &str = "projectItem";
pub const M_WORKFLOW: &str = "projectWorkflow";

#[derive(Debug, Clone, FromRow)]
pub struct ProjectRow {
    pub id: i64,
    pub owner_id: i64,
    pub number: i64,
    pub title: String,
    pub short_description: Option<String>,
    pub readme: Option<String>,
    pub public: bool,
    pub closed: bool,
    pub closed_at: Option<DateTime<Utc>>,
    pub creator_id: Option<i64>,
    pub linked_repo_ids: Vec<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ProjectRow {
    /// `SELECT` list over `projects p`.
    pub const SELECT: &'static str = "SELECT p.id, p.owner_id, p.number, p.title, \
        p.short_description, p.readme, p.public, p.closed, p.closed_at, p.creator_id, \
        ARRAY(SELECT l.repo_id FROM project_linked_repos l WHERE l.project_id = p.id ORDER BY l.repo_id) \
        AS linked_repo_ids, p.created_at, p.updated_at FROM projects p";

    pub async fn find(db: impl PgExecutor<'_>, id: i64) -> Result<Option<Self>, sqlx::Error> {
        sqlx::query_as(&format!("{} WHERE p.id = $1", Self::SELECT))
            .bind(id)
            .fetch_optional(db)
            .await
    }

    pub fn sync_json(&self) -> Value {
        json!({
            "id": self.id,
            "ownerId": self.owner_id,
            "number": self.number,
            "title": self.title,
            "shortDescription": self.short_description,
            "readme": self.readme,
            "public": self.public,
            "closed": self.closed,
            "closedAt": ts(self.closed_at),
            "creatorId": self.creator_id,
            "linkedRepoIds": self.linked_repo_ids,
            "createdAt": Timestamp(self.created_at),
            "updatedAt": Timestamp(self.updated_at),
        })
    }
}

#[derive(Debug, Clone, FromRow)]
pub struct FieldRow {
    pub id: i64,
    pub project_id: i64,
    pub name: String,
    pub data_type: String,
    pub position: i32,
    pub options: Option<Value>,
    pub iterations: Option<Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl FieldRow {
    pub const COLUMNS: &'static str =
        "id, project_id, name, data_type, position, options, iterations, created_at, updated_at";

    pub fn sync_json(&self) -> Value {
        json!({
            "id": self.id,
            "projectId": self.project_id,
            "name": self.name,
            "dataType": self.data_type,
            "position": self.position,
            "options": self.options,
            "iterations": self.iterations,
            "createdAt": Timestamp(self.created_at),
            "updatedAt": Timestamp(self.updated_at),
        })
    }

    pub fn is_builtin(&self) -> bool {
        matches!(
            self.data_type.as_str(),
            "title" | "assignees" | "labels" | "repository" | "milestone"
        )
    }

    pub fn is_select(&self) -> bool {
        matches!(self.data_type.as_str(), "single_select" | "status")
    }

    /// Ids of the single-select options.
    pub fn option_ids(&self) -> Vec<String> {
        ids_of(self.options.as_ref())
    }

    /// Ids of the iterations.
    pub fn iteration_ids(&self) -> Vec<String> {
        ids_of(self.iterations.as_ref().and_then(|c| c.get("iterations")))
    }
}

fn ids_of(list: Option<&Value>) -> Vec<String> {
    list.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|o| o.get("id").and_then(Value::as_str).map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

#[derive(Debug, Clone, FromRow)]
pub struct ViewRow {
    pub id: i64,
    pub project_id: i64,
    pub number: i64,
    pub name: String,
    pub layout: String,
    pub position: i32,
    pub filter: String,
    pub group_by_field_id: Option<i64>,
    pub column_field_id: Option<i64>,
    pub date_field_id: Option<i64>,
    pub sort_by: Value,
    pub visible_field_ids: Vec<i64>,
    pub hidden_column_ids: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ViewRow {
    pub const COLUMNS: &'static str = "id, project_id, number, name, layout, position, filter, \
        group_by_field_id, column_field_id, date_field_id, sort_by, visible_field_ids, \
        hidden_column_ids, created_at, updated_at";

    pub fn sync_json(&self) -> Value {
        json!({
            "id": self.id,
            "projectId": self.project_id,
            "number": self.number,
            "name": self.name,
            "layout": self.layout,
            "position": self.position,
            "filter": self.filter,
            "groupByFieldId": self.group_by_field_id,
            "columnFieldId": self.column_field_id,
            "dateFieldId": self.date_field_id,
            "sortBy": self.sort_by,
            "visibleFieldIds": self.visible_field_ids,
            "hiddenColumnIds": self.hidden_column_ids,
            "createdAt": Timestamp(self.created_at),
            "updatedAt": Timestamp(self.updated_at),
        })
    }
}

#[derive(Debug, Clone, FromRow)]
pub struct ItemRow {
    pub id: i64,
    pub project_id: i64,
    pub content_type: String,
    pub issue_id: Option<i64>,
    pub title: Option<String>,
    pub body: Option<String>,
    pub assignee_ids: Vec<i64>,
    pub archived: bool,
    pub archived_at: Option<DateTime<Utc>>,
    pub position: String,
    pub view_positions: Value,
    pub field_values: Value,
    pub creator_id: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ItemRow {
    /// `SELECT` list over `project_items i` (values aggregated per item).
    pub const SELECT: &'static str = "SELECT i.id, i.project_id, i.content_type, i.issue_id, \
        i.title, i.body, i.assignee_ids, i.archived, i.archived_at, i.position, i.view_positions, \
        COALESCE((SELECT jsonb_object_agg(v.field_id::text, v.value) FROM project_item_values v \
                  WHERE v.item_id = i.id), '{}'::jsonb) AS field_values, \
        i.creator_id, i.created_at, i.updated_at FROM project_items i";

    pub async fn find(db: impl PgExecutor<'_>, id: i64) -> Result<Option<Self>, sqlx::Error> {
        sqlx::query_as(&format!("{} WHERE i.id = $1", Self::SELECT))
            .bind(id)
            .fetch_optional(db)
            .await
    }

    pub async fn find_many(db: impl PgExecutor<'_>, ids: &[i64]) -> Result<Vec<Self>, sqlx::Error> {
        sqlx::query_as(&format!(
            "{} WHERE i.id = ANY($1) ORDER BY i.id",
            Self::SELECT
        ))
        .bind(ids)
        .fetch_all(db)
        .await
    }

    pub fn is_draft(&self) -> bool {
        self.content_type == "DraftIssue"
    }

    pub fn sync_json(&self) -> Value {
        let draft = self.is_draft();
        json!({
            "id": self.id,
            "projectId": self.project_id,
            "contentType": self.content_type,
            "issueId": self.issue_id,
            "title": if draft { json!(self.title) } else { Value::Null },
            "body": if draft { json!(self.body) } else { Value::Null },
            "assigneeIds": self.assignee_ids,
            "archived": self.archived,
            "position": self.position,
            "viewPositions": self.view_positions,
            "values": self.field_values,
            "creatorId": self.creator_id,
            "createdAt": Timestamp(self.created_at),
            "updatedAt": Timestamp(self.updated_at),
        })
    }
}

#[derive(Debug, Clone, FromRow)]
pub struct WorkflowRow {
    pub id: i64,
    pub project_id: i64,
    pub kind: String,
    pub enabled: bool,
    pub config: Value,
    pub updated_at: DateTime<Utc>,
}

impl WorkflowRow {
    pub const COLUMNS: &'static str = "id, project_id, kind, enabled, config, updated_at";

    pub fn sync_json(&self) -> Value {
        json!({
            "id": self.id,
            "projectId": self.project_id,
            "kind": self.kind,
            "enabled": self.enabled,
            "config": self.config,
            "updatedAt": Timestamp(self.updated_at),
        })
    }
}

pub const WORKFLOW_KINDS: &[&str] = &[
    "item_added",
    "item_reopened",
    "item_closed",
    "pr_merged",
    "auto_add",
    "auto_archive",
];

/// Sync scope of a project owner (`org:{id}` or `user:{id}`).
pub fn owner_scope(owner_id: i64, owner_is_org: bool) -> String {
    if owner_is_org {
        bgh_core::sync::org_scope(owner_id)
    } else {
        bgh_core::sync::user_scope(owner_id)
    }
}
