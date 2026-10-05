//! Secret scanning alerts (`secret_scanning_alerts`, migration 7700): row
//! types and GitHub's REST shapes (`secret-scanning-alert`,
//! `secret-scanning-location`), shared by the REST API (bgh-security) and
//! the `secret_scanning_alert` webhook payloads (bgh-notify).

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use crate::error::ApiResult;
use crate::models::api::SimpleUser;
use crate::state::AppState;
use crate::time::{Timestamp, ts};

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AlertRow {
    pub id: i64,
    pub repo_id: i64,
    pub number: i64,
    pub secret_type: String,
    pub secret_type_display_name: String,
    pub secret_hash: String,
    pub secret_sealed: Vec<u8>,
    pub custom_pattern_id: Option<i64>,
    pub state: String,
    pub resolution: Option<String>,
    pub resolution_comment: Option<String>,
    pub resolved_by_id: Option<i64>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub push_protection_bypassed: bool,
    pub push_protection_bypassed_by_id: Option<i64>,
    pub push_protection_bypassed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl AlertRow {
    pub const COLUMNS: &'static str = "id, repo_id, number, secret_type, secret_type_display_name, \
        secret_hash, secret_sealed, custom_pattern_id, state, resolution, resolution_comment, \
        resolved_by_id, resolved_at, push_protection_bypassed, push_protection_bypassed_by_id, \
        push_protection_bypassed_at, created_at, updated_at";

    pub async fn find(
        db: impl sqlx::PgExecutor<'_>,
        repo_id: i64,
        id: i64,
    ) -> Result<Option<Self>, sqlx::Error> {
        sqlx::query_as(&format!(
            "SELECT {} FROM secret_scanning_alerts WHERE id = $1 AND repo_id = $2",
            Self::COLUMNS
        ))
        .bind(id)
        .bind(repo_id)
        .fetch_optional(db)
        .await
    }
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct LocationRow {
    pub id: i64,
    pub alert_id: i64,
    pub commit_sha: String,
    pub path: String,
    pub blob_sha: String,
    pub start_line: i32,
    pub end_line: i32,
    pub start_column: i32,
    pub end_column: i32,
    pub created_at: DateTime<Utc>,
}

impl LocationRow {
    pub const COLUMNS: &'static str = "id, alert_id, commit_sha, path, blob_sha, start_line, \
        end_line, start_column, end_column, created_at";

    pub async fn find(db: impl sqlx::PgExecutor<'_>, id: i64) -> Result<Option<Self>, sqlx::Error> {
        sqlx::query_as(&format!(
            "SELECT {} FROM secret_scanning_locations WHERE id = $1",
            Self::COLUMNS
        ))
        .bind(id)
        .fetch_optional(db)
        .await
    }
}

/// `secret-scanning-location` `details` of a commit location.
pub fn location_details(state: &AppState, owner: &str, repo: &str, l: &LocationRow) -> Value {
    let urls = &state.urls;
    json!({
        "path": l.path,
        "start_line": l.start_line,
        "end_line": l.end_line,
        "start_column": l.start_column,
        "end_column": l.end_column,
        "blob_sha": l.blob_sha,
        "blob_url": urls.api(&format!("/repos/{owner}/{repo}/git/blobs/{}", l.blob_sha)),
        "commit_sha": l.commit_sha,
        "commit_url": urls.api(&format!("/repos/{owner}/{repo}/git/commits/{}", l.commit_sha)),
    })
}

/// `secret-scanning-location`.
pub fn location(state: &AppState, owner: &str, repo: &str, l: &LocationRow) -> Value {
    json!({"type": "commit", "details": location_details(state, owner, repo, l)})
}

/// Render alerts of one repository (`owner/repo`) in input order. Users
/// and first locations are batch-loaded.
pub async fn render(
    state: &AppState,
    owner: &str,
    repo: &str,
    rows: &[AlertRow],
) -> ApiResult<Vec<Value>> {
    let users = crate::views::users_by_id(
        state,
        rows.iter()
            .flat_map(|r| [r.resolved_by_id, r.push_protection_bypassed_by_id]),
    )
    .await?;
    let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
    // First location and location count per alert, in one query.
    let firsts: Vec<(i64, i64, LocationRow)> = if ids.is_empty() {
        vec![]
    } else {
        let rows: Vec<FirstLocation> = sqlx::query_as(&format!(
            "SELECT DISTINCT ON (alert_id) count(*) OVER (PARTITION BY alert_id) AS n, {}
               FROM secret_scanning_locations WHERE alert_id = ANY($1)
              ORDER BY alert_id, id",
            LocationRow::COLUMNS
        ))
        .bind(&ids)
        .fetch_all(&state.db)
        .await?;
        rows.into_iter()
            .map(|f| (f.loc.alert_id, f.n, f.loc))
            .collect()
    };
    let firsts: HashMap<i64, (i64, LocationRow)> =
        firsts.into_iter().map(|(a, n, l)| (a, (n, l))).collect();
    let user = |id: Option<i64>| id.map(|id| SimpleUser::or_ghost(&state.urls, users.get(&id)));
    let base = format!("/repos/{owner}/{repo}/secret-scanning/alerts");
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let secret = crate::secretbox::open(state, &r.secret_sealed).unwrap_or_default();
        let first = firsts.get(&r.id);
        out.push(json!({
            "number": r.number,
            "created_at": Timestamp(r.created_at),
            "updated_at": Timestamp(r.updated_at),
            "url": state.urls.api(&format!("{base}/{}", r.number)),
            "html_url": state.urls.html(&format!(
                "/{owner}/{repo}/security/secret-scanning/{}", r.number
            )),
            "locations_url": state.urls.api(&format!("{base}/{}/locations", r.number)),
            "state": r.state,
            "resolution": r.resolution,
            "resolved_at": ts(r.resolved_at),
            "resolved_by": user(r.resolved_by_id),
            "resolution_comment": r.resolution_comment,
            "secret_type": r.secret_type,
            "secret_type_display_name": r.secret_type_display_name,
            "secret": secret,
            "push_protection_bypassed": r.push_protection_bypassed,
            "push_protection_bypassed_by": user(r.push_protection_bypassed_by_id),
            "push_protection_bypassed_at": ts(r.push_protection_bypassed_at),
            "push_protection_bypass_request_reviewer": null,
            "push_protection_bypass_request_reviewer_comment": null,
            "push_protection_bypass_request_comment": null,
            "push_protection_bypass_request_html_url": null,
            "validity": "unknown",
            "publicly_leaked": false,
            "multi_repo": false,
            "is_base64_encoded": false,
            "first_location_detected": first.map(|(_, l)| location_details(state, owner, repo, l)),
            "has_more_locations": first.is_some_and(|(n, _)| *n > 1),
        }));
    }
    Ok(out)
}

#[derive(sqlx::FromRow)]
struct FirstLocation {
    n: i64,
    #[sqlx(flatten)]
    loc: LocationRow,
}

/// One alert by id with its repository's `owner/name` (`None` if gone).
pub async fn render_one(state: &AppState, alert_id: i64) -> ApiResult<Option<(AlertRow, Value)>> {
    let row: Option<AlertRow> = sqlx::query_as(&format!(
        "SELECT {} FROM secret_scanning_alerts WHERE id = $1",
        AlertRow::COLUMNS
    ))
    .bind(alert_id)
    .fetch_optional(&state.db)
    .await?;
    let Some(row) = row else { return Ok(None) };
    let names: Option<(String, String)> = sqlx::query_as(
        "SELECT u.login, r.name FROM repositories r JOIN users u ON u.id = r.owner_id
          WHERE r.id = $1",
    )
    .bind(row.repo_id)
    .fetch_optional(&state.db)
    .await?;
    let Some((owner, repo)) = names else {
        return Ok(None);
    };
    let mut v = render(state, &owner, &repo, std::slice::from_ref(&row)).await?;
    Ok(v.pop().map(|v| (row, v)))
}
